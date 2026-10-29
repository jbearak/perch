// SPDX-License-Identifier: GPL-3.0-only

//! `perch list [--json] [--all]`.

use std::process::ExitCode;

use serde_json::{Value, json};

use super::session::{query_info, view};
use super::{fail, usage_error};
use crate::clock;
use crate::runtime::{self, Housekeeping};

pub(super) fn run(args: &[String]) -> ExitCode {
    let mut as_json = false;
    let mut all = false;
    for a in args {
        match a.as_str() {
            "--json" => as_json = true,
            "--all" | "-a" => all = true,
            other => return usage_error(&format!("list: unknown argument `{other}`")),
        }
    }
    let rt = match runtime::open_default() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    let mut entries = runtime::scan(&rt, Housekeeping::Full);
    if !all {
        entries.retain(|e| !e.meta.is_tombstone());
    }
    // Ask every live supervisor at once, each with its own deadline.
    let infos: Vec<Option<Value>> = std::thread::scope(|s| {
        let handles: Vec<_> = entries
            .iter()
            .map(|e| {
                let rt = &rt;
                s.spawn(move || e.live.then(|| query_info(rt, e).ok()).flatten())
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().flatten())
            .collect()
    });
    let views: Vec<Value> = entries
        .iter()
        .zip(infos)
        .map(|(e, info)| view(&rt, e, info.as_ref()))
        .collect();
    if as_json {
        println!("{:#}", json!({ "schema": 1, "sessions": views }));
    } else {
        print_table(&views);
    }
    ExitCode::SUCCESS
}

/// `running`, `exited (exit code 0)` and so on.
pub(super) fn state_text(v: &Value) -> String {
    let state = v["state"].as_str().unwrap_or("?");
    match v.get("exit").filter(|e| !e.is_null()) {
        Some(exit) => {
            let exit: crate::meta::Exit = serde_json::from_value(exit.clone()).unwrap_or_default();
            format!("{state} ({})", exit.describe())
        }
        None => state.to_owned(),
    }
}

fn home_relative(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && home != "/" => match path.strip_prefix(&home) {
            Some("") => "~".to_owned(),
            Some(rest) if rest.starts_with('/') => format!("~{rest}"),
            _ => path.to_owned(),
        },
        _ => path.to_owned(),
    }
}

fn print_table(views: &[Value]) {
    if views.is_empty() {
        println!("no sessions");
        return;
    }
    let now = clock::unix_now();
    let rows: Vec<[String; 6]> = views
        .iter()
        .map(|v| {
            let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
            let age = v["createdAt"]
                .as_str()
                .and_then(clock::parse_rfc3339)
                .map_or_else(|| "?".to_owned(), |t| clock::short_age(now - t));
            [
                s("id"),
                s("name"),
                s("kind"),
                state_text(v),
                age,
                home_relative(&s("cwd")),
            ]
        })
        .collect();
    let header = ["ID", "NAME", "KIND", "STATE", "AGE", "CWD"];
    let mut widths = header.map(str::len);
    for r in &rows {
        for (w, c) in widths.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let line = |cells: [&str; 6]| {
        let mut out = String::new();
        for (i, (c, w)) in cells.iter().zip(widths).enumerate() {
            if i == 5 {
                out.push_str(c);
            } else {
                out.push_str(&format!("{c:<w$}  "));
            }
        }
        println!("{}", out.trim_end());
    };
    line(header);
    for r in &rows {
        line([&r[0], &r[1], &r[2], &r[3], &r[4], &r[5]]);
    }
}
