//! X-T3 5.11 battery-gauge frames on the power controller's SPI channel.
//!
//! The gauge is serial resource 0x11 (0x106e084, table 0x13e4cbc): SPI 1, chip
//! select GPIO 12.6, beside the power controller (resource 0x10, GPIO 12.5).
//! 0x10a0af4 shifts four eight-bit words. A read (0x10a2cc0) sends the even
//! command and three zero bytes; the firmware replaces the first received
//! byte with the command, requires the four bytes to sum to zero modulo 256
//! and takes the 16-bit value from the second and third bytes. A write
//! (0x10a2e4c) sends the command with bit 0 set, the 16-bit value and a
//! two's-complement checksum; its reply is ignored.
//!
//! Read commands answer their declared input, or the last value written to a
//! declared writable command of the same number. What the gauge measures is
//! not modeled: every value is a declaration.
use crate::spi::Input;
use fujimod_emulator_ip::socionext::gpio::SharedPins;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    kind: String,
    port: usize,
    pin: usize,
    active_high: bool,
    idle: bool,
    inputs: BTreeMap<u8, u16>,
    writable: BTreeSet<u8>,
    stored: BTreeMap<u8, u16>,
    edge: u64,
    phase: u8,
    frame: [u8; 4],
    response: u16,
}

pub struct GaugeSerial {
    pins: SharedPins,
    state: State,
}

impl GaugeSerial {
    /// `inputs` and `writable` name even commands (bit 0 is the write flag).
    pub fn new(
        pins: SharedPins,
        port: usize,
        pin: usize,
        active_high: bool,
        idle: bool,
        inputs: BTreeMap<u8, u16>,
        writable: Vec<u8>,
    ) -> Result<Self, String> {
        let writable_set: BTreeSet<u8> = writable.iter().copied().collect();
        if port >= 16
            || pin >= 16
            || inputs.keys().chain(&writable_set).any(|c| c & 1 != 0)
            || writable_set.len() != writable.len()
        {
            return Err("Gauge SPI needs even commands and physical GPIO wiring".into());
        }
        Ok(Self {
            pins,
            state: State {
                kind: "gauge-registers".into(),
                port,
                pin,
                active_high,
                idle,
                inputs,
                writable: writable_set,
                stored: BTreeMap::new(),
                edge: 0,
                phase: 0,
                frame: [0; 4],
                response: 0,
            },
        })
    }
}

impl Input for GaugeSerial {
    fn exchange(&mut self, bits: u32, tx: u16, _tick: u64) -> Result<u16, String> {
        let s = &mut self.state;
        let (level, edge) = self.pins.lock().unwrap().output(s.port, s.pin);
        if edge != s.edge {
            s.phase = 0;
            s.edge = edge;
        }
        if level != s.active_high {
            return Ok(if s.idle { u16::MAX >> (16 - bits) } else { 0 });
        }
        if bits != 8 {
            return Err("Gauge SPI requires eight-bit serial words".into());
        }
        s.frame[usize::from(s.phase)] = tx as u8;
        let command = s.frame[0] & !1;
        let write = s.frame[0] & 1 != 0;
        let reply = match s.phase {
            0 => {
                s.response = if write {
                    0
                } else if let Some(value) = s.stored.get(&command) {
                    *value
                } else {
                    *s.inputs
                        .get(&command)
                        .ok_or_else(|| format!("Unsupported gauge SPI read command {command:#04x}"))?
                };
                0
            }
            1 => s.response >> 8,
            2 => s.response & 255,
            _ => {
                if write {
                    if s.frame.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) != 0 {
                        return Err("Invalid gauge SPI write checksum".into());
                    }
                    if !s.writable.contains(&command) {
                        return Err(format!(
                            "Unsupported gauge SPI write command {command:#04x} value {:#06x}",
                            u16::from_be_bytes([s.frame[1], s.frame[2]])
                        ));
                    }
                    s.stored
                        .insert(command, u16::from_be_bytes([s.frame[1], s.frame[2]]));
                    0
                } else {
                    let [hi, lo] = s.response.to_be_bytes();
                    u16::from(0u8.wrapping_sub(command.wrapping_add(hi).wrapping_add(lo)))
                }
            }
        };
        s.phase = (s.phase + 1) % 4;
        Ok(reply)
    }

    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap()
    }

    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        let s = &self.state;
        if state.kind != s.kind
            || state.port != s.port
            || state.pin != s.pin
            || state.active_high != s.active_high
            || state.idle != s.idle
            || state.inputs != s.inputs
            || state.writable != s.writable
            || state.stored.keys().any(|c| !s.writable.contains(c))
            || state.phase > 3
        {
            return Err("Invalid gauge SPI checkpoint or physical input".into());
        }
        self.state = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::{Cx, Device, Host, Record};
    use fujimod_emulator_ip::socionext::gpio::{Gpio, Pins};
    const BASE: u64 = 0x10000;

    struct Clock;
    impl Host for Clock {
        fn now(&self) -> u64 {
            0
        }
        fn schedule(&mut self, _: usize, _: u64, _: u64) {
            unreachable!()
        }
        fn set_irq(&mut self, _: u32, _: bool) {
            unreachable!()
        }
        fn raise_irq(&mut self, _: usize, _: u32) -> Result<(), String> {
            unreachable!()
        }
        fn ram_read(&self, _: u64, _: &mut [u8]) -> Result<(), String> {
            unreachable!()
        }
        fn ram_write(&mut self, _: u64, _: &[u8]) -> Result<(), String> {
            unreachable!()
        }
        fn record(&mut self, _: Record) {}
    }
    // GPIO 12.6 as an output (direction bit 22), driven high or low.
    fn select(gpio: &mut Gpio, high: bool) {
        gpio.write(
            &mut Cx::new(&mut Clock, 0, 0, 0),
            BASE + 12 * 64,
            4,
            (1 << 22) | if high { 1 << 6 } else { 0 },
        );
    }
    fn frame(g: &mut GaugeSerial, bytes: [u8; 4]) -> Result<[u8; 4], String> {
        let mut out = [0; 4];
        for (i, b) in bytes.into_iter().enumerate() {
            out[i] = g.exchange(8, u16::from(b), 0)? as u8;
        }
        Ok(out)
    }

    #[test]
    fn reads_answer_x_t3_checksums_and_writes_are_stored() {
        let pins = Pins::new([0; 16]);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let mut g = GaugeSerial::new(
            pins.clone(),
            12,
            6,
            true,
            false,
            BTreeMap::from([(0, 0xfd50), (0xce, 0x02e0)]),
            vec![0xca],
        )
        .unwrap();
        select(&mut gpio, true);
        // Read 0xce: reply [_, 0x02, 0xe0, ck] with 0xce + 0x02 + 0xe0 + ck = 0 mod 256.
        let r = frame(&mut g, [0xce, 0, 0, 0]).unwrap();
        assert_eq!((r[1], r[2]), (0x02, 0xe0));
        assert_eq!(0xceu8.wrapping_add(r[1]).wrapping_add(r[2]).wrapping_add(r[3]), 0);
        // Write 0xcb (command 0xca) = 0x0201 with a two's-complement checksum.
        let ck = 0u8.wrapping_sub(0xcbu8.wrapping_add(0x02).wrapping_add(0x01));
        frame(&mut g, [0xcb, 0x02, 0x01, ck]).unwrap();
        let r = frame(&mut g, [0xca, 0, 0, 0]).unwrap();
        assert_eq!((r[1], r[2]), (0x02, 0x01));
        // An undeclared read and a bad write checksum are refused.
        assert!(frame(&mut g, [0x14, 0, 0, 0]).is_err());
        let mut g2 = GaugeSerial::new(pins.clone(), 12, 6, true, false, BTreeMap::new(), vec![0xca])
            .unwrap();
        assert!(frame(&mut g2, [0xcb, 0x02, 0x01, 0]).is_err());
        // Deselected, the gauge drives its idle level and restarts its frame.
        select(&mut gpio, false);
        assert_eq!(g.exchange(8, 0xce, 0), Ok(0));
    }
}
