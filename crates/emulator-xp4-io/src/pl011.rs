//! ARM PrimeCell PL011 UART, by default with nothing attached.
//!
//! X-T3 5.11 has three ports at `0xfff39000 + 0x1000·n` (n = 0..2). Its unit-5
//! driver (`0x1073eec` open, `0x107391c` baud rate) writes `+0x30` CR = 0, the
//! integer and fractional divisors `+0x24` / `+0x28`, then read-modify-writes
//! `+0x2c` LCR_H; `0x10739c8` clears bits of `+0x38` IMSC; `+0x40` MIS is read.
//! The update program opens port 0 and sends `01 03 9a 00 21` until it gives up.
//!
//! Model: the transmitter drains at once (FR: TX FIFO empty, never busy). Each
//! transmitted byte raises the transmit interrupt (RIS bit 5) until ICR clears
//! it, as QEMU's PL011 does. Transmitted bytes are kept (bounded) for the report.
//! The receiver is empty (FR: RX FIFO empty; DR reads 0) unless a scripted reply
//! is declared ([`Pl011::add_reply`]): when the bytes sent end with its trigger,
//! its answer is queued for reading, and RIS bit 4 stays set while bytes wait.
//! A reply is a test hypothesis, not a model of the far end. A port may
//! instead have an RL78 flash loader attached ([`Pl011::attach_rl78`]): every
//! byte sent goes to it and its answers are queued for reading; on a single
//! wire (TOOL0) the port also reads back each byte it sends. MIS = RIS & IMSC
//! drives the optional level line.
use crate::rl78::{Rl78, SIGNATURE};
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;

/// Register window size in bytes.
pub const WINDOW: u64 = 0x1000;
const TX_EMPTY: u32 = 1 << 7;
const RX_EMPTY: u32 = 1 << 4;
const TX_INTERRUPT: u32 = 1 << 5;
const RX_INTERRUPT: u32 = 1 << 4;
const KEPT_BYTES: usize = 4096;
const REPLIES: usize = 16;
const REPLY_BYTES: usize = 256;
/// PeriphID0..3 and PCellID0..3 of an r1p5 PL011.
const IDS: [u32; 8] = [0x11, 0x10, 0x34, 0x00, 0x0d, 0xf0, 0x05, 0xb1];

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Reply {
    when: Vec<u8>,
    answer: Vec<u8>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    ilpr: u32,
    ibrd: u32,
    fbrd: u32,
    lcr_h: u32,
    cr: u32,
    ifls: u32,
    imsc: u32,
    ris: u32,
    dmacr: u32,
    sent: Vec<u8>,
    sent_count: u64,
    #[serde(default)]
    replies: Vec<Reply>,
    /// The last bytes sent, as long as the longest trigger.
    #[serde(default)]
    heard: Vec<u8>,
    #[serde(default)]
    rx: VecDeque<u8>,
    #[serde(default)]
    received_count: u64,
    #[serde(default)]
    loader: Option<Rl78>,
    #[serde(default)]
    echo: bool,
}

pub struct Pl011 {
    name: &'static str,
    base: u64,
    irq: Option<u32>,
    state: State,
}

impl Pl011 {
    pub fn new(base: u64, irq: Option<u32>) -> Result<Self, String> {
        if base & 0xfff != 0 || irq.is_some_and(|line| !(32..512).contains(&line)) {
            return Err("Invalid PL011 UART".into());
        }
        // One name per port, so reports and checkpoints keep each one apart.
        let name = Box::leak(format!("pl011-uart@{base:#x}").into_boxed_str());
        Ok(Self {
            name,
            base,
            irq,
            state: State {
                ifls: 0x12,
                cr: 0x300,
                ..State::default()
            },
        })
    }
    pub fn base(&self) -> u64 {
        self.base
    }
    /// Declares a scripted answer: once the bytes sent end with `when`, the
    /// port receives `answer`.
    pub fn add_reply(&mut self, when: Vec<u8>, answer: Vec<u8>) -> Result<(), String> {
        if when.is_empty()
            || when.len() > REPLY_BYTES
            || answer.len() > REPLY_BYTES
            || self.state.replies.len() >= REPLIES
            || self.state.loader.is_some()
        {
            return Err("Invalid PL011 scripted reply".into());
        }
        self.state.replies.push(Reply { when, answer });
        Ok(())
    }
    /// Attaches an RL78 flash loader with this silicon signature; `echo` when
    /// the port shares one wire with it.
    pub fn attach_rl78(&mut self, signature: [u8; SIGNATURE], echo: bool) -> Result<(), String> {
        if !self.state.replies.is_empty() || self.state.loader.is_some() {
            return Err("PL011 far end already declared".into());
        }
        self.state.loader = Some(Rl78::new(signature));
        self.state.echo = echo;
        Ok(())
    }
    fn hear(&mut self, byte: u8) {
        let s = &mut self.state;
        if let Some(loader) = s.loader.as_mut() {
            let room = KEPT_BYTES.saturating_sub(s.rx.len());
            let echo = s.echo.then_some(byte);
            s.rx.extend(echo.into_iter().chain(loader.receive(byte)).take(room));
            return;
        }
        let longest = s.replies.iter().map(|r| r.when.len()).max().unwrap_or(0);
        if longest == 0 {
            return;
        }
        s.heard.push(byte);
        if s.heard.len() > longest {
            s.heard.drain(..s.heard.len() - longest);
        }
        if let Some(reply) = s.replies.iter().find(|r| s.heard.ends_with(&r.when)) {
            let room = KEPT_BYTES.saturating_sub(s.rx.len());
            s.rx.extend(reply.answer.iter().take(room));
            s.heard.clear();
        }
    }
    fn update(&mut self, cx: &mut Cx<'_>) {
        if self.state.rx.is_empty() {
            self.state.ris &= !RX_INTERRUPT;
        } else {
            self.state.ris |= RX_INTERRUPT;
        }
        if let Some(line) = self.irq {
            cx.set_irq(line, self.state.ris & self.state.imsc != 0);
        }
    }
}

impl Device for Pl011 {
    fn name(&self) -> &'static str {
        self.name
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let offset = address.wrapping_sub(self.base);
        let data = offset == 0 && matches!(size, 1 | 2 | 4);
        if !data && (size != 4 || offset & 3 != 0 || offset >= WINDOW) {
            cx.unmodeled();
            return 0;
        }
        if offset == 0 {
            let byte = self.state.rx.pop_front();
            if byte.is_some() {
                self.state.received_count += 1;
                self.update(cx);
            }
            return u64::from(byte.unwrap_or(0));
        }
        let s = &self.state;
        u64::from(match offset {
            0x004 => 0,
            0x018 => TX_EMPTY | if s.rx.is_empty() { RX_EMPTY } else { 0 },
            0x020 => s.ilpr,
            0x024 => s.ibrd,
            0x028 => s.fbrd,
            0x02c => s.lcr_h,
            0x030 => s.cr,
            0x034 => s.ifls,
            0x038 => s.imsc,
            0x03c => s.ris,
            0x040 => s.ris & s.imsc,
            0x048 => s.dmacr,
            0xfe0..=0xffc => IDS[((offset - 0xfe0) / 4) as usize],
            _ => {
                cx.unmodeled();
                0
            }
        })
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        let offset = address.wrapping_sub(self.base);
        let data = offset == 0 && matches!(size, 1 | 2 | 4);
        if !data && (size != 4 || offset & 3 != 0 || offset >= WINDOW) {
            return cx.unmodeled();
        }
        let value = value as u32;
        let s = &mut self.state;
        match offset {
            0x000 => {
                if s.sent.len() < KEPT_BYTES {
                    s.sent.push(value as u8);
                }
                s.sent_count += 1;
                s.ris |= TX_INTERRUPT;
            }
            0x004 => {}
            0x020 => s.ilpr = value & 0xff,
            0x024 => s.ibrd = value & 0xffff,
            0x028 => s.fbrd = value & 0x3f,
            0x02c => s.lcr_h = value & 0xff,
            0x030 => s.cr = value & 0xffff,
            0x034 => s.ifls = value & 0x3f,
            0x038 => s.imsc = value & 0x7ff,
            0x044 => s.ris &= !value,
            0x048 => s.dmacr = value & 7,
            _ => return cx.unmodeled(),
        }
        if offset == 0 {
            self.hear(value as u8);
        }
        self.update(cx);
    }
    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap_or(Value::Null)
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        let longest = state.replies.iter().map(|r| r.when.len()).max().unwrap_or(0);
        if state.sent.len() > KEPT_BYTES
            || state.rx.len() > KEPT_BYTES
            || state.replies.len() > REPLIES
            || state
                .replies
                .iter()
                .any(|r| r.when.is_empty() || r.when.len() > REPLY_BYTES || r.answer.len() > REPLY_BYTES)
            || state.heard.len() > longest
            || state.loader.as_ref().is_some_and(|l| !l.valid() || !state.replies.is_empty())
            || state.echo && state.loader.is_none()
        {
            return Err("PL011 UART checkpoint".into());
        }
        self.state = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::recording::RecordingHost;

    #[test]
    fn an_idle_port_takes_the_driver_setup_and_drains_what_it_sends() {
        let mut u = Pl011::new(0xfff39000, Some(97)).unwrap();
        let mut h = RecordingHost::default();
        let w = |u: &mut Pl011, h: &mut RecordingHost, a: u64, s: u8, v: u64| u.write(&mut Cx::new(h, 0, 0, 0), a, s, v);
        let r = |u: &mut Pl011, h: &mut RecordingHost, a: u64| u.read(&mut Cx::new(h, 0, 0, 0), a, 4);
        // 0x1073eec / 0x107391c: disable, divisors, line control.
        w(&mut u, &mut h, 0xfff39030, 4, 0);
        w(&mut u, &mut h, 0xfff39024, 4, 26);
        w(&mut u, &mut h, 0xfff39028, 4, 3);
        w(&mut u, &mut h, 0xfff3902c, 4, 0x70);
        assert_eq!(r(&mut u, &mut h, 0xfff39024), 26);
        assert_eq!(r(&mut u, &mut h, 0xfff39028), 3);
        assert_eq!(r(&mut u, &mut h, 0xfff39018), 0x90);
        assert_eq!(r(&mut u, &mut h, 0xfff39000), 0);
        // A transmitted byte raises TX; masked until IMSC enables it.
        w(&mut u, &mut h, 0xfff39030, 4, 0x301);
        w(&mut u, &mut h, 0xfff39000, 1, 0x41);
        assert_eq!(r(&mut u, &mut h, 0xfff3903c), TX_INTERRUPT as u64);
        assert_eq!(r(&mut u, &mut h, 0xfff39040), 0);
        w(&mut u, &mut h, 0xfff39038, 4, u64::from(TX_INTERRUPT));
        assert_eq!(r(&mut u, &mut h, 0xfff39040), TX_INTERRUPT as u64);
        assert_eq!(h.irqs.last(), Some(&(97, true)));
        w(&mut u, &mut h, 0xfff39044, 4, u64::from(TX_INTERRUPT));
        assert_eq!(r(&mut u, &mut h, 0xfff39040), 0);
        assert_eq!(h.irqs.last(), Some(&(97, false)));
        assert_eq!(u.state.sent, vec![0x41]);
        assert_eq!(r(&mut u, &mut h, 0xfff39fe0), 0x11);
        assert_eq!(r(&mut u, &mut h, 0xfff39ffc), 0xb1);
        assert_eq!(u.name(), "pl011-uart@0xfff39000");
        assert_ne!(u.name(), Pl011::new(0xfff3a000, None).unwrap().name());
    }

    #[test]
    fn a_scripted_reply_answers_the_update_frame_and_survives_a_checkpoint() {
        let mut u = Pl011::new(0xfff39000, Some(97)).unwrap();
        let mut h = RecordingHost::default();
        let w = |u: &mut Pl011, h: &mut RecordingHost, a: u64, v: u64| u.write(&mut Cx::new(h, 0, 0, 0), a, 4, v);
        let r = |u: &mut Pl011, h: &mut RecordingHost, a: u64| u.read(&mut Cx::new(h, 0, 0, 0), a, 4);
        let frame = [0x01, 0x03, 0x9a, 0x00, 0x21];
        u.add_reply(frame.to_vec(), vec![0x06, 0x01]).unwrap();
        assert!(u.add_reply(Vec::new(), vec![1]).is_err());
        w(&mut u, &mut h, 0xfff39038, u64::from(RX_INTERRUPT));
        // The first four bytes do not answer; the fifth completes the trigger.
        for byte in &frame[..4] {
            w(&mut u, &mut h, 0xfff39000, u64::from(*byte));
        }
        assert_eq!(r(&mut u, &mut h, 0xfff39018), 0x90);
        w(&mut u, &mut h, 0xfff39000, 0x21);
        assert_eq!(r(&mut u, &mut h, 0xfff39018), 0x80);
        assert_eq!(r(&mut u, &mut h, 0xfff39040), RX_INTERRUPT as u64);
        assert_eq!(h.irqs.last(), Some(&(97, true)));
        let saved = u.save();
        let mut resumed = Pl011::new(0xfff39000, Some(97)).unwrap();
        resumed.restore(&saved).unwrap();
        assert_eq!(r(&mut resumed, &mut h, 0xfff39000), 0x06);
        assert_eq!(r(&mut resumed, &mut h, 0xfff39000), 0x01);
        assert_eq!(r(&mut resumed, &mut h, 0xfff39018), 0x90);
        assert_eq!(r(&mut resumed, &mut h, 0xfff3903c) & u64::from(RX_INTERRUPT), 0);
        assert_eq!(h.irqs.last(), Some(&(97, false)));
        assert_eq!(resumed.state.received_count, 2);
    }

    #[test]
    fn an_attached_rl78_loader_answers_through_the_receiver() {
        let mut u = Pl011::new(0xfff39000, Some(253)).unwrap();
        let mut h = RecordingHost::default();
        let w = |u: &mut Pl011, h: &mut RecordingHost, a: u64, v: u64| u.write(&mut Cx::new(h, 0, 0, 0), a, 4, v);
        let r = |u: &mut Pl011, h: &mut RecordingHost, a: u64| u.read(&mut Cx::new(h, 0, 0, 0), a, 4);
        u.attach_rl78(crate::rl78::DEFAULT_SIGNATURE, true).unwrap();
        // One far end per port.
        assert!(u.add_reply(vec![1], vec![2]).is_err());
        let mut u2 = Pl011::new(0xfff3a000, None).unwrap();
        u2.add_reply(vec![1], vec![2]).unwrap();
        assert!(u2.attach_rl78(crate::rl78::DEFAULT_SIGNATURE, false).is_err());
        w(&mut u, &mut h, 0xfff39038, u64::from(RX_INTERRUPT));
        for byte in [0x01, 0x03, 0x9a, 0x00, 0x21, 0x42, 0x03] {
            w(&mut u, &mut h, 0xfff39000, byte);
        }
        assert_eq!(h.irqs.last(), Some(&(253, true)));
        // One wire: the seven bytes sent come back, then the loader's answer.
        let got: Vec<u8> = (0..14).map(|_| r(&mut u, &mut h, 0xfff39000) as u8).collect();
        assert_eq!(&got[..7], &[0x01, 0x03, 0x9a, 0x00, 0x21, 0x42, 0x03]);
        assert_eq!(&got[7..], &[0x02, 0x03, 0x06, 0x20, 0x00, 0xd7, 0x03]);
        assert_eq!(r(&mut u, &mut h, 0xfff39018), 0x90);
        let mut resumed = Pl011::new(0xfff39000, Some(253)).unwrap();
        resumed.restore(&u.save()).unwrap();
        assert!(resumed.state.loader.is_some());
    }
}
