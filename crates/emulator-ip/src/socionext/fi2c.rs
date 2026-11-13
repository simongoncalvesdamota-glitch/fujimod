//! Socionext/Fujitsu F_I2C master (the controller Linux drives as
//! `i2c-synquacer`): byte registers at a four-byte stride, BSR +0x00,
//! BCR +0x04, CCR +0x08, ADR +0x0c, DAR +0x10, CSR +0x14, FSR +0x18,
//! BC2R +0x1c. Attached targets are a [`crate::dw_i2c::Bus`].
//!
//! XP4 X-T3 5.11 (driver 0x1077528 and handler 0x107712c, channel n at
//! 0xfff3d000 + 0x1000·n): with the bus free (BSR.BB clear) it writes the
//! address byte to DAR and sets BCR.MSS to start. Each byte ends with BCR.INT;
//! the handler reads BSR (TRX, LRB, FBT, AL) and BCR, writes the next byte to
//! DAR or reads the received one, and clears INT to continue. MSS with SCC
//! restarts with DAR as the new address byte; clearing MSS stops. A byte the
//! master receives is acknowledged when BCR.ACK is set by the write that
//! requests it, and LRB then reads the inverse. LRB after the address or a
//! written byte reads set when no target answers the address. A byte takes the
//! declared ticks. BC2R reads both lines high (an idle bus) with its written
//! drive bits. Slave mode, arbitration loss, bus errors and the clock divider
//! are not modeled; CCR, ADR, CSR and FSR read back their writes.
use crate::dw_i2c::Bus;
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const WINDOW: u64 = 0x20;
const BSR: u64 = 0x00;
const BCR: u64 = 0x04;
const CCR: u64 = 0x08;
const ADR: u64 = 0x0c;
const DAR: u64 = 0x10;
const CSR: u64 = 0x14;
const FSR: u64 = 0x18;
const BC2R: u64 = 0x1c;

const BB: u8 = 0x80;
const LRB: u8 = 0x10;
const TRX: u8 = 0x08;
const FBT: u8 = 0x01;

const INT: u8 = 0x01;
const INTE: u8 = 0x02;
const ACK: u8 = 0x08;
const MSS: u8 = 0x10;
const SCC: u8 = 0x20;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub base: u64,
    pub irq: u32,
    pub byte_ticks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Op {
    Address { address: u8, read: bool },
    Write(u8),
    Read { ack: bool },
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    bsr: u8,
    bcr: u8,
    dar: u8,
    words: [u8; 4],
    bc2r: u8,
    master: bool,
    addressed: bool,
    pending: Option<Op>,
    deadline: Option<u64>,
    line: bool,
}

pub struct Controller<B: Bus> {
    options: Options,
    bus: B,
    state: State,
}

impl<B: Bus> Controller<B> {
    pub fn new(options: Options, bus: B) -> Result<Self, String> {
        if options.byte_ticks == 0 || options.base & 0xff != 0 {
            return Err("F_I2C needs an aligned base and a non-zero byte time".into());
        }
        Ok(Self {
            options,
            bus,
            state: State::default(),
        })
    }
    fn schedule(&mut self, cx: &mut Cx<'_>, op: Op) {
        let at = cx.now() + self.options.byte_ticks;
        self.state.pending = Some(op);
        self.state.deadline = Some(at);
        cx.schedule(at, 0);
    }
    fn update_line(&mut self, cx: &mut Cx<'_>) {
        for (line, level) in self.bus.take_lines() {
            cx.set_irq(line, level);
        }
        let level = self.state.bcr & (INT | INTE) == INT | INTE;
        if level != self.state.line {
            self.state.line = level;
            cx.set_irq(self.options.irq, level);
        }
    }
    fn write_bcr(&mut self, cx: &mut Cx<'_>, value: u8) {
        let s = &mut self.state;
        let old = s.bcr;
        // INT only clears from software; SCC and BER read back as 0.
        s.bcr = value & !(INT | SCC | 0x80) | old & value & INT;
        let mss = value & MSS != 0;
        if !s.master && mss {
            s.master = true;
            s.bsr = BB;
            let op = Op::Address { address: s.dar >> 1, read: s.dar & 1 != 0 };
            self.schedule(cx, op);
        } else if s.master && !mss {
            s.master = false;
            s.addressed = false;
            s.bsr = 0;
            s.bcr &= !INT;
            s.pending = None;
            s.deadline = None;
            self.bus.stop();
        } else if s.master && value & SCC != 0 {
            let op = Op::Address { address: s.dar >> 1, read: s.dar & 1 != 0 };
            self.schedule(cx, op);
        } else if s.master && old & INT != 0 && value & INT == 0 {
            let op = if s.bsr & TRX != 0 {
                Op::Write(s.dar)
            } else {
                Op::Read { ack: value & ACK != 0 }
            };
            self.schedule(cx, op);
        }
    }
}

impl<B: Bus> Device for Controller<B> {
    fn name(&self) -> &'static str {
        "socionext-f-i2c"
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let offset = address.wrapping_sub(self.options.base);
        if !matches!(size, 1 | 4) || offset & 3 != 0 || offset >= WINDOW {
            cx.unmodeled();
            return 0;
        }
        let s = &self.state;
        u64::from(match offset {
            BSR => s.bsr,
            BCR => s.bcr,
            DAR => s.dar,
            BC2R => 0x30 | s.bc2r & 3,
            CCR | ADR | CSR | FSR => s.words[(offset / 4 - 2).min(3) as usize],
            _ => {
                cx.unmodeled();
                0
            }
        })
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        let offset = address.wrapping_sub(self.options.base);
        if !matches!(size, 1 | 4) || offset & 3 != 0 || offset >= WINDOW || value > 0xff {
            cx.unmodeled();
            return;
        }
        let value = value as u8;
        match offset {
            BCR => self.write_bcr(cx, value),
            DAR => self.state.dar = value,
            BC2R => self.state.bc2r = value & 3,
            CCR | ADR | CSR | FSR => self.state.words[(offset / 4 - 2).min(3) as usize] = value,
            _ => {
                cx.unmodeled();
                return;
            }
        }
        self.update_line(cx);
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        let Some(at) = self.state.deadline.filter(|at| *at <= cx.now()) else {
            return;
        };
        let _ = at;
        let Some(op) = self.state.pending.take() else {
            self.state.deadline = None;
            return;
        };
        self.state.deadline = None;
        match op {
            Op::Address { address, read } => {
                let ack = self.bus.start(u16::from(address), read);
                let s = &mut self.state;
                s.addressed = ack;
                s.bsr = BB | FBT | if read { 0 } else { TRX } | if ack { 0 } else { LRB };
            }
            Op::Write(byte) => {
                if self.state.addressed {
                    self.bus.write(byte);
                }
                let s = &mut self.state;
                s.bsr = BB | TRX | if s.addressed { 0 } else { LRB };
            }
            Op::Read { ack } => {
                let byte = if self.state.addressed {
                    match self.bus.read() {
                        Ok(byte) => byte,
                        Err(error) => {
                            cx.fail(error);
                            return;
                        }
                    }
                } else {
                    0xff
                };
                let s = &mut self.state;
                s.dar = byte;
                s.bsr = BB | if ack { 0 } else { LRB };
            }
        }
        self.state.bcr |= INT;
        self.update_line(cx);
    }
    fn event(&mut self, cx: &mut Cx<'_>, _token: u64) {
        self.advance(cx);
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        self.state.deadline
    }
    fn save(&self) -> Value {
        serde_json::json!({ "controller": self.state, "bus": self.bus.save() })
    }
    fn set_i2c_registers(
        &mut self,
        base: u64,
        address: u16,
        registers: &std::collections::BTreeMap<u8, u16>,
    ) -> Result<bool, String> {
        if base != self.options.base {
            return Ok(false);
        }
        if !self.bus.set_registers(address, registers)? {
            return Err(format!("F_I2C {base:#x}: no declared target {address:#x}"));
        }
        Ok(true)
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value["controller"].clone())
            .map_err(|e| format!("invalid F_I2C checkpoint: {e}"))?;
        self.bus.restore(&value["bus"])?;
        self.state = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::recording::RecordingHost;

    /// One target at 0x36 whose register 4 reads 0x5440, high byte first.
    #[derive(Default)]
    struct Gauge {
        active: bool,
        selected: Option<u8>,
        index: usize,
    }
    impl Bus for Gauge {
        fn start(&mut self, address: u16, read: bool) -> bool {
            self.active = address == 0x36;
            self.index = 0;
            if !read {
                self.selected = None;
            }
            self.active
        }
        fn write(&mut self, byte: u8) {
            self.selected = Some(byte);
        }
        fn read(&mut self) -> Result<u8, String> {
            if self.selected != Some(4) {
                return Err("undeclared register".into());
            }
            self.index += 1;
            Ok([0x54, 0x40][self.index - 1])
        }
        fn stop(&mut self) {
            self.active = false;
        }
        fn save(&self) -> Value {
            Value::Null
        }
        fn restore(&mut self, _: &Value) -> Result<(), String> {
            Ok(())
        }
    }
    const BASE: u64 = 0xfff3e000;
    fn wr(c: &mut Controller<Gauge>, h: &mut RecordingHost, o: u64, v: u8) {
        c.write(&mut Cx::new(h, 0, 0, 0), BASE + o, 1, u64::from(v));
    }
    fn rd(c: &mut Controller<Gauge>, h: &mut RecordingHost, o: u64) -> u8 {
        c.read(&mut Cx::new(h, 0, 0, 0), BASE + o, 1) as u8
    }
    fn finish(c: &mut Controller<Gauge>, h: &mut RecordingHost) {
        h.now += 100;
        c.advance(&mut Cx::new(h, 0, 0, 0));
        assert_eq!(rd(c, h, BCR) & INT, INT, "byte completion raises INT");
    }

    #[test]
    fn x_t3_register_read_writes_selects_restarts_and_reads_two_bytes() {
        let mut c = Controller::new(Options { base: BASE, irq: 66, byte_ticks: 100 }, Gauge::default()).unwrap();
        let mut h = RecordingHost::default();
        assert_eq!(rd(&mut c, &mut h, BSR) & BB, 0);
        // Start: DAR = 0x36 << 1 (write), BCR = BEIE | MSS | INTE.
        wr(&mut c, &mut h, DAR, 0x6c);
        wr(&mut c, &mut h, BCR, 0x52);
        finish(&mut c, &mut h);
        assert!(h.irqs.contains(&(66, true)));
        assert_eq!(rd(&mut c, &mut h, BSR), BB | FBT | TRX);
        // Register byte 4, continue (INT cleared).
        wr(&mut c, &mut h, DAR, 4);
        wr(&mut c, &mut h, BCR, 0x52);
        assert_eq!(h.irqs.last(), Some(&(66, false)));
        finish(&mut c, &mut h);
        assert_eq!(rd(&mut c, &mut h, BSR), BB | TRX);
        // Repeated start for reading: DAR = 0x6d, BCR = BEIE | SCC | MSS | INTE.
        wr(&mut c, &mut h, DAR, 0x6d);
        wr(&mut c, &mut h, BCR, 0x72);
        finish(&mut c, &mut h);
        assert_eq!(rd(&mut c, &mut h, BSR), BB | FBT);
        // Two bytes: ACK the first, not the last.
        wr(&mut c, &mut h, BCR, 0x5a);
        finish(&mut c, &mut h);
        assert_eq!((rd(&mut c, &mut h, DAR), rd(&mut c, &mut h, BSR)), (0x54, BB));
        wr(&mut c, &mut h, BCR, 0x52);
        finish(&mut c, &mut h);
        assert_eq!((rd(&mut c, &mut h, DAR), rd(&mut c, &mut h, BSR)), (0x40, BB | LRB));
        // Stop: MSS cleared frees the bus.
        wr(&mut c, &mut h, BCR, 0x42);
        assert_eq!(rd(&mut c, &mut h, BSR) & BB, 0);
        assert_eq!(rd(&mut c, &mut h, BC2R) & 0x30, 0x30);
    }

    #[test]
    fn an_absent_address_reads_lrb_after_the_address_byte() {
        let mut c = Controller::new(Options { base: BASE, irq: 66, byte_ticks: 100 }, Gauge::default()).unwrap();
        let mut h = RecordingHost::default();
        wr(&mut c, &mut h, DAR, 0x30 << 1);
        wr(&mut c, &mut h, BCR, 0x52);
        finish(&mut c, &mut h);
        assert_eq!(rd(&mut c, &mut h, BSR), BB | FBT | TRX | LRB);
    }
}
