// SPDX-License-Identifier: GPL-3.0-only

//! Commands on one session: `info`, `kill`, `dismiss`, `rename`, `annotate`.

use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{fail, usage_error};
use crate::client::{Client, ClientError, INFO_TIMEOUT};
use crate::runtime::{self, Entry, Housekeeping, RuntimeDir};
use crate::sys::{self, Lock};
use crate::{clock, procinfo, supervisor};

/// The label control clients send in `Hello`.
pub(super) const LABEL: &str = "perch-cli";
/// How long other control requests may take.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens the runtime directory and finds a session.
pub(super) fn find(query: &str) -> Result<(RuntimeDir, Entry), String> {
    let rt = runtime::open_default()?;
    let entries = runtime::scan(&rt, Housekeeping::MarkLost);
    let entry = runtime::resolve(entries, query)?;
    Ok((rt, entry))
}

pub(super) fn connect(
    rt: &RuntimeDir,
    e: &Entry,
    timeout: Duration,
) -> Result<Client, ClientError> {
    Client::connect(&e.path(rt).join("sock"), LABEL, timeout)
}

/// Asks a live supervisor for its live fields, within 500 ms in all.
pub(super) fn query_info(rt: &RuntimeDir, e: &Entry) -> Result<Value, ClientError> {
    let deadline = Instant::now() + INFO_TIMEOUT;
    let mut c = connect(rt, e, INFO_TIMEOUT)?;
    let left = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1));
    c.request("info", json!({}), left)
}

/// Live fields `info` returns that a session view takes over.
const LIVE_FIELDS: &[&str] = &[
    "state",
    "exit",
    "attached",
    "clientLabel",
    "clientPid",
    "rows",
    "cols",
    "lastOutputAt",
    "lastInputAt",
    "model",
    "memoryBytes",
    "cwd",
    "foreground",
];

/// A session as `list --json` and `info --json` show it: the metadata, the
/// directory, and the live fields if the supervisor answered. A live
/// session that did not answer in time is `unresponsive`.
pub(super) fn view(rt: &RuntimeDir, e: &Entry, info: Option<&Value>) -> Value {
    let mut v = serde_json::to_value(&e.meta).unwrap_or_else(|_| json!({}));
    let obj = v.as_object_mut().expect("metadata is an object");
    obj.insert("dir".into(), json!(e.path(rt).to_string_lossy()));
    obj.insert("attached".into(), json!(false));
    match (e.live, info) {
        (true, Some(info)) => {
            for &k in LIVE_FIELDS {
                if let Some(x) = info.get(k) {
                    obj.insert(k.into(), x.clone());
                }
            }
        }
        (true, None) => {
            obj.insert("state".into(), json!("unresponsive"));
        }
        (false, _) => {}
    }
    v
}

fn one_session(
    cmd: &str,
    args: &[String],
    flags: &[&str],
) -> Result<(String, Vec<String>), ExitCode> {
    let mut session = None;
    let mut seen = Vec::new();
    for a in args {
        if flags.contains(&a.as_str()) {
            seen.push(a.clone());
        } else if a.starts_with("--") {
            return Err(usage_error(&format!("{cmd}: unknown option `{a}`")));
        } else if session.is_none() {
            session = Some(a.clone());
        } else {
            return Err(usage_error(&format!("{cmd}: unexpected argument `{a}`")));
        }
    }
    let session = session.ok_or_else(|| usage_error(&format!("{cmd}: which session?")))?;
    Ok((session, seen))
}

// ---------------------------------------------------------------------------

/// `12 MB`, `1.5 GB`: decimal units, as memory is shown elsewhere.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "kB", "MB", "GB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 || v >= 10.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

pub(super) fn info(args: &[String]) -> ExitCode {
    let (query, flags) = match one_session("info", args, &["--json"]) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let (rt, e) = match find(&query) {
        Ok(x) => x,
        Err(msg) => return fail(&msg),
    };
    let live = e.live.then(|| query_info(&rt, &e).ok()).flatten();
    let v = view(&rt, &e, live.as_ref());
    if flags.iter().any(|f| f == "--json") {
        println!("{v:#}");
        return ExitCode::SUCCESS;
    }
    let s = |k: &str| match &v[k] {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut lines = vec![
        ("id", s("id")),
        ("name", s("name")),
        ("kind", s("kind")),
        ("state", super::list::state_text(&v)),
        ("pid", s("pid")),
        ("cwd", s("cwd")),
        ("dir", s("dir")),
        ("command", e.meta.command.join(" ")),
        ("created", s("createdAt")),
        ("host", s("host")),
    ];
    if let (Some(r), Some(c)) = (v["rows"].as_u64(), v["cols"].as_u64()) {
        lines.push(("size", format!("{c}x{r}")));
    }
    if let Some(o) = &e.meta.owner {
        lines.push(("owner", o.clone()));
    }
    if !s("lastOutputAt").is_empty() {
        lines.push(("last output", s("lastOutputAt")));
    }
    if let Some(bytes) = v["memoryBytes"].as_u64() {
        lines.push(("memory", format!("about {}", human_bytes(bytes))));
    }
    if let Some(name) = v["foreground"]["name"].as_str() {
        lines.push(("foreground", format!("{name} ({})", v["foreground"]["pid"])));
    }
    if let Some(m) = v["model"].as_str() {
        lines.push(("model", m.to_owned()));
    }
    for (k, val) in &e.meta.annotations {
        lines.push(("annotation", format!("{k}={val}")));
    }
    if !e.meta.degraded.is_empty() {
        lines.push(("degraded", e.meta.degraded.join(", ")));
    }
    for (k, val) in lines {
        println!("{:<12} {val}", format!("{k}:"));
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------

pub(super) fn kill(args: &[String]) -> ExitCode {
    let (query, flags) = match one_session("kill", args, &["--force", "--pid"]) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let force = flags.iter().any(|f| f == "--force");
    let by_pid = flags.iter().any(|f| f == "--pid");
    let (rt, e) = match find(&query) {
        Ok(x) => x,
        Err(msg) => return fail(&msg),
    };
    if !e.live {
        return fail(&format!(
            "session {} has already ended ({}); `perch dismiss {}` removes it",
            e.meta.id, e.meta.state, e.meta.id
        ));
    }
    let result = if by_pid {
        kill_by_pid(&rt, &e, force)
    } else {
        kill_by_request(&rt, &e, force)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => fail(&msg),
    }
}

fn kill_by_request(rt: &RuntimeDir, e: &Entry, force: bool) -> Result<(), String> {
    let unreachable = |err: ClientError| {
        format!(
            "cannot reach the supervisor of {}: {err}. `perch kill --pid {}` signals it directly.",
            e.meta.id, e.meta.id
        )
    };
    let mut c = connect(rt, e, REQUEST_TIMEOUT).map_err(unreachable)?;
    c.request("kill", json!({ "force": force }), REQUEST_TIMEOUT)
        .map_err(unreachable)?;
    // The session ends after the grace period at most.
    let wait = clock::scaled(supervisor::KILL_GRACE) + Duration::from_secs(5);
    c.wait_closed(wait)
        .map_err(|err| format!("the supervisor of {} did not finish: {err}", e.meta.id))
}

/// Whether the supervisor still holds the session's lock.
fn lock_held(rt: &RuntimeDir, e: &Entry) -> bool {
    let Ok(fd) = sys::open_dir_at(rt.fd.as_fd(), &e.dir_name) else {
        return false;
    };
    !sys::try_flock(fd.as_fd(), Lock::Shared).unwrap_or(true)
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Recovery for a supervisor no client can talk to: signal the program's
/// session, then the supervisor, using the PIDs in `meta.json`, after
/// checking each PID's owner and start time and that the lock is held.
fn kill_by_pid(rt: &RuntimeDir, e: &Entry, force: bool) -> Result<(), String> {
    let uid = sys::getuid();
    let m = &e.meta;
    let check = |what: &str, pid: i32, start: &str| -> Result<bool, String> {
        if pid <= 0 {
            return Ok(false);
        }
        match procinfo::start_time(pid) {
            None => Ok(false),
            Some(now) if now != start => Err(format!(
                "{what} PID {pid} has a different start time than recorded ({now} vs {start}): \
                 it is another process, so Perch leaves it alone"
            )),
            Some(_) if procinfo::real_uid(pid) != Some(uid) => Err(format!(
                "{what} PID {pid} does not belong to you; leaving it alone"
            )),
            Some(_) => Ok(true),
        }
    };
    let program = match (m.pid, &m.pid_start) {
        (Some(pid), Some(start)) => check("program", pid, start)?.then_some(pid),
        _ => None,
    };
    let sup =
        check("supervisor", m.supervisor_pid, &m.supervisor_start)?.then_some(m.supervisor_pid);
    if !lock_held(rt, e) {
        return Err(format!(
            "the supervisor of {} no longer holds its lock; nothing to signal",
            m.id
        ));
    }

    if let Some(pid) = program {
        // Re-check right before signaling.
        if procinfo::is_same_process(pid, m.pid_start.as_deref().unwrap_or(""), uid) {
            let scope = || {
                let mut s = procinfo::session_members(pid);
                if !s.contains(&pid)
                    && procinfo::is_same_process(pid, m.pid_start.as_deref().unwrap_or(""), uid)
                {
                    s.push(pid);
                }
                s
            };
            if !force {
                for p in scope() {
                    let _ = sys::kill(p, sys::sig::HUP);
                    let _ = sys::kill(p, sys::sig::CONT);
                }
                wait_until(clock::scaled(supervisor::KILL_GRACE), || scope().is_empty());
            }
            for p in scope() {
                let _ = sys::kill(p, sys::sig::KILL);
            }
        }
    }
    if let Some(pid) = sup
        && procinfo::is_same_process(pid, &m.supervisor_start, uid)
    {
        let _ = sys::kill(pid, sys::sig::TERM);
        let _ = sys::kill(pid, sys::sig::CONT);
        if !wait_until(Duration::from_secs(2), || !lock_held(rt, e))
            && procinfo::is_same_process(pid, &m.supervisor_start, uid)
        {
            let _ = sys::kill(pid, sys::sig::KILL);
        }
    }
    if !wait_until(Duration::from_secs(5), || !lock_held(rt, e)) {
        return Err(format!("the supervisor of {} still holds its lock", m.id));
    }
    // The user asked for it: leave no tombstone behind.
    remove_session_dir(rt, e)
}

/// Removes an ended session's directory under an exclusive lock.
fn remove_session_dir(rt: &RuntimeDir, e: &Entry) -> Result<(), String> {
    let fd = match sys::open_dir_at(rt.fd.as_fd(), &e.dir_name) {
        Ok(fd) => fd,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.to_string()),
    };
    // A concurrent `list` may hold a shared lock for a moment.
    if !wait_until(Duration::from_secs(2), || {
        sys::try_flock(fd.as_fd(), Lock::Exclusive).unwrap_or(false)
    }) {
        return Err(format!(
            "session {} is locked; is it still running?",
            e.meta.id
        ));
    }
    let m = crate::meta::read(fd.as_fd()).map_err(|err| format!("cannot read meta.json: {err}"))?;
    if m.id != e.meta.id {
        return Err(format!(
            "{} now holds another session",
            e.path(rt).display()
        ));
    }
    runtime::remove_locked(rt.fd.as_fd(), &e.dir_name, fd.as_fd())
        .map_err(|err| format!("cannot remove {}: {err}", e.path(rt).display()))
}

// ---------------------------------------------------------------------------

pub(super) fn dismiss(args: &[String]) -> ExitCode {
    let (query, _) = match one_session("dismiss", args, &[]) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let (rt, e) = match find(&query) {
        Ok(x) => x,
        Err(msg) => return fail(&msg),
    };
    if e.live {
        return fail(&format!(
            "session {} is running; `perch kill {}` ends it",
            e.meta.id, e.meta.id
        ));
    }
    match remove_session_dir(&rt, &e) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => fail(&msg),
    }
}

fn control(rt: &RuntimeDir, e: &Entry, op: &str, fields: Value) -> Result<Value, String> {
    if !e.live {
        return Err(format!(
            "session {} has ended ({})",
            e.meta.id, e.meta.state
        ));
    }
    let mut c = connect(rt, e, REQUEST_TIMEOUT).map_err(|err| format!("{}: {err}", e.meta.id))?;
    if !c.welcome.capabilities.iter().any(|cap| cap == op) {
        return Err(format!(
            "the supervisor of {} does not support `{op}`; it runs an older Perch",
            e.meta.id
        ));
    }
    c.request(op, fields, REQUEST_TIMEOUT)
        .map_err(|err| format!("{}: {err}", e.meta.id))
}

pub(super) fn rename(args: &[String]) -> ExitCode {
    let [query, name] = args else {
        return usage_error("usage: perch rename <session> <name>");
    };
    if name.is_empty() {
        return usage_error("rename: the name must not be empty");
    }
    let (rt, e) = match find(query) {
        Ok(x) => x,
        Err(msg) => return fail(&msg),
    };
    match control(&rt, &e, "rename", json!({ "name": name })) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => fail(&msg),
    }
}

pub(super) fn annotate(args: &[String]) -> ExitCode {
    let mut query = None;
    let mut set = BTreeMap::new();
    let mut unset = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--unset" {
            match it.next() {
                Some(k) if !k.is_empty() => unset.push(k.clone()),
                _ => return usage_error("annotate: --unset needs a key"),
            }
        } else if let Some(k) = a.strip_prefix("--unset=") {
            unset.push(k.to_owned());
        } else if query.is_none() {
            query = Some(a.clone());
        } else {
            match a.split_once('=') {
                Some((k, v)) if !k.is_empty() => {
                    set.insert(k.to_owned(), v.to_owned());
                }
                _ => return usage_error(&format!("annotate: `{a}` is not K=V")),
            }
        }
    }
    let Some(query) = query else {
        return usage_error("usage: perch annotate <session> K=V... [--unset K]");
    };
    if set.is_empty() && unset.is_empty() {
        return usage_error("annotate: nothing to set or unset");
    }
    let (rt, e) = match find(&query) {
        Ok(x) => x,
        Err(msg) => return fail(&msg),
    };
    match control(&rt, &e, "annotate", json!({ "set": set, "unset": unset })) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => fail(&msg),
    }
}
