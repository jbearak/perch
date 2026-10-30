// SPDX-License-Identifier: GPL-3.0-only

//! `perch new`: start a session, and attach to it unless `-d`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::attach::{default_label, parse_restore};
use super::{Args, fail, usage_error};
use crate::client::attach::{self, DetachKey};
use crate::proto::RestoreMode;
use crate::supervisor::{self, Config, Outcome};
use crate::sys;
use crate::{env_links, runtime};

/// History lines kept when `--scrollback` is not given.
pub const DEFAULT_SCROLLBACK: u32 = 1000;
/// The most history lines a session keeps.
pub const MAX_SCROLLBACK: u32 = 10_000;
/// The PTY size without a creator terminal.
const DEFAULT_SIZE: sys::Winsize = sys::Winsize { rows: 24, cols: 80 };

#[derive(Default)]
struct Options {
    id: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    owner: Option<String>,
    annotations: BTreeMap<String, String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    /// `--link-env`, or `None` for the default list.
    link_env: Option<Vec<String>>,
    scrollback: Option<u32>,
    detached: bool,
    label: Option<String>,
    detach_key: Option<Option<DetachKey>>,
    restore: Option<RestoreMode>,
    command: Vec<String>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut a = Args::new(args);
    while let Some(flag) = a.next_arg() {
        match flag {
            "--" => {
                o.command = a.rest().to_vec();
                break;
            }
            "-d" | "--detached" => o.detached = true,
            "--id" => {
                let id = a.value(flag)?;
                if !runtime::valid_id(id) {
                    return Err(format!("--id `{id}`: an ID is 8 characters from [a-z0-9]"));
                }
                o.id = Some(id.to_owned());
            }
            "--name" => o.name = Some(non_empty(flag, a.value(flag)?)?),
            "--kind" => o.kind = Some(a.value(flag)?.to_owned()),
            "--owner" => o.owner = Some(non_empty(flag, a.value(flag)?)?),
            "--annotate" => {
                let (k, v) = key_value(flag, a.value(flag)?)?;
                o.annotations.insert(k, v);
            }
            "--cwd" => o.cwd = Some(non_empty(flag, a.value(flag)?)?),
            "--env" => {
                let (k, v) = key_value(flag, a.value(flag)?)?;
                if k.contains('\0') || v.contains('\0') {
                    return Err("--env: no NUL bytes".into());
                }
                o.env.push((k, v));
            }
            "--scrollback" => {
                o.scrollback = Some(super::attach::parse_scrollback(a.value(flag)?)?);
            }
            // How it attaches, and re-attaches after a takeover.
            "--label" => o.label = Some(a.value(flag)?.to_owned()),
            "--detach-key" => o.detach_key = Some(DetachKey::parse(a.value(flag)?)?),
            "--restore" => o.restore = Some(parse_restore(a.value(flag)?)?),
            "--link-env" => {
                let name = a.value(flag)?;
                if !env_links::valid_name(name) {
                    return Err(format!("--link-env `{name}`: not a variable name"));
                }
                let list = o.link_env.get_or_insert_with(Vec::new);
                if !list.iter().any(|n| n == name) {
                    list.push(name.to_owned());
                }
            }
            f if f.starts_with('-') => return Err(format!("unknown option `{f}`")),
            _ => {
                // The command starts here.
                a.i -= 1;
                o.command = a.rest().to_vec();
                break;
            }
        }
    }
    if o.command.is_empty() {
        return Err("missing command: perch new [options] -- <command> [args...]".into());
    }
    Ok(o)
}

fn non_empty(flag: &str, v: &str) -> Result<String, String> {
    if v.is_empty() {
        Err(format!("{flag} must not be empty"))
    } else {
        Ok(v.to_owned())
    }
}

fn key_value(flag: &str, v: &str) -> Result<(String, String), String> {
    match v.split_once('=') {
        Some((k, val)) if !k.is_empty() => Ok((k.to_owned(), val.to_owned())),
        _ => Err(format!("{flag} `{v}`: expected NAME=VALUE")),
    }
}

/// Clamps a terminal size to 1 through 500 in each direction.
pub fn clamp_size(ws: sys::Winsize) -> sys::Winsize {
    sys::Winsize {
        rows: ws.rows.clamp(1, 500),
        cols: ws.cols.clamp(1, 500),
    }
}

/// The creator's terminal: the first of stdin, stdout and stderr that is one.
fn creator_terminal() -> Option<(sys::Winsize, Option<sys::Termios>)> {
    (0..3).find(|&fd| sys::isatty(fd)).map(|fd| {
        // 0 by 0 means the size was never set.
        let size = sys::get_winsize(fd)
            .filter(|ws| ws.rows != 0 || ws.cols != 0)
            .unwrap_or(DEFAULT_SIZE);
        (clamp_size(size), sys::tcgetattr(fd))
    })
}

/// The creator's working directory: `$PWD` when it names the same
/// directory (keeping the user's symlinked path, as shells do), else
/// `getcwd`.
fn creator_cwd() -> Result<PathBuf, String> {
    let real =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    if let Some(pwd) = std::env::var_os("PWD").map(PathBuf::from)
        && pwd.is_absolute()
        && let (Ok(a), Ok(b)) = (std::fs::metadata(&pwd), std::fs::metadata(&real))
    {
        use std::os::unix::fs::MetadataExt;
        if a.dev() == b.dev() && a.ino() == b.ino() {
            return Ok(pwd);
        }
    }
    Ok(real)
}

pub(super) fn run(args: &[String]) -> ExitCode {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => return usage_error(&e),
    };
    match start(o) {
        Ok(code) => code,
        Err(e) => fail(&e),
    }
}

fn start(o: Options) -> Result<ExitCode, String> {
    let rt = runtime::open_default()?;
    let rt_str = rt
        .path
        .to_str()
        .ok_or("the runtime directory's path is not valid UTF-8")?
        .to_owned();
    let id = match o.id {
        Some(id) => id,
        None => loop {
            let id = runtime::new_id();
            if sys::fstatat(std::os::fd::AsFd::as_fd(&rt.fd), id.as_ref()).is_err() {
                break id;
            }
        },
    };
    let dir = format!("{rt_str}/{id}");
    let expand = |s: &str| runtime::expand_placeholders(s, &dir, &id);

    let command = o
        .command
        .iter()
        .map(|c| expand(c))
        .collect::<Result<Vec<_>, _>>()?;
    let cwd = match &o.cwd {
        Some(c) => {
            let p = Path::new(c);
            if p.is_absolute() {
                p.to_owned()
            } else {
                creator_cwd()?.join(p)
            }
        }
        None => creator_cwd()?,
    };
    if !cwd.is_dir() {
        return Err(format!("{} is not a directory", cwd.display()));
    }

    // The creator's environment, then --env, then Perch's own variables.
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let set = |env: &mut Vec<(OsString, OsString)>, k: OsString, v: OsString| {
        env.retain(|(n, _)| *n != k);
        env.push((k, v));
    };
    if o.cwd.is_some() {
        set(&mut env, "PWD".into(), cwd.clone().into_os_string());
    }
    for (k, v) in &o.env {
        set(&mut env, k.into(), expand(v)?.into());
    }
    // Each linked variable that is set becomes the path of its link.
    let names: Vec<String> = o
        .link_env
        .unwrap_or_else(|| env_links::DEFAULT.iter().map(|&n| n.to_owned()).collect());
    let mut links = BTreeMap::new();
    for name in &names {
        let current = env
            .iter()
            .find(|(k, _)| k.as_os_str() == name.as_str())
            .map(|(_, v)| v.clone());
        if let Some(value) = current.filter(|v| !v.is_empty()) {
            set(
                &mut env,
                name.into(),
                format!("{dir}/{}/{name}", env_links::DIR).into(),
            );
            links.insert(name.clone(), value);
        }
    }
    set(&mut env, "PERCH_SESSION_ID".into(), id.clone().into());
    set(&mut env, "PERCH_SESSION_DIR".into(), dir.clone().into());
    let env: Vec<OsString> = env
        .into_iter()
        .map(|(k, v)| {
            let mut e = k.into_vec();
            e.push(b'=');
            e.extend(v.into_vec());
            OsString::from_vec(e)
        })
        .collect();

    let name = o.name.unwrap_or_else(|| {
        Path::new(&command[0])
            .file_name()
            .map_or_else(|| command[0].clone(), |n| n.to_string_lossy().into_owned())
    });
    let (size, termios) = creator_terminal().unwrap_or((DEFAULT_SIZE, None));
    let scrollback = o.scrollback.unwrap_or(DEFAULT_SCROLLBACK);
    let cfg = Config {
        runtime: rt.path.clone(),
        id: id.clone(),
        name,
        kind: o.kind.unwrap_or_default(),
        owner: o.owner,
        annotations: o.annotations,
        command,
        env,
        cwd,
        umask: sys::current_umask(),
        size,
        termios,
        scrollback,
        attach: !o.detached,
        links,
    };
    let sock = rt.path.join(&id).join("sock");
    // The creator's own attach sends the values the links start with.
    let link_values = env_links::client_values(&cfg.links.keys().cloned().collect::<Vec<_>>());
    drop(rt);
    match supervisor::start(cfg)? {
        Outcome::Running { .. } => {
            println!("{id}");
            Ok(ExitCode::SUCCESS)
        }
        Outcome::Ready => {
            // Test hook: a creator that dies before it attaches.
            #[cfg(feature = "test-hooks")]
            if std::env::var_os("PERCH_TEST_NO_ATTACH").is_some() {
                println!("{id}");
                return Ok(ExitCode::SUCCESS);
            }
            let opts = attach::Options {
                sock,
                label: o.label.unwrap_or_else(default_label),
                restore: RestoreMode::None,
                reattach: o.restore.unwrap_or(RestoreMode::Screen),
                scrollback,
                if_detached: false,
                detach_key: o.detach_key.unwrap_or(Some(DetachKey::DEFAULT)),
                env: link_values,
            };
            Ok(ExitCode::from(attach::run(&opts)))
        }
        Outcome::StartFailed { message, exit_code } => {
            eprintln!("perch: {message}");
            Ok(ExitCode::from(exit_code))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|&a| a.to_owned()).collect()
    }

    #[test]
    fn parses_options_and_command() {
        let o = parse(&args(&[
            "-d",
            "--name",
            "R",
            "--kind=r",
            "--annotate",
            "a=b=c",
            "--env",
            "X=1",
            "--scrollback",
            "50000",
            "--",
            "R",
            "--vanilla",
        ]))
        .unwrap();
        assert!(o.detached);
        assert_eq!(o.name.as_deref(), Some("R"));
        assert_eq!(o.kind.as_deref(), Some("r"));
        assert_eq!(o.annotations.get("a").map(String::as_str), Some("b=c"));
        assert_eq!(o.env, vec![("X".to_owned(), "1".to_owned())]);
        assert_eq!(o.scrollback, Some(MAX_SCROLLBACK));
        assert_eq!(o.command, args(&["R", "--vanilla"]));

        let o = parse(&args(&["-d", "sh", "-c", "--name"])).unwrap();
        assert_eq!(o.command, args(&["sh", "-c", "--name"]));

        assert!(parse(&args(&["-d"])).is_err());
        assert!(parse(&args(&["-d", "--id", "UPPER123", "--", "x"])).is_err());
        assert!(parse(&args(&["-d", "--scrollback", "-1", "--", "x"])).is_err());
        assert!(parse(&args(&["-d", "--env", "NOEQUALS", "--", "x"])).is_err());
    }

    #[test]
    fn clamps_sizes() {
        let c = |rows, cols| clamp_size(sys::Winsize { rows, cols });
        assert_eq!(c(0, 1000), sys::Winsize { rows: 1, cols: 500 });
        assert_eq!(c(24, 80), sys::Winsize { rows: 24, cols: 80 });
        assert_eq!(c(501, 1), sys::Winsize { rows: 500, cols: 1 });
    }
}
