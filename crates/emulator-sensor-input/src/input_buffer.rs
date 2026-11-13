//! Input-buffer write-select enables, frame causes and statistics DMA.
//! Processor wiring supplies the CSR layout, supported outputs and frame clock.
use super::statistics::{Planes, Statistics};
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Output {
    pub enable_word: usize,
    pub bit: u32,
    pub low: u64,
    pub high: u64,
    pub high_mask: u32,
    pub columns: usize,
    pub rows: usize,
    pub planes: Planes,
}
#[derive(Clone)]
pub struct Wiring {
    pub base: u64,
    pub irq: u32,
    pub status: u64,
    pub irq_control: u64,
    pub ready: u64,
    pub max_offset: u64,
    pub write_limit: u64,
    pub enables: [u32; 2],
    pub causes: [u32; 2],
    pub outputs: Vec<Output>,
    pub reset_ticks: u64,
    /// None receives frame edges from an explicitly connected upstream model.
    pub period_ticks: Option<u64>,
    /// Ignore enable bits of undeclared channels instead of reporting them
    /// (XP4 X-T3 5.11 live view sets some); they are never armed.
    pub ignore_undeclared: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    regs: BTreeMap<u64, u32>,
    generation: u64,
    armed: bool,
    frames: u64,
    deadline: Option<u64>,
    reset_deadline: Option<u64>,
    /// Every enable bit the firmware set in +0x04 / +0x08, declared or not
    /// (report only: undeclared bits arm nothing).
    #[serde(default)]
    requested: [u32; 2],
    /// The last writes to the control words +0x00, +0x04, +0x08, +0x0c,
    /// +0x224, +0x228 and +0x2e4, as (tick, offset, value) (report only).
    #[serde(default)]
    control_writes: std::collections::VecDeque<(u64, u64, u32)>,
    /// Declared memory read-back law: (event bits of +0x0c, ticks). Writing 1
    /// to +0x224 (X-T3 5.11 still capture; live view writes 0x80605) starts one
    /// read-back of the RAW in RAM that ends after `ticks` with these bits.
    #[serde(default)]
    readback: Option<(u32, u64)>,
    #[serde(default)]
    readback_deadline: Option<u64>,
    #[serde(default)]
    readbacks: u64,
}
const CONTROL_WRITES: usize = 64;
impl State {
    fn r(&self, o: u64) -> u32 {
        self.regs.get(&o).copied().unwrap_or(0)
    }
}
#[derive(Clone)]
pub struct InputBuffer {
    state: Arc<Mutex<State>>,
    wiring: Wiring,
    statistics: Option<Statistics>,
}
impl InputBuffer {
    /// Declares the memory read-back law (a test hypothesis about which +0x0c
    /// event ends a read-back; see `State::readback`).
    pub fn set_readback(&self, event: u32, ticks: u64) -> Result<(), String> {
        if event == 0 || event & !0xffff != 0 || ticks == 0 {
            return Err("invalid input-buffer read-back law".into());
        }
        self.state.lock().unwrap().readback = Some((event, ticks));
        Ok(())
    }
    pub fn set_statistics(&mut self, statistics: Statistics) {
        self.statistics = Some(statistics);
    }
    pub fn new(wiring: Wiring, statistics: Option<Statistics>) -> Result<Self, String> {
        if wiring.base & 0xfff != 0
            || wiring.period_ticks == Some(0)
            || wiring.enables.iter().any(|m| m & !0xffff != 0)
            || wiring.outputs.iter().any(|o| {
                o.enable_word > 1
                    || o.bit.count_ones() != 1
                    || o.bit & wiring.enables[o.enable_word] == 0
                    || !(1..=16).contains(&o.columns)
                    || !(1..=16).contains(&o.rows)
            })
        {
            return Err("invalid input-buffer wiring".into());
        }
        Ok(Self {
            state: Arc::default(),
            wiring,
            statistics,
        })
    }
    fn irq(&self, s: &State, cx: &mut Cx<'_>) {
        cx.set_irq(
            self.wiring.irq,
            s.r(self.wiring.status) != 0 && s.r(self.wiring.irq_control) & 0x100 != 0,
        );
    }
    /// A physical upstream frame edge. No firmware task or program state is read.
    pub fn frame(&self, cx: &mut Cx<'_>) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        if !s.armed || s.reset_deadline.is_some() {
            return Ok(());
        }
        let mut pending =
            (s.r(4) & self.wiring.causes[0]) | ((s.r(8) & self.wiring.causes[1]) << 16);
        for output in &self.wiring.outputs {
            if s.r(4 + output.enable_word as u64 * 4) & output.bit != 0 {
                let address = u64::from(s.r(output.low))
                    | (u64::from(s.r(output.high) & output.high_mask) << 32);
                if address == 0 {
                    if output.planes.completes() {
                        pending &= !(output.bit << (output.enable_word * 16));
                    }
                } else {
                    self.statistics
                        .as_ref()
                        .ok_or("input-buffer output has no statistics source")?
                        .complete(
                            cx,
                            address,
                            output.columns,
                            output.rows,
                            output.planes.resolve(|o| s.r(o)),
                        )?;
                }
            }
        }
        let pending = s.r(self.wiring.status) | pending;
        s.regs.insert(self.wiring.status, pending);
        s.frames += 1;
        self.irq(&s, cx);
        Ok(())
    }
    fn schedule(&self, s: &mut State, cx: &mut Cx<'_>, anchor: u64) {
        if let Some(period) = self.wiring.period_ticks {
            let Some(at) = anchor.checked_add(period) else {
                cx.fail("input-buffer deadline overflow");
                return;
            };
            s.deadline = Some(at);
            cx.schedule(at, s.generation);
        }
    }
}
impl Device for InputBuffer {
    fn name(&self) -> &'static str {
        "imaging-input-buffer"
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, n: u8) -> u64 {
        self.advance(cx);
        let o = address.wrapping_sub(self.wiring.base);
        if n != 4 || o & 3 != 0 || o > self.wiring.max_offset {
            cx.unmodeled();
            return 0;
        }
        let s = self.state.lock().unwrap();
        u64::from(if o == 0 {
            (s.r(o) & !1) | u32::from(s.reset_deadline.is_none())
        } else if o == self.wiring.ready {
            s.r(o)
                | if s.r(self.wiring.irq_control) & 0x100 == 0 && s.reset_deadline.is_none() {
                    0x10000
                } else {
                    0
                }
        } else {
            s.r(o)
        })
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, n: u8, value: u64) {
        self.advance(cx);
        let o = address.wrapping_sub(self.wiring.base);
        if n != 4 || o & 3 != 0 || o > self.wiring.max_offset {
            cx.unmodeled();
            return;
        }
        if o > self.wiring.write_limit {
            cx.unmodeled();
            return;
        }
        let value = value as u32;
        let mut s = self.state.lock().unwrap();
        if matches!(o, 0 | 4 | 8 | 0xc | 0x224 | 0x228 | 0x2e4) {
            if s.control_writes.len() == CONTROL_WRITES {
                s.control_writes.pop_front();
            }
            s.control_writes.push_back((cx.now(), o, value));
        }
        if o == 0 && value & 1 == 0 {
            s.generation = s.generation.wrapping_add(1);
            s.armed = false;
            s.deadline = None;
            s.regs.insert(0, value & !1);
            s.regs.insert(self.wiring.status, 0);
            s.reset_deadline = None;
            if self.wiring.reset_ticks != 0 {
                let Some(at) = cx.now().checked_add(self.wiring.reset_ticks) else {
                    cx.fail("input-buffer reset overflow");
                    return;
                };
                s.reset_deadline = Some(at);
                cx.schedule(at, s.generation);
            }
        } else if o == 0x224 && value == 1 && s.readback.is_some() {
            s.regs.insert(o, value);
            let ticks = s.readback.unwrap().1;
            let Some(at) = cx.now().checked_add(ticks) else {
                cx.fail("input-buffer read-back overflow");
                return;
            };
            s.readback_deadline = Some(at);
            cx.schedule(at, s.generation);
        } else if o == self.wiring.status {
            let pending = s.r(o) & !value;
            s.regs.insert(o, pending);
        } else if matches!(o, 4 | 8) {
            let word = (o / 4 - 1) as usize;
            let strobe = value >> 16;
            s.requested[word] = (s.requested[word] & !strobe) | (value & strobe & 0xffff);
            if !self.wiring.ignore_undeclared
                && value & (value >> 16) & !self.wiring.enables[word] != 0
            {
                cx.unmodeled();
            }
            let mask = (value >> 16) & self.wiring.enables[word];
            let enabled = (s.r(o) & !mask) | (value & mask);
            s.regs.insert(o, enabled);
            let active =
                s.r(4) & self.wiring.enables[0] != 0 || s.r(8) & self.wiring.enables[1] != 0;
            if active && !s.armed {
                s.armed = true;
                s.generation = s.generation.wrapping_add(1);
                let now = cx.now();
                self.schedule(&mut s, cx, now);
            } else if !active && s.armed {
                s.armed = false;
                s.deadline = None;
                s.generation = s.generation.wrapping_add(1);
            }
        } else {
            s.regs.insert(o, value);
        }
        self.irq(&s, cx);
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        {
            let mut s = self.state.lock().unwrap();
            if s.reset_deadline.is_some_and(|at| at <= cx.now()) {
                s.reset_deadline = None;
            }
            if s.readback_deadline.is_some_and(|at| at <= cx.now()) {
                s.readback_deadline = None;
                s.readbacks += 1;
                let event = s.readback.map_or(0, |r| r.0);
                let pending = s.r(self.wiring.status) | event;
                s.regs.insert(self.wiring.status, pending);
                self.irq(&s, cx);
            }
        }
        loop {
            let at = {
                let mut s = self.state.lock().unwrap();
                let Some(at) = s.deadline.filter(|at| *at <= cx.now()) else {
                    break;
                };
                s.deadline = None;
                at
            };
            if let Err(error) = self.frame(cx) {
                cx.fail(error);
                return;
            }
            let mut s = self.state.lock().unwrap();
            if s.armed {
                self.schedule(&mut s, cx, at);
            }
        }
    }
    fn event(&mut self, cx: &mut Cx<'_>, _: u64) {
        // Reset and periodic frame clocks may overlap. Each has an exact saved
        // deadline; stale or early service cannot manufacture either effect.
        self.advance(cx);
    }
    fn next_deadline(&self, _: u64) -> Option<u64> {
        let s = self.state.lock().unwrap();
        s.deadline
            .into_iter()
            .chain(s.reset_deadline)
            .chain(s.readback_deadline)
            .min()
    }
    fn save(&self) -> Value {
        serde_json::json!({"state":*self.state.lock().unwrap()})
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value["state"].clone())
            .map_err(|e| format!("input-buffer checkpoint: {e}"))?;
        if state.deadline.is_some() && (!state.armed || self.wiring.period_ticks.is_none()) {
            return Err("input-buffer clock/deadline disagree".into());
        }
        if state.readback_deadline.is_some() && state.readback.is_none() {
            return Err("input-buffer read-back without a law".into());
        }
        *self.state.lock().unwrap() = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::recording::RecordingHost;
    #[test]
    fn undeclared_enable_bits_are_reported_but_arm_nothing() {
        let mut input = InputBuffer::new(
            Wiring {
                base: 0x200000,
                irq: 106,
                status: 12,
                irq_control: 0x2e4,
                ready: 0x2e8,
                max_offset: 0x2e8,
                write_limit: 0x2e4,
                ignore_undeclared: true,
                enables: [0x100, 0],
                causes: [0x100, 0],
                outputs: vec![],
                reset_ticks: 8,
                period_ticks: None,
            },
            None,
        )
        .unwrap();
        let mut host = RecordingHost::default();
        // Strobe in the high half: set bits 0..5, then clear bit 2.
        input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200004, 4, 0x003f_003f);
        input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200004, 4, 0x0004_0000);
        let s = input.state.lock().unwrap();
        assert_eq!(s.requested, [0x3b, 0]);
        assert_eq!(s.control_writes.len(), 2);
        assert_eq!(s.control_writes[1].1, 4);
        assert_eq!(s.r(4), 0);
        assert!(!s.armed);
    }

    #[test]
    fn a_memory_read_back_ends_once_with_its_declared_event() {
        let mut input = InputBuffer::new(
            Wiring {
                base: 0x200000,
                irq: 106,
                status: 12,
                irq_control: 0x2e4,
                ready: 0x2e8,
                max_offset: 0x2e8,
                write_limit: 0x2e4,
                ignore_undeclared: true,
                enables: [0x100, 0],
                causes: [0x100, 0],
                outputs: vec![],
                reset_ticks: 8,
                period_ticks: None,
            },
            None,
        )
        .unwrap();
        let mut host = RecordingHost::default();
        // Without a law, +0x224 is a plain word.
        input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200224, 4, 1);
        assert_eq!(input.next_deadline(0), None);
        input.set_readback(0x2, 100).unwrap();
        assert!(input.set_readback(0, 100).is_err());
        input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x2002e4, 4, 0x120);
        host.now = 10;
        input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200224, 4, 1);
        assert_eq!(input.next_deadline(10), Some(110));
        let saved = input.save();
        host.now = 110;
        input.event(&mut Cx::new(&mut host, 0, 0, 0), 0);
        assert_eq!(input.read(&mut Cx::new(&mut host, 0, 0, 0), 0x20000c, 4), 2);
        assert_eq!(host.irqs.last(), Some(&(106, true)));
        // Acknowledged once, it does not come back.
        input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x20000c, 4, 2);
        assert_eq!(host.irqs.last(), Some(&(106, false)));
        assert_eq!(input.next_deadline(110), None);
        assert_eq!(input.state.lock().unwrap().readbacks, 1);
        let mut resumed = input.clone();
        resumed.restore(&saved).unwrap();
        assert_eq!(resumed.next_deadline(10), Some(110));
    }

    #[test]
    fn upstream_edges_dma_and_w1c_are_independent_of_program_and_early_service() {
        fn experiment(cpu: usize, pc: u64) -> Value {
            let mut statistics = Statistics::new(0x100000, 0x7000).unwrap();
            statistics.set_scene(&serde_json::json!({"luma":10,"black_level":0,"signal_shift":0,"samples_per_cell":4})).unwrap();
            let mut input = InputBuffer::new(
                Wiring {
                    base: 0x200000,
                    irq: 106,
                    status: 12,
                    irq_control: 0x2e4,
                    ready: 0x2e8,
                    max_offset: 0x2e8,
                    write_limit: 0x2e4,
                    ignore_undeclared: false,
                    enables: [0x100, 0],
                    causes: [0x100, 0],
                    outputs: vec![Output {
                        enable_word: 0,
                        bit: 0x100,
                        low: 0x254,
                        high: 0x258,
                        high_mask: 3,
                        columns: 16,
                        rows: 16,
                        planes: Planes::Rgb,
                    }],
                    reset_ticks: 8,
                    period_ticks: None,
                },
                Some(statistics.clone()),
            )
            .unwrap();
            let mut host = RecordingHost::default();
            host.now = 10;
            input.write(&mut Cx::new(&mut host, cpu, pc, 0), 0x200000, 4, 0);
            assert_eq!(input.next_deadline(10), Some(18));
            let saved = input.save();
            host.now = 17;
            input.event(&mut Cx::new(&mut host, cpu, pc, 0), 1);
            assert_eq!(input.save(), saved);
            host.now = 18;
            input.event(&mut Cx::new(&mut host, cpu, pc, 0), 1);
            assert_eq!(input.next_deadline(18), None);
            assert_eq!(
                input.read(&mut Cx::new(&mut host, cpu, pc, 0), 0x200000, 4),
                1
            );
            for (offset, value) in [(4, 0x1000100), (0x2e4, 0x120)] {
                input.write(
                    &mut Cx::new(&mut host, cpu, pc, 0),
                    0x200000 + offset,
                    4,
                    value,
                );
            }
            assert_eq!(input.next_deadline(18), None); // No invented periodic poll.
            input.frame(&mut Cx::new(&mut host, cpu, pc, 0)).unwrap();
            assert_eq!(
                input.read(&mut Cx::new(&mut host, cpu, pc, 0), 0x20000c, 4),
                0
            );
            input.write(&mut Cx::new(&mut host, cpu, pc, 0), 0x200254, 4, 0x101060);
            host.now = 100;
            input.frame(&mut Cx::new(&mut host, cpu, pc, 0)).unwrap();
            assert_eq!(
                input.read(&mut Cx::new(&mut host, cpu, pc, 0), 0x20000c, 4),
                0x100
            );
            for address in [0x101060, 0x101460, 0x101860] {
                assert_eq!(
                    statistics.read(&mut Cx::new(&mut host, cpu, pc, 0), address, 4),
                    40
                );
            }
            assert_eq!(host.irqs.last(), Some(&(106, true)));
            input.write(&mut Cx::new(&mut host, cpu, pc, 0), 0x20000c, 4, 0x100);
            assert_eq!(host.irqs.last(), Some(&(106, false)));
            {
                let mut cx = Cx::new(&mut host, cpu, pc, 0);
                input.write(&mut cx, 0x200004, 4, 0x3ff03ff);
                assert!(cx.is_unmodeled()); // Remaining active channels have no completion.
            }
            let saved = input.save();
            let pixels = statistics.save();
            assert_eq!(input.save(), saved);
            assert_eq!(statistics.save(), pixels);
            assert!(host.ram.is_empty());
            serde_json::json!([saved, pixels])
        }
        assert_eq!(experiment(0, 0x1000), experiment(3, 0xabcdef));
    }

    #[test]
    fn statistics_outputs_fill_their_destinations_and_leave_the_status_alone() {
        use crate::statistics::StatisticsOutputs;
        fn bench(extra: bool) -> (InputBuffer, Statistics, RecordingHost) {
            let mut statistics = Statistics::new(0x100000, 0x7000).unwrap();
            statistics.set_scene(&serde_json::json!({"luma": 2048})).unwrap();
            let mut outputs = vec![Output {
                enable_word: 0,
                bit: 0x100,
                low: 0x254,
                high: 0x258,
                high_mask: 3,
                columns: 16,
                rows: 16,
                planes: Planes::Rgb,
            }];
            if extra {
                outputs.extend(StatisticsOutputs::default().outputs().into_iter().map(
                    |(low, columns, rows, planes)| Output {
                        enable_word: 0,
                        bit: 0x100,
                        low,
                        high: low + 4,
                        high_mask: 3,
                        columns,
                        rows,
                        planes,
                    },
                ));
            }
            let mut input = InputBuffer::new(
                Wiring {
                    base: 0x200000,
                    irq: 106,
                    status: 12,
                    irq_control: 0x2e4,
                    ready: 0x2e8,
                    max_offset: 0x2e8,
                    write_limit: 0x2e4,
                    ignore_undeclared: true,
                    enables: [0x100, 0],
                    causes: [0x100, 0],
                    outputs,
                    reset_ticks: 8,
                    period_ticks: None,
                },
                Some(statistics.clone()),
            )
            .unwrap();
            let mut host = RecordingHost::default();
            host.now = 10;
            input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200000, 4, 0);
            host.now = 18;
            input.event(&mut Cx::new(&mut host, 0, 0, 0), 1);
            for (offset, value) in [(4, 0x1000100), (0x2e4, 0x120)] {
                input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200000 + offset, 4, value);
            }
            (input, statistics, host)
        }
        let (mut stock, stock_stats, mut stock_host) = bench(false);
        let (mut input, mut statistics, mut host) = bench(true);
        let status = |input: &mut InputBuffer, host: &mut RecordingHost| {
            input.read(&mut Cx::new(host, 0, 0, 0), 0x20000c, 4)
        };
        // Nothing programmed: the extra outputs withdraw no cause bit.
        stock.frame(&mut Cx::new(&mut stock_host, 0, 0, 0)).unwrap();
        input.frame(&mut Cx::new(&mut host, 0, 0, 0)).unwrap();
        assert_eq!(status(&mut input, &mut host), status(&mut stock, &mut stock_host));
        // 0x254 alone: same status, same SRAM, same checkpoint as the stock wiring.
        for (bench_input, bench_host) in [(&mut stock, &mut stock_host), (&mut input, &mut host)] {
            bench_input.write(&mut Cx::new(bench_host, 0, 0, 0), 0x200254, 4, 0x101060);
            bench_input.frame(&mut Cx::new(bench_host, 0, 0, 0)).unwrap();
        }
        assert_eq!(status(&mut input, &mut host), 0x100);
        assert_eq!(status(&mut stock, &mut stock_host), 0x100);
        assert_eq!(statistics.save(), stock_stats.save());
        assert_eq!(input.save(), stock.save());
        // Every destination and black window programmed (Runs photo § 18 capture windows).
        for (offset, value) in [
            (0x244, 0x1021a0),
            (0x25c, 0x102060),
            (0x2b4, 0x106000),
            (0x274, 0x1029a0),
            (0x27c, 0x1029c0),
            (0x284, 0x1029e0),
            (0x1ac, 0x000c_0015),
            (0x1b0, 0x0114_03ff),
            (0x1b4, 0x0004_0007),
            (0x1b8, 0x03ff),
            (0x1bc, 0x0004_0007),
            (0x1c0, 0x03ff),
        ] {
            input.write(&mut Cx::new(&mut host, 0, 0, 0), 0x200000 + offset, 4, value);
        }
        input.frame(&mut Cx::new(&mut host, 0, 0, 0)).unwrap();
        assert_eq!(status(&mut input, &mut host), 0x100);
        let mut read = |address: u64, size: u8| statistics.read(&mut Cx::new(&mut host, 0, 0, 0), address, size);
        // Grid: (2048 << 2) >> 2 in the high half of every 7x7 zone, nothing beyond.
        assert_eq!(read(0x1021a0, 4), 2048 << 16);
        assert_eq!(read(0x1021a0 + 6 * 0x40 + 6 * 4, 4), 2048 << 16);
        assert_eq!(read(0x1021a0 + 7 * 4, 4), 0);
        // Records: band 0 sums over the firmware's 153 x 153 sites, black
        // included; the other bands hold one site, hence no sample.
        assert_eq!(read(0x102060, 8), 9216 * (23409 * 2 / 9));
        assert_eq!(read(0x102068, 8), 9216 * (23409 * 5 / 9));
        assert_eq!(read(0x102078, 8), 0);
        assert_eq!(read(0x102080, 8), 0);
        // Histogram: 64 x 64 points in class 8192 >> 6.
        assert_eq!(read(0x106000 + 128 * 4, 4), 4096);
        assert_eq!(read(0x106000 + 127 * 4, 4), 0);
        // Black: 1024 x (22 x 1024 x 4) and 1024 x (8 x 1024 x 4) on all four channels.
        assert_eq!(read(0x1029a0, 8), 0x580_0000);
        assert_eq!(read(0x1029b8, 8), 0x580_0000);
        assert_eq!(read(0x1029c0, 8), 0x200_0000);
        assert_eq!(read(0x1029f8, 8), 0x200_0000);
        assert!(host.ram.is_empty());
    }
}
