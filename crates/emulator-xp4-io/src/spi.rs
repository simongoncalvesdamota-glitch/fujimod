//! XP4 synchronous serial FIFO. The input pin is explicit; this does not
//! implement a companion protocol.
use fujimod_emulator_devices::{Cx, Device, Record, Value as Field};
use serde_json::{Value, json};
use std::collections::VecDeque;

pub trait Input: Send {
    fn exchange(&mut self, bits: u32, tx: u16, tick: u64) -> Result<u16, String>;
    fn save(&self) -> Value;
    fn restore(&mut self, value: &Value) -> Result<(), String>;
    /// Changes an outside-driven word mid-run (a switch the user moves).
    /// `Ok(false)`: this part has no such word.
    fn set_input(&mut self, _register: u8, _value: u16) -> Result<bool, String> {
        Ok(false)
    }
}
pub struct FixedPin(pub bool);
impl Input for FixedPin {
    fn exchange(&mut self, bits: u32, _: u16, _: u64) -> Result<u16, String> {
        Ok(if self.0 { u16::MAX >> (16 - bits) } else { 0 })
    }
    fn save(&self) -> Value {
        json!({"kind":"fixed-pin", "miso":self.0})
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        if value != &self.save() {
            return Err("XP4 SPI fixed pin differs".into());
        }
        Ok(())
    }
}
/// Several serial parts on one controller, each behind its own chip select
/// (X-T3 5.11: power controller on GPIO 12.5 and battery gauge on GPIO 12.6,
/// both on SPI 1). Every part sees every word and answers only while selected;
/// unselected parts drive their idle level, so they must idle low and the bus
/// returns the OR of their answers.
pub struct SharedBus(pub Vec<Box<dyn Input>>);
impl Input for SharedBus {
    fn exchange(&mut self, bits: u32, tx: u16, tick: u64) -> Result<u16, String> {
        let mut rx = 0;
        for part in &mut self.0 {
            rx |= part.exchange(bits, tx, tick)?;
        }
        Ok(rx)
    }
    fn save(&self) -> Value {
        json!({"kind": "shared-bus", "parts": self.0.iter().map(|p| p.save()).collect::<Vec<_>>()})
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let parts = value["parts"]
            .as_array()
            .filter(|p| value["kind"] == "shared-bus" && p.len() == self.0.len())
            .ok_or("XP4 SPI shared bus differs")?;
        for (part, saved) in self.0.iter_mut().zip(parts) {
            part.restore(saved)?;
        }
        Ok(())
    }
    fn set_input(&mut self, register: u8, value: u16) -> Result<bool, String> {
        for part in &mut self.0 {
            if part.set_input(register, value)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
pub struct Spi {
    base: u64,
    input: Box<dyn Input>,
    control: u32,
    enable: u32,
    divider: u32,
    causes: u32,
    tx: VecDeque<u16>,
    rx: VecDeque<u16>,
    pending: Option<(u64, u64, u16, u32)>,
    generation: u64,
    /// +0x24 DMA request enables: bit 0 transmit, bit 1 receive. The X-T3
    /// lens driver (0x106E0B0) sets both on SPI0. Stored and read back; the
    /// FIFO, shift timing and polling status are unchanged by it.
    request: u32,
}
impl Spi {
    /// See [`Input::set_input`].
    pub fn set_input(&mut self, register: u8, value: u16) -> Result<bool, String> {
        self.input.set_input(register, value)
    }
    pub fn new(base: u64, input: Box<dyn Input>) -> Self {
        Self {
            base,
            input,
            control: 0,
            enable: 0,
            divider: 0,
            causes: 0,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            pending: None,
            generation: 0,
            request: 0,
        }
    }
    fn start(&mut self, cx: &mut Cx<'_>) {
        self.start_at(cx, cx.now());
    }
    fn start_at(&mut self, cx: &mut Cx<'_>, now: u64) {
        if self.pending.is_some() || self.enable & 2 == 0 {
            return;
        }
        let Some(word) = self.tx.pop_front() else {
            return;
        };
        let bits = (self.control & 15) + 1;
        self.generation += 1;
        let at = now + u64::from(bits * self.divider * ((self.control >> 8 & 255) + 1));
        self.pending = Some((at, self.generation, word, bits));
        cx.schedule(at, self.generation);
    }
    fn access(
        &mut self,
        cx: &mut Cx<'_>,
        address: u64,
        size: u8,
        write: bool,
        value: u32,
    ) -> Result<u64, String> {
        let offset = address.wrapping_sub(self.base);
        if size != 4 || !matches!(offset, 0 | 4 | 8 | 0xc | 0x10 | 0x14 | 0x20 | 0x24) {
            cx.unmodeled();
            return Ok(0);
        }
        if !write {
            return Ok(match offset {
                0 => self.control,
                4 => self.enable,
                8 => u32::from(
                    self.rx
                        .pop_front()
                        .ok_or("XP4 SPI receive FIFO underflow")?,
                ),
                0xc => {
                    u32::from(self.tx.is_empty())
                        | (u32::from(self.tx.len() < 8) << 1)
                        | (u32::from(!self.rx.is_empty()) << 2)
                        | (u32::from(self.rx.len() == 8) << 3)
                        | (u32::from(self.pending.is_some()) << 4)
                }
                0x10 => self.divider,
                0x14 => self.causes,
                0x24 => self.request,
                _ => 0,
            } as u64);
        }
        match offset {
            0 => {
                if self.pending.is_some() || value & !0xffff != 0 || value & 0x30 != 0 {
                    return Err("Unsupported XP4 SPI format or active reconfiguration".into());
                }
                self.control = value;
            }
            4 => {
                if value & !0xb != 0 {
                    return Err("XP4 slave SPI is not modeled".into());
                }
                if self.pending.is_some() && value & 2 != 0 && value != self.enable {
                    return Err("XP4 SPI mode change while shifting".into());
                }
                self.enable = value;
                if value & 2 == 0 {
                    self.pending = None;
                    self.tx.clear();
                    self.generation += 1;
                } else {
                    self.start(cx);
                }
            }
            8 => {
                if self.control & 15 < 3
                    || !(2..=254).contains(&self.divider)
                    || self.divider & 1 != 0
                {
                    return Err("XP4 SPI word length/divider is not configured".into());
                }
                if self.tx.len() == 8 {
                    return Err("XP4 SPI transmit FIFO overflow".into());
                }
                self.tx.push_back(value as u16);
                self.start(cx);
            }
            0xc => return Err("Write to XP4 SPI status".into()),
            0x10 => {
                if self.pending.is_some() {
                    return Err("XP4 SPI clock change while shifting".into());
                }
                self.divider = value;
            }
            0x14 => self.causes &= !value,
            0x20 => {
                if value & !3 != 0 {
                    return Err("Unknown XP4 SPI FIFO clear bits".into());
                }
                if value & 1 != 0 {
                    self.tx.clear();
                }
                if value & 2 != 0 {
                    self.rx.clear();
                }
            }
            0x24 => {
                if value & !3 != 0 {
                    return Err("Unknown XP4 SPI request enable bits".into());
                }
                if value != self.request {
                    cx.record(Record::new(
                        "XP4 SPI DMA request enable",
                        [
                            ("base", Field::int(self.base)),
                            ("request", Field::int(value)),
                        ],
                    ));
                }
                self.request = value;
            }
            _ => {}
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::Host;
    #[derive(Default)]
    struct Clock {
        now: u64,
        records: Vec<Record>,
    }
    impl Host for Clock {
        fn now(&self) -> u64 {
            self.now
        }
        fn schedule(&mut self, _: usize, _: u64, _: u64) {}
        fn set_irq(&mut self, _: u32, _: bool) {
            panic!("polling SPI has no IRQ")
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
        fn record(&mut self, r: Record) {
            self.records.push(r);
        }
    }
    fn trace(pc: u64, pin: bool) -> (Value, Vec<Record>) {
        let mut m = Spi::new(0x1000, Box::new(FixedPin(pin)));
        let mut h = Clock::default();
        for (o, v) in [(0, 0xc7), (0x10, 2), (4, 0xa), (8, 0x12), (8, 0x34)] {
            let mut cx = Cx::new(&mut h, 3, pc, 0);
            m.write(&mut cx, 0x1000 + o, 4, v);
            assert_eq!(cx.refusal(), None);
        }
        assert_eq!(m.next_deadline(0), Some(16));
        let before = m.save();
        h.now = 15;
        m.event(&mut Cx::new(&mut h, 1, pc, 0), 1);
        assert_eq!(m.save(), before);
        h.now = 16;
        m.event(&mut Cx::new(&mut h, 1, pc, 0), 1);
        assert_eq!(m.next_deadline(16), Some(32));
        assert_eq!(m.rx.front(), Some(&(if pin { 255 } else { 0 })));
        let before = m.save();
        assert_eq!(m.save(), before);
        assert_eq!(
            m.read(&mut Cx::new(&mut h, 0, pc, 0), 0x1008, 4),
            if pin { 255 } else { 0 }
        );
        assert!(m.rx.is_empty());
        // Disabling aborts the queued shift; its old scheduled token is inert.
        m.write(&mut Cx::new(&mut h, 0, pc, 0), 0x1004, 4, 8);
        h.now = 32;
        m.event(&mut Cx::new(&mut h, 0, pc, 0), 2);
        assert!(m.rx.is_empty());
        assert_eq!(m.next_deadline(32), None);
        assert_eq!(h.records.len(), 1);
        (m.save(), h.records)
    }
    #[test]
    fn pin_sampling_fifo_deadlines_and_cancel_ignore_program_state() {
        for pin in [false, true] {
            assert_eq!(trace(0x2000, pin), trace(0x123456, pin));
        }
    }
    #[test]
    fn pending_fifo_words_keep_their_edges_after_restore() {
        let mut source = Spi::new(0x1000, Box::new(FixedPin(true)));
        let mut clock = Clock::default();
        for (offset, value) in [(0, 0xc7), (0x10, 2), (4, 0xa), (8, 0x12), (8, 0x34)] {
            source.write(&mut Cx::new(&mut clock, 0, 0, 0), 0x1000 + offset, 4, value);
        }
        let mut resumed = Spi::new(0x1000, Box::new(FixedPin(true)));
        resumed.restore(&source.save()).unwrap();
        for (now, token) in [(15, 1), (16, 1), (31, 2), (32, 2)] {
            clock.now = now;
            source.event(&mut Cx::new(&mut clock, 0, 0x1000, 0), token);
            resumed.event(&mut Cx::new(&mut clock, 3, 0xdead0000, 0), token);
            assert_eq!(source.save(), resumed.save());
        }
        assert_eq!(source.rx, [255, 255]);
    }
    #[test]
    fn dma_request_enable_is_stored_and_leaves_the_fifo_path_unchanged() {
        let run = |request: Option<u64>| {
            let mut m = Spi::new(0xfff32000, Box::new(FixedPin(true)));
            let mut h = Clock::default();
            // Lens driver order: format (bits 6/7, 8-bit), enable, divider, +0x24, causes.
            let mut writes = vec![(0, 0xc7), (4, 0xa), (0x10, 2)];
            writes.extend(request.map(|r| (0x24, r)));
            writes.extend([(0x14, 0xf), (8, 0x12), (8, 0x34)]);
            for (o, v) in writes {
                let mut cx = Cx::new(&mut h, 0, 0x106e0ec, 0);
                m.write(&mut cx, 0xfff32000 + o, 4, v);
                assert_eq!(cx.refusal(), None, "{o:#x} <- {v:#x}");
            }
            for (now, token) in [(16, 1), (32, 2)] {
                h.now = now;
                m.event(&mut Cx::new(&mut h, 0, 0, 0), token);
            }
            let request = m.read(&mut Cx::new(&mut h, 0, 0, 0), 0xfff32024, 4);
            let words: Vec<u64> = (0..2)
                .map(|_| m.read(&mut Cx::new(&mut h, 0, 0, 0), 0xfff32008, 4))
                .collect();
            (request, words, m.save(), h.records)
        };
        let (none, none_words, none_state, none_records) = run(None);
        let (zero, zero_words, zero_state, zero_records) = run(Some(0));
        let (dma, dma_words, dma_state, dma_records) = run(Some(3));
        assert_eq!((none, zero, dma), (0, 0, 3));
        assert_eq!(none_words, [255, 255]);
        assert_eq!((&zero_words, &dma_words), (&none_words, &none_words));
        // Writing 0 is invisible; 3 adds only the stored key and one record.
        assert_eq!((&zero_state, &zero_records), (&none_state, &none_records));
        assert!(none_state.get("request").is_none());
        assert_eq!(dma_state["request"], 3);
        assert_eq!(dma_records.len(), none_records.len() + 1);
        assert_eq!(dma_records[0].name, "XP4 SPI DMA request enable");
        let mut resumed = Spi::new(0xfff32000, Box::new(FixedPin(true)));
        resumed.restore(&dma_state).unwrap();
        assert_eq!(resumed.save(), dma_state);
        resumed.restore(&none_state).unwrap();
        assert_eq!(resumed.request, 0);
        let mut bad = dma_state.clone();
        bad["request"] = json!(4);
        assert!(resumed.restore(&bad).is_err());
        let mut h = Clock::default();
        let mut cx = Cx::new(&mut h, 0, 0, 0);
        resumed.write(&mut cx, 0xfff32024, 4, 4);
        assert!(cx.refusal().is_some());
    }
    #[test]
    fn queued_words_keep_their_clock_edges_when_events_are_delivered_late() {
        let mut outcomes = vec![];
        for calls in [[16, 32], [64, 64]] {
            let mut m = Spi::new(0x1000, Box::new(FixedPin(false)));
            let mut h = Clock::default();
            for (o, v) in [(0, 0xc7), (0x10, 2), (4, 0xb), (8, 0x12), (8, 0x34)] {
                m.write(&mut Cx::new(&mut h, 0, 0, 0), 0x1000 + o, 4, v);
            }
            for (i, now) in calls.into_iter().enumerate() {
                h.now = now;
                m.event(&mut Cx::new(&mut h, 0, 0, 0), i as u64 + 1);
            }
            assert_eq!(m.rx, [0x12, 0x34]);
            assert_eq!(m.next_deadline(h.now), None);
            outcomes.push((m.save(), h.records));
        }
        assert_eq!(outcomes[0], outcomes[1]);
    }
}
impl Device for Spi {
    fn name(&self) -> &'static str {
        match self.base {
            0xfff32000 => "xp4-spi0",
            0xfff33000 => "xp4-spi1",
            0xfff34000 => "xp4-spi2",
            0xfff35000 => "xp4-spi3",
            0xfff36000 => "xp4-spi4",
            0xfff37000 => "xp4-spi5",
            0xfff38000 => "xp4-spi6",
            _ => "xp4-spi",
        }
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        match self.access(cx, address, size, false, 0) {
            Ok(v) => v,
            Err(e) => {
                cx.fail(e);
                0
            }
        }
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        if let Err(e) = self.access(cx, address, size, true, value as u32) {
            cx.fail(e);
        }
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        self.pending.map(|p| p.0)
    }
    fn event(&mut self, cx: &mut Cx<'_>, token: u64) {
        let Some((at, generation, tx, bits)) = self.pending else {
            return;
        };
        if generation != token || cx.now() < at {
            return;
        }
        self.pending = None;
        if self.rx.len() == 8 {
            cx.fail("XP4 SPI receive FIFO overflow");
            return;
        }
        let mask = u16::MAX >> (16 - bits);
        let rx = if self.enable & 1 != 0 {
            tx & mask
        } else {
            match self.input.exchange(bits, tx & mask, at) {
                Ok(value) => value & mask,
                Err(error) => {
                    cx.fail(error);
                    return;
                }
            }
        };
        self.rx.push_back(rx);
        self.causes |= 1;
        cx.record(Record::new(
            "XP4 SPI word complete",
            [
                ("base", Field::int(self.base)),
                ("tx", Field::int(tx & mask)),
                ("rx", Field::int(rx)),
                ("bits", Field::int(bits)),
                ("tick", Field::int(at)),
            ],
        ));
        self.start_at(cx, at);
    }
    fn save(&self) -> Value {
        let mut state = json!({"base":self.base,"input":self.input.save(),"control":self.control,"enable":self.enable,
            "divider":self.divider,"causes":self.causes,"tx":self.tx,"rx":self.rx,"pending":self.pending,
            "generation":self.generation});
        // Absent when zero: checkpoints of polling-only runs stay byte-identical.
        if self.request != 0 {
            state["request"] = json!(self.request);
        }
        state
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct State {
            base: u64,
            input: Value,
            control: u32,
            enable: u32,
            divider: u32,
            causes: u32,
            tx: VecDeque<u16>,
            rx: VecDeque<u16>,
            pending: Option<(u64, u64, u16, u32)>,
            generation: u64,
            #[serde(default)]
            request: u32,
        }
        let state: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if state.base != self.base
            || state.control & !0xffcf != 0
            || state.enable & !0xb != 0
            || state.tx.len() > 8
            || state.rx.len() > 8
            || state.causes & !1 != 0
            || state.request & !3 != 0
            || state.pending.is_some_and(|(_, token, _, bits)| {
                token != state.generation
                    || state.enable & 2 == 0
                    || state.divider == 0
                    || bits != (state.control & 15) + 1
            })
        {
            return Err("Invalid XP4 SPI checkpoint or base".into());
        }
        self.input.restore(&state.input)?;
        self.control = state.control;
        self.enable = state.enable;
        self.divider = state.divider;
        self.causes = state.causes;
        self.tx = state.tx;
        self.rx = state.rx;
        self.pending = state.pending;
        self.generation = state.generation;
        self.request = state.request;
        Ok(())
    }
}
