// SPDX-License-Identifier: GPL-3.0-only

//! `perch attach` and `perch detach`, and the attach that ends `perch new`.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::json;

use super::{Args, fail, usage_error};
use crate::client::Client;
use crate::client::attach::{self, DetachKey, Options};
use crate::proto::RestoreMode;
use crate::runtime::{self, Entry, Housekeeping, RuntimeDir};
use crate::{procinfo, sys};

/// History rows a `full` restore sends when `--scrollback` is not given.
pub const DEFAULT_SCROLLBACK: u32 = 1000;

/// The CLI's client label: `tty:<ttyname>`, or `tty:none`.
pub fn default_label() -> String {
    let tty = (0..3).find_map(sys::ttyname);
    format!("tty:{}", tty.as_deref().unwrap_or("none"))
}

/// `--restore full|screen`.
pub fn parse_restore(v: &str) -> Result<RestoreMode, String> {
    match v {
        "full" => Ok(RestoreMode::Full),
        "screen" => Ok(RestoreMode::Screen),
        _ => Err(format!("--restore `{v}`: full or screen")),
    }
}

/// `--scrollback LINES`.
pub fn parse_scrollback(v: &str) -> Result<u32, String> {
    let n: u64 = v
        .parse()
        .map_err(|_| format!("--scrollback `{v}` is not a number of lines"))?;
    Ok(u32::try_from(n.min(u64::from(super::new::MAX_SCROLLBACK))).expect("clamped"))
}

/// The session this process runs in, if `PERCH_SESSION_ID` names one and
/// this process really is in it: its terminal session is the program's
/// (`getsid`). A process that left with its own `setsid` keeps the variable
/// but is not trusted.
pub fn current_session(rt: &RuntimeDir, entries: &[Entry]) -> Option<String> {
    let _ = rt;
    let id = std::env::var("PERCH_SESSION_ID").ok()?;
    let e = entries.iter().find(|e| e.meta.id == id && e.live)?;
    let pid = e.meta.pid?;
    let start = e.meta.pid_start.as_deref()?;
    let sid = sys::getsid(0).ok()?;
    (sid == pid && procinfo::start_time(pid).as_deref() == Some(start)).then_some(id)
}

/// `perch attach <session> [--label L] [--if-detached] [--detach-key K|none]
/// [--restore full|screen] [--scrollback LINES]`.
pub(super) fn run(args: &[String]) -> ExitCode {
    let mut session = None;
    let mut label = None;
    let mut if_detached = false;
    let mut key = Some(DetachKey::DEFAULT);
    let mut restore = RestoreMode::Screen;
    let mut scrollback = DEFAULT_SCROLLBACK;
    let mut a = Args::new(args);
    let parsed: Result<(), String> = (|| {
        while let Some(flag) = a.next_arg() {
            match flag {
                "--label" => label = Some(a.value(flag)?.to_owned()),
                "--if-detached" => if_detached = true,
                "--detach-key" => key = DetachKey::parse(a.value(flag)?)?,
                "--restore" => restore = parse_restore(a.value(flag)?)?,
                "--scrollback" => scrollback = parse_scrollback(a.value(flag)?)?,
                f if f.starts_with('-') => return Err(format!("attach: unknown option `{f}`")),
                s if session.is_none() => session = Some(s.to_owned()),
                s => return Err(format!("attach: unexpected argument `{s}`")),
            }
        }
        Ok(())
    })();
    if let Err(e) = parsed {
        return usage_error(&e);
    }
    let Some(session) = session else {
        return usage_error("attach: which session?");
    };
    let rt = match runtime::open_default() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    let entries = runtime::scan(&rt, Housekeeping::MarkLost);
    if let Some(here) = current_session(&rt, &entries)
        && let Ok(e) = runtime::resolve(entries.clone(), &session)
        && e.meta.id == here
    {
        return fail(&format!(
            "cannot attach session {here} from inside itself; detach with `perch detach`"
        ));
    }
    let e = match runtime::resolve(entries, &session) {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    if !e.live {
        return fail(&format!(
            "session {} has ended ({}); `perch dismiss {}` removes it",
            e.meta.id, e.meta.state, e.meta.id
        ));
    }
    let opts = Options {
        sock: e.path(&rt).join("sock"),
        label: label.unwrap_or_else(default_label),
        restore,
        reattach: restore,
        scrollback,
        if_detached,
        detach_key: key,
    };
    drop(rt);
    ExitCode::from(attach::run(&opts))
}

/// `perch detach [<session>]`, by default the session it runs in.
pub(super) fn detach(args: &[String]) -> ExitCode {
    let session = match args {
        [] => None,
        [s] if !s.starts_with('-') => Some(s.clone()),
        _ => return usage_error("usage: perch detach [<session>]"),
    };
    let rt = match runtime::open_default() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    let entries = runtime::scan(&rt, Housekeeping::MarkLost);
    let query = match session {
        Some(s) => s,
        None => match current_session(&rt, &entries) {
            Some(id) => id,
            None if std::env::var_os("PERCH_SESSION_ID").is_some() => {
                return fail(
                    "PERCH_SESSION_ID is set, but this process is not in that session; name the session",
                );
            }
            None => return usage_error("detach: which session? (not inside one)"),
        },
    };
    let e = match runtime::resolve(entries, &query) {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    let sock = e.path(&rt).join("sock");
    match detach_session(&sock) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => fail(&format!("no terminal is attached to {}", e.meta.id)),
        Err(err) => fail(&err),
    }
}

fn detach_session(sock: &Path) -> Result<bool, String> {
    let timeout = Duration::from_secs(5);
    let mut c = Client::connect(sock, "perch-cli", timeout).map_err(|e| e.to_string())?;
    let r = c
        .request("detach", json!({}), timeout)
        .map_err(|e| e.to_string())?;
    Ok(r["detached"] == json!(true))
}
