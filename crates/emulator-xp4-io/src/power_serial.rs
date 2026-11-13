//! Bounded physical power-controller SPI register protocol, not a CPU harness.
//! 10a09fc clocks a 24-bit frame; 10a1d40 supplies the six-bit register address.
use crate::{rtc::Counter, spi::Input};
use fujimod_emulator_ip::socionext::gpio::SharedPins;
use serde_json::{Value, json};
use std::collections::BTreeMap;
/// Direct stop/load/start law over the shared counter.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct Rtc(Counter);
impl Rtc {
    fn counter(&self, tick: u64) -> Result<u32, String> {
        self.0.counter(tick)
    }
    fn read(&self, register: u8, tick: u64) -> Result<u16, String> {
        Ok(match register {
            32 => {
                if self.0.running {
                    0x8000
                } else {
                    0
                }
            }
            34 => self.counter(tick)? as u16,
            35 => (self.counter(tick)? >> 16) as u16,
            _ => unreachable!(),
        })
    }
    fn write(&mut self, register: u8, value: u16, tick: u64) -> Result<(), String> {
        if register == 32 {
            if !matches!(value, 0 | 0x8000) {
                return Err("Unsupported power RTC control bits".into());
            }
            self.0.set_running(value != 0, tick)?;
        } else {
            if self.0.running {
                return Err("Power RTC counter write while running".into());
            }
            let seconds = if register == 34 {
                (self.0.seconds & 0xffff0000) | u32::from(value)
            } else {
                (self.0.seconds & 0xffff) | u32::from(value) << 16
            };
            self.0.load(seconds, tick);
        }
        Ok(())
    }
}
pub struct PowerSerial {
    pins: SharedPins,
    port: usize,
    pin: usize,
    active_high: bool,
    idle: bool,
    inputs: BTreeMap<u8, u16>,
    /// Declared words moved mid-run (`set_input`); they read before `inputs`.
    moved: BTreeMap<u8, u16>,
    registers: BTreeMap<(bool, u8), u16>,
    edge: u64,
    phase: u8,
    header: u8,
    high: u8,
    response: u16,
    rtc: Rtc,
}
impl PowerSerial {
    pub fn new(
        pins: SharedPins,
        port: usize,
        pin: usize,
        active_high: bool,
        idle: bool,
        inputs: BTreeMap<u8, u16>,
        rtc_ticks_per_second: u64,
    ) -> Result<Self, String> {
        if inputs.iter().any(|(k, v)| (3..=6).contains(k) && *v > 1023)
            || port >= 16
            || pin >= 16
            // 13 and 27 are read-only status words X-T3 5.11 reads after the
            // first-start page (13 under semaphore 0x8f, 27 by the battery
            // monitor 0x10a1d90); their values are explicit body inputs.
            // 48..=55 hold the 16-byte settings header the firmware keeps in
            // the power controller (0x1124598 -> 0x10a33e8 writes 0x30..0x37):
            // a body that already finished first start declares it.
            || inputs
                .keys()
                .any(|k| !matches!(*k, 0..=6 | 13 | 16 | 24 | 27 | 32 | 34 | 35 | 48..=55))
            || [0, 1, 2, 3, 32, 34, 35]
                .iter()
                .any(|n| !inputs.contains_key(n))
            || inputs.get(&32).is_some_and(|v| !matches!(*v, 0 | 0x8000))
            || !(1..=1_000_000_000).contains(&rtc_ticks_per_second)
        {
            return Err(
                "Power SPI needs explicit ADC/status inputs and a physical chip select".into(),
            );
        }
        let rtc = Rtc(Counter::new(
            u32::from(inputs[&34]) | u32::from(inputs[&35]) << 16,
            inputs[&32] != 0,
            rtc_ticks_per_second,
        )?);
        Ok(Self {
            pins,
            port,
            pin,
            active_high,
            idle,
            inputs,
            moved: BTreeMap::new(),
            registers: BTreeMap::new(),
            edge: 0,
            phase: 0,
            header: 0,
            high: 0,
            response: 0,
            rtc,
        })
    }
    /// Words the firmware writes through 10a20d4. X-T3 5.11 also writes 13,
    /// 14, 16, 17 and 37 after the first-start page; a written word reads back
    /// its last write, an unwritten one its declared input (or 0).
    fn input(&self, register: u8) -> Option<&u16> {
        self.moved.get(&register).or_else(|| self.inputs.get(&register))
    }
    fn writable(register: u8) -> bool {
        matches!(register, 2 | 13 | 14 | 16 | 17 | 18 | 32 | 34 | 35 | 37 | 38 | 48..=55)
    }
}
impl Input for PowerSerial {
    fn exchange(&mut self, bits: u32, tx: u16, tick: u64) -> Result<u16, String> {
        let (level, edge) = self.pins.lock().unwrap().output(self.port, self.pin);
        if edge != self.edge {
            self.phase = 0;
            self.edge = edge;
        }
        if level != self.active_high {
            return Ok(if self.idle {
                u16::MAX >> (16 - bits)
            } else {
                0
            });
        }
        if bits != 8 {
            return Err("Power SPI requires eight-bit serial words".into());
        }
        let tx = tx as u8;
        match self.phase {
            0 => {
                self.header = tx;
                let register = tx >> 2;
                let bank = tx & 2 != 0;
                if bank && matches!(register, 32 | 34 | 35) {
                    return Err("Power RTC second bank is not modeled".into());
                }
                // Input 24 opts into a provisional fixed-response fixture.
                // The board accepts it only in discovery, never as strict hardware.
                let probe = self.inputs.contains_key(&24);
                if probe && bank && matches!(register, 23 | 24) {
                    return Err("Provisional power probe supports bank zero only".into());
                }
                if tx & 1 != 0 {
                    if !Self::writable(register) && !(probe && register == 23) {
                        return Err(format!("Unsupported power SPI write register{register}"));
                    }
                    self.response = 0;
                } else if matches!(register, 32 | 34 | 35) {
                    self.response = self.rtc.read(register, tick)?;
                } else if Self::writable(register) {
                    self.response = *self
                        .registers
                        .get(&(bank, register))
                        .or_else(|| self.input(register))
                        .unwrap_or(&0);
                } else if let Some(v) = self.input(register) {
                    self.response = *v;
                } else {
                    return Err(format!("Unsupported power SPI read register{register}"));
                }
                self.phase = 1;
                Ok(0)
            }
            1 => {
                self.high = tx;
                self.phase = 2;
                Ok(self.response >> 8)
            }
            _ => {
                if self.header & 1 != 0 {
                    let register = self.header >> 2;
                    let value = u16::from(self.high) << 8 | u16::from(tx);
                    if register == 23 {
                        if !self.inputs.contains_key(&24) || self.header & 2 != 0 || value != 0x0400
                        {
                            return Err("Unsupported provisional power probe payload".into());
                        }
                        // No retained register, inferred ready bit, or latency law:
                        // selector 24 remains the explicitly declared raw input.
                    } else if matches!(register, 32 | 34 | 35) {
                        self.rtc.write(register, value, tick)?;
                    } else {
                        if register == 38 && value & 0x8000 != 0 {
                            return Err("Power RTC alarm interrupt is not modeled".into());
                        }
                        self.registers
                            .insert((self.header & 2 != 0, register), value);
                    }
                }
                self.phase = 0;
                Ok(self.response & 255)
            }
        }
    }
    fn save(&self) -> Value {
        json!({"kind":"power-registers","port":self.port,"pin":self.pin,"active_high":self.active_high,
        "idle":self.idle,"inputs":self.inputs,"moved":self.moved,"registers":self.registers.iter().map(|(&(b,r),&v)|(b,r,v)).collect::<Vec<_>>(),
        "phase":self.phase,"header":self.header,"high":self.high,"response":self.response,"edge":self.edge,"rtc":self.rtc})
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct State {
            kind: String,
            port: usize,
            pin: usize,
            active_high: bool,
            idle: bool,
            inputs: BTreeMap<u8, u16>,
            #[serde(default)]
            moved: BTreeMap<u8, u16>,
            registers: Vec<(bool, u8, u16)>,
            phase: u8,
            header: u8,
            high: u8,
            response: u16,
            edge: u64,
            rtc: Rtc,
        }
        let state: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        let registers: BTreeMap<_, _> = state
            .registers
            .iter()
            .map(|(b, r, v)| ((*b, *r), *v))
            .collect();
        if state.kind != "power-registers"
            || state.port != self.port
            || state.pin != self.pin
            || state.active_high != self.active_high
            || state.idle != self.idle
            || state.inputs != self.inputs
            || state
                .moved
                .keys()
                .any(|r| Self::writable(*r) || !self.inputs.contains_key(r))
            || state.rtc.0.ticks_per_second != self.rtc.0.ticks_per_second
            || state.phase > 2
            || registers.len() != state.registers.len()
            || registers
                .keys()
                .any(|(_, r)| !Self::writable(*r) || [32, 34, 35].contains(r))
        {
            return Err("Invalid power SPI checkpoint or physical input".into());
        }
        self.moved = state.moved;
        self.registers = registers;
        self.phase = state.phase;
        self.header = state.header;
        self.high = state.high;
        self.response = state.response;
        self.edge = state.edge;
        self.rtc = state.rtc;
        Ok(())
    }
    /// A declared read-only word the outside world drives. X-T3 5.11: word 0
    /// is the key group the firmware samples through 10a1d40 (group 0), bit 10
    /// the power switch (0x8080 = ON, 0x8480 = OFF). Words the firmware writes
    /// keep their own law, and ADC words stay 10-bit.
    fn set_input(&mut self, register: u8, value: u16) -> Result<bool, String> {
        if Self::writable(register)
            || !self.inputs.contains_key(&register)
            || ((3..=6).contains(&register) && value > 1023)
        {
            return Err(format!("Power SPI input {register} cannot change mid-run"));
        }
        self.moved.insert(register, value);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_ip::socionext::gpio::{Gpio, Pins};
    const BASE: u64 = 0x10000;
    use fujimod_emulator_devices::{Cx, Device, Host, Record};
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
    #[test]
    fn rtc_counter_stop_load_start_wrap_and_observation() {
        let mut rtc = Rtc(Counter::new(0, false, 10).unwrap());
        rtc.write(34, 0xfffe, 4).unwrap();
        rtc.write(35, 0xffff, 5).unwrap();
        rtc.write(32, 0x8000, 6).unwrap();
        assert_eq!(rtc.read(34, 15).unwrap(), 0xfffe);
        assert_eq!(rtc.read(34, 16).unwrap(), 0xffff);
        assert_eq!(rtc.counter(26).unwrap(), 0);
        let saved = serde_json::to_value(&rtc).unwrap();
        for _ in 0..10 {
            assert_eq!(rtc.counter(36).unwrap(), 1);
        }
        assert_eq!(serde_json::to_value(&rtc).unwrap(), saved);
        assert!(rtc.write(34, 12, 36).is_err());
        assert!(rtc.write(32, 1, 36).is_err());
        rtc.write(32, 0, 36).unwrap();
        assert_eq!(rtc.counter(100).unwrap(), 1);
    }
    #[test]
    fn checkpoint_preserves_partial_frame_running_rtc_and_gpio_connection() {
        let pins = Pins::new([0; 16]);
        let inputs = [(0, 0), (1, 0), (2, 0), (3, 512), (32, 0), (34, 0), (35, 0)].into();
        let mut source =
            PowerSerial::new(pins.clone(), 12, 5, true, false, inputs, 54_000_000).unwrap();
        let mut resumed = PowerSerial::new(
            pins.clone(),
            12,
            5,
            true,
            false,
            source.inputs.clone(),
            54_000_000,
        )
        .unwrap();
        let mut gpio = Gpio::new(BASE, pins).unwrap();
        gpio.write(
            &mut Cx::new(&mut Clock, 0, 0, 0),
            BASE + 12 * 64,
            4,
            (1 << 21) | (1 << 5),
        );
        source.rtc.write(32, 0x8000, 7).unwrap();
        assert_eq!(
            source.save()["rtc"],
            json!({"seconds":0,"anchor":7,"running":true,"ticks_per_second":54_000_000})
        );
        source.exchange(8, 18 * 4 + 1, 8).unwrap();
        source.exchange(8, 0x12, 9).unwrap();
        resumed.restore(&source.save()).unwrap();
        for byte in [0x34, 18 * 4, 0, 0] {
            assert_eq!(
                source.exchange(8, byte, 10).unwrap(),
                resumed.exchange(8, byte, 10).unwrap()
            );
        }
        assert_eq!(source.save(), resumed.save());
        assert_eq!(resumed.rtc.counter(54_000_007).unwrap(), 1);
        gpio.write(
            &mut Cx::new(&mut Clock, 0, 0, 0),
            BASE + 12 * 64,
            4,
            1 << 21,
        );
        assert_eq!(resumed.exchange(8, 0, 11).unwrap(), 0);
    }
    #[test]
    fn a_declared_settings_header_reads_back_until_the_firmware_rewrites_it() {
        let pins = Pins::new([0; 16]);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let base = [(0, 0), (1, 0), (2, 0), (3, 512), (32, 0), (34, 0), (35, 0)];
        let mut inputs: BTreeMap<u8, u16> = base.into();
        inputs.insert(50, 2000);
        inputs.insert(54, 28);
        let mut peer =
            PowerSerial::new(pins.clone(), 12, 5, true, false, inputs, 54_000_000).unwrap();
        let mut beyond: BTreeMap<u8, u16> = base.into();
        beyond.insert(56, 1);
        assert!(PowerSerial::new(pins, 12, 5, true, false, beyond, 54_000_000).is_err());
        gpio.write(
            &mut Cx::new(&mut Clock, 0, 0, 0),
            BASE + 12 * 64,
            4,
            (1 << 21) | (1 << 5),
        );
        let read = |peer: &mut PowerSerial, register: u8| {
            peer.exchange(8, u16::from(register) << 2, 0).unwrap();
            let high = peer.exchange(8, 0, 0).unwrap();
            high << 8 | peer.exchange(8, 0, 0).unwrap()
        };
        assert_eq!(read(&mut peer, 50), 2000);
        assert_eq!(read(&mut peer, 54), 28);
        assert_eq!(read(&mut peer, 49), 0);
        // The firmware's own write wins over the declared value.
        for byte in [54 * 4 + 1, 0, 30] {
            peer.exchange(8, byte, 0).unwrap();
        }
        assert_eq!(read(&mut peer, 54), 30);
    }
    #[test]
    fn the_power_switch_word_changes_mid_run_and_survives_a_checkpoint() {
        let pins = Pins::new([0; 16]);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let inputs: BTreeMap<u8, u16> =
            [(0, 0x8080), (1, 0), (2, 0), (3, 512), (32, 0), (34, 0), (35, 0)].into();
        let mut peer =
            PowerSerial::new(pins.clone(), 12, 5, true, false, inputs.clone(), 54_000_000).unwrap();
        gpio.write(
            &mut Cx::new(&mut Clock, 0, 0, 0),
            BASE + 12 * 64,
            4,
            (1 << 21) | (1 << 5),
        );
        let read = |peer: &mut PowerSerial, register: u8| {
            peer.exchange(8, u16::from(register) << 2, 0).unwrap();
            let high = peer.exchange(8, 0, 0).unwrap();
            high << 8 | peer.exchange(8, 0, 0).unwrap()
        };
        assert_eq!(read(&mut peer, 0), 0x8080);
        // X-T3 5.11: bit 10 of key group 0 is the power switch.
        assert_eq!(peer.set_input(0, 0x8480), Ok(true));
        assert_eq!(read(&mut peer, 0), 0x8480);
        // Firmware-written, undeclared and out-of-range ADC words are refused.
        assert!(peer.set_input(2, 1).is_err());
        assert!(peer.set_input(5, 1).is_err());
        assert!(peer.set_input(3, 1024).is_err());
        let saved = peer.save();
        let mut resumed = PowerSerial::new(pins, 12, 5, true, false, inputs, 54_000_000).unwrap();
        resumed.restore(&saved).unwrap();
        assert_eq!(read(&mut resumed, 0), 0x8480);
    }
    #[test]
    fn physical_selection_frames_latches_and_unknown_commands() {
        let pins = Pins::new([0; 16]);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let mut peer = PowerSerial::new(
            pins,
            12,
            5,
            true,
            false,
            [(0, 0), (1, 0), (2, 0), (3, 512), (32, 0), (34, 0), (35, 0)].into(),
            54_000_000,
        )
        .unwrap();
        assert_eq!(peer.exchange(8, 12, 0).unwrap(), 0); // Unselected wire is low.
        let mut host = Clock;
        let mut cx = Cx::new(&mut host, 3, 0x765432, 0);
        gpio.write(&mut cx, BASE + 12 * 64, 4, (1 << 21) | (1 << 5));
        assert_eq!(
            [
                peer.exchange(8, 12, 0).unwrap(),
                peer.exchange(8, 0, 0).unwrap(),
                peer.exchange(8, 0, 0).unwrap()
            ],
            [0, 2, 0]
        );
        assert_eq!(
            [
                peer.exchange(8, 18 * 4 + 1, 0).unwrap(),
                peer.exchange(8, 0x12, 0).unwrap(),
                peer.exchange(8, 0x34, 0).unwrap()
            ],
            [0, 0, 0]
        );
        assert_eq!(
            [
                peer.exchange(8, 18 * 4, 0).unwrap(),
                peer.exchange(8, 0, 0).unwrap(),
                peer.exchange(8, 0, 0).unwrap()
            ],
            [0, 0x12, 0x34]
        );
        let saved = peer.save();
        assert_eq!(peer.save(), saved);
        peer.exchange(8, 12, 0).unwrap();
        gpio.write(&mut cx, BASE + 12 * 64, 4, 1 << 21);
        gpio.write(&mut cx, BASE + 12 * 64, 4, (1 << 21) | (1 << 5));
        assert_eq!(peer.exchange(8, 12, 0).unwrap(), 0);
        gpio.write(&mut cx, BASE + 12 * 64, 4, 1 << 21);
        gpio.write(&mut cx, BASE + 12 * 64, 4, (1 << 21) | (1 << 5));
        assert!(peer.exchange(8, 63 * 4, 0).is_err());
        peer.exchange(8, 38 * 4 + 1, 0).unwrap();
        peer.exchange(8, 0x80, 0).unwrap();
        assert!(peer.exchange(8, 0, 0).is_err());
        assert!(cx.refusal().is_none());
    }

    #[test]
    fn provisional_probe_is_explicit_exact_and_preserves_raw_status() {
        let pins = Pins::new([0; 16]);
        let mut gpio = Gpio::new(BASE, pins.clone()).unwrap();
        let make = |status: Option<u16>| {
            let mut inputs: BTreeMap<u8, u16> =
                [(0, 0), (1, 0), (2, 0), (3, 512), (32, 0), (34, 0), (35, 0)].into();
            if let Some(status) = status {
                inputs.insert(24, status);
            }
            PowerSerial::new(pins.clone(), 12, 5, true, false, inputs, 54_000_000).unwrap()
        };
        let mut host = Clock;
        let mut cx = Cx::new(&mut host, 3, 0, 0);
        gpio.write(&mut cx, BASE + 12 * 64, 4, (1 << 21) | (1 << 5));
        assert!(make(None).exchange(8, 0x5d, 0).is_err());
        assert!(make(None).exchange(8, 0x60, 0).is_err());
        for status in [0x00a5, 0xff0c] {
            let mut peer = make(Some(status));
            assert_eq!(peer.exchange(8, 0x5d, 0).unwrap(), 0);
            assert_eq!(peer.exchange(8, 4, 0).unwrap(), 0);
            let saved = peer.save();
            let mut restored = make(Some(status));
            restored.restore(&saved).unwrap();
            assert_eq!(restored.exchange(8, 0, 0).unwrap(), 0);
            assert_eq!(restored.save()["registers"], json!([]));
            assert_eq!(restored.exchange(8, 0x60, 0).unwrap(), 0);
            assert_eq!(restored.exchange(8, 0, 0).unwrap(), status >> 8);
            assert_eq!(restored.exchange(8, 0, 0).unwrap(), status & 255);
            assert!(make(Some(status ^ 1)).restore(&saved).is_err());
        }
        let mut wrong = make(Some(0xa5));
        wrong.exchange(8, 0x5d, 0).unwrap();
        wrong.exchange(8, 4, 0).unwrap();
        assert!(wrong.exchange(8, 1, 0).is_err());
        assert!(make(Some(0xa5)).exchange(8, 0x5f, 0).is_err());
        assert!(make(Some(0xa5)).exchange(8, 0x62, 0).is_err());
        let mut cancelled = make(Some(0xa5));
        cancelled.exchange(8, 0x5d, 0).unwrap();
        gpio.write(&mut cx, BASE + 12 * 64, 4, 1 << 21);
        gpio.write(&mut cx, BASE + 12 * 64, 4, (1 << 21) | (1 << 5));
        assert_eq!(cancelled.exchange(8, 0x60, 0).unwrap(), 0);
        assert_eq!(cancelled.exchange(8, 0, 0).unwrap(), 0);
        assert_eq!(cancelled.exchange(8, 0, 0).unwrap(), 0xa5);
    }
}
