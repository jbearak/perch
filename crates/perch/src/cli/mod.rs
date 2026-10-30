// SPDX-License-Identifier: GPL-3.0-only

//! The command line.

mod attach;
mod doctor;
mod list;
mod new;
mod selfinstall;
mod send;
mod session;
mod version;

use std::process::ExitCode;

const USAGE: &str = "\
usage: perch new [options] -- <command> [args...]   (attaches unless -d)
           --id ID  --name NAME  --kind KIND  --owner EXT  --annotate K=V
           --cwd DIR  --env NAME=VALUE  --link-env NAME  --no-link-env
           --scrollback LINES (default 1000, at most 10000)
           -d, --detached  --label LABEL  --detach-key KEY|none  --restore full|screen
       perch attach <session> [--label LABEL] [--if-detached] [--detach-key KEY|none]
           [--restore full|screen] [--scrollback LINES]
       perch detach [<session>]
       perch list [--json] [--all]
       perch info <session> [--json]
       perch send <session> [--text TEXT | --stdin] [--paste] [--submit]
           exit 3: the input queue is full, nothing was sent;
           exit 4: the connection dropped, the input may not have been sent
       perch kill <session> [--force] [--pid]
       perch dismiss <session>
       perch rename <session> <name>
       perch annotate <session> K=V... [--unset K]
       perch doctor [--json]
       perch version [--json]";

/// Run the command in `args` (without the program name).
pub fn run(args: &[String]) -> ExitCode {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("version") => version::run(rest),
        Some("--version" | "-V") if args.len() == 1 => version::run(&[]),
        Some("new") => new::run(rest),
        Some("attach") => attach::run(rest),
        Some("detach") => attach::detach(rest),
        Some("list" | "ls") => list::run(rest),
        Some("info") => session::info(rest),
        Some("send") => send::run(rest),
        Some("doctor") => doctor::run(rest),
        Some("kill") => session::kill(rest),
        Some("dismiss") => session::dismiss(rest),
        Some("rename") => session::rename(rest),
        Some("annotate") => session::annotate(rest),
        // Hidden: the extension runs it; users have no reason to.
        Some("self-install") => selfinstall::run(rest),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => usage_error(&format!("unknown command `{other}`")),
        None => usage_error("missing command"),
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("perch: {msg}\n{USAGE}");
    ExitCode::from(2)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("perch: {msg}");
    ExitCode::FAILURE
}

/// Splits `--flag=value` into its parts.
fn split_eq(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((f, v)) if f.starts_with("--") => (f, Some(v)),
        _ => (arg, None),
    }
}

/// An argument iterator that understands `--flag value` and `--flag=value`.
struct Args<'a> {
    args: &'a [String],
    i: usize,
    inline: Option<&'a str>,
}

impl<'a> Args<'a> {
    fn new(args: &'a [String]) -> Args<'a> {
        Args {
            args,
            i: 0,
            inline: None,
        }
    }

    /// The next argument, with any `=value` split off for [`Args::value`].
    fn next_arg(&mut self) -> Option<&'a str> {
        let a = self.args.get(self.i)?;
        self.i += 1;
        let (flag, inline) = split_eq(a);
        self.inline = inline;
        Some(flag)
    }

    /// The value of the flag just returned.
    fn value(&mut self, flag: &str) -> Result<&'a str, String> {
        if let Some(v) = self.inline.take() {
            return Ok(v);
        }
        let v = self
            .args
            .get(self.i)
            .ok_or_else(|| format!("{flag} needs a value"))?;
        self.i += 1;
        Ok(v)
    }

    /// Everything not yet consumed.
    fn rest(&self) -> &'a [String] {
        &self.args[self.i..]
    }
}
