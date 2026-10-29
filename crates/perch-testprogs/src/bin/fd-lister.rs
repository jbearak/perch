// SPDX-License-Identifier: GPL-3.0-only

//! `fd-lister OUT`: writes the open descriptors, one per line, to OUT, then
//! waits for SIGTERM or SIGHUP. Run inside a session, it should see only
//! 0, 1 and 2.

fn main() {
    let out = std::env::args().nth(1).expect("usage: fd-lister OUT");
    // Scan before opening anything.
    let open: Vec<i32> = (0..65_536)
        .filter(|&fd| perch::sys::fd_is_open(fd))
        .collect();
    let text: String = open.iter().map(|fd| format!("{fd}\n")).collect();
    perch_testprogs::write_atomically(std::path::Path::new(&out), &text);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
