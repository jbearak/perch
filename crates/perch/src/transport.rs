// SPDX-License-Identifier: GPL-3.0-only

//! What the pure state machines in `flow` and `input` need from a client
//! socket or the PTY master: a write that never blocks.

/// A non-blocking byte sink.
pub trait Transport {
    /// Offers `bytes` and returns how many the peer accepted: all, some, or
    /// 0 when a write would block. Errors belong to the caller: an adapter
    /// over a descriptor records them and returns 0.
    fn write(&mut self, bytes: &[u8]) -> usize;
}

/// A transport for tests: it accepts up to a budget the test sets, as a
/// socket or PTY whose reader drains exactly that much, and records every
/// byte accepted.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct Scripted {
    budget: usize,
    pub(crate) out: Vec<u8>,
}

#[cfg(test)]
impl Scripted {
    pub(crate) fn new() -> Scripted {
        Scripted::default()
    }

    /// Lets the next writes accept `n` more bytes in total.
    pub(crate) fn allow(&mut self, n: usize) {
        self.budget = self.budget.saturating_add(n);
    }

    /// Accept everything from now on.
    pub(crate) fn open(&mut self) {
        self.budget = usize::MAX;
    }

    /// Accept nothing more until allowed again.
    pub(crate) fn close(&mut self) {
        self.budget = 0;
    }

    /// Whether a write now would accept anything.
    pub(crate) fn writable(&self) -> bool {
        self.budget > 0
    }

    /// Takes the bytes accepted so far.
    pub(crate) fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

#[cfg(test)]
impl Transport for Scripted {
    fn write(&mut self, bytes: &[u8]) -> usize {
        let n = bytes.len().min(self.budget);
        self.budget -= n;
        self.out.extend_from_slice(&bytes[..n]);
        n
    }
}
