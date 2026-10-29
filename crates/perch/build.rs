// SPDX-License-Identifier: GPL-3.0-only

//! Builds `perch-vt` with Zig and links it statically. The behavior is fixed
//! by "The build contract" in docs/plan.md; change the plan before this.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ZIG_VERSION: &str = "0.17.0";
const MACOS_DEPLOYMENT_TARGET: &str = "11.0";

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let target = env::var("TARGET").expect("TARGET");
    let perch_vt = manifest_dir
        .join("../../perch-vt")
        .canonicalize()
        .unwrap_or_else(|e| fail(&format!("cannot find perch-vt/ next to crates/: {e}")));

    println!("cargo:rerun-if-env-changed=PERCH_ZIG");
    println!("cargo:rerun-if-env-changed=ZIG_GLOBAL_CACHE_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TEST_HOOKS");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    rerun_if_changed_under(&perch_vt);

    let zig_target = zig_target(&target).unwrap_or_else(|| {
        fail(&format!(
            "perch-vt has no Zig target for Rust target `{target}`"
        ))
    });
    if target.ends_with("-apple-darwin") {
        // build.rs cannot set the environment of rustc's own link step, so
        // .cargo/config.toml sets it; this checks both halves agree.
        match env::var("MACOSX_DEPLOYMENT_TARGET") {
            Ok(v) if v == MACOS_DEPLOYMENT_TARGET => {}
            other => fail(&format!(
                "MACOSX_DEPLOYMENT_TARGET must be {MACOS_DEPLOYMENT_TARGET} so Rust and perch-vt \
                 target the same macOS (got {other:?}); .cargo/config.toml sets it"
            )),
        }
    }

    let zig = env::var_os("PERCH_ZIG").unwrap_or_else(|| "zig".into());
    check_zig_version(Path::new(&zig));

    let mut cmd = Command::new(&zig);
    cmd.current_dir(&out_dir)
        .arg("build")
        .arg("--build-file")
        .arg(perch_vt.join("build.zig"))
        .arg("--cache-dir")
        .arg(out_dir.join("zig-cache"))
        .arg("--prefix")
        .arg(out_dir.join("zig-out"))
        .arg(format!("-Dtarget={zig_target}"))
        .args([
            "-Doptimize=ReleaseSafe",
            "-Dcpu=baseline",
            "-Dsimd=false",
            "-Dvt-features=-kitty-graphics",
        ])
        // Relative on purpose: Zig rejects Ghostty's path dependencies when
        // the local package directory is absolute. This lands in $OUT_DIR.
        .env("ZIG_LOCAL_PKG_DIR", "zig-pkg")
        .env("MACOSX_DEPLOYMENT_TARGET", MACOS_DEPLOYMENT_TARGET);
    if env::var_os("CARGO_FEATURE_TEST_HOOKS").is_some() {
        cmd.arg("-Dtest-observer=true");
    }
    let output = cmd.output().unwrap_or_else(|e| {
        fail(&format!(
            "cannot run `{} build`: {e}",
            zig.to_string_lossy()
        ))
    });
    if !output.status.success() {
        fail(&format!(
            "`zig build` for perch-vt failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let lib_dir = out_dir.join("zig-out/lib");
    if !lib_dir.join("libperch-vt.a").is_file() {
        fail(&format!(
            "zig build did not produce {}",
            lib_dir.join("libperch-vt.a").display()
        ));
    }
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=perch-vt");

    let commit = ghostty_commit(&perch_vt.join("build.zig.zon"));
    println!("cargo:rustc-env=PERCH_GHOSTTY_COMMIT={commit}");
}

/// The Zig target for a Rust target. perch-vt needs no libc of its own, so a
/// musl object links into a gnu binary too.
fn zig_target(rust_target: &str) -> Option<&'static str> {
    Some(match rust_target {
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "aarch64-apple-darwin" => "aarch64-macos.11.0",
        "x86_64-apple-darwin" => "x86_64-macos.11.0",
        _ => return None,
    })
}

fn check_zig_version(zig: &Path) {
    let output = Command::new(zig).arg("version").output().unwrap_or_else(|e| {
        fail(&format!(
            "cannot run `{} version` ({e}). Install Zig {ZIG_VERSION} and put it on PATH or set PERCH_ZIG",
            zig.display()
        ))
    });
    let version = String::from_utf8_lossy(&output.stdout);
    let version = version.trim();
    if !output.status.success() || version != ZIG_VERSION {
        fail(&format!(
            "perch-vt needs Zig {ZIG_VERSION} exactly; `{} version` printed `{version}`. \
             Set PERCH_ZIG to a Zig {ZIG_VERSION} binary",
            zig.display()
        ));
    }
}

/// Everything under perch-vt/ except what a manual `zig build` there leaves.
/// Cargo scans a directory given here recursively, so the top level is listed
/// entry by entry.
fn rerun_if_changed_under(dir: &Path) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", dir.display())));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", dir.display())));
        if matches!(
            entry.file_name().to_str(),
            Some(".zig-cache" | "zig-out" | "zig-pkg")
        ) {
            continue;
        }
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }
}

/// The commit in the `git+https://...#<commit>` URL of the ghostty dependency.
fn ghostty_commit(zon: &Path) -> String {
    let text = fs::read_to_string(zon)
        .unwrap_or_else(|e| fail(&format!("cannot read {}: {e}", zon.display())));
    text.lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix(".url = \"git+https://github.com/ghostty-org/ghostty#")
        })
        .filter_map(|rest| rest.strip_suffix("\","))
        .next()
        .unwrap_or_else(|| fail(&format!("no pinned ghostty URL in {}", zon.display())))
        .to_string()
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    panic!("{msg}");
}
