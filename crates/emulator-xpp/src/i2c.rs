//! Declared register-file parts on one X-Processor Pro I2C bus.
//!
//! The first byte written after a write address, masked by `select_mask`,
//! selects a register (a mask keeps command bits out of the address); later
//! bytes write it and reads return it, `width` bytes per register in the
//! declared byte order before the selection moves to the next register.
//! Register values are declared inputs, and a read of an undeclared register
//! is unsupported. Addresses without a part are NACKed. Written values read
//! back; no part behavior beyond the register file is modeled, except an
//! opt-in interrupt output: a part may declare (register, value) writes that
//! drive its line high when their transfer ends with a stop, and the line
//! goes low at the next start addressed to the part, like a controller's
//! interrupt that a host access clears. The last transfers are kept for
//! inspection.
use fujimod_emulator_ip::dw_i2c::Bus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

const KEPT_TRANSFERS: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub address: u16,
    /// Bits of the first written byte that select the register.
    pub select_mask: u8,
    /// Bytes per register: 1 or 2.
    pub width: u8,
    pub little_endian: bool,
    pub registers: BTreeMap<u8, u16>,
    /// Registers that count up by one each time they are fully read, like a
    /// controller's scan counter that firmware polls for signs of life.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counters: Vec<u8>,
    /// Opt-in interrupt output; absent, the part drives no line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<Interrupt>,
}

/// A part's interrupt output: which writes raise it and on which line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interrupt {
    pub line: u32,
    /// (register, value) pairs; a complete write of one arms the line.
    pub writes: Vec<(u8, u16)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transfer {
    pub address: u16,
    pub read: bool,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    registers: Vec<BTreeMap<u8, u16>>,
    active: Option<usize>,
    selected: Option<u8>,
    byte: u8,
    transfers: VecDeque<Transfer>,
    /// The open write transfer wrote an interrupt value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    armed: bool,
    /// Parts whose interrupt line is high.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    raised: Vec<usize>,
    /// Line changes the controller has not driven yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lines: Vec<(u32, bool)>,
}

pub struct Parts {
    parts: Vec<Part>,
    state: State,
}

impl Parts {
    pub fn new(parts: Vec<Part>) -> Result<Self, String> {
        let mut addresses: Vec<_> = parts.iter().map(|p| p.address).collect();
        addresses.sort_unstable();
        addresses.dedup();
        if addresses.len() != parts.len()
            || parts.iter().any(|p| {
                p.address > 0x7f
                    || !matches!(p.width, 1 | 2)
                    || p.width == 1 && p.registers.values().any(|v| *v > 0xff)
                    || p.interrupt.as_ref().is_some_and(|i| i.writes.is_empty())
            })
        {
            return Err("Invalid I2C part address, width, register value or interrupt".into());
        }
        let state = State {
            registers: parts.iter().map(|p| p.registers.clone()).collect(),
            ..State::default()
        };
        Ok(Self { parts, state })
    }
    pub fn transfers(&self) -> impl Iterator<Item = &Transfer> {
        self.state.transfers.iter()
    }
    fn log(&mut self, byte: u8) {
        if let Some(transfer) = self.state.transfers.back_mut() {
            transfer.bytes.push(byte);
        }
    }
    /// Byte `n` of a register value in the part's byte order.
    fn shift(part: &Part, n: u8) -> u8 {
        8 * if part.little_endian {
            n
        } else {
            part.width - 1 - n
        }
    }
    fn next_byte(&mut self, part: &Part) {
        let s = &mut self.state;
        s.byte += 1;
        if s.byte == part.width {
            s.byte = 0;
            s.selected = s.selected.map(|r| r.wrapping_add(1) & part.select_mask);
        }
    }
}

impl Bus for Parts {
    fn start(&mut self, address: u16, read: bool) -> bool {
        let s = &mut self.state;
        s.active = self.parts.iter().position(|p| p.address == address);
        s.byte = 0;
        s.armed = false;
        if let Some(index) = s.active {
            if let Some(at) = s.raised.iter().position(|i| *i == index) {
                s.raised.remove(at);
                let line = self.parts[index].interrupt.as_ref().map_or(0, |i| i.line);
                s.lines.push((line, false));
            }
        }
        if !read {
            s.selected = None;
        }
        if s.transfers.len() == KEPT_TRANSFERS {
            s.transfers.pop_front();
        }
        s.transfers.push_back(Transfer {
            address,
            read,
            bytes: Vec::new(),
        });
        s.active.is_some()
    }
    fn write(&mut self, byte: u8) {
        self.log(byte);
        let Some(index) = self.state.active else {
            return;
        };
        let part = self.parts[index].clone();
        let Some(register) = self.state.selected else {
            self.state.selected = Some(byte & part.select_mask);
            return;
        };
        let shift = Self::shift(&part, self.state.byte);
        let value = self.state.registers[index].entry(register).or_default();
        *value = *value & !(0xff << shift) | u16::from(byte) << shift;
        let written = *value;
        if self.state.byte + 1 == part.width
            && part.interrupt.as_ref().is_some_and(|i| i.writes.contains(&(register, written)))
        {
            self.state.armed = true;
        }
        self.next_byte(&part);
    }
    fn read(&mut self) -> Result<u8, String> {
        let Some(index) = self.state.active else {
            return Err("I2C read without an addressed part".into());
        };
        let part = self.parts[index].clone();
        let register = self.state.selected.unwrap_or(0);
        let Some(value) = self.state.registers[index].get(&register).copied() else {
            return Err(format!(
                "I2C part {:#x} register {register:#x} is not declared",
                part.address
            ));
        };
        let byte = (value >> Self::shift(&part, self.state.byte)) as u8;
        if self.state.byte + 1 == part.width && part.counters.contains(&register) {
            let mask = if part.width == 1 { 0xff } else { 0xffff };
            self.state.registers[index].insert(register, value.wrapping_add(1) & mask);
        }
        self.next_byte(&part);
        self.log(byte);
        Ok(byte)
    }
    fn stop(&mut self) {
        let s = &mut self.state;
        if let Some(index) = s.active.filter(|_| s.armed) {
            if !s.raised.contains(&index) {
                s.raised.push(index);
                let line = self.parts[index].interrupt.as_ref().map_or(0, |i| i.line);
                s.lines.push((line, true));
            }
        }
        s.armed = false;
        s.active = None;
    }
    fn take_lines(&mut self) -> Vec<(u32, bool)> {
        std::mem::take(&mut self.state.lines)
    }
    fn set_registers(&mut self, address: u16, registers: &BTreeMap<u8, u16>) -> Result<bool, String> {
        let Some(index) = self.parts.iter().position(|p| p.address == address) else {
            return Ok(false);
        };
        if self.parts[index].width == 1 && registers.values().any(|v| *v > 0xff) {
            return Err(format!("I2C part {address:#x}: byte register value above 0xff"));
        }
        self.state.registers[index].extend(registers);
        Ok(true)
    }
    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap_or(Value::Null)
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value.clone())
            .map_err(|e| format!("invalid I2C parts checkpoint: {e}"))?;
        self.check(&state)?;
        self.state = state;
        Ok(())
    }
    fn checkpoint(&self) -> Result<fujimod_emulator_devices::checkpoint::Checkpoint, String> {
        fujimod_emulator_devices::checkpoint::Checkpoint::encode("xpp-i2c-parts", &self.state)
    }
    fn validate_checkpoint(
        &self,
        saved: &fujimod_emulator_devices::checkpoint::Checkpoint,
    ) -> Result<(), String> {
        if !saved.children().is_empty() || !saved.blobs().is_empty() {
            return Err("I2C parts checkpoint has unexpected parts".into());
        }
        self.check(&saved.decode("xpp-i2c-parts")?)
    }
    fn restore_checkpoint(
        &mut self,
        saved: &fujimod_emulator_devices::checkpoint::Checkpoint,
    ) -> Result<(), String> {
        self.validate_checkpoint(saved)?;
        self.state = saved.decode("xpp-i2c-parts")?;
        Ok(())
    }
}
impl Parts {
    fn check(&self, state: &State) -> Result<(), String> {
        if state.registers.len() != self.parts.len()
            || state.active.is_some_and(|i| i >= self.parts.len())
            || state.transfers.len() > KEPT_TRANSFERS
            || state.raised.iter().any(|i| self.parts.get(*i).is_none_or(|p| p.interrupt.is_none()))
        {
            return Err("I2C parts checkpoint differs from the declared parts".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauge() -> Parts {
        Parts::new(vec![Part {
            address: 0x36,
            select_mask: 0xff,
            width: 2,
            little_endian: true,
            registers: [(0x10, 0x1234)].into(),
            counters: Vec::new(),
            interrupt: None,
        }])
        .unwrap()
    }

    #[test]
    fn word_registers_select_read_and_write_in_byte_order() {
        let mut parts = gauge();
        assert!(!parts.start(0x37, false));
        assert!(parts.start(0x36, false));
        parts.write(0x10);
        assert!(parts.start(0x36, true));
        assert_eq!(parts.read(), Ok(0x34));
        assert_eq!(parts.read(), Ok(0x12));
        assert!(parts.read().unwrap_err().contains("register 0x11"));
        parts.stop();
        let saved = parts.save();
        assert!(parts.start(0x36, false));
        for byte in [0x18, 0xbb, 0x03] {
            parts.write(byte);
        }
        assert!(parts.start(0x36, false));
        parts.write(0x18);
        assert!(parts.start(0x36, true));
        assert_eq!(parts.read(), Ok(0xbb));
        assert_eq!(parts.read(), Ok(0x03));
        assert_eq!(parts.transfers().count(), 6);
        parts.restore(&saved).unwrap();
        assert_eq!(parts.transfers().count(), 3);
        assert!(
            Parts::new(vec![Part {
                address: 0x80,
                select_mask: 0xff,
                width: 1,
                little_endian: true,
                registers: BTreeMap::new(),
                counters: Vec::new(),
                interrupt: None,
            }])
            .is_err()
        );
    }

    #[test]
    fn outside_registers_replace_a_declared_report() {
        // X-T3 5.11 touch controller 0x55: 16-bit register 0x0000 sent as
        // [0x00, 0x00] lands the report in byte registers 1..11.
        let mut parts = Parts::new(vec![Part {
            address: 0x55,
            select_mask: 0xff,
            width: 1,
            little_endian: true,
            registers: (0..12).map(|r| (r, 0)).collect(),
            counters: Vec::new(),
            interrupt: None,
        }])
        .unwrap();
        assert_eq!(parts.set_registers(0x56, &BTreeMap::new()), Ok(false));
        assert!(parts.set_registers(0x55, &[(1, 0x100)].into()).is_err());
        assert_eq!(parts.set_registers(0x55, &[(1, 1), (3, 0x68)].into()), Ok(true));
        assert!(parts.start(0x55, false));
        parts.write(0x00);
        parts.write(0x00);
        assert!(parts.start(0x55, true));
        assert_eq!(parts.read(), Ok(1));
        assert_eq!(parts.read(), Ok(0));
        assert_eq!(parts.read(), Ok(0x68));
        let saved = parts.save();
        parts.restore(&saved).unwrap();
    }

    #[test]
    fn a_select_mask_keeps_command_bits_out_of_the_register_address() {
        // X-T20 2.11 0xc76484: TMD2772-style command byte 0xa0 | address.
        let mut parts = Parts::new(vec![Part {
            address: 0x39,
            select_mask: 0x1f,
            width: 1,
            little_endian: true,
            registers: [(0x18, 0x34), (0x19, 0x02)].into(),
            counters: Vec::new(),
            interrupt: None,
        }])
        .unwrap();
        assert!(parts.start(0x39, false));
        parts.write(0xb8);
        assert!(parts.start(0x39, true));
        assert_eq!(parts.read(), Ok(0x34));
        assert_eq!(parts.read(), Ok(0x02));
    }

    #[test]
    fn a_counter_register_counts_up_after_each_full_read() {
        let mut parts = Parts::new(vec![Part {
            address: 0x55,
            select_mask: 0xff,
            width: 1,
            little_endian: false,
            registers: [(0x1c, 0), (0x1d, 0xff)].into(),
            counters: vec![0x1d],
            interrupt: None,
        }])
        .unwrap();
        let mut reads = Vec::new();
        for _ in 0..3 {
            assert!(parts.start(0x55, false));
            parts.write(0x1c);
            parts.write(0x06);
            assert!(parts.start(0x55, true));
            reads.push(parts.read().unwrap());
            parts.stop();
        }
        assert_eq!(reads, [0xff, 0x00, 0x01]);
    }

    #[test]
    fn a_declared_write_raises_the_interrupt_until_the_next_access() {
        // X-T3 5.11 0x10D75C4: the driver writes 0x1c = 0x08 and waits for
        // the controller's interrupt on GIC 153.
        let mut parts = Parts::new(vec![Part {
            address: 0x55,
            select_mask: 0xff,
            width: 1,
            little_endian: false,
            registers: [(0x1c, 0), (0x1d, 2)].into(),
            counters: Vec::new(),
            interrupt: Some(Interrupt { line: 153, writes: vec![(0x1c, 0x08)] }),
        }])
        .unwrap();
        let write = |parts: &mut Parts, bytes: &[u8]| {
            assert!(parts.start(0x55, false));
            for byte in bytes {
                parts.write(*byte);
            }
            parts.stop();
        };
        write(&mut parts, &[0x1c, 0x00]);
        assert!(parts.take_lines().is_empty());
        write(&mut parts, &[0x1c, 0x08]);
        assert_eq!(parts.take_lines(), [(153, true)]);
        let saved = parts.save();
        write(&mut parts, &[0x1c, 0x08]);
        assert_eq!(parts.take_lines(), [(153, false), (153, true)]);
        // The pointer moved past 0x1c, so the read returns 0x1d.
        assert!(parts.start(0x55, true));
        assert_eq!(parts.read(), Ok(2));
        parts.stop();
        assert_eq!(parts.take_lines(), [(153, false)]);
        parts.restore(&saved).unwrap();
        assert!(parts.start(0x55, false));
        assert_eq!(parts.take_lines(), [(153, false)]);
        // Without the option the part never drives a line and saves as before.
        let mut plain = gauge();
        assert!(plain.start(0x36, false));
        plain.write(0x10);
        plain.stop();
        assert!(plain.take_lines().is_empty());
        assert!(plain.save().get("raised").is_none());
    }
}
