//! XP4 16-channel timer/PWM unit (X-T3 5.11: `0xfff3c000`, interrupts GIC 41..56).
//!
//! Kernel driver (X-T3 5.11, unit 0 `0x59594..0x5a330`), counts in 54 MHz ticks
//! (`0x59a60`):
//! - `+0x000` run word: bit `16 + n` enables the write of bit `n`, which runs
//!   (1) or stops (0) channel `n` (`0x5963c` start `0x10001 << n`, `0x59674`
//!   stop `0x10000 << n`); it reads back the running channels.
//! - `+0x010 + 4n` channel control: bit 4 one-shot, bits 12..13 mode, bits 14/15
//!   output options, bit 16 set by every setup, bit 5 cleared by `0x596ec`, bit 6
//!   cleared by `0x596a8`. Here bit 6 reads set once the channel has expired and
//!   stays set until a write clears it.
//! - `+0x100 + 16n`: `+0` count, `+4` count - 1 (the interval setup `0x59c80`
//!   writes both), `+8` / `+0xc` compare values (PWM, kept as written).
//! - `+0x080` shared PWM word, kept as written.
//!
//! A running channel expires after `+4` + 1 ticks, pulses its edge interrupt and
//! reloads from `+4`; a one-shot channel (control bit 4) stops instead. The
//! firmware's one-shot service (`0x106e738`) stops the channel and calls its
//! callback, e.g. the still-capture exposure timer (channel 7, GIC 48).
//! What the PWM outputs drive is not modeled.
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Register window size in bytes.
pub const WINDOW: u64 = 0x200;
const CHANNELS: usize = 16;
const EXPIRED: u32 = 1 << 6;
const ONE_SHOT: u32 = 1 << 4;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Channel {
    control: u32,
    words: [u32; 4],
    running: bool,
    next: Option<u64>,
    expiries: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    channels: Vec<Channel>,
    shared: u32,
}

pub struct Timers16 {
    base: u64,
    first_irq: u32,
    state: State,
}

impl Timers16 {
    pub fn new(base: u64, first_irq: u32) -> Result<Self, String> {
        if base & 0xfff != 0 || !(32..512 - CHANNELS as u32).contains(&first_irq) {
            return Err("Invalid XP4 16-channel timer".into());
        }
        Ok(Self {
            base,
            first_irq,
            state: State { channels: vec![Channel::default(); CHANNELS], shared: 0 },
        })
    }
    fn period(c: &Channel) -> u64 {
        u64::from(c.words[1]) + 1
    }
    /// Outside pulses on a channel used as a counter (X-T3 5.11: the command
    /// dials, rear on channel 2, front on channel 4, read at `+0x100 + 16n` by
    /// `0x5983c`): adds `delta` to the count word (wrapping) and returns the
    /// new count. Touches nothing else: no expiry, no interrupt, no schedule.
    pub fn add_count(&mut self, channel: usize, delta: i32) -> Result<u32, String> {
        let c = self.state.channels.get_mut(channel).ok_or("Invalid XP4 timer channel")?;
        c.words[0] = c.words[0].wrapping_add(delta as u32);
        Ok(c.words[0])
    }
    fn schedule(&mut self, cx: &mut Cx<'_>, n: usize) {
        let c = &mut self.state.channels[n];
        c.next = c.running.then(|| cx.now() + Self::period(c));
        if let Some(at) = c.next {
            cx.schedule(at, n as u64);
        }
    }
}

impl Device for Timers16 {
    fn name(&self) -> &'static str {
        "xp4-timers16"
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let offset = address.wrapping_sub(self.base);
        if size != 4 || offset & 3 != 0 || offset >= WINDOW {
            cx.unmodeled();
            return 0;
        }
        let s = &self.state;
        u64::from(match offset {
            0x000 => s.channels.iter().enumerate().fold(0, |bits, (n, c)| bits | (u32::from(c.running) << n)),
            0x010..=0x04c => s.channels[((offset - 0x10) / 4) as usize].control,
            0x080 => s.shared,
            0x100..=0x1fc => s.channels[((offset - 0x100) / 16) as usize].words[((offset & 0xf) / 4) as usize],
            _ => {
                cx.unmodeled();
                0
            }
        })
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        let offset = address.wrapping_sub(self.base);
        if size != 4 || offset & 3 != 0 || offset >= WINDOW || value > u64::from(u32::MAX) {
            return cx.unmodeled();
        }
        let value = value as u32;
        match offset {
            0x000 => {
                for n in 0..CHANNELS {
                    if value & (1 << (16 + n)) == 0 {
                        continue;
                    }
                    let run = value & (1 << n) != 0;
                    if run != self.state.channels[n].running {
                        self.state.channels[n].running = run;
                        self.schedule(cx, n);
                    }
                }
            }
            0x010..=0x04c => {
                let c = &mut self.state.channels[((offset - 0x10) / 4) as usize];
                // The expiry flag only clears; software cannot set it.
                c.control = value & !EXPIRED | (c.control & value & EXPIRED);
            }
            0x080 => self.state.shared = value,
            0x100..=0x1fc => {
                let n = ((offset - 0x100) / 16) as usize;
                self.state.channels[n].words[((offset & 0xf) / 4) as usize] = value;
            }
            _ => cx.unmodeled(),
        }
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        let now = cx.now();
        for n in 0..CHANNELS {
            let c = &mut self.state.channels[n];
            let Some(at) = c.next else { continue };
            if at > now {
                continue;
            }
            c.expiries += 1;
            c.control |= EXPIRED;
            if c.control & ONE_SHOT != 0 {
                c.running = false;
                c.next = None;
            } else {
                let period = Self::period(c);
                c.next = Some(at + ((now - at) / period + 1) * period);
                cx.schedule(c.next.unwrap(), n as u64);
            }
            let irq = self.first_irq + n as u32;
            cx.set_irq(irq, true);
            cx.set_irq(irq, false);
        }
    }
    fn event(&mut self, cx: &mut Cx<'_>, _token: u64) {
        self.advance(cx);
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        self.state.channels.iter().filter_map(|c| c.next).min()
    }
    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap_or(Value::Null)
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if state.channels.len() != CHANNELS {
            return Err("XP4 16-channel timer checkpoint".into());
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
    fn outside_pulses_move_only_the_count_word_of_their_channel() {
        let mut t = Timers16::new(0xfff3c000, 41).unwrap();
        let before = t.save();
        assert!(t.add_count(16, 1).is_err());
        assert_eq!(t.save(), before);
        assert_eq!(t.add_count(2, 1).unwrap(), 1);
        assert_eq!(t.add_count(2, -2).unwrap(), u32::MAX);
        assert_eq!(t.add_count(2, 1).unwrap(), 0);
        t.state.channels[4].words[0] = 0x7fff_ffff;
        assert_eq!(t.add_count(4, 1).unwrap(), 0x8000_0000);
        assert_eq!(t.add_count(4, -1).unwrap(), 0x7fff_ffff);
        for (n, c) in t.state.channels.iter().enumerate() {
            assert_eq!(c.words[1..], [0, 0, 0]);
            assert!(!c.running && c.next.is_none() && c.control == 0);
            if n != 4 {
                assert_eq!(c.words[0], 0);
            }
        }
        assert_eq!(t.next_deadline(0), None);
    }

    #[test]
    fn an_interval_channel_pulses_its_edge_line_and_a_one_shot_stops() {
        let mut t = Timers16::new(0xfff3c000, 41).unwrap();
        let mut h = RecordingHost::default();
        let w = |t: &mut Timers16, h: &mut RecordingHost, a: u64, v: u64| t.write(&mut Cx::new(h, 0, 0, 0), a, 4, v);
        let r = |t: &mut Timers16, h: &mut RecordingHost, a: u64| t.read(&mut Cx::new(h, 0, 0, 0), a, 4);
        // 0x59c80, one-shot on channel 7: control 0x10010, count 100, count - 1.
        w(&mut t, &mut h, 0xfff3c010 + 4 * 7, 0x10010);
        w(&mut t, &mut h, 0xfff3c100 + 16 * 7, 100);
        w(&mut t, &mut h, 0xfff3c104 + 16 * 7, 99);
        // 0x5963c: start.
        w(&mut t, &mut h, 0xfff3c000, 0x10001 << 7);
        assert_eq!(r(&mut t, &mut h, 0xfff3c000), 1 << 7);
        assert_eq!(t.next_deadline(0), Some(100));
        h.now = 99;
        t.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert!(h.irqs.is_empty());
        h.now = 100;
        t.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(h.irqs, vec![(48, true), (48, false)]);
        assert_eq!(r(&mut t, &mut h, 0xfff3c000), 0);
        assert_eq!(r(&mut t, &mut h, 0xfff3c010 + 4 * 7) & 0x40, 0x40);
        // 0x596a8 clears the flag.
        w(&mut t, &mut h, 0xfff3c010 + 4 * 7, 0x10010);
        assert_eq!(r(&mut t, &mut h, 0xfff3c010 + 4 * 7), 0x10010);
        // A periodic channel reloads until it is stopped (0x59674).
        w(&mut t, &mut h, 0xfff3c010, 0x10000);
        w(&mut t, &mut h, 0xfff3c104, 9);
        w(&mut t, &mut h, 0xfff3c000, 0x10001);
        h.now = 130;
        t.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(h.irqs.last(), Some(&(41, false)));
        assert_eq!(t.next_deadline(0), Some(140));
        w(&mut t, &mut h, 0xfff3c000, 0x10000);
        assert_eq!(t.next_deadline(0), None);
    }
}
