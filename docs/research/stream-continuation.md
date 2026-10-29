# Ghostty's stream continuation as a replacement for the hold-back rule (M2c)

Evaluated 2026-10-29 against Ghostty `521cf2c` (the commit `perch-vt` pins), as "Restore on attach" in [design.md](../design.md) and M2c in [plan.md](../plan.md) ask. **Result: not adopted. The hold-back rule and the 16 MiB rule stay.** The design is unchanged.

## What Ghostty offers

`src/terminal/stream.zig` and `src/terminal/stream_continuation.zig`:

- `Stream.Options.continuation_max_bytes`: when set (and the stream has an allocator), the stream keeps the suffix of its input needed to rebuild an unfinished sequence or UTF-8 character, starting at the "replay start" (the last ESC, or a UTF-8 lead byte).
- `Stream.writeContinuation(writer)` writes that suffix, leaving out bytes whose effect has already happened (a BEL or LF inside an unfinished CSI). It fails with `ContinuationDisabled`, or with `ContinuationUnavailable` when the suffix passed `continuation_max_bytes` (the tracker is then `broken` until the stream returns to ground or a new replay start arrives).
- `stream_continuation.validate(bytes)` and a handler-free `BoundaryScanner` check that a suffix is unfinished, minimal and inert.

The adoption would be: feed the model everything, and after a restore send the client `writeContinuation`'s bytes instead of the bytes the supervisor held back.

## The criteria

The plan adopts the continuation only if all three hold.

1. **Exact for every fixture split at every offset.** Not run, because criterion 3 fails first and the change would not remove what it was meant to remove (below).
2. **`ContinuationUnavailable` has a tested fallback, including exhaustion in the middle of a screen-affecting CSI, and recovery.** The only available fallback is the one the design already has. Once the suffix passes its bound, a client attaching then cannot be given the start of the sequence, so the supervisor has to withhold the rest of that sequence from it, exactly as the 16 MiB rule does now. Meanwhile the model has been fed the sequence and is in its middle, so a restore taken then reflects a half-applied CSI (Ghostty executes C0 controls inside a CSI as they arrive). That is the state the 16 MiB rule marks `partial`. The continuation does not remove that rule; it moves its threshold from the supervisor's buffer to Ghostty's.
3. **A small API Perch can keep up with.** The API is small, but its contract excludes Perch: "Replay semantics are guaranteed for the standard TerminalStream handler. Custom handlers, including handlers that intercept vtRaw, are unsupported", and `continuation_max_bytes` is "only supported by TerminalStream". `perch-vt` runs `ghostty.Stream(Vt.Handler)`, a handler that wraps Ghostty's so that the test observer sees each action (the oracle for the tracker's differential tests), and that compiles to a plain forward in production builds. Adopting the continuation would mean either giving up the observer, and with it the tracker differential, or relying on behavior Ghostty documents as unsupported.

## What would not change

- The supervisor still needs the tracker for what it must know synchronously: where queries end (to cut model frames), bracketed paste, and RIS. The design already says so.
- Withholding a sequence's remainder from a client that attaches in its middle stays, for the reason in criterion 2.
- The hold-back rule costs little: the held bytes are one unfinished sequence, at most 16 MiB, and the fidelity harness checks restores made in the middle of CSI, OSC, DCS and split UTF-8 (`partial-sequences`, `split-utf8`, `oversized-apc`, `oversized-csi`).

## When to look again

If Ghostty supports continuation for custom stream handlers (or `perch-vt` no longer needs one), criterion 1 can be run with the fixtures and `model-dump`: for each fixture and each split offset, a restore plus the continuation, then the rest of the stream, must match the model fed the whole stream.
