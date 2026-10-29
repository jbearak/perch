# Emulator spike (2026-10-29)

Question: which terminal emulator crate should hold Perch's screen model? See [design.md](../design.md#restore-on-attach).

Outcome: this spike chose avt over vt100. Design revision 8 later replaced avt with libghostty-vt, at the maintainer's direction; libghostty-vt was not part of this spike. The results stay here as the record of why vt100 was rejected.

Candidates checked: `vt100` 0.18.1 (MIT) and `avt` 0.19.0 (Apache-2.0). `alacritty_terminal` (Apache-2.0) reflows and answers queries but has no serializer. libghostty-vt (MIT) needs Zig. Neither of those two was run.

Results on an Apple M-series laptop, release build:

| Check | vt100 | avt |
|-------|-------|-----|
| Restore while in alternate screen, then `ESC[?1049l` | not possible through the public API | primary screen equal to the source |
| Five 190-char lines at 200 cols, resize to 80 | rows truncated | 5 of 5 logical lines intact |
| 10,000 lines of history at 120 cols | +20.8 MB | +20.8 MB |
| Throughput, mixed SGR text | 111 MB/s | 92 MB/s |

Reproduce with this `src/main.rs` in a scratch crate depending on `avt = "=0.19.0"` and `vt100 = "=0.18.1"`, run with `cargo run --release`.

```rust
use std::time::Instant;
fn rss_kb() -> u64 {
    let out = std::process::Command::new("ps").args(["-o","rss=","-p",&std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}
fn main() {
    // 1. alt screen restore
    let mut src = avt::Terminal::with_scrollback(80, 10, Some(100)).unwrap();
    src.feed("prompt> ls\r\nfile1 file2\r\nprompt> vim\r\n");
    src.feed("\x1b[?1049h\x1b[2J\x1b[H~ vim content ~");
    let mut dst = avt::Terminal::new(80, 10).unwrap();
    dst.feed(src.dump());
    src.feed("\x1b[?1049l"); dst.feed("\x1b[?1049l");
    println!("1 alt-restore primary equal: {}", src.screen().text() == dst.screen().text());
    println!("   primary after exit: {:?}", &dst.screen().text()[..3]);
    // 2. reflow narrower with long lines
    let mut t = avt::Terminal::with_scrollback(200, 10, Some(1000)).unwrap();
    let long: String = (0..190).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
    for _ in 0..5 { t.feed(format!("{long}\r\n")); }
    t.resize(80, 10);
    let joined: Vec<String> = { let mut v = vec![]; let mut cur = String::new();
        for l in t.primary_screen().all_lines() { cur.push_str(l.text().trim_end()); if !l.is_wrapped() { v.push(std::mem::take(&mut cur)); } } v };
    println!("2 reflow keeps 190-char lines: {}", joined.iter().filter(|s| s.len()==190).count());
    // 3. memory: 10k lines scrollback at 120 cols, typical R output (~60 chars)
    let base = rss_kb();
    let mut m = avt::Terminal::with_scrollback(120, 40, Some(10_000)).unwrap();
    for i in 0..10_000 { m.feed(format!("[{i}] \x1b[32m 0.1234 0.5678 0.9012 3.4567 \x1b[0m some R output text here\r\n")); }
    println!("3 avt 10k lines x120: +{} KB", rss_kb() - base);
    let base = rss_kb();
    let mut v = vt100::Parser::new(std::num::NonZero::new(40).unwrap(), std::num::NonZero::new(120).unwrap(), 10_000);
    for i in 0..10_000 { v.process(format!("[{i}] \x1b[32m 0.1234 0.5678 0.9012 3.4567 \x1b[0m some R output text here\r\n").as_bytes()); }
    println!("3 vt100 10k lines x120: +{} KB", rss_kb() - base);
    // 4. throughput
    let chunk: String = (0..1000).map(|i| format!("{i} \x1b[1mbold\x1b[0m line of output with numbers 1.2345 6.789\r\n")).collect();
    let mut a = avt::Terminal::with_scrollback(120, 40, Some(10_000)).unwrap();
    let n = 100_000_000 / chunk.len();
    let s = Instant::now(); for _ in 0..n { a.feed(chunk.as_str()); }
    println!("4 avt throughput: {:.0} MB/s", 100.0 / s.elapsed().as_secs_f64());
    let mut b = vt100::Parser::new(std::num::NonZero::new(40).unwrap(), std::num::NonZero::new(120).unwrap(), 0);
    let s = Instant::now(); for _ in 0..n { b.process(chunk.as_bytes()); }
    println!("4 vt100 throughput: {:.0} MB/s", 100.0 / s.elapsed().as_secs_f64());
    std::hint::black_box((&m, &v, &a, &b));
}
```
