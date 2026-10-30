// SPDX-License-Identifier: GPL-3.0-only AND MIT
//
// A port of the parser state machine in Ghostty (src/terminal/Parser.zig,
// src/terminal/parse_table.zig and the byte-at-a-time path of
// src/terminal/stream.zig) and of its UTF-8 decoder
// (src/terminal/UTF8Decoder.zig), at the commit perch-vt pins.
// Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors.
// The UTF-8 decoder's tables: Copyright (c) 2008-2009 Bjoern Hoehrmann.
// Both used under the MIT license; see NOTICE for its text.

//! The supervisor's boundary tracker: three facts about the program's
//! output, known as the supervisor reads it, without waiting for the model.
//!
//! - Whether the stream is at ground, between sequences and UTF-8
//!   characters, and if not, what kind of sequence is unfinished.
//! - Where the queries Perch answers end (DSR 5, DSR 6, DA1, DA2), and
//!   where RIS is, as byte offsets from the tracker's creation.
//! - Whether bracketed paste is on.
//!
//! It follows Ghostty exactly, because it must agree with the model that
//! Ghostty runs: the differential tests feed both the same bytes. That
//! includes Ghostty's choices where terminals differ. A C1 control encoded
//! as UTF-8 (`C2 9B`) is ignored, and a raw C1 byte at ground is invalid
//! UTF-8, so neither starts a CSI there; raw C1 bytes act only inside a
//! sequence. `ESC Z` (DECID) is DA1. DECSTR leaves bracketed paste alone.
//!
//! It also holds the reply shapes the input side recognizes in client
//! input ([`reply_kind`]).

use crate::queries::Kind;

/// Ghostty's limit on CSI parameters; a CSI with more is dropped.
const MAX_PARAMS: usize = 24;
/// Ghostty's limit on collected intermediates; more are ignored.
const MAX_INTERMEDIATE: usize = 4;
/// Bracketed paste's DEC private mode number.
const BRACKETED_PASTE: u16 = 2004;

/// What the tracker reports at a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// A query Perch answers.
    Query(Kind),
    /// `CSI ? 2004 h`, `l`, `s` or `r`, or 2004 among other parameters.
    Paste(PasteOp),
    /// RIS (`ESC c`). Bracketed paste is off and its saved value cleared.
    Reset,
}

/// What a sequence did to bracketed paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteOp {
    Set,
    Reset,
    /// XTSAVE: remember the current value.
    Save,
    /// XTRESTORE: take the remembered value.
    Restore,
}

/// A [`Seen`] and the number of bytes fed up to and including the byte
/// that completed it, counted from the tracker's creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub seen: Seen,
    pub end: u64,
}

/// The kind of an unfinished sequence, known from its introducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfinished {
    /// A UTF-8 character.
    Utf8,
    /// `ESC`, with or without intermediates.
    Escape,
    Csi,
    Dcs,
    /// An OSC; `number` is its leading number once a `;` has ended it.
    Osc {
        number: Option<u32>,
    },
    /// SOS, PM or APC.
    SosPmApc,
}

/// Ghostty's parser states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiIntermediate,
    CsiParam,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

/// The transition actions of Ghostty's table that matter here. `Param` and
/// the put actions only feed handlers Perch does not track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    None,
    Execute,
    Collect,
    EscDispatch,
    CsiDispatch,
    OscPut,
}

/// How much of an OSC's leading number is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscNumber {
    Digits { value: u32, len: u8 },
    Done(Option<u32>),
}

/// The tracker for one output stream.
#[derive(Debug, Clone)]
pub struct Tracker {
    state: State,
    utf8: Utf8Decoder,
    intermediates: [u8; MAX_INTERMEDIATE],
    intermediates_len: usize,
    params: [u16; MAX_PARAMS],
    params_len: usize,
    /// Whether a parameter separator was a colon.
    params_colon: bool,
    param_acc: u16,
    /// Whether `param_acc` holds a parameter to finalize at dispatch.
    param_acc_set: bool,
    osc: OscNumber,
    paste: bool,
    paste_saved: bool,
    fed: u64,
    ground_at: u64,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker::new()
    }
}

impl Tracker {
    /// A tracker at ground, as at a terminal's creation: bracketed paste
    /// off, and its saved value off.
    pub fn new() -> Tracker {
        Tracker {
            state: State::Ground,
            utf8: Utf8Decoder::default(),
            intermediates: [0; MAX_INTERMEDIATE],
            intermediates_len: 0,
            params: [0; MAX_PARAMS],
            params_len: 0,
            params_colon: false,
            param_acc: 0,
            param_acc_set: false,
            osc: OscNumber::Done(None),
            paste: false,
            paste_saved: false,
            fed: 0,
            ground_at: 0,
        }
    }

    /// Feeds output bytes and returns what they held, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &c in bytes {
            self.fed += 1;
            if self.state == State::Ground {
                self.next_utf8(c);
            } else {
                self.next_non_utf8(c, &mut events);
            }
            if self.at_ground() {
                self.ground_at = self.fed;
            }
        }
        events
    }

    /// Feeds bytes until the tracker is next at ground, and returns how
    /// many it took and what they held. Used to find exactly where a long
    /// sequence ends.
    pub fn feed_to_ground(&mut self, bytes: &[u8]) -> (usize, Vec<Event>) {
        let mut events = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            events.extend(self.feed(&[b]));
            if self.at_ground() {
                return (i + 1, events);
            }
        }
        (bytes.len(), events)
    }

    /// Whether the parser and its UTF-8 decoder are both between sequences.
    pub fn at_ground(&self) -> bool {
        self.state == State::Ground && self.utf8.state == UTF8_ACCEPT
    }

    /// The bytes fed since creation.
    pub fn fed(&self) -> u64 {
        self.fed
    }

    /// The offset of the last point at ground. Bytes after it are an
    /// unfinished sequence or character; feeding the model up to here and
    /// the rest later gives it the same bytes in the same order.
    pub fn ground_offset(&self) -> u64 {
        self.ground_at
    }

    /// What is unfinished, or `None` at ground.
    pub fn unfinished(&self) -> Option<Unfinished> {
        Some(match self.state {
            State::Ground if self.utf8.state == UTF8_ACCEPT => return None,
            State::Ground => Unfinished::Utf8,
            State::Escape | State::EscapeIntermediate => Unfinished::Escape,
            State::CsiEntry | State::CsiIntermediate | State::CsiParam | State::CsiIgnore => {
                Unfinished::Csi
            }
            State::DcsEntry
            | State::DcsParam
            | State::DcsIntermediate
            | State::DcsPassthrough
            | State::DcsIgnore => Unfinished::Dcs,
            State::OscString => Unfinished::Osc {
                number: match self.osc {
                    OscNumber::Done(n) => n,
                    OscNumber::Digits { .. } => None,
                },
            },
            State::SosPmApcString => Unfinished::SosPmApc,
        })
    }

    /// Whether the program has bracketed paste on.
    pub fn bracketed_paste(&self) -> bool {
        self.paste
    }

    /// A byte at ground goes through the UTF-8 decoder (stream.zig's
    /// `nextUtf8`): an ill-formed sequence ends as a replacement character
    /// and the byte that broke it is decoded again.
    fn next_utf8(&mut self, c: u8) {
        let (cp, consumed) = self.utf8.next(c);
        if let Some(cp) = cp {
            self.handle_codepoint(cp);
        }
        if !consumed {
            let (cp, consumed) = self.utf8.next(c);
            debug_assert!(consumed);
            if let Some(cp) = cp {
                self.handle_codepoint(cp);
            }
        }
    }

    /// stream.zig's `handleCodepoint`. Only ESC changes the parser: other
    /// C0 controls execute, C1 controls decoded from UTF-8 are ignored, and
    /// the rest print.
    fn handle_codepoint(&mut self, cp: u32) {
        if cp == 0x1b {
            self.state = State::Escape;
            self.clear();
        }
    }

    /// stream.zig's `nextNonUtf8`: its fast paths for CSI entry and CSI
    /// parameters, then the parse table.
    fn next_non_utf8(&mut self, c: u8, events: &mut Vec<Event>) {
        match self.state {
            State::Escape if c == b'[' => {
                self.state = State::CsiEntry;
                return;
            }
            State::CsiParam => {
                match c {
                    // C0 controls execute or are ignored, inside the CSI.
                    0x00..=0x17 | 0x19 | 0x1c..=0x1f => {}
                    0x18 | 0x1a => self.state = State::Ground,
                    b'0'..=b'9' => {
                        if self.params_len < MAX_PARAMS {
                            self.param_acc = self.param_acc.saturating_mul(10);
                            self.param_acc = self.param_acc.saturating_add(u16::from(c - b'0'));
                            self.param_acc_set = true;
                        }
                    }
                    b':' | b';' => self.separator(c),
                    0x40..=0x7e => self.csi_dispatch(c, events),
                    0x7f => {}
                    _ => self.table(c, events),
                }
                return;
            }
            State::CsiEntry => match c {
                b'0'..=b'9' => {
                    self.state = State::CsiParam;
                    self.param_acc = u16::from(c - b'0');
                    self.param_acc_set = true;
                    return;
                }
                b';' => {
                    self.state = State::CsiParam;
                    self.params[0] = 0;
                    self.params_len = 1;
                    return;
                }
                // A private marker, such as `?`.
                0x3c..=0x3f => {
                    self.state = State::CsiParam;
                    self.collect(c);
                    return;
                }
                0x40..=0x7e => {
                    self.csi_dispatch(c, events);
                    return;
                }
                _ => {}
            },
            _ => {}
        }
        self.table(c, events);
    }

    /// Parser.zig's `next`: the table's transition, with its exit,
    /// transition and entry actions.
    fn table(&mut self, c: u8, events: &mut Vec<Event>) {
        let (next, action) = transition(self.state, c);
        let changed = next != self.state;
        match action {
            Action::None => {}
            // C0 controls do nothing tracked. A C1 control is ESC plus
            // `c - 0x40`, with no intermediates: 0x9A is DECID.
            Action::Execute => {
                if c > 0x7f {
                    self.esc_dispatch(c - 0x40, true, events);
                }
            }
            Action::Collect => self.collect(c),
            Action::EscDispatch => self.esc_dispatch(c, self.intermediates_len == 0, events),
            Action::CsiDispatch => self.csi_dispatch(c, events),
            Action::OscPut => self.osc_put(c),
        }
        self.state = next;
        if changed {
            match next {
                State::Escape | State::DcsEntry | State::CsiEntry => self.clear(),
                State::OscString => {
                    self.osc = OscNumber::Digits { value: 0, len: 0 };
                }
                _ => {}
            }
        }
    }

    fn clear(&mut self) {
        self.intermediates_len = 0;
        self.params_len = 0;
        self.params_colon = false;
        self.param_acc = 0;
        self.param_acc_set = false;
    }

    fn collect(&mut self, c: u8) {
        if self.intermediates_len < MAX_INTERMEDIATE {
            self.intermediates[self.intermediates_len] = c;
            self.intermediates_len += 1;
        }
    }

    fn separator(&mut self, c: u8) {
        if self.params_len < MAX_PARAMS {
            self.params[self.params_len] = self.param_acc;
            self.params_colon |= c == b':';
            self.params_len += 1;
            self.param_acc = 0;
            self.param_acc_set = false;
        }
    }

    /// stream.zig's `csiDispatchFinal` and the cases of `csiDispatch` that
    /// matter here.
    fn csi_dispatch(&mut self, fin: u8, events: &mut Vec<Event>) {
        self.state = State::Ground;
        if self.params_len >= MAX_PARAMS {
            return;
        }
        if self.param_acc_set {
            self.params[self.params_len] = self.param_acc;
            self.params_len += 1;
        }
        // Colons are allowed only in SGR.
        if fin != b'm' && self.params_colon {
            return;
        }
        let params = &self.params[..self.params_len];
        let query = |kind| Event {
            seen: Seen::Query(kind),
            end: self.fed,
        };
        match (fin, &self.intermediates[..self.intermediates_len]) {
            // DA ignores its parameters.
            (b'c', []) => events.push(query(Kind::Da1)),
            (b'c', [b'>']) => events.push(query(Kind::Da2)),
            (b'n', []) => match params {
                [5] => events.push(query(Kind::Dsr5)),
                [6] => events.push(query(Kind::Dsr6)),
                _ => {}
            },
            (b'h' | b'l' | b's' | b'r', [b'?']) => {
                let op = match fin {
                    b'h' => PasteOp::Set,
                    b'l' => PasteOp::Reset,
                    b's' => PasteOp::Save,
                    _ => PasteOp::Restore,
                };
                // Each parameter is its own mode change, in order.
                for _ in params.iter().filter(|p| **p == BRACKETED_PASTE) {
                    match op {
                        PasteOp::Set => self.paste = true,
                        PasteOp::Reset => self.paste = false,
                        PasteOp::Save => self.paste_saved = self.paste,
                        PasteOp::Restore => self.paste = self.paste_saved,
                    }
                    events.push(Event {
                        seen: Seen::Paste(op),
                        end: self.fed,
                    });
                }
            }
            _ => {}
        }
    }

    /// The cases of stream.zig's `escDispatch` that matter here.
    fn esc_dispatch(&mut self, fin: u8, no_intermediates: bool, events: &mut Vec<Event>) {
        if !no_intermediates {
            return;
        }
        let seen = match fin {
            // DECID.
            b'Z' => Seen::Query(Kind::Da1),
            // RIS: modes back to their defaults, saved modes cleared.
            b'c' => {
                self.paste = false;
                self.paste_saved = false;
                Seen::Reset
            }
            _ => return,
        };
        events.push(Event {
            seen,
            end: self.fed,
        });
    }

    fn osc_put(&mut self, c: u8) {
        if let OscNumber::Digits { value, len } = self.osc {
            self.osc = match c {
                b'0'..=b'9' => OscNumber::Digits {
                    value: value.saturating_mul(10).saturating_add(u32::from(c - b'0')),
                    len: len.saturating_add(1),
                },
                b';' if len > 0 => OscNumber::Done(Some(value)),
                _ => OscNumber::Done(None),
            };
        }
    }
}

/// parse_table.zig's table, as a function. Every state takes the
/// "anywhere" transitions except where a state overrides them: OSC and the
/// DCS passthrough and ignore states treat 0x80 to 0xFF as data. A byte a
/// state does not list leaves it where it is.
fn transition(state: State, c: u8) -> (State, Action) {
    use Action as A;
    use State as S;

    let high_is_data = matches!(state, S::OscString | S::DcsPassthrough | S::DcsIgnore);
    if !(high_is_data && c >= 0x80) {
        match c {
            0x18 | 0x1a | 0x80..=0x8f | 0x91..=0x97 | 0x99 | 0x9a => {
                return (S::Ground, A::Execute);
            }
            0x9c => return (S::Ground, A::None),
            0x1b => return (S::Escape, A::None),
            0x98 | 0x9e | 0x9f => return (S::SosPmApcString, A::None),
            0x9b => return (S::CsiEntry, A::None),
            0x90 => return (S::DcsEntry, A::None),
            0x9d => return (S::OscString, A::None),
            _ => {}
        }
    }

    // C0 controls other than CAN, SUB and ESC.
    let c0 = matches!(c, 0x00..=0x17 | 0x19 | 0x1c..=0x1f);
    let stay = (state, A::None);
    match state {
        // Bytes at ground go through the UTF-8 decoder, not the table.
        S::Ground => stay,
        S::Escape => match c {
            _ if c0 => (state, A::Execute),
            0x20..=0x2f => (S::EscapeIntermediate, A::Collect),
            b'X' | b'^' | b'_' => (S::SosPmApcString, A::None),
            b'P' => (S::DcsEntry, A::None),
            b'[' => (S::CsiEntry, A::None),
            b']' => (S::OscString, A::None),
            0x30..=0x7e => (S::Ground, A::EscDispatch),
            _ => stay,
        },
        S::EscapeIntermediate => match c {
            _ if c0 => (state, A::Execute),
            0x20..=0x2f => (state, A::Collect),
            0x30..=0x7e => (S::Ground, A::EscDispatch),
            _ => stay,
        },
        S::SosPmApcString => stay,
        S::DcsEntry => match c {
            0x20..=0x2f => (S::DcsIntermediate, A::Collect),
            b':' => (S::DcsIgnore, A::None),
            0x30..=0x39 | b';' => (S::DcsParam, A::None),
            0x3c..=0x3f => (S::DcsParam, A::Collect),
            0x40..=0x7e => (S::DcsPassthrough, A::None),
            _ => stay,
        },
        S::DcsIntermediate => match c {
            0x20..=0x2f => (state, A::Collect),
            0x30..=0x3f => (S::DcsIgnore, A::None),
            0x40..=0x7e => (S::DcsPassthrough, A::None),
            _ => stay,
        },
        S::DcsIgnore | S::DcsPassthrough => stay,
        S::DcsParam => match c {
            b':' | 0x3c..=0x3f => (S::DcsIgnore, A::None),
            0x20..=0x2f => (S::DcsIntermediate, A::Collect),
            0x40..=0x7e => (S::DcsPassthrough, A::None),
            _ => stay,
        },
        S::CsiParam => match c {
            _ if c0 => (state, A::Execute),
            0x40..=0x7e => (S::Ground, A::CsiDispatch),
            0x3c..=0x3f => (S::CsiIgnore, A::None),
            0x20..=0x2f => (S::CsiIntermediate, A::Collect),
            _ => stay,
        },
        S::CsiIgnore => match c {
            _ if c0 => (state, A::Execute),
            0x40..=0x7e => (S::Ground, A::None),
            _ => stay,
        },
        S::CsiIntermediate => match c {
            _ if c0 => (state, A::Execute),
            0x20..=0x2f => (state, A::Collect),
            0x40..=0x7e => (S::Ground, A::CsiDispatch),
            0x30..=0x3f => (S::CsiIgnore, A::None),
            _ => stay,
        },
        S::CsiEntry => match c {
            _ if c0 => (state, A::Execute),
            0x40..=0x7e => (S::Ground, A::CsiDispatch),
            b':' => (S::CsiIgnore, A::None),
            0x20..=0x2f => (S::CsiIntermediate, A::Collect),
            // Digits, `;` and private markers take the fast path in
            // `next_non_utf8` and never reach the table.
            _ => stay,
        },
        S::OscString => match c {
            0x07 => (S::Ground, A::None),
            0x20..=0xff => (state, A::OscPut),
            _ => stay,
        },
    }
}

/// Höhrmann's UTF-8 DFA, as Ghostty adapts it (UTF8Decoder.zig).
#[derive(Debug, Clone, Copy, Default)]
struct Utf8Decoder {
    accumulator: u32,
    state: u8,
}

const UTF8_ACCEPT: u8 = 0;
const UTF8_REJECT: u8 = 12;

#[rustfmt::skip]
const UTF8_CLASSES: [u8; 256] = [
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,  0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
    1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,  9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,9,
    7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,  7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,
    8,8,2,2,2,2,2,2,2,2,2,2,2,2,2,2,  2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,
   10,3,3,3,3,3,3,3,3,3,3,3,3,4,3,3, 11,6,6,6,5,8,8,8,8,8,8,8,8,8,8,8,
];

#[rustfmt::skip]
const UTF8_TRANSITIONS: [u8; 108] = [
     0,12,24,36,60,96,84,12,12,12,48,72, 12,12,12,12,12,12,12,12,12,12,12,12,
    12, 0,12,12,12,12,12, 0,12, 0,12,12, 12,24,12,12,12,12,12,24,12,24,12,12,
    12,12,12,12,12,12,12,24,12,12,12,12, 12,24,12,12,12,12,12,12,12,24,12,12,
    12,12,12,12,12,12,12,36,12,36,12,12, 12,36,12,12,12,12,12,36,12,36,12,12,
    12,36,12,12,12,12,12,12,12,12,12,12,
];

impl Utf8Decoder {
    /// The codepoint the byte completes, if any, and whether the byte was
    /// consumed. An ill-formed sequence gives U+FFFD; if it was the byte
    /// after the first that broke it, that byte is not consumed and must
    /// be fed again.
    fn next(&mut self, byte: u8) -> (Option<u32>, bool) {
        let class = UTF8_CLASSES[usize::from(byte)];
        let initial = self.state;
        if self.state != UTF8_ACCEPT {
            self.accumulator = (self.accumulator << 6) | u32::from(byte & 0x3f);
        } else {
            self.accumulator = (0xff >> class) & u32::from(byte);
        }
        self.state = UTF8_TRANSITIONS[usize::from(self.state + class)];
        match self.state {
            UTF8_ACCEPT => {
                let cp = self.accumulator;
                self.accumulator = 0;
                (Some(cp), true)
            }
            UTF8_REJECT => {
                self.accumulator = 0;
                self.state = UTF8_ACCEPT;
                (Some(0xfffd), initial == UTF8_ACCEPT)
            }
            _ => (None, true),
        }
    }
}

/// The reply shape of a CSI in client input, given its parameter bytes
/// (private marker included) and final byte, for a CSI without
/// intermediates: `CSI 0 n`, `CSI <r>;<c> R`, `CSI ? ... c`, `CSI > ... c`.
pub fn reply_kind(params: &[u8], fin: u8) -> Option<Kind> {
    let numeric = |p: &[u8]| p.iter().all(|b| b.is_ascii_digit() || *b == b';');
    match (fin, params.first()) {
        (b'n', _) if params == b"0" => Some(Kind::Dsr5),
        (b'R', _) => {
            let mut parts = params.split(|b| *b == b';');
            let (Some(r), Some(c), None) = (parts.next(), parts.next(), parts.next()) else {
                return None;
            };
            let digits = |p: &[u8]| !p.is_empty() && p.iter().all(u8::is_ascii_digit);
            (digits(r) && digits(c)).then_some(Kind::Dsr6)
        }
        (b'c', Some(b'?')) if numeric(&params[1..]) => Some(Kind::Da1),
        (b'c', Some(b'>')) if numeric(&params[1..]) => Some(Kind::Da2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(bytes: &[u8]) -> (Tracker, Vec<Event>) {
        let mut t = Tracker::new();
        let events = t.feed(bytes);
        (t, events)
    }

    fn seen(bytes: &[u8]) -> Vec<Seen> {
        feed(bytes).1.into_iter().map(|e| e.seen).collect()
    }

    fn query(kind: Kind, end: u64) -> Event {
        Event {
            seen: Seen::Query(kind),
            end,
        }
    }

    #[test]
    fn utf8_encoded_c1_csi_is_ignored() {
        // U+009B, as Ghostty ignores it and xterm.js does not.
        let (t, events) = feed(b"\xc2\x9b6n");
        assert!(events.is_empty());
        assert!(t.at_ground());
        // A raw 0x9B at ground is invalid UTF-8, not CSI.
        let (t, events) = feed(b"\x9b6n");
        assert!(events.is_empty());
        assert!(t.at_ground());
        // Inside a sequence, a raw 0x9B is CSI.
        assert_eq!(feed(b"\x1b\x9b6n").1, [query(Kind::Dsr6, 4)]);
        assert_eq!(seen(b"\x1b[1\x9b5n"), [Seen::Query(Kind::Dsr5)]);
    }

    #[test]
    fn can_and_sub_abort_csi() {
        for abort in [0x18, 0x1a] {
            let (t, events) = feed(&[0x1b, b'[', b'6', abort, b'n']);
            assert!(events.is_empty(), "{abort:#x}");
            assert!(t.at_ground());
            // Before any parameter, too.
            assert!(seen(&[0x1b, b'[', abort, b'c']).is_empty());
            assert!(seen(&[0x1b, b'[', b'?', b'1', b' ', abort, b'c']).is_empty());
        }
    }

    #[test]
    fn can_and_sub_abort_osc_and_strings() {
        for abort in [0x18, 0x1a] {
            for intro in [
                &b"\x1b]0;title"[..],
                b"\x1bPq",
                b"\x1b_Gx",
                b"\x1bX",
                b"\x1b^",
            ] {
                let mut bytes = intro.to_vec();
                bytes.push(abort);
                let (t, _) = feed(&bytes);
                assert!(t.at_ground(), "{bytes:?}");
                bytes.extend_from_slice(b"\x1b[5n");
                assert_eq!(seen(&bytes), [Seen::Query(Kind::Dsr5)], "{bytes:?}");
            }
        }
    }

    #[test]
    fn esc_in_osc_starts_a_new_sequence() {
        let (t, events) = feed(b"\x1b]2;t\x1b[6n");
        assert_eq!(events, [query(Kind::Dsr6, 9)]);
        assert!(t.at_ground());
        // ST is ESC then `\`, which is an escape sequence of its own.
        let (t, _) = feed(b"\x1b]2;t\x1b");
        assert_eq!(t.unfinished(), Some(Unfinished::Escape));
    }

    #[test]
    fn bel_and_st_end_osc() {
        let (t, _) = feed(b"\x1b]0;x\x07");
        assert!(t.at_ground());
        let (t, _) = feed(b"\x1b]0;x\x1b\\");
        assert!(t.at_ground());
        // An 8-bit ST is data in an OSC and in DCS passthrough...
        for bytes in [&b"\x1b]0;x\x9c"[..], b"\x1bPqx\x9c"] {
            let (t, _) = feed(bytes);
            assert!(!t.at_ground(), "{bytes:?}");
        }
        // ...but ends an APC.
        let (t, _) = feed(b"\x1b_x\x9c");
        assert!(t.at_ground());
        // BEL ends nothing but an OSC.
        let (t, _) = feed(b"\x1bPqx\x07");
        assert_eq!(t.unfinished(), Some(Unfinished::Dcs));
    }

    #[test]
    fn c0_controls_inside_csi_parameters_do_not_end_it() {
        assert_eq!(feed(b"\x1b[\n6\r\x08n").1, [query(Kind::Dsr6, 7)]);
        assert_eq!(seen(b"\x1b[?20\n04\x07h"), [Seen::Paste(PasteOp::Set)]);
        let (t, _) = feed(b"\x1b[?20\n");
        assert_eq!(t.unfinished(), Some(Unfinished::Csi));
        assert!(!t.bracketed_paste());
    }

    #[test]
    fn split_utf8_at_every_offset() {
        let text = "aé€😀\x1b[6nz".as_bytes();
        let char_ends = [1, 3, 6, 10, 11, 12, 13, 14, 15];
        for cut in 0..=text.len() {
            let mut t = Tracker::new();
            let mut events = t.feed(&text[..cut]);
            let ground = cut == 0 || char_ends.contains(&cut);
            let in_csi = (11..14).contains(&cut);
            assert_eq!(t.at_ground(), ground && !in_csi, "cut {cut}");
            if !ground {
                assert_eq!(t.unfinished(), Some(Unfinished::Utf8), "cut {cut}");
            }
            events.extend(t.feed(&text[cut..]));
            assert_eq!(events, [query(Kind::Dsr6, 14)], "cut {cut}");
            assert!(t.at_ground());
        }
    }

    #[test]
    fn esc_breaks_a_utf8_character() {
        // The broken character ends as U+FFFD and ESC still starts a CSI.
        assert_eq!(feed(b"\xe2\x82\x1b[5n").1, [query(Kind::Dsr5, 6)]);
        let (t, _) = feed(b"\xe2\x82\x1b");
        assert_eq!(t.unfinished(), Some(Unfinished::Escape));
        assert_eq!(t.ground_offset(), 0);
    }

    #[test]
    fn bracketed_paste_set_save_reset_restore() {
        let mut t = Tracker::new();
        t.feed(b"\x1b[?2004h");
        assert!(t.bracketed_paste());
        assert_eq!(
            t.feed(b"\x1b[?2004s")
                .iter()
                .map(|e| e.seen)
                .collect::<Vec<_>>(),
            [Seen::Paste(PasteOp::Save)]
        );
        t.feed(b"\x1b[?2004l");
        assert!(!t.bracketed_paste());
        t.feed(b"\x1b[?2004r");
        assert!(t.bracketed_paste());
        // Among other modes, and once per parameter.
        assert_eq!(
            seen(b"\x1b[?1000;2004;2004l"),
            [Seen::Paste(PasteOp::Reset), Seen::Paste(PasteOp::Reset)]
        );
        // The ANSI form is another mode, and an unknown one.
        assert!(seen(b"\x1b[2004h").is_empty());
    }

    #[test]
    fn ris_clears_paste_and_its_saved_value_and_decstr_does_not() {
        let mut t = Tracker::new();
        t.feed(b"\x1b[?2004h\x1b[?2004s");
        assert_eq!(
            t.feed(b"\x1bc"),
            [Event {
                seen: Seen::Reset,
                end: 18
            }]
        );
        assert!(!t.bracketed_paste());
        t.feed(b"\x1b[?2004r");
        assert!(!t.bracketed_paste());
        t.feed(b"\x1b[?2004h\x1b[!p");
        assert!(t.bracketed_paste());
    }

    #[test]
    fn queries_and_their_offsets() {
        assert_eq!(
            feed(b"ab\x1b[5n\x1b[6n\x1b[c\x1b[>c\x1b[0c\x1b[>1;2c").1,
            [
                query(Kind::Dsr5, 6),
                query(Kind::Dsr6, 10),
                query(Kind::Da1, 13),
                query(Kind::Da2, 17),
                query(Kind::Da1, 21),
                query(Kind::Da2, 28),
            ]
        );
        // Not queries Perch answers: DA3, DECXCPR, other DSRs, and DSR
        // with extra parameters or intermediates.
        for bytes in [
            &b"\x1b[=c"[..],
            b"\x1b[?6n",
            b"\x1b[?5n",
            b"\x1b[6;1n",
            b"\x1b[n",
            b"\x1b[ 6n",
            b"\x1b[?c",
            b"\x1b[>?c",
        ] {
            assert!(seen(bytes).is_empty(), "{bytes:?}");
        }
        // Leading zeros and saturation, as Ghostty counts.
        assert_eq!(seen(b"\x1b[0006n"), [Seen::Query(Kind::Dsr6)]);
        assert!(seen(b"\x1b[99999999n").is_empty());
    }

    #[test]
    fn decid_is_da1() {
        assert_eq!(feed(b"\x1bZ").1, [query(Kind::Da1, 2)]);
        assert!(seen(b"\x1b#Z").is_empty());
        // A C1 DECID inside any sequence but a string's data.
        assert_eq!(feed(b"\x1b[12\x9a").1, [query(Kind::Da1, 5)]);
        assert!(seen(b"\x1b]0;\x9a").is_empty());
        assert!(seen(b"\x9a").is_empty());
    }

    #[test]
    fn dropped_csis() {
        // 24 separators is too many parameters.
        let mut many = b"\x1b[?".to_vec();
        many.extend_from_slice(&b"1;".repeat(24));
        many.extend_from_slice(b"2004h");
        assert!(seen(&many).is_empty());
        // 23 is not.
        let mut ok = b"\x1b[?".to_vec();
        ok.extend_from_slice(&b"1;".repeat(23));
        ok.extend_from_slice(b"2004h");
        assert_eq!(seen(&ok), [Seen::Paste(PasteOp::Set)]);
        // Colons are for SGR only.
        assert!(seen(b"\x1b[?2004:1h").is_empty());
        // A private marker after a parameter ignores the rest.
        assert!(seen(b"\x1b[6?n").is_empty());
        assert!(seen(b"\x1b[:6n").is_empty());
    }

    #[test]
    fn unfinished_kinds() {
        for (bytes, kind) in [
            (&b"\xf0\x9f"[..], Unfinished::Utf8),
            (b"\x1b", Unfinished::Escape),
            (b"\x1b(", Unfinished::Escape),
            (b"\x1b[?20", Unfinished::Csi),
            (b"\x1bP1;2", Unfinished::Dcs),
            (b"\x1bPqdata", Unfinished::Dcs),
            (b"\x1b]5", Unfinished::Osc { number: None }),
            (b"\x1b]52;c;AAAA", Unfinished::Osc { number: Some(52) }),
            (b"\x1b]1337;File=", Unfinished::Osc { number: Some(1337) }),
            (b"\x1b]x;", Unfinished::Osc { number: None }),
            (b"\x1b_Gdata", Unfinished::SosPmApc),
            (b"\x1b^pm", Unfinished::SosPmApc),
            (b"\x1bXsos", Unfinished::SosPmApc),
        ] {
            let (t, _) = feed(bytes);
            assert_eq!(t.unfinished(), Some(kind), "{bytes:?}");
            assert_eq!(t.ground_offset(), 0, "{bytes:?}");
        }
        let (t, _) = feed(b"ok\x1b[3");
        assert_eq!(t.ground_offset(), 2);
        assert_eq!(t.fed(), 5);
    }

    #[test]
    fn reply_shapes() {
        assert_eq!(reply_kind(b"0", b'n'), Some(Kind::Dsr5));
        assert_eq!(reply_kind(b"24;80", b'R'), Some(Kind::Dsr6));
        assert_eq!(reply_kind(b"?1;2", b'c'), Some(Kind::Da1));
        assert_eq!(reply_kind(b">0;0;0", b'c'), Some(Kind::Da2));
        assert_eq!(reply_kind(b"5", b'n'), None);
        assert_eq!(reply_kind(b"?6;1", b'R'), None);
        assert_eq!(reply_kind(b"", b'c'), None);
    }
}
