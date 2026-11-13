//! SLVS-EC causes, frame phases and explicit sensor/input-engine wiring.
use super::sensor::Sensor;
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default, Serialize, Deserialize)]
struct State {
    regs: BTreeMap<u64, u32>,
    generation: u64,
    armed: bool,
    frames: u64,
    #[serde(default)]
    active: bool,
    frame_deadline: Option<u64>,
    end_deadline: Option<u64>,
    reset_deadline: Option<u64>,
    /// When a deferred still read-out ends (see [`Receiver::with_still_readout`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    still_deadline: Option<u64>,
    /// Whole-frame still read-out: channel 0 was already requested when the
    /// current frame started.
    #[serde(default, skip_serializing_if = "is_false")]
    still_started: bool,
    /// The current frame timed by the sensor's timing generator words (see
    /// [`super::sensor::TgCadence`]); none with the declared period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tg_frame: Option<TgFrame>,
    /// Frame periods still withheld after a timing generator start (see
    /// [`super::sensor::TgRestartDelay`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    withheld: u32,
    /// The current frame is timed without data: the sensor is in standby
    /// (see [`super::sensor::Sensor::use_standby_timing`]).
    #[serde(default, skip_serializing_if = "is_false")]
    timing_only: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct TgFrame {
    period: u64,
    active: u64,
    /// Fraction of a tick carried to the next frame, in clock_hz-ths.
    rest: u64,
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &u32) -> bool {
    *n == 0
}
/// How long a still read-out (channel 0) lasts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StillReadout {
    /// The declared ticks from the start of the frame that serves it.
    Ticks(u64),
    /// Whole frames: a read-out requested after a frame started is served by
    /// the next frame, ending at the next start plus the active period.
    WholeFrames,
}
/// Per-link control field positions and levels, selected by the processor map.
#[derive(Clone, Copy)]
pub struct Control {
    pub run_mask: u32,
    pub run_value: u32,
    pub reset_pulse: u32,
    pub reset_ticks: u64,
}
pub struct Wiring {
    pub base: u64,
    pub normal_irqs: Vec<u32>,
    pub error_irqs: Vec<u32>,
    pub output_channels: Vec<usize>,
    pub timing_output: bool,
    pub control: Control,
}
pub struct Receiver {
    state: State,
    sensor: Sensor,
    wiring: Wiring,
    period_ticks: u64,
    active_ticks: u64,
    input_buffer: Option<super::input_buffer::InputBuffer>,
    still_readout: Option<StillReadout>,
}
impl Receiver {
    pub fn new(
        sensor: Sensor,
        wiring: Wiring,
        period_ticks: u64,
        active_ticks: u64,
    ) -> Result<Self, String> {
        if wiring.base & 0xfff != 0
            || !(1..=2).contains(&wiring.normal_irqs.len())
            || wiring.error_irqs.len() != wiring.normal_irqs.len()
            || wiring.output_channels.iter().any(|c| *c > 1)
            || period_ticks < 2
            || active_ticks == 0
            || active_ticks >= period_ticks
            || wiring.control.run_value & !wiring.control.run_mask != 0
            || wiring.control.reset_pulse != 0 && wiring.control.reset_ticks == 0
        {
            return Err("invalid SLVS receiver wiring/timing".into());
        }
        Ok(Self {
            state: State::default(),
            sensor,
            wiring,
            period_ticks,
            active_ticks,
            input_buffer: None,
            still_readout: None,
        })
    }
    /// A still read-out (channel 0 requested) lasts `ticks` from the start of
    /// the frame that serves it instead of ending with that frame. Until then
    /// the frame ends keep their timing output, but neither channel 0 nor the
    /// input buffer's frame events complete; both complete at the deadline.
    /// X-T3 5.11: the still's GIC 97 / GIC 106 ends must follow the next VI
    /// fall (GIC 102), as a full-sensor read-out takes several frames.
    /// With [`StillReadout::WholeFrames`] it lasts until the end of the first
    /// frame that started after the request (X-T3 5.11, suite 36: a transfer
    /// cannot join a frame already under way).
    pub fn with_still_readout(mut self, readout: StillReadout) -> Result<Self, String> {
        if readout == StillReadout::Ticks(0) || self.wiring.output_channels.first() != Some(&0) {
            return Err("A still read-out needs a positive duration and channel 0".into());
        }
        self.still_readout = Some(readout);
        Ok(self)
    }
    /// Channel 0 and the input buffer's frame events, at a still's end.
    fn finish_still(&mut self, cx: &mut Cx<'_>) -> Result<(), String> {
        if let Some(input) = &self.input_buffer {
            input.frame(cx)?;
        }
        self.sensor.frame(cx, 0)
    }
    pub fn with_input_buffer(mut self, input: super::input_buffer::InputBuffer) -> Self {
        self.input_buffer = Some(input);
        self
    }
    fn reg(&self, o: u64) -> u32 {
        self.state.regs.get(&o).copied().unwrap_or_else(|| {
            // 330b720 calls 319992c4 to set bit4, preserving reset bit0.
            // 1457308 rejects bit4=1/bit0=0; a zero reset word silently
            // prevents the stock receiver initializer from starting a link.
            if o == 0 {
                if self.links() == 2 { 0x10001 } else { 1 }
            } else {
                0
            }
        })
    }
    fn enabled(&self, link: usize) -> bool {
        let shift = link * 16;
        self.reg(0) >> shift & self.wiring.control.run_mask == self.wiring.control.run_value
            && self.state.reset_deadline.is_none()
    }
    fn links(&self) -> usize {
        self.wiring.normal_irqs.len()
    }
    fn irq(&self, cx: &mut Cx<'_>) {
        for link in 0..self.links() {
            let base = 0x100 + link as u64 * 0x100;
            let intid = self.wiring.normal_irqs[link];
            cx.set_irq(intid, self.reg(base + 8) & self.reg(base + 12) & 0xf3 != 0);
            // Functional healthy link: no synthetic receiver error causes.
            cx.set_irq(self.wiring.error_irqs[link], false);
        }
    }
    /// A link runs, the sensor streams and, with the cadence law on, the
    /// timing generator runs: a stopped generator sends neither frames nor
    /// frame timing until it restarts.
    /// With standby timing on, a sensor in standby keeps the frame timing,
    /// links stopped or not (X-T3 5.11 stops link 0, +0 <- 0x10001, right
    /// after it sleeps the sensor, and counts start pulses on GIC 101).
    fn active_source(&self) -> bool {
        ((0..self.links()).any(|i| self.enabled(i)) && self.sensor.streaming()
            || self.sensor.timed_in_standby())
            && !self.sensor.tg_stopped()
    }
    fn arm(&mut self, cx: &mut Cx<'_>) {
        let now = cx.now();
        self.arm_at(cx, now);
    }
    fn arm_at(&mut self, cx: &mut Cx<'_>, anchor: u64) {
        // A generator stopped since the last look ends the frame under way,
        // even if it already runs again: it restarts with a new phase.
        if self.sensor.take_tg_stop() && self.state.armed {
            self.disarm();
        }
        // A generator start with a restart delay set: the phase armed now
        // withholds its first periods. A start the receiver does not arm on
        // (no link, sensor in standby) is dropped.
        let start = self.sensor.take_tg_start();
        let enabled = self.active_source();
        if enabled && !self.state.armed {
            self.state.armed = true;
            if start {
                self.state.withheld = self.sensor.tg_restart_delay();
            }
            self.state.generation = self.state.generation.wrapping_add(1);
            // A fresh phase: no remainder carried from earlier frames.
            self.state.tg_frame = None;
            let (period, _) = self.next_frame();
            let at = anchor.saturating_add(period);
            self.state.frame_deadline = Some(at);
            cx.schedule(at, self.state.generation);
        } else if !enabled && self.state.armed {
            self.disarm();
        }
    }
    fn disarm(&mut self) {
        self.state.withheld = 0;
        self.state.armed = false;
        self.state.active = false;
        self.state.timing_only = false;
        self.state.generation = self.state.generation.wrapping_add(1);
        self.state.frame_deadline = None;
        self.state.end_deadline = None;
    }
    /// Period and active ticks of the frame starting now: those of the
    /// timing generator words when the sensor times frames from them and they
    /// give a valid frame, else the declared ones.
    fn next_frame(&mut self) -> (u64, u64) {
        let rest = self.state.tg_frame.map_or(0, |f| f.rest);
        self.state.tg_frame = self.sensor.tg_cadence().and_then(|(cadence, clocks)| {
            let (period, rest) = cadence.ticks(clocks?, rest)?;
            let active = u64::try_from(
                u128::from(period) * u128::from(self.active_ticks) / u128::from(self.period_ticks),
            )
            .ok()?;
            (period >= 2 && active > 0 && active < period).then_some(TgFrame {
                period,
                active,
                rest,
            })
        });
        (self.frame_period(), self.frame_active())
    }
    fn frame_period(&self) -> u64 {
        self.state.tg_frame.map_or(self.period_ticks, |f| f.period)
    }
    fn frame_active(&self) -> u64 {
        self.state.tg_frame.map_or(self.active_ticks, |f| f.active)
    }
    fn known(&self, o: u64) -> bool {
        if o <= 0x40 {
            return true;
        }
        (0..self.links()).any(|link| {
            o.checked_sub(link as u64 * 0x100)
                .is_some_and(|v| (0x100..=0x150).contains(&v))
        })
    }
}
impl Device for Receiver {
    fn name(&self) -> &'static str {
        "imaging-receiver"
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let o = address.wrapping_sub(self.wiring.base);
        self.advance(cx);
        if size != 4 || o & 3 != 0 || !self.known(o) {
            cx.unmodeled();
            return 0;
        }
        if o % 0x100 == 0x44 && o >= 0x100 {
            return self.state.frames & 0x1fff;
        }
        if o % 0x100 == 0x48 && o >= 0x100 {
            let n = self.state.frames & 0x1fff;
            return n | n << 16;
        }
        u64::from(self.reg(o))
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        let o = address.wrapping_sub(self.wiring.base);
        self.advance(cx);
        if size != 4 || o & 3 != 0 || !self.known(o) {
            cx.unmodeled();
            return;
        }
        let value = value as u32;
        if o == 0 {
            let mask = (0..self.links()).fold(0, |mask, link| {
                mask | self.wiring.control.reset_pulse << (link * 16)
            });
            if value & mask != 0 {
                let Some(at) = cx.now().checked_add(self.wiring.control.reset_ticks) else {
                    cx.fail("SLVS reset deadline overflow");
                    return;
                };
                self.state.reset_deadline = Some(at);
            }
        }
        if o >= 0x100 && o % 0x100 == 8 {
            self.state.regs.insert(o, self.reg(o) & !(value & 0xf3));
        } else {
            self.state.regs.insert(o, value);
        }
        self.irq(cx);
        self.arm(cx);
        if o == 0
            && let Some(at) = self.state.reset_deadline
        {
            cx.schedule(at, self.state.generation);
        }
    }
    fn event(&mut self, cx: &mut Cx<'_>, token: u64) {
        if token & !(1 << 63) == self.state.generation
            || self.state.still_deadline.is_some_and(|at| at <= cx.now())
        {
            self.advance(cx);
        }
    }
    fn next_deadline(&self, now: u64) -> Option<u64> {
        if self.active_source() != self.state.armed
            || self.state.armed && self.sensor.tg_stop_pending()
            || self.sensor.tg_start_pending()
        {
            return Some(now);
        }
        self.state
            .frame_deadline
            .into_iter()
            .chain(self.state.end_deadline)
            .chain(self.state.reset_deadline)
            .chain(self.state.still_deadline)
            .min()
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        let anchor = self
            .state
            .reset_deadline
            .filter(|at| *at <= cx.now())
            .unwrap_or(cx.now());
        if self.state.reset_deadline.is_some_and(|at| at <= cx.now()) {
            self.state.reset_deadline = None;
            let mask = (0..self.links()).fold(0, |mask, link| {
                mask | self.wiring.control.reset_pulse << (link * 16)
            });
            self.state.regs.insert(0, self.reg(0) & !mask);
        }
        self.arm_at(cx, anchor);
        loop {
            if self.state.still_deadline.is_some_and(|at| {
                at <= cx.now()
                    && self
                        .state
                        .frame_deadline
                        .into_iter()
                        .chain(self.state.end_deadline)
                        .all(|other| at < other)
            }) {
                self.state.still_deadline = None;
                if let Err(e) = self.finish_still(cx) {
                    cx.fail(e);
                    return;
                }
                continue;
            }
            let next = self
                .state
                .frame_deadline
                .into_iter()
                .map(|at| (at, false))
                .chain(self.state.end_deadline.map(|at| (at, true)))
                .min();
            let Some((at, end)) = next.filter(|(at, _)| *at <= cx.now()) else {
                break;
            };
            if end {
                self.state.end_deadline = None;
            }
            // Each frame start times the frame it opens.
            let (period, active) = if end { (0, 0) } else { self.next_frame() };
            if end && self.state.active {
                self.state.active = false;
                // A still read-out longer than this frame defers channel 0 and
                // the input buffer's events to its own deadline.
                let still = self.state.still_deadline.is_some()
                    || self.still_readout.is_some_and(|readout| {
                        let due = match readout {
                            StillReadout::Ticks(ticks) => {
                                (at - self.frame_active()).saturating_add(ticks)
                            }
                            StillReadout::WholeFrames if self.state.still_started => at,
                            StillReadout::WholeFrames => at.saturating_add(self.frame_period()),
                        };
                        let defer = self.sensor.requested(0) && due > at;
                        if defer {
                            self.state.still_deadline = Some(due);
                            cx.schedule(due, self.state.generation | (1 << 63));
                        }
                        defer
                    });
                if let (false, Some(input)) = (still, &self.input_buffer) {
                    if let Err(error) = input.frame(cx) {
                        cx.fail(error);
                        return;
                    }
                }
                if self.wiring.timing_output {
                    self.sensor.timing(cx, false);
                }
                for &channel in self
                    .wiring
                    .output_channels
                    .iter()
                    .filter(|&&c| !(still && c == 0))
                {
                    if let Err(e) = self.sensor.frame(cx, channel) {
                        cx.fail(e);
                        return;
                    }
                }
                self.state.frames += 1;
                for link in 0..self.links() {
                    if self.enabled(link) {
                        let cause = 0x108 + link as u64 * 0x100;
                        // Bits1/0 map to the two frame synchronization event flags
                        // in3307b7c (param+10/+14): shutdown/wake waits can advance.
                        self.state.regs.insert(cause, self.reg(cause) | 2);
                    }
                }
                self.irq(cx);
            } else if end && self.state.timing_only {
                // End of a frame timed in standby: nothing is signalled.
                self.state.timing_only = false;
            } else if !end && self.state.withheld > 0 {
                // A withheld period after a generator start: the cadence goes
                // on, but no frame starts (no timing, cause, IRQ, read-out).
                self.state.withheld -= 1;
            } else if !end && self.sensor.streaming() {
                self.state.active = true;
                self.state.still_started = self.still_readout == Some(StillReadout::WholeFrames)
                    && self.sensor.requested(0);
                if self.wiring.timing_output {
                    self.sensor.timing(cx, true);
                }
                for link in 0..self.links() {
                    if self.enabled(link) {
                        let cause = 0x108 + link as u64 * 0x100;
                        self.state.regs.insert(cause, self.reg(cause) | 1);
                    }
                }
                self.irq(cx);
                let deadline = at.saturating_add(active);
                self.state.end_deadline = Some(deadline);
                cx.schedule(deadline, self.state.generation | (1 << 63));
            } else if !end && self.sensor.timed_in_standby() {
                // Sensor in standby: only the start pulse goes on (no timing
                // line, cause, IRQ, read-out or frame count).
                self.state.timing_only = true;
                if self.wiring.timing_output {
                    self.sensor.standby_pulse(cx);
                }
                let deadline = at.saturating_add(active);
                self.state.end_deadline = Some(deadline);
                cx.schedule(deadline, self.state.generation | (1 << 63));
            }
            if !end {
                let deadline = at.saturating_add(period);
                self.state.frame_deadline = Some(deadline);
                cx.schedule(deadline, self.state.generation);
            }
        }
    }
    fn save(&self) -> Value {
        serde_json::json!({"state":self.state,"period_ticks":self.period_ticks,"active_ticks":self.active_ticks})
    }
    fn restore(&mut self, v: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(v["state"].clone())
            .map_err(|e| format!("receiver checkpoint: {e}"))?;
        let period = v["period_ticks"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("invalid receiver period")?;
        let active = v["active_ticks"]
            .as_u64()
            .filter(|n| *n > 0 && *n < period)
            .ok_or("invalid receiver active period")?;
        self.state = state;
        self.period_ticks = period;
        self.active_ticks = active;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::recording::RecordingHost;
    const BASE: u64 = 0xff940000;
    const PERIOD: u64 = 16_667_000;
    const ACTIVE: u64 = PERIOD / 2;
    #[test]
    fn reset_pulse_stop_level_exact_phases_and_ack_do_not_depend_on_program() {
        for (cpu, pc) in [(0, 0), (3, 0x12345678)] {
            let sensor = Sensor::new(None, (0x51, 8));
            {
                let mut s = sensor.0.lock().unwrap();
                s.writes = 1;
                s.registers.insert(0, 4);
            }
            let mut rx = Receiver::new(
                sensor,
                Wiring {
                    base: 0xfff69000,
                    normal_irqs: vec![296, 298],
                    error_irqs: vec![297, 297],
                    output_channels: vec![],
                    timing_output: false,
                    control: Control {
                        run_mask: 0x11,
                        run_value: 0,
                        reset_pulse: 0x10,
                        reset_ticks: 8,
                    },
                },
                100,
                50,
            )
            .unwrap();
            let mut host = RecordingHost::default();
            assert_eq!(
                rx.read(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69000, 4),
                0x10001
            );
            rx.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69000, 4, 0x10011);
            assert_eq!(rx.next_deadline(0), Some(8));
            let saved = rx.save();
            host.now = 7;
            rx.event(&mut Cx::new(&mut host, cpu, pc, 0), 0);
            assert_eq!(rx.save(), saved);
            host.now = 8;
            rx.advance(&mut Cx::new(&mut host, cpu, pc, 0));
            assert_eq!(
                rx.read(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69000, 4),
                0x10001
            );
            assert_eq!(rx.next_deadline(8), None);
            rx.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff6910c, 4, 3);
            rx.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69000, 4, 0x10000);
            assert_eq!(rx.next_deadline(8), Some(108));
            let saved = rx.save();
            host.now = 107;
            rx.event(&mut Cx::new(&mut host, cpu, pc, 0), 1);
            assert_eq!(rx.save(), saved);
            host.now = 108;
            rx.advance(&mut Cx::new(&mut host, cpu, pc, 0));
            assert!(irq(&host, 296));
            assert_eq!(rx.next_deadline(108), Some(158));
            rx.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69108, 4, 1);
            assert!(!irq(&host, 296));
            host.now = 158;
            rx.advance(&mut Cx::new(&mut host, cpu, pc, 0));
            assert_eq!(
                rx.read(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69108, 4),
                2
            );
            rx.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69108, 4, 2);
            rx.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff69000, 4, 0x10001);
            assert!(!irq(&host, 296));
            assert_eq!(rx.next_deadline(host.now), None);
            assert!(host.ram.is_empty());
        }
    }
    fn rig(streaming: bool) -> (Sensor, Receiver, RecordingHost) {
        rig_pulsed(streaming, None)
    }
    fn rig_pulsed(
        streaming: bool,
        start_pulse: Option<(u32, u32)>,
    ) -> (Sensor, Receiver, RecordingHost) {
        let sensor = Sensor::new(
            Some(super::super::sensor::ImageRoutes {
                dma: Some([162, 163]),
                timing: [166, 164],
                latch_unrouted: false,
                start_pulse,
            }),
            (0x59, 4),
        );
        {
            let mut s = sensor.0.lock().unwrap();
            s.image = BTreeMap::from([
                (0x40, 1),
                (0x44, 4),
                (0x48, 2),
                (0x4c, 1),
                (0x50, 0x100),
                (0x60, 1),
            ]);
            if streaming {
                s.registers.insert(0, 0x14);
                s.writes = 1;
            }
        }
        let receiver = Receiver::new(
            sensor.clone(),
            Wiring {
                base: BASE,
                normal_irqs: vec![154, 156],
                error_irqs: vec![155, 157],
                output_channels: vec![0, 1],
                timing_output: true,
                control: Control {
                    run_mask: 0x11,
                    run_value: 0x11,
                    reset_pulse: 0,
                    reset_ticks: 0,
                },
            },
            PERIOD,
            ACTIVE,
        )
        .unwrap();
        let host = RecordingHost {
            ram: vec![0x55; 0x1000],
            ram_base: 0x1_0000_0000,
            ..Default::default()
        };
        (sensor, receiver, host)
    }
    fn irq(host: &RecordingHost, line: u32) -> bool {
        host.irqs
            .iter()
            .rev()
            .find(|(n, _)| *n == line)
            .is_some_and(|(_, v)| *v)
    }
    fn enable(receiver: &mut Receiver, host: &mut RecordingHost) {
        let mut cx = Cx::new(host, 2, 0xfeed, 0);
        receiver.write(&mut cx, BASE + 0x10c, 4, 3);
        receiver.write(&mut cx, BASE, 4, 0x11);
        assert!(cx.refusal().is_none());
    }
    #[test]
    fn a_read_out_may_write_its_lines_at_the_programmed_pitch() {
        let ramp = [[0u8, 0, 31, 0, 62, 0, 93, 0], [17, 0, 48, 0, 79, 0, 110, 0]];
        for pitched in [false, true] {
            let (sensor, _, mut host) = rig(true);
            // Channel 0 pitch word: 12 bytes for 4 samples.
            sensor.0.lock().unwrap().image.insert(0x58, 12);
            if pitched {
                sensor.use_line_pitch();
            }
            sensor.frame(&mut Cx::new(&mut host, 0, 0, 0), 0).unwrap();
            assert_eq!(&host.ram[0x100..0x108], &ramp[0]);
            if pitched {
                assert_eq!(&host.ram[0x108..0x10c], &[0x55; 4]);
                assert_eq!(&host.ram[0x10c..0x114], &ramp[1]);
            } else {
                assert_eq!(&host.ram[0x108..0x110], &ramp[1]);
            }
        }
    }
    #[test]
    fn standby_dma_high_address_irq_ack_and_cancel() {
        let (sensor, mut receiver, mut host) = rig(false);
        enable(&mut receiver, &mut host);
        assert_eq!(receiver.next_deadline(0), None);
        assert!(host.events.is_empty()); // Guest-controlled standby is not polled.
        {
            let mut s = sensor.0.lock().unwrap();
            s.registers.insert(0, 0x14);
            s.writes = 1;
        }
        assert_eq!(receiver.next_deadline(0), Some(0));
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert_eq!(receiver.next_deadline(0), Some(PERIOD));
        let initial = receiver.save();
        let token = initial["state"]["generation"].as_u64().unwrap();
        host.now = PERIOD - 1;
        receiver.event(&mut Cx::new(&mut host, 0, 0, 0), token);
        assert_eq!(receiver.save(), initial);
        assert_eq!(&host.ram[0x100..0x110], &[0x55; 16]);
        host.now = PERIOD;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(irq(&host, 154));
        host.now = PERIOD + ACTIVE;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert_eq!(
            &host.ram[0x100..0x110],
            &[0, 0, 31, 0, 62, 0, 93, 0, 17, 0, 48, 0, 79, 0, 110, 0]
        );
        assert_eq!(sensor.0.lock().unwrap().last_dma[0], (0x1_0000_0100, 4, 2));
        assert!(irq(&host, 162));
        receiver.write(&mut Cx::new(&mut host, 0, 0, 0), BASE + 0x108, 4, 3);
        sensor.image_write(&mut Cx::new(&mut host, 0, 0, 0), 0x64, 1);
        assert!(!irq(&host, 154));
        assert!(!irq(&host, 162));
        host.now = PERIOD * 2 + ACTIVE;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert_eq!(&host.ram[0x100..0x102], &131u16.to_le_bytes());
        receiver.write(&mut Cx::new(&mut host, 0, 0, 0), BASE, 4, 0);
        host.now = PERIOD * 4;
        receiver.event(&mut Cx::new(&mut host, 0, 0, 0), token);
        assert_eq!(receiver.next_deadline(host.now), None);
        assert_eq!(sensor.0.lock().unwrap().frames[0], 2);
        let saved = receiver.save();
        receiver.restore(&saved).unwrap();
        assert!(receiver.restore(&Value::Null).is_err());
        assert_eq!(receiver.save(), saved);
    }
    #[test]
    fn a_still_read_out_may_outlast_its_frame() {
        let (sensor, receiver, mut host) = rig(true);
        assert!(
            Receiver::new(
                sensor.clone(),
                Wiring {
                    base: BASE,
                    normal_irqs: vec![154],
                    error_irqs: vec![155],
                    output_channels: vec![],
                    timing_output: true,
                    control: Control {
                        run_mask: 0x11,
                        run_value: 0x11,
                        reset_pulse: 0,
                        reset_ticks: 0
                    },
                },
                PERIOD,
                ACTIVE
            )
            .unwrap()
            .with_still_readout(StillReadout::Ticks(PERIOD))
            .is_err()
        );
        assert!(
            rig(true)
                .1
                .with_still_readout(StillReadout::Ticks(0))
                .is_err()
        );
        let mut receiver = receiver
            .with_still_readout(StillReadout::Ticks(PERIOD + 100))
            .unwrap();
        enable(&mut receiver, &mut host);
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        // Frame 1 starts at PERIOD with channel 0 requested; it ends at
        // PERIOD + ACTIVE without the still, which is due at 2·PERIOD + 100.
        host.now = PERIOD;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        host.now = PERIOD + ACTIVE;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(!irq(&host, 162));
        assert_eq!(&host.ram[0x100..0x104], &[0x55; 4]);
        assert_eq!(receiver.next_deadline(host.now), Some(PERIOD * 2));
        host.now = PERIOD * 2;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert_eq!(receiver.next_deadline(host.now), Some(PERIOD * 2 + 100));
        assert!(!irq(&host, 162));
        let saved = receiver.save();
        assert_eq!(saved["state"]["still_deadline"], PERIOD * 2 + 100);
        host.now = PERIOD * 2 + 100;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(irq(&host, 162));
        assert_eq!(&host.ram[0x100..0x104], &[0, 0, 31, 0]);
        assert_eq!(sensor.0.lock().unwrap().frames[0], 1);
        assert!(receiver.save()["state"].get("still_deadline").is_none());
        // The restored deferral completes the same way.
        let (_, other, mut other_host) = rig(true);
        let mut other = other
            .with_still_readout(StillReadout::Ticks(PERIOD + 100))
            .unwrap();
        other.restore(&saved).unwrap();
        other_host.now = PERIOD * 2 + 100;
        other.event(&mut Cx::new(&mut other_host, 0, 0, 0), 0);
        assert!(irq(&other_host, 162));
    }
    #[test]
    fn a_whole_frame_still_read_out_skips_the_frame_under_way() {
        // Requested before the first frame starts: served by that frame.
        let (_, receiver, mut host) = rig(true);
        let mut receiver = receiver
            .with_still_readout(StillReadout::WholeFrames)
            .unwrap();
        enable(&mut receiver, &mut host);
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        host.now = PERIOD;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(receiver.save()["state"]["still_started"].as_bool().unwrap());
        host.now = PERIOD + ACTIVE;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(irq(&host, 162));
        // Requested while a frame is under way: served by the next one.
        let (sensor, receiver, mut host) = rig(true);
        sensor.0.lock().unwrap().image.insert(0x40, 0);
        let mut receiver = receiver
            .with_still_readout(StillReadout::WholeFrames)
            .unwrap();
        enable(&mut receiver, &mut host);
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        host.now = PERIOD;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        host.now = PERIOD + 10;
        sensor.image_write(&mut Cx::new(&mut host, 0, 0, 0), 0x40, 1);
        host.now = PERIOD + ACTIVE;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(!irq(&host, 162));
        assert_eq!(receiver.next_deadline(host.now), Some(PERIOD * 2));
        host.now = PERIOD * 2;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        host.now = PERIOD * 2 + ACTIVE;
        receiver.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert!(irq(&host, 162));
        assert_eq!(&host.ram[0x100..0x104], &[0, 0, 31, 0]);
    }
    #[test]
    fn delayed_frame_service_has_the_same_state_and_pixels_as_each_clock_edge() {
        let (sensor_a, mut a, mut ah) = rig(true);
        let (sensor_b, mut b, mut bh) = rig(true);
        enable(&mut a, &mut ah);
        enable(&mut b, &mut bh);
        for frame in 1..=4 {
            ah.now = frame * PERIOD;
            a.advance(&mut Cx::new(&mut ah, 0, 0, 0));
            ah.now = frame * PERIOD + ACTIVE;
            a.advance(&mut Cx::new(&mut ah, 0, 0, 0));
        }
        bh.now = 4 * PERIOD + ACTIVE;
        b.advance(&mut Cx::new(&mut bh, 0, 0, 0));
        assert_eq!(a.save(), b.save());
        assert_eq!(ah.ram, bh.ram);
        assert_eq!(
            serde_json::to_value(&*sensor_a.0.lock().unwrap()).unwrap(),
            serde_json::to_value(&*sensor_b.0.lock().unwrap()).unwrap()
        );
        sensor_b.0.lock().unwrap().registers.insert(0, 1);
        assert_eq!(b.next_deadline(bh.now), Some(bh.now));
        b.advance(&mut Cx::new(&mut bh, 0, 0, 0));
        assert_eq!(b.next_deadline(bh.now), None);
    }
    /// Timing generator words at 0xfff6c000 +0x184/+0x190/+0x194/+0x198/+0x19c,
    /// written through the command block as X-T3 5.11 does.
    fn program_tg(sensor: &Sensor, host: &mut RecordingHost, words: [u32; 5]) {
        let mut command =
            super::super::sensor::Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
        for (o, w) in super::super::sensor::TG_WORDS.into_iter().zip(words) {
            let mut cx = Cx::new(host, 0, 0x104c9d8, 0);
            command.write(&mut cx, 0xfff6c000 + o, 4, u64::from(w));
            assert!(cx.refusal().is_none() && !cx.is_unmodeled());
        }
    }
    /// V79 (4K 23.98p): VMAX 7176, 6960 A lines of 418 clocks, B lines of 420
    /// clocks = 3 000 000 clocks; upper halves set to check the masks.
    const V79: [u32; 5] = [1, 0x0001_1b30, 0x0002_1c08, 0x0003_01a2, 0x0003_01a4];
    /// Mode table line at 59.94: HA 540, nA 2104, HB 542, VMAX 2224 = 1 201 200 clocks.
    const P5994: [u32; 5] = [1, 2104, 2224, 540, 542];
    fn x_t3_cadence() -> super::super::sensor::TgCadence {
        super::super::sensor::TgCadence::new(72_000_000, 54_000_000).unwrap()
    }
    #[test]
    fn timing_generator_cadence_off_is_identity() {
        let (_, mut plain, mut ph) = rig(true);
        let (sensor, mut rx, mut host) = rig(true);
        program_tg(&sensor, &mut host, V79);
        enable(&mut plain, &mut ph);
        enable(&mut rx, &mut host);
        for now in [PERIOD, PERIOD + ACTIVE, 2 * PERIOD, 2 * PERIOD + ACTIVE] {
            assert_eq!(rx.next_deadline(host.now), Some(now));
            assert_eq!(rx.next_deadline(host.now), plain.next_deadline(ph.now));
            (host.now, ph.now) = (now, now);
            rx.advance(&mut Cx::new(&mut host, 0, 0, 0));
            plain.advance(&mut Cx::new(&mut ph, 0, 0, 0));
            assert_eq!(rx.save(), plain.save());
            assert!(rx.save()["state"].get("tg_frame").is_none());
        }
        assert_eq!(rx.save()["state"]["frames"], 2);
    }
    #[test]
    fn timing_generator_cadence_times_frames_from_its_words() {
        for (words, period) in [(P5994, 900_900u64), (V79, 2_250_000)] {
            let (sensor, mut rx, mut host) = rig(true);
            sensor.use_tg_cadence(x_t3_cadence());
            program_tg(&sensor, &mut host, words);
            enable(&mut rx, &mut host);
            assert_eq!(rx.next_deadline(0), Some(period));
            host.now = period;
            rx.advance(&mut Cx::new(&mut host, 0, 0, 0));
            // Active part scaled like the declared one (half a frame here).
            assert_eq!(rx.next_deadline(period), Some(period + period / 2));
            assert_eq!(rx.save()["state"]["frame_deadline"], 2 * period);
            host.now = 2 * period;
            rx.advance(&mut Cx::new(&mut host, 0, 0, 0));
            assert_eq!(rx.save()["state"]["frames"], 1);
            assert_eq!(rx.save()["state"]["frame_deadline"], 3 * period);
            // The frame timing survives a checkpoint.
            let saved = rx.save();
            let (_, mut back, _) = rig(true);
            back.restore(&saved).unwrap();
            assert_eq!(back.save(), saved);
        }
    }
    /// Runs (`true`) or stops the timing generator (+0x184 bit 0), as
    /// 0x1f565b0 / 0x1f565fc do.
    fn run_tg(sensor: &Sensor, host: &mut RecordingHost, on: bool) {
        let mut command =
            super::super::sensor::Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
        let mut cx = Cx::new(host, 0, 0x104ca30, 0);
        command.write(&mut cx, 0xfff6c184, 4, u64::from(on));
        assert!(cx.refusal().is_none() && !cx.is_unmodeled());
    }
    fn step(rx: &mut Receiver, host: &mut RecordingHost, now: u64) {
        host.now = now;
        rx.advance(&mut Cx::new(host, 0, 0, 0));
    }
    #[test]
    fn a_stopped_timing_generator_sends_no_frame_until_it_restarts() {
        const P: u64 = 2_250_000;
        let (sensor, mut rx, mut host) = rig(true);
        sensor.use_tg_cadence(x_t3_cadence());
        // Never started: no frame.
        enable(&mut rx, &mut host);
        assert_eq!(rx.next_deadline(0), None);
        program_tg(&sensor, &mut host, V79);
        assert_eq!(rx.next_deadline(0), Some(0));
        step(&mut rx, &mut host, 0);
        assert_eq!(rx.next_deadline(0), Some(P));
        step(&mut rx, &mut host, P);
        step(&mut rx, &mut host, P + P / 2);
        assert_eq!(rx.save()["state"]["frames"], 1);
        // Stopped during the next frame: that frame never ends and no frame
        // nor frame timing follows, however long the generator stays off.
        step(&mut rx, &mut host, 2 * P);
        assert_eq!(rx.save()["state"]["active"], true);
        run_tg(&sensor, &mut host, false);
        assert_eq!(rx.next_deadline(2 * P + 1), Some(2 * P + 1));
        step(&mut rx, &mut host, 2 * P + 1);
        assert_eq!(rx.next_deadline(2 * P + 1), None);
        let (irqs, timing) = (host.irqs.len(), sensor.image_reg(0x144));
        for k in 3..=12 {
            step(&mut rx, &mut host, k * P);
            assert_eq!(rx.next_deadline(k * P), None);
        }
        let state = &rx.save()["state"];
        assert_eq!((state["frames"].as_u64(), state["active"].as_bool()), (Some(1), Some(false)));
        assert_eq!((host.irqs.len(), sensor.image_reg(0x144)), (irqs, timing));
        // Restarted: a whole frame later, the normal cadence again.
        let restart = 12 * P + 12_345;
        host.now = restart;
        run_tg(&sensor, &mut host, true);
        assert_eq!(rx.next_deadline(restart), Some(restart));
        step(&mut rx, &mut host, restart);
        assert_eq!(rx.next_deadline(restart), Some(restart + P));
        for k in 1..=3 {
            step(&mut rx, &mut host, restart + k * P);
            step(&mut rx, &mut host, restart + k * P + P / 2);
        }
        assert_eq!(rx.save()["state"]["frames"], 4);
        assert_eq!(rx.save()["state"]["frame_deadline"], restart + 4 * P);
    }
    #[test]
    fn a_restart_right_after_a_stop_starts_a_new_phase() {
        const P: u64 = 2_250_000;
        let (sensor, mut rx, mut host) = rig(true);
        sensor.use_tg_cadence(x_t3_cadence());
        program_tg(&sensor, &mut host, V79);
        enable(&mut rx, &mut host);
        step(&mut rx, &mut host, P);
        step(&mut rx, &mut host, P + P / 2);
        assert_eq!(rx.save()["state"]["frame_deadline"], 2 * P);
        // X-T3 5.11 0x20c342c: stop then start, with no receiver access between.
        let at = P + P / 2 + 1_000;
        host.now = at;
        run_tg(&sensor, &mut host, false);
        run_tg(&sensor, &mut host, true);
        assert_eq!(rx.next_deadline(at), Some(at));
        step(&mut rx, &mut host, at);
        assert_eq!(rx.next_deadline(at), Some(at + P));
        step(&mut rx, &mut host, 2 * P);
        assert_eq!(rx.save()["state"]["frames"], 1);
        step(&mut rx, &mut host, at + P);
        step(&mut rx, &mut host, at + P + P / 2);
        assert_eq!(rx.save()["state"]["frames"], 2);
        assert_eq!(rx.save()["state"]["frame_deadline"], at + 2 * P);
    }
    #[test]
    fn with_the_cadence_law_off_a_stopped_generator_changes_nothing() {
        let (_, mut plain, mut ph) = rig(true);
        let (sensor, mut rx, mut host) = rig(true);
        let mut stopped = V79;
        stopped[0] = 0;
        program_tg(&sensor, &mut host, stopped);
        enable(&mut plain, &mut ph);
        enable(&mut rx, &mut host);
        for now in [PERIOD, PERIOD + ACTIVE, 2 * PERIOD - 7, 2 * PERIOD, 2 * PERIOD + ACTIVE] {
            if now == 2 * PERIOD - 7 {
                run_tg(&sensor, &mut host, true);
                run_tg(&sensor, &mut host, false);
                run_tg(&sensor, &mut host, true);
            }
            assert_eq!(rx.next_deadline(now), plain.next_deadline(now));
            step(&mut rx, &mut host, now);
            step(&mut plain, &mut ph, now);
            assert_eq!(rx.save(), plain.save());
        }
        assert_eq!(rx.save()["state"]["frames"], 2);
        assert_eq!(host.irqs, ph.irqs);
    }
    /// IRQ line changes so far: each frame start raises the receiver line.
    fn frame_starts(host: &RecordingHost) -> usize {
        host.irqs.len()
    }
    #[test]
    fn a_restart_delay_of_zero_changes_nothing() {
        let (plain_sensor, mut plain, mut ph) = rig(true);
        let (sensor, mut rx, mut host) = rig(true);
        for s in [&plain_sensor, &sensor] {
            s.use_tg_cadence(x_t3_cadence());
        }
        sensor.set_tg_restart_delay(0).unwrap();
        program_tg(&plain_sensor, &mut ph, V79);
        program_tg(&sensor, &mut host, V79);
        enable(&mut plain, &mut ph);
        enable(&mut rx, &mut host);
        let mut now = 0;
        for k in 0..12u64 {
            if k == 5 {
                for (s, h) in [(&plain_sensor, &mut ph), (&sensor, &mut host)] {
                    h.now = now + 1_000;
                    run_tg(s, h, false);
                    run_tg(s, h, true);
                }
            }
            let a = rx.next_deadline(now).unwrap();
            assert_eq!(Some(a), plain.next_deadline(now));
            now = a;
            step(&mut rx, &mut host, now);
            step(&mut plain, &mut ph, now);
            assert_eq!(rx.save(), plain.save());
            assert!(rx.save()["state"].get("withheld").is_none());
        }
        assert_eq!(host.irqs, ph.irqs);
        assert!(rx.save()["state"]["frames"].as_u64().unwrap() >= 3);
    }
    #[test]
    fn a_restart_delay_withholds_its_periods_after_each_start() {
        const P: u64 = 2_250_000;
        let (sensor, mut rx, mut host) = rig(true);
        sensor.use_tg_cadence(x_t3_cadence());
        program_tg(&sensor, &mut host, V79);
        enable(&mut rx, &mut host);
        step(&mut rx, &mut host, P);
        step(&mut rx, &mut host, P + P / 2);
        assert_eq!(rx.save()["state"]["frames"], 1);
        sensor.set_tg_restart_delay(3).unwrap();
        // X-T3 5.11 0x20c342c: stop then start, with no receiver access between.
        let restart = P + P / 2 + 1_000;
        let withheld = |rx: &mut Receiver, host: &mut RecordingHost, restart: u64, n: u64| {
            host.now = restart;
            run_tg(&sensor, host, false);
            run_tg(&sensor, host, true);
            assert_eq!(rx.next_deadline(restart), Some(restart));
            step(rx, host, restart);
            assert_eq!(rx.save()["state"]["withheld"], n);
            let (irqs, timing, frames, ram) = (
                host.irqs.len(),
                sensor.image_reg(0x144),
                rx.save()["state"]["frames"].clone(),
                host.ram.clone(),
            );
            // N periods: the cadence goes on, but no frame start or end, no
            // cause, IRQ, timing or read-out, and no frame counted.
            for k in 1..=n {
                assert_eq!(rx.next_deadline(restart + (k - 1) * P), Some(restart + k * P));
                step(rx, host, restart + k * P);
                assert_eq!(rx.save()["state"]["active"], false);
                assert_eq!(rx.save()["state"]["frame_deadline"], restart + (k + 1) * P);
                assert_eq!((host.irqs.len(), sensor.image_reg(0x144)), (irqs, timing));
                assert_eq!(rx.save()["state"]["frames"], frames);
                assert!(host.ram == ram);
            }
            assert!(rx.save()["state"].get("withheld").is_none());
            // Then the normal cadence: a frame starts N + 1 periods after the start.
            let first = restart + (n + 1) * P;
            assert_eq!(rx.next_deadline(restart + n * P), Some(first));
            let starts = frame_starts(host);
            step(rx, host, first);
            assert_eq!(rx.save()["state"]["active"], true);
            assert!(frame_starts(host) > starts);
            step(rx, host, first + P / 2);
            assert_eq!(
                rx.save()["state"]["frames"].as_u64(),
                frames.as_u64().map(|f| f + 1)
            );
            step(rx, host, first + P);
            step(rx, host, first + P + P / 2);
            assert_eq!(rx.save()["state"]["frame_deadline"], first + 2 * P);
            first + P + P / 2
        };
        let after = withheld(&mut rx, &mut host, restart, 3);
        assert_eq!(rx.save()["state"]["frames"], 3);
        // The next start withholds again; a checkpoint keeps the count.
        let restart = after + 7_000;
        host.now = restart;
        run_tg(&sensor, &mut host, false);
        run_tg(&sensor, &mut host, true);
        step(&mut rx, &mut host, restart);
        step(&mut rx, &mut host, restart + P);
        let saved = rx.save();
        assert_eq!(saved["state"]["withheld"], 2);
        let (_, mut back, _) = rig(true);
        back.restore(&saved).unwrap();
        assert_eq!(back.save(), saved);
        let starts = frame_starts(&host);
        step(&mut rx, &mut host, restart + 2 * P);
        step(&mut rx, &mut host, restart + 3 * P);
        assert_eq!(frame_starts(&host), starts);
        step(&mut rx, &mut host, restart + 4 * P);
        assert!(frame_starts(&host) > starts);
        step(&mut rx, &mut host, restart + 4 * P + P / 2);
        assert_eq!(rx.save()["state"]["frames"], 4);
        // A delay changed between two starts applies to the next one.
        sensor.set_tg_restart_delay(1).unwrap();
        let after = withheld(&mut rx, &mut host, restart + 4 * P + P / 2 + 3_000, 1);
        assert_eq!(rx.save()["state"]["frames"], 6);
        // Off again: the next start sends its first frame one period later.
        sensor.set_tg_restart_delay(0).unwrap();
        let restart = after + 5_000;
        host.now = restart;
        run_tg(&sensor, &mut host, false);
        run_tg(&sensor, &mut host, true);
        step(&mut rx, &mut host, restart);
        let starts = frame_starts(&host);
        step(&mut rx, &mut host, restart + P);
        assert!(frame_starts(&host) > starts);
    }
    /// Raised timing lines (IRQ 166 start, 164 end) and receiver causes.
    /// Raised start pulses (165), timing lines (166 + 164), receiver causes (154).
    fn standby_counts(host: &RecordingHost) -> (usize, usize, usize) {
        let up = |line| host.irqs.iter().filter(|&&(n, v)| n == line && v).count();
        (up(165), up(166) + up(164), up(154))
    }
    #[test]
    fn standby_timing_keeps_only_the_start_pulse_without_data() {
        for on in [false, true] {
            let (sensor, mut rx, mut host) = rig_pulsed(false, Some((2, 165)));
            {
                // Programmed, then put in standby (register 0 bit 0 set).
                let mut s = sensor.0.lock().unwrap();
                s.registers.insert(0, 0x15);
                s.writes = 1;
                s.image.insert(0x140, 15);
            }
            if on {
                sensor.use_standby_timing();
            }
            assert!(!sensor.streaming());
            assert_eq!(sensor.timed_in_standby(), on);
            // Links stopped: the start pulse still goes on in standby.
            for k in 1..=4 {
                step(&mut rx, &mut host, k * ACTIVE);
            }
            let (pulses, timing, _) = standby_counts(&host);
            assert_eq!(pulses > 0, on, "{pulses}");
            assert_eq!(timing, 0);
            enable(&mut rx, &mut host);
            for k in 5..=10 {
                step(&mut rx, &mut host, k * ACTIVE);
            }
            let (pulses, timing, causes) = standby_counts(&host);
            assert_eq!((timing, causes), (0, 0));
            assert_eq!(rx.save()["state"]["frames"], 0);
            assert!(host.ram.iter().all(|&b| b == 0x55));
            if on {
                assert_eq!(rx.save()["state"]["armed"], true);
                assert!(pulses >= 2, "{pulses}");
            } else {
                assert_eq!(pulses, 0);
                assert_eq!(rx.save()["state"]["armed"], false);
            }
            // Wake (register 0 bit 0 clear): data frames resume.
            sensor.0.lock().unwrap().registers.insert(0, 0x14);
            for k in 11..=16 {
                step(&mut rx, &mut host, k * ACTIVE);
            }
            let frames = rx.save()["state"]["frames"].as_u64().unwrap();
            assert!(frames >= 2, "{frames}");
            let (_, timing, causes) = standby_counts(&host);
            assert!(timing > 0 && causes > 0, "{timing} {causes}");
            assert_eq!(rx.save()["state"].get("timing_only").is_some(), false);
        }
    }
    #[test]
    fn standby_timing_follows_a_stopped_generator() {
        let (sensor, mut rx, mut host) = rig_pulsed(false, Some((2, 165)));
        {
            let mut s = sensor.0.lock().unwrap();
            s.registers.insert(0, 0x15);
            s.writes = 1;
            s.image.insert(0x140, 15);
        }
        sensor.use_tg_cadence(x_t3_cadence());
        sensor.use_standby_timing();
        enable(&mut rx, &mut host);
        // Generator stopped: no timing even in standby.
        run_tg(&sensor, &mut host, false);
        for k in 1..=4 {
            step(&mut rx, &mut host, k * ACTIVE);
        }
        assert_eq!(standby_counts(&host), (0, 0, 0));
        assert_eq!(rx.save()["state"]["armed"], false);
    }
    #[test]
    fn a_start_the_receiver_does_not_arm_on_is_dropped() {
        const P: u64 = 2_250_000;
        // Sensor in standby: the start is seen, the receiver stays unarmed.
        let (sensor, mut rx, mut host) = rig(false);
        sensor.use_tg_cadence(x_t3_cadence());
        sensor.set_tg_restart_delay(2).unwrap();
        enable(&mut rx, &mut host);
        program_tg(&sensor, &mut host, V79);
        assert!(sensor.tg_start_pending());
        assert_eq!(rx.next_deadline(0), Some(0));
        step(&mut rx, &mut host, 0);
        assert!(!sensor.tg_start_pending());
        assert_eq!(rx.next_deadline(0), None);
        // Streaming later, with the generator already running: no delay.
        sensor.0.lock().unwrap().registers.insert(0, 0x14);
        sensor.0.lock().unwrap().writes = 1;
        host.now = 10;
        assert_eq!(rx.next_deadline(10), Some(10));
        step(&mut rx, &mut host, 10);
        assert!(rx.save()["state"].get("withheld").is_none());
        step(&mut rx, &mut host, 10 + P);
        assert_eq!(rx.save()["state"]["active"], true);
    }
}
