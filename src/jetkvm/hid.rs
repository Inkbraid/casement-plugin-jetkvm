//! Keyboard and mouse for the device: Casement's PC set-1 scancodes as USB
//! HID usages, and the messages JetKVM takes for them.
//!
//! The device takes HID input on its `hidrpc` data channel (JetKVM's
//! internal/hidrpc): a handshake, then keypress reports (one key down or up;
//! the device keeps the full report), absolute pointer reports and, while a
//! key is held, a keep-alive every 50 ms — without it the device releases
//! the key after 100 ms, so a dropped connection can't leave a key stuck.
//! The wheel goes through JSON-RPC (`wheelReport`) on the `rpc` channel.
//! Devices whose firmware predates HID-RPC get everything as JSON-RPC.

use serde_json::json;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub const HANDSHAKE: u8 = 0x01;
pub const KEYPRESS_REPORT: u8 = 0x05;
pub const POINTER_REPORT: u8 = 0x03;
pub const KEEPALIVE: u8 = 0x09;
pub const HIDRPC_VERSION: u8 = 0x01;

/// How often a held key is kept alive.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(50);

/// The largest absolute pointer coordinate (the HID descriptor's maximum).
pub const ABS_MAX: i32 = 32767;

/// USB HID keyboard usage (page 0x07) for a PC set-1 scancode as Casement
/// sends it: 0x100 added for E0-prefixed keys.
pub fn usage(scancode: u32) -> Option<u8> {
    let u = match scancode {
        0x01 => 0x29,                                  // Escape
        0x02..=0x0A => 0x1E + (scancode - 0x02) as u8, // 1..9
        0x0B => 0x27,                                  // 0
        0x0C => 0x2D,                                  // -
        0x0D => 0x2E,                                  // =
        0x0E => 0x2A,                                  // Backspace
        0x0F => 0x2B,                                  // Tab
        0x10 => 0x14,                                  // Q
        0x11 => 0x1A,                                  // W
        0x12 => 0x08,                                  // E
        0x13 => 0x15,                                  // R
        0x14 => 0x17,                                  // T
        0x15 => 0x1C,                                  // Y
        0x16 => 0x18,                                  // U
        0x17 => 0x0C,                                  // I
        0x18 => 0x12,                                  // O
        0x19 => 0x13,                                  // P
        0x1A => 0x2F,                                  // [
        0x1B => 0x30,                                  // ]
        0x1C => 0x28,                                  // Enter
        0x1D => 0xE0,                                  // Left Ctrl
        0x1E => 0x04,                                  // A
        0x1F => 0x16,                                  // S
        0x20 => 0x07,                                  // D
        0x21 => 0x09,                                  // F
        0x22 => 0x0A,                                  // G
        0x23 => 0x0B,                                  // H
        0x24 => 0x0D,                                  // J
        0x25 => 0x0E,                                  // K
        0x26 => 0x0F,                                  // L
        0x27 => 0x33,                                  // ;
        0x28 => 0x34,                                  // '
        0x29 => 0x35,                                  // `
        0x2A => 0xE1,                                  // Left Shift
        0x2B => 0x31, // \ (# on ISO keyboards reports the same usage)
        0x2C => 0x1D, // Z
        0x2D => 0x1B, // X
        0x2E => 0x06, // C
        0x2F => 0x19, // V
        0x30 => 0x05, // B
        0x31 => 0x11, // N
        0x32 => 0x10, // M
        0x33 => 0x36, // ,
        0x34 => 0x37, // .
        0x35 => 0x38, // /
        0x36 => 0xE5, // Right Shift
        0x37 => 0x55, // Keypad *
        0x38 => 0xE2, // Left Alt
        0x39 => 0x2C, // Space
        0x3A => 0x39, // Caps Lock
        0x3B..=0x44 => 0x3A + (scancode - 0x3B) as u8, // F1..F10
        0x45 => 0x53, // Num Lock
        0x46 => 0x47, // Scroll Lock
        0x47 => 0x5F, // Keypad 7
        0x48 => 0x60, // Keypad 8
        0x49 => 0x61, // Keypad 9
        0x4A => 0x56, // Keypad -
        0x4B => 0x5C, // Keypad 4
        0x4C => 0x5D, // Keypad 5
        0x4D => 0x5E, // Keypad 6
        0x4E => 0x57, // Keypad +
        0x4F => 0x59, // Keypad 1
        0x50 => 0x5A, // Keypad 2
        0x51 => 0x5B, // Keypad 3
        0x52 => 0x62, // Keypad 0
        0x53 => 0x63, // Keypad .
        0x54 => 0x46, // SysRq (Alt+Print Screen)
        0x56 => 0x64, // the ISO key next to Left Shift
        0x57 => 0x44, // F11
        0x58 => 0x45, // F12
        0x59 => 0x67, // Keypad =
        0x64..=0x6E => 0x68 + (scancode - 0x64) as u8, // F13..F23
        0x70 => 0x88, // Katakana/Hiragana (International 2)
        0x73 => 0x87, // Ro (International 1)
        0x76 => 0x73, // F24
        0x79 => 0x8A, // Henkan (International 4)
        0x7B => 0x8B, // Muhenkan (International 5)
        0x7D => 0x89, // Yen (International 3)
        0x7E => 0x85, // Keypad , (Brazilian)
        0xF1 => 0x91, // Hanja (LANG2)
        0xF2 => 0x90, // Hangul (LANG1)
        0x11C => 0x58, // Keypad Enter
        0x11D => 0xE4, // Right Ctrl
        0x120 => 0x7F, // Mute
        0x12E => 0x81, // Volume Down
        0x130 => 0x80, // Volume Up
        0x135 => 0x54, // Keypad /
        0x137 => 0x46, // Print Screen
        0x138 => 0xE6, // Right Alt (AltGr)
        0x145 => 0x53, // Num Lock, as Windows reports it
        0x146 => 0x48, // Ctrl+Break reports Pause
        0x147 => 0x4A, // Home
        0x148 => 0x52, // Up
        0x149 => 0x4B, // Page Up
        0x14B => 0x50, // Left
        0x14D => 0x4F, // Right
        0x14F => 0x4D, // End
        0x150 => 0x51, // Down
        0x151 => 0x4E, // Page Down
        0x152 => 0x49, // Insert
        0x153 => 0x4C, // Delete
        0x15B => 0xE3, // Left Windows
        0x15C => 0xE7, // Right Windows
        0x15D => 0x65, // Menu (Application)
        0x15E => 0x66, // Power
        _ => return None,
    };
    Some(u)
}

const PAUSE_PREFIX: u32 = 0x21D;
const PAUSE: u32 = 0x45;
const PAUSE_USAGE: u8 = 0x48;

fn is_modifier(usage: u8) -> bool {
    (0xE0..=0xE7).contains(&usage)
}

/// HID buttons: bit 0 left, bit 1 right, bit 2 middle. Casement: 1 left,
/// 2 middle, 4 right.
pub fn hid_buttons(casement: u32) -> u8 {
    let mut b = 0;
    if casement & 1 != 0 {
        b |= 1;
    }
    if casement & 4 != 0 {
        b |= 2;
    }
    if casement & 2 != 0 {
        b |= 4;
    }
    b
}

/// A pointer position in surface pixels as the device's 0..=32767.
pub fn scale(pos: i32, size: u32) -> i32 {
    if size <= 1 {
        return 0;
    }
    let pos = pos.clamp(0, size as i32 - 1) as i64;
    ((pos * ABS_MAX as i64 + (size as i64 - 1) / 2) / (size as i64 - 1)) as i32
}

pub fn handshake() -> Vec<u8> {
    vec![HANDSHAKE, HIDRPC_VERSION]
}

pub fn keypress(usage: u8, press: bool) -> Vec<u8> {
    vec![KEYPRESS_REPORT, usage, press as u8]
}

pub fn pointer(x: i32, y: i32, buttons: u8) -> Vec<u8> {
    let mut m = vec![POINTER_REPORT];
    m.extend_from_slice(&x.to_be_bytes());
    m.extend_from_slice(&y.to_be_bytes());
    m.push(buttons);
    m
}

pub fn keepalive() -> Vec<u8> {
    vec![KEEPALIVE]
}

/// Where HID input goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Waiting for the handshake answer.
    Pending,
    HidRpc,
    /// Firmware without HID-RPC: JSON-RPC on the `rpc` channel.
    JsonRpc,
}

/// A message for one of the device's channels.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Hid(Vec<u8>),
    Rpc(serde_json::Value),
}

/// The keyboard and mouse state, turning Casement's input into messages.
pub struct Hid {
    pub mode: Mode,
    pause_prefix: bool,
    pause_down: bool,
    /// Keys down, as usages.
    held: BTreeSet<u8>,
    buttons: u8,
    last_pos: (i32, i32),
    next_keepalive: Option<Instant>,
    rpc_id: u64,
}

impl Default for Hid {
    fn default() -> Self {
        Hid {
            mode: Mode::Pending,
            pause_prefix: false,
            pause_down: false,
            held: BTreeSet::new(),
            buttons: 0,
            last_pos: (0, 0),
            next_keepalive: None,
            rpc_id: 0,
        }
    }
}

impl Hid {
    fn rpc(&mut self, method: &str, params: serde_json::Value) -> Out {
        self.rpc_id += 1;
        Out::Rpc(json!({ "jsonrpc": "2.0", "method": method, "params": params, "id": self.rpc_id }))
    }

    /// A key from Casement.
    pub fn key(&mut self, scancode: u32, down: bool, now: Instant) -> Vec<Out> {
        if scancode == PAUSE_PREFIX {
            self.pause_prefix = down;
            return Vec::new();
        }
        let usage = if scancode == PAUSE && (self.pause_prefix || (!down && self.pause_down)) {
            self.pause_prefix = false;
            self.pause_down = down;
            PAUSE_USAGE
        } else {
            match usage(scancode) {
                Some(u) => u,
                None => {
                    log::debug!("no HID usage for scancode {scancode:#x}");
                    return Vec::new();
                }
            }
        };
        if down {
            if !is_modifier(usage) && self.held.insert(usage) && self.next_keepalive.is_none() {
                self.next_keepalive = Some(now + KEEPALIVE_INTERVAL);
            }
            if is_modifier(usage) {
                self.held.insert(usage);
            }
        } else {
            self.held.remove(&usage);
            if !self.held.iter().any(|u| !is_modifier(*u)) {
                self.next_keepalive = None;
            }
        }
        vec![match self.mode {
            Mode::JsonRpc => self.rpc("keypressReport", json!({ "key": usage, "press": down })),
            _ => Out::Hid(keypress(usage, down)),
        }]
    }

    /// The pointer from Casement, in surface pixels of a `size` picture.
    pub fn pointer(&mut self, x: i32, y: i32, buttons: u32, size: (u32, u32)) -> Vec<Out> {
        let (ax, ay) = (scale(x, size.0), scale(y, size.1));
        self.buttons = hid_buttons(buttons);
        self.last_pos = (ax, ay);
        vec![self.pointer_report(ax, ay)]
    }

    fn pointer_report(&mut self, x: i32, y: i32) -> Out {
        let buttons = self.buttons;
        match self.mode {
            Mode::JsonRpc => self.rpc(
                "absMouseReport",
                json!({ "x": x, "y": y, "buttons": buttons }),
            ),
            _ => Out::Hid(pointer(x, y, buttons)),
        }
    }

    /// A wheel turn, positive away from the user (as HID's wheel).
    pub fn wheel(&mut self, delta: i32) -> Vec<Out> {
        let mut out = Vec::new();
        let mut left = delta;
        while left != 0 {
            let step = left.clamp(-127, 127);
            left -= step;
            out.push(self.rpc("wheelReport", json!({ "wheelY": step, "wheelX": 0 })));
        }
        out
    }

    /// Keep-alives due by `now` for held keys.
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        match self.next_keepalive {
            Some(t) if now >= t && self.mode == Mode::HidRpc => {
                let mut next = t + KEEPALIVE_INTERVAL;
                if next <= now {
                    next = now + KEEPALIVE_INTERVAL;
                }
                self.next_keepalive = Some(next);
                vec![Out::Hid(keepalive())]
            }
            _ => Vec::new(),
        }
    }

    /// When `tick` next has something to send.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.mode == Mode::HidRpc {
            self.next_keepalive
        } else {
            None
        }
    }

    /// Releases every held key and button, e.g. before disconnecting.
    pub fn release_all(&mut self) -> Vec<Out> {
        let held: Vec<u8> = self.held.iter().copied().collect();
        let mut out = Vec::new();
        for u in held {
            out.push(match self.mode {
                Mode::JsonRpc => self.rpc("keypressReport", json!({ "key": u, "press": false })),
                _ => Out::Hid(keypress(u, false)),
            });
        }
        self.held.clear();
        self.next_keepalive = None;
        if self.buttons != 0 {
            self.buttons = 0;
            let (x, y) = self.last_pos;
            out.push(self.pointer_report(x, y));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hid_bytes(v: Vec<Out>) -> Vec<Vec<u8>> {
        v.into_iter()
            .map(|o| match o {
                Out::Hid(b) => b,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn letters_digits_and_e0_keys() {
        assert_eq!(usage(0x1E), Some(0x04)); // A
        assert_eq!(usage(0x2C), Some(0x1D)); // Z
        assert_eq!(usage(0x10), Some(0x14)); // Q
        assert_eq!(usage(0x02), Some(0x1E)); // 1
        assert_eq!(usage(0x0A), Some(0x26)); // 9
        assert_eq!(usage(0x0B), Some(0x27)); // 0
        assert_eq!(usage(0x3B), Some(0x3A)); // F1
        assert_eq!(usage(0x44), Some(0x43)); // F10
        assert_eq!(usage(0x57), Some(0x44)); // F11
        assert_eq!(usage(0x11D), Some(0xE4)); // Right Ctrl
        assert_eq!(usage(0x148), Some(0x52)); // Up
        assert_eq!(usage(0x153), Some(0x4C)); // Delete
        assert_eq!(usage(0x15B), Some(0xE3)); // Left Windows
        assert_eq!(usage(0x47), Some(0x5F)); // Keypad 7
        assert_eq!(usage(0x147), Some(0x4A)); // Home
        assert_eq!(usage(0x999), None);
    }

    #[test]
    fn every_set1_letter_maps_to_a_distinct_usage() {
        let letters = [
            0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
        ];
        let usages: Vec<u8> = letters.iter().map(|s| usage(*s).unwrap()).collect();
        assert_eq!(usages, (0x04..=0x1D).collect::<Vec<u8>>(), "A..Z in order");
    }

    #[test]
    fn keys_hold_and_keep_alive() {
        let t0 = Instant::now();
        let mut h = Hid {
            mode: Mode::HidRpc,
            ..Default::default()
        };
        assert_eq!(hid_bytes(h.key(0x1E, true, t0)), vec![vec![0x05, 0x04, 1]]);
        assert!(h.tick(t0).is_empty());
        assert_eq!(h.next_deadline(), Some(t0 + KEEPALIVE_INTERVAL));
        assert_eq!(hid_bytes(h.tick(t0 + KEEPALIVE_INTERVAL)), vec![vec![0x09]]);
        assert_eq!(h.next_deadline(), Some(t0 + 2 * KEEPALIVE_INTERVAL));
        assert_eq!(hid_bytes(h.key(0x1E, false, t0)), vec![vec![0x05, 0x04, 0]]);
        assert_eq!(h.next_deadline(), None);
        // Modifiers alone need no keep-alive (the device doesn't auto-release them).
        h.key(0x2A, true, t0);
        assert_eq!(h.next_deadline(), None);
        assert_eq!(hid_bytes(h.release_all()), vec![vec![0x05, 0xE1, 0]]);
    }

    #[test]
    fn pause_comes_as_prefix_and_0x45() {
        let t0 = Instant::now();
        let mut h = Hid {
            mode: Mode::HidRpc,
            ..Default::default()
        };
        assert!(h.key(0x21D, true, t0).is_empty());
        assert_eq!(hid_bytes(h.key(0x45, true, t0)), vec![vec![0x05, 0x48, 1]]);
        assert!(h.key(0x21D, false, t0).is_empty());
        assert_eq!(hid_bytes(h.key(0x45, false, t0)), vec![vec![0x05, 0x48, 0]]);
        // On its own 0x45 is Num Lock.
        assert_eq!(hid_bytes(h.key(0x45, true, t0)), vec![vec![0x05, 0x53, 1]]);
    }

    #[test]
    fn pointer_scaling_and_buttons() {
        assert_eq!(scale(0, 1920), 0);
        assert_eq!(scale(1919, 1920), ABS_MAX);
        assert_eq!(scale(5000, 1920), ABS_MAX);
        assert_eq!(scale(-3, 1920), 0);
        assert_eq!(scale(960, 1921), 16384);
        assert_eq!(hid_buttons(1), 1);
        assert_eq!(hid_buttons(4), 2);
        assert_eq!(hid_buttons(2), 4);
        assert_eq!(hid_buttons(7), 7);
        let mut h = Hid {
            mode: Mode::HidRpc,
            ..Default::default()
        };
        let b = hid_bytes(h.pointer(1919, 0, 4, (1920, 1080)));
        assert_eq!(b, vec![vec![0x03, 0, 0, 0x7F, 0xFF, 0, 0, 0, 0, 2]]);
        assert_eq!(
            hid_bytes(h.release_all()),
            vec![vec![0x03, 0, 0, 0x7F, 0xFF, 0, 0, 0, 0, 0]]
        );
    }

    #[test]
    fn json_rpc_fallback_and_wheel() {
        let mut h = Hid {
            mode: Mode::JsonRpc,
            ..Default::default()
        };
        let out = h.key(0x1E, true, Instant::now());
        let Out::Rpc(v) = &out[0] else { panic!() };
        assert_eq!(v["method"], "keypressReport");
        assert_eq!(v["params"], json!({ "key": 4, "press": true }));
        let out = h.wheel(-300);
        let steps: Vec<i64> = out
            .iter()
            .map(|o| match o {
                Out::Rpc(v) => v["params"]["wheelY"].as_i64().unwrap(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(steps, vec![-127, -127, -46]);
    }
}
