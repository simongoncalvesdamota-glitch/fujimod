//! Eight-byte sensor command packets fetched by programmed DMA.
//! Completion is scheduled; stock code owns its busy polls, cleanup and retries.
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize)]
pub struct SensorState {
    pub registers: BTreeMap<u16, u8>,
    pub commands: u64,
    pub writes: u64,
    pub reads: u64,
    #[serde(default)]
    pub image: BTreeMap<u64, u32>,
    #[serde(default)]
    pub frames: [u64; 2],
    #[serde(default)]
    pub last_dma: [(u64, u32, u32); 2],
    /// A read-out start request taken but not yet served (see `frame`).
    #[serde(default)]
    pub armed: [bool; 2],
}
impl Default for SensorState {
    fn default() -> Self {
        // Functional reset assumption: standby until stock programming clears
        // register0 bit0. Stock sleep path330a6e0 sets this bit; wake clears it.
        Self {
            registers: BTreeMap::from([(0, 1)]),
            commands: 0,
            writes: 0,
            reads: 0,
            image: BTreeMap::new(),
            frames: [0; 2],
            last_dma: [(0, 0, 0); 2],
            armed: [false; 2],
        }
    }
}
#[derive(Clone, Copy)]
pub struct ImageRoutes {
    pub dma: Option<[u32; 2]>,
    pub timing: [u32; 2],
    /// Without DMA routes, latch the read-out DMA configuration words (and
    /// +0x38) instead of refusing them; no image moves and no DMA interrupt
    /// rises. XP4 X-T3 5.11 programs them when live view starts.
    pub latch_unrouted: bool,
    /// (cause mask, line): at each frame start, pulse this edge-triggered
    /// line once while a cause of the mask is enabled in +0x140. XP4 X-T3
    /// 5.11 enables causes 0..2 (+0x140 = 7) and lists three imaging lines
    /// (0x149d2d4: GIC 100, 102, 101); its still capture counts frames on GIC
    /// 101 (0x20cfbd8 installs 0x20d0134, which calls 0x20cfa24 after N
    /// pulses). What else the cause signals is not modeled.
    pub start_pulse: Option<(u32, u32)>,
}
/// Frame cadence of the sensor timing generator words of the command block
/// (opt-in; default: the declared frame period). X-T3 5.11 programs them in
/// 0xfff6c000 (setters 0x104c9d8..0x104cfc8, values 0x1f566dc from the
/// structure 0x2a6d830 that 0x20c5730 computes for the mode and cadence):
/// +0x184 bit 0 runs the generator, +0x190 low half = lines of HA clocks
/// (nA), +0x194 low half = lines per frame (VMAX), +0x198 / +0x19c bits 0..13
/// = clocks of an A / B line (HA, HB). A frame lasts
/// HA * nA + HB * (VMAX - nA) clocks of `clock_hz` (X-T3 5.11: 72 MHz, table
/// 0x22aca90 code 2), counted in ticks of `tick_hz`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TgCadence {
    pub clock_hz: u64,
    pub tick_hz: u64,
}
/// Frames the sensor withholds after each timing generator start (opt-in,
/// needs [`TgCadence`]): a real sensor driven again after +0x184 bit 0 goes
/// from 0 to 1 may need a few vertical periods before it sends a frame. With
/// `frames` = N, the receiver keeps the cadence but its first N periods after
/// each start send nothing: no frame start or end (GIC 102 / 100), no frame
/// cause, no read-out DMA (GIC 97), and the frame counter does not count them.
/// The first frame starts at the start plus N + 1 periods. 0 = no delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TgRestartDelay {
    pub frames: u32,
}
/// Command-block offsets of the timing generator words, in the order
/// [`TgCadence::clocks`] reads them.
pub const TG_WORDS: [u64; 5] = [0x184, 0x190, 0x194, 0x198, 0x19c];
impl TgCadence {
    pub fn new(clock_hz: u64, tick_hz: u64) -> Result<Self, String> {
        if clock_hz == 0 || tick_hz == 0 {
            return Err("timing generator cadence needs positive clock and tick rates".into());
        }
        Ok(Self { clock_hz, tick_hz })
    }
    /// Clocks per frame of the words at [`TG_WORDS`]; none while the
    /// generator is stopped or a line count or line length is zero.
    pub fn clocks(words: [u32; 5]) -> Option<u64> {
        if words[0] & 1 == 0 {
            return None;
        }
        let vmax = u64::from(words[2] & 0xffff);
        let a_lines = u64::from(words[1] & 0xffff).min(vmax);
        let (ha, hb) = (u64::from(words[3] & 0x3fff), u64::from(words[4] & 0x3fff));
        if vmax == 0 || ha == 0 || hb == 0 {
            return None;
        }
        Some(ha * a_lines + hb * (vmax - a_lines))
    }
    /// Ticks of a frame of `clocks` after the remainder `rest` (in
    /// clock_hz-ths of a tick) of the previous frames, and the new remainder.
    pub fn ticks(&self, clocks: u64, rest: u64) -> Option<(u64, u64)> {
        let total = u128::from(clocks) * u128::from(self.tick_hz) + u128::from(rest);
        let clock = u128::from(self.clock_hz);
        Some((u64::try_from(total / clock).ok()?, (total % clock) as u64))
    }
}
/// Sensor analog gain law (opt-in, phase A of the exposure model): the SPI
/// gain code of lane 0, registers 0x30 (bits 0..7) and 0x31 (bits 8..11), as
/// X-T3 5.11 computes it (0x20c3050 -> 0x20c2c88: code = 4095 * (1 -
/// 10^(-dB/20)), capped at 0xefc = 3836) and queues it (0x20c1ae8 ->
/// 0x20bb150). Linear gain = 4095 / (4095 - code). Signals are scaled by
/// gain(code) / gain(`gain_ref`), `gain_ref` being the code the body sends
/// at its base sensitivity; a code never written scales by 1. With
/// `conversion_gain`, register 0x2f bit 0 (the high conversion gain the body
/// selects above base, T44-ex640) multiplies the signal by that ratio; the
/// reference is taken with the bit clear.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorExposure {
    pub gain_ref: u16,
    #[serde(default)]
    pub conversion_gain: Option<f64>,
    /// Exposure time law ([`SensorShutter`], opt-in; absent = gain only).
    #[serde(default)]
    pub shutter: Option<SensorShutter>,
}
/// Exposure time law (opt-in, needs the gain law): the signal also scales
/// with the integration time the firmware programs over SPI. X-T3 5.11
/// (0x20c721c -> 0x20c6050 -> 0x20c6004 -> 0x20c0c90, T60c) sends, in the
/// modes that pass 0x20c54a0, n = registers 0x21 / 0x22 (frames minus one)
/// and SHS = registers 0x25 / 0x26 computed from the pose P8 of the AE in
/// eighths of a line: raw = (n + 1) * 8 * VMAX - P8 + C, SHS = round(raw /
/// 16). Its inverse gives the lines of integration:
/// lines = (n + 1) * VMAX + C / 8 - 2 * SHS (T60c: 0x8c n 3 SHS 19 -> 11450.75
/// for P8 / 8 = 11451; 0x8b n 3 SHS 3 VMAX 3156 -> 12618.75 for 12618).
/// VMAX is the timing generator word +0x194 (low half); while it holds
/// `free_run_vmax` (0xffdc in live view 0x8c / 0x9e, where the firmware
/// counts a frame of 2872 lines), `free_run_lines` stands for it. C is
/// `offset_eighths` (0x20c5920(0): 6 at HA 418). Signals scale by lines /
/// `ref_lines`; with neither SHS byte written the factor is 1. The law reads
/// the registers as the firmware leaves them: still capture tables that
/// carry SHS 0x13 and n 0 give VMAX + C / 8 - 38 lines, whatever the
/// shutter does.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorShutter {
    pub ref_lines: f64,
    pub offset_eighths: u16,
    pub free_run_vmax: u16,
    pub free_run_lines: u16,
}
/// SPI registers of SHS, low byte first.
pub const SHS_REGISTERS: [u16; 2] = [0x25, 0x26];
/// SPI registers of n (frames of integration minus one), low byte first.
pub const FRAMES_REGISTERS: [u16; 2] = [0x21, 0x22];
/// Fewest lines the law gives (an eighth of a line, the unit of P8).
pub const SHUTTER_LINES_MIN: f64 = 0.125;
fn register_pair(registers: &BTreeMap<u16, u8>, pair: [u16; 2]) -> Option<u16> {
    let [low, high] = pair.map(|r| registers.get(&r).copied());
    if low.is_none() && high.is_none() {
        return None;
    }
    Some(u16::from(low.unwrap_or(0)) | (u16::from(high.unwrap_or(0)) << 8))
}
impl SensorShutter {
    pub fn new(
        ref_lines: f64,
        offset_eighths: u16,
        free_run_vmax: u16,
        free_run_lines: u16,
    ) -> Result<Self, String> {
        if !(ref_lines.is_finite() && ref_lines > 0.0) {
            return Err("sensor shutter reference lines must be positive".into());
        }
        if free_run_lines == 0 {
            return Err("sensor shutter free-run lines must be positive".into());
        }
        Ok(Self { ref_lines, offset_eighths, free_run_vmax, free_run_lines })
    }
    /// Lines of integration for the latched registers and the timing
    /// generator VMAX word; none while SHS is unwritten or VMAX is 0.
    pub fn lines(&self, registers: &BTreeMap<u16, u8>, tg_vmax: u16) -> Option<f64> {
        let shs = register_pair(registers, SHS_REGISTERS)?;
        let n = register_pair(registers, FRAMES_REGISTERS).unwrap_or(0);
        let vmax = if tg_vmax == self.free_run_vmax { self.free_run_lines } else { tg_vmax };
        if vmax == 0 {
            return None;
        }
        let lines = (f64::from(n) + 1.0) * f64::from(vmax) + f64::from(self.offset_eighths) / 8.0
            - 2.0 * f64::from(shs);
        Some(lines.max(SHUTTER_LINES_MIN))
    }
    /// Signal factor of the exposure time (1 while SHS is unwritten).
    pub fn factor(&self, registers: &BTreeMap<u16, u8>, tg_vmax: u16) -> f64 {
        self.lines(registers, tg_vmax).map_or(1.0, |lines| lines / self.ref_lines)
    }
}
/// SPI registers of the lane 0 gain code, low byte first.
pub const GAIN_REGISTERS: [u16; 2] = [0x30, 0x31];
/// Highest gain code the firmware sends (0x20c2c88 cap).
pub const GAIN_CODE_MAX: u16 = 0xefc;
/// SPI register whose bit 0 selects the high conversion gain of lane 0.
pub const CONVERSION_REGISTER: u16 = 0x2f;
impl SensorExposure {
    pub fn new(gain_ref: u16, conversion_gain: Option<f64>) -> Result<Self, String> {
        if gain_ref > GAIN_CODE_MAX {
            return Err(format!("sensor gain reference exceeds code {GAIN_CODE_MAX}"));
        }
        if conversion_gain.is_some_and(|ratio| !(ratio.is_finite() && ratio > 0.0)) {
            return Err("sensor conversion gain must be a positive ratio".into());
        }
        Ok(Self { gain_ref, conversion_gain, shutter: None })
    }
    /// The same law with the exposure time law (checked again).
    pub fn with_shutter(self, shutter: SensorShutter) -> Result<Self, String> {
        let shutter = SensorShutter::new(
            shutter.ref_lines,
            shutter.offset_eighths,
            shutter.free_run_vmax,
            shutter.free_run_lines,
        )?;
        Ok(Self { shutter: Some(shutter), ..self })
    }
    /// Linear gain of a 12-bit code (capped like the firmware).
    pub fn linear(code: u16) -> f64 {
        4095.0 / (4095.0 - f64::from(code.min(GAIN_CODE_MAX)))
    }
    /// The gain code latched in `registers`; none while neither byte has
    /// been written.
    pub fn code(registers: &BTreeMap<u16, u8>) -> Option<u16> {
        let [low, high] = GAIN_REGISTERS.map(|r| registers.get(&r).copied());
        if low.is_none() && high.is_none() {
            return None;
        }
        Some(u16::from(low.unwrap_or(0)) | (u16::from(high.unwrap_or(0) & 0xf) << 8))
    }
    /// Signal factor for the latched registers (1 while unwritten).
    pub fn factor(&self, registers: &BTreeMap<u16, u8>) -> f64 {
        Self::code(registers).map_or(1.0, |code| {
            let high = registers.get(&CONVERSION_REGISTER).is_some_and(|bits| bits & 1 != 0);
            let conversion = match (self.conversion_gain, high) {
                (Some(ratio), true) => ratio,
                _ => 1.0,
            };
            Self::linear(code) * conversion / Self::linear(self.gain_ref)
        })
    }
    /// Signal factor with the exposure time law, `tg_vmax` being the timing
    /// generator word +0x194 (low half); the gain factor alone without it.
    pub fn factor_at(&self, registers: &BTreeMap<u16, u8>, tg_vmax: u16) -> f64 {
        match self.shutter {
            None => self.factor(registers),
            Some(shutter) => self.factor(registers) * shutter.factor(registers, tg_vmax),
        }
    }
}
/// Opt-in read-out laws shared by every clone of a sensor: the scene painter
/// (default: the moving test ramp), the line pitch (default: lines packed)
/// and the timing generator cadence (default: the declared period).
#[derive(Clone, Default)]
pub struct ReadOut {
    painter: Option<super::statistics::Statistics>,
    /// Write each line at the channel's pitch word (+0x18), as X-T3 5.11
    /// programs it (0x31e0 = 12768 bytes for 6276 samples).
    pitch: bool,
    /// The cadence law (opt-in).
    tg: Option<TgCadence>,
    /// The last command-block words at [`TG_WORDS`], always mirrored by
    /// [`Command`], which keeps and checkpoints them.
    tg_words: [u32; 5],
    /// With the cadence law on, the generator was stopped (+0x184 bit 0
    /// 1 -> 0) since the receiver last looked: its next frame starts a new
    /// phase, even when the firmware restarts it at once (X-T3 5.11
    /// 0x20c342c: 0x1f565fc then 0x1f565b0, 37 instructions apart).
    tg_stop: bool,
    /// Periods withheld after each generator start ([`TgRestartDelay`]);
    /// 0 = none. Needs the cadence law.
    tg_restart_delay: u32,
    /// A generator start (+0x184 bit 0, 0 -> 1) seen with a restart delay
    /// set, waiting for the receiver ([`Sensor::take_tg_start`]).
    tg_start: bool,
    /// Frame timing in standby (opt-in, [`Sensor::use_standby_timing`]).
    standby_timing: bool,
    /// The sensor gain law (opt-in).
    exposure: Option<SensorExposure>,
    /// Mirror of the VMAX word (+0x194, low half) for the exposure time law,
    /// shared with the statistics that follow the exposure.
    tg_vmax: Arc<AtomicU32>,
}
type ScenePainter = Arc<Mutex<ReadOut>>;
#[derive(Clone)]
pub struct Sensor(
    pub Arc<Mutex<SensorState>>,
    Option<ImageRoutes>,
    (u16, u8),
    ScenePainter,
);
impl Sensor {
    pub fn new(routes: Option<ImageRoutes>, read_gate: (u16, u8)) -> Self {
        Self(Arc::default(), routes, read_gate, Arc::default())
    }
    /// Paint read-out frames from this statistics scene instead of the ramp.
    pub fn paint_scene(&self, scene: super::statistics::Statistics) {
        self.3.lock().unwrap().painter = Some(scene);
    }
    /// Write read-out lines at the channel's programmed pitch.
    pub fn use_line_pitch(&self) {
        self.3.lock().unwrap().pitch = true;
    }
    /// Time frames from the timing generator words of the command block.
    pub fn use_tg_cadence(&self, cadence: TgCadence) {
        self.3.lock().unwrap().tg = Some(cadence);
    }
    /// Scale the painted RAW and the input-buffer statistics by the gain the
    /// firmware programs over SPI. Call after [`Sensor::paint_scene`].
    pub fn use_sensor_exposure(&self, exposure: SensorExposure) -> Result<(), String> {
        let mut read_out = self.3.lock().unwrap();
        let painter = read_out
            .painter
            .as_ref()
            .ok_or("The sensor exposure law needs the RAW scene")?;
        painter.follow_exposure(self.0.clone(), exposure, read_out.tg_vmax.clone());
        read_out.exposure = Some(exposure);
        Ok(())
    }
    /// Signal factor of the sensor gain law now (1 when the law is off).
    pub fn exposure_factor(&self) -> f64 {
        let (exposure, vmax) = {
            let read_out = self.3.lock().unwrap();
            (read_out.exposure, read_out.tg_words[2] as u16)
        };
        exposure.map_or(1.0, |e| e.factor_at(&self.0.lock().unwrap().registers, vmax))
    }
    /// Mirror a command-block word of the timing generator.
    pub fn tg_write(&self, offset: u64, value: u32) {
        if let Some(k) = TG_WORDS.iter().position(|&o| o == offset) {
            let mut read_out = self.3.lock().unwrap();
            if k == 0 && read_out.tg.is_some() && read_out.tg_words[0] & 1 != 0 && value & 1 == 0 {
                read_out.tg_stop = true;
            }
            if k == 0
                && read_out.tg.is_some()
                && read_out.tg_restart_delay != 0
                && read_out.tg_words[0] & 1 == 0
                && value & 1 != 0
            {
                read_out.tg_start = true;
            }
            read_out.tg_words[k] = value;
            if k == 2 {
                read_out.tg_vmax.store(value & 0xffff, Ordering::Relaxed);
            }
        }
    }
    /// With the cadence law on, whether the generator is stopped (+0x184
    /// bit 0 clear): the sensor is not driven and sends no frame (X-T3 5.11
    /// 0x20c48ec stops it while 0x20cfd4c recomputes the mode). Always false
    /// with the law off.
    pub fn tg_stopped(&self) -> bool {
        let read_out = self.3.lock().unwrap();
        read_out.tg.is_some() && read_out.tg_words[0] & 1 == 0
    }
    /// Keep the frame start pulse while the programmed sensor is in standby
    /// (register 0 bit 0 set), receiver links stopped or not: the receiver
    /// goes on timing frames and pulses the start line only, with no timing
    /// output and no frame data (no receiver cause, read-out or input-buffer
    /// frame). X-T3 5.11 ccdtgSetSleepModeForMV (0x20c37a8) sleeps the
    /// sensor, stops link 0, and counts start pulses (slot 2, GIC 101,
    /// callback 0x20986ac -> 0x20a1c24) to wake it; each end-of-frame timing
    /// line (slot 0, GIC 100, 0x209c94c -> 0x2098094 -> 0x2097f9c) re-arms
    /// that count, so it must stay quiet in standby.
    pub fn use_standby_timing(&self) {
        self.3.lock().unwrap().standby_timing = true;
    }
    /// With [`Sensor::use_standby_timing`], whether the programmed sensor is
    /// in standby now, so the receiver times frames without data.
    pub fn timed_in_standby(&self) -> bool {
        self.3.lock().unwrap().standby_timing && {
            let s = self.0.lock().unwrap();
            s.writes != 0 && s.registers.get(&0).copied().unwrap_or(1) & 1 != 0
        }
    }
    /// The start pulse alone, for a frame timed in standby: no timing line.
    pub fn standby_pulse(&self, cx: &mut Cx<'_>) {
        let Some(routes) = self.1 else { return };
        let enabled = self.0.lock().unwrap().image.get(&0x140).copied().unwrap_or(0);
        if let Some((mask, irq)) = routes.start_pulse
            && enabled & mask != 0
        {
            cx.set_irq(irq, true);
            cx.set_irq(irq, false);
        }
    }
    /// Whether a generator stop is waiting for [`Sensor::take_tg_stop`].
    pub fn tg_stop_pending(&self) -> bool {
        self.3.lock().unwrap().tg_stop
    }
    /// Take the generator stop seen since the last call (law on only).
    pub fn take_tg_stop(&self) -> bool {
        std::mem::take(&mut self.3.lock().unwrap().tg_stop)
    }
    /// Periods withheld after each later generator start (0 = none). Needs
    /// the cadence law. The receiver reads the delay when it arms on the
    /// start; 0 also drops a start not taken yet.
    pub fn set_tg_restart_delay(&self, frames: u32) -> Result<(), String> {
        let mut read_out = self.3.lock().unwrap();
        if read_out.tg.is_none() {
            return Err("A timing generator restart delay needs the cadence law (imaging_tg_cadence)".into());
        }
        read_out.tg_restart_delay = frames;
        if frames == 0 {
            read_out.tg_start = false;
        }
        Ok(())
    }
    /// Periods withheld after each generator start (0 = none).
    pub fn tg_restart_delay(&self) -> u32 {
        self.3.lock().unwrap().tg_restart_delay
    }
    /// Whether a generator start is waiting for [`Sensor::take_tg_start`].
    pub fn tg_start_pending(&self) -> bool {
        self.3.lock().unwrap().tg_start
    }
    /// Take the generator start seen since the last call (only with a
    /// restart delay set).
    pub fn take_tg_start(&self) -> bool {
        std::mem::take(&mut self.3.lock().unwrap().tg_start)
    }
    /// The timing generator words at [`TG_WORDS`], for read-out laws.
    pub fn tg_words(&self) -> [u32; 5] {
        self.3.lock().unwrap().tg_words
    }
    /// The timing generator cadence law, when enabled, and the clocks per
    /// frame its words give now.
    pub fn tg_cadence(&self) -> Option<(TgCadence, Option<u64>)> {
        let read_out = self.3.lock().unwrap();
        read_out
            .tg
            .map(|cadence| (cadence, TgCadence::clocks(read_out.tg_words)))
    }
    pub fn image_reg(&self, offset: u64) -> u32 {
        self.0
            .lock()
            .unwrap()
            .image
            .get(&offset)
            .copied()
            .unwrap_or(0)
    }
    pub fn image_write(&self, cx: &mut Cx<'_>, offset: u64, value: u32) {
        let mut s = self.0.lock().unwrap();
        if matches!(offset, 0x64 | 0xe4 | 0x144) {
            let old = s.image.get(&offset).copied().unwrap_or(0);
            s.image.insert(offset, old & !(value & 15));
        } else {
            s.image.insert(offset, value);
        }
        // Bit 0 of +0x40 / +0xc0 requests one read-out. X-T3 5.11 still
        // capture sets it (0x1dcb570 -> 0x1049910, en0 = 1) and clears it again
        // about 0.2 ms later (0x1dcbe14), well before the next frame ends: the
        // request is latched and served by the next frame.
        if let Some(channel) = [0x40, 0xc0].iter().position(|&b| b == offset)
            && value & 1 != 0
        {
            s.armed[channel] = true;
        }
        let Some(routes) = self.1 else { return };
        if let Some(dma) = routes.dma {
            for (enable, pending, irq) in [(0x60, 0x64, dma[0]), (0xe0, 0xe4, dma[1])] {
                cx.set_irq(
                    irq,
                    s.image.get(&enable).copied().unwrap_or(0)
                        & s.image.get(&pending).copied().unwrap_or(0)
                        & 15
                        != 0,
                );
            }
        }
        let flags =
            s.image.get(&0x140).copied().unwrap_or(0) & s.image.get(&0x144).copied().unwrap_or(0);
        cx.set_irq(routes.timing[0], flags & 3 != 0);
        cx.set_irq(routes.timing[1], flags & 12 != 0);
    }
    pub fn timing(&self, cx: &mut Cx<'_>, start: bool) {
        let Some(routes) = self.1 else { return };
        let mut s = self.0.lock().unwrap();
        // Global sensor timing remains active even without a RAW DMA buffer.
        // 1604184(event1) ->IRQ166 ->1603d98 ->imgdc32c6bd8/15ae.
        let pending = s.image.get(&0x144).copied().unwrap_or(0) | if start { 3 } else { 12 };
        s.image.insert(0x144, pending);
        let enabled = s.image.get(&0x140).copied().unwrap_or(0);
        cx.set_irq(routes.timing[0], pending & enabled & 3 != 0);
        cx.set_irq(routes.timing[1], pending & enabled & 12 != 0);
        if let Some((mask, irq)) = routes.start_pulse
            && start
            && enabled & mask != 0
        {
            cx.set_irq(irq, true);
            cx.set_irq(irq, false);
        }
    }
    /// Whether a read-out of `channel` is requested (start bit set or latched).
    pub fn requested(&self, channel: usize) -> bool {
        let s = self.0.lock().unwrap();
        let base = [0x40, 0xc0][channel];
        s.image.get(&base).copied().unwrap_or(0) & 1 != 0 || s.armed[channel]
    }
    pub fn frame(&self, cx: &mut Cx<'_>, channel: usize) -> Result<(), String> {
        let Some(routes) = self.1 else { return Ok(()) };
        let Some(dma) = routes.dma else { return Ok(()) };
        let mut s = self.0.lock().unwrap();
        let base = [0x40, 0xc0][channel];
        let reg = |o| s.image.get(&o).copied().unwrap_or(0);
        if reg(base) & 1 == 0 && !s.armed[channel] {
            return Ok(());
        }
        let address = u64::from(reg(base + 0x10)) | (u64::from(reg(base + 0xc) & 7) << 32);
        let width = reg(base + 4) & 0x7fff;
        let height = reg(base + 8) & 0x7fff;
        // No framebuffer override: shape and destination come from143c828/dafc.
        if address == 0 || width == 0 || height == 0 {
            return Ok(());
        }
        let size = u64::from(width) * u64::from(height) * 2;
        if size > 128 * 1024 * 1024 {
            return Err("receiver frame exceeds functional DMA limit".into());
        }
        let frame = s.frames[channel];
        let ReadOut {
            painter,
            pitch,
            exposure,
            tg_words,
            ..
        } = self.3.lock().unwrap().clone();
        let gain = exposure.map_or(1.0, |e| e.factor_at(&s.registers, tg_words[2] as u16));
        let line = u64::from(width) * 2;
        let pitch = match u64::from(reg(base + 0x18)) {
            p if pitch && p > line => p,
            _ => line,
        };
        let bytes = if let Some(scene) = painter {
            scene.paint_raw_scaled(width as usize, height as usize, gain)
        } else {
            let mut bytes = Vec::with_capacity(size as usize);
            for y in 0..height {
                for x in 0..width {
                    // Explicit RAW14-in-LE16 moving ramp for the normal REC14 profile.
                    let sample =
                        ((u64::from(x) * 31 + u64::from(y) * 17 + frame * 131) & 0x3fff) as u16;
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
            }
            bytes
        };
        if pitch == line {
            cx.ram_write(address, &bytes)?;
        } else {
            for (y, row) in bytes.chunks_exact(line as usize).enumerate() {
                cx.ram_write(address + y as u64 * pitch, row)?;
            }
        }
        s.frames[channel] += 1;
        s.last_dma[channel] = (address, width, height);
        s.armed[channel] = false;
        let pending = base + 0x24;
        let value = s.image.get(&pending).copied().unwrap_or(0) | 1;
        s.image.insert(pending, value);
        let enabled = s.image.get(&(base + 0x20)).copied().unwrap_or(0);
        cx.set_irq(dma[channel], value & enabled & 15 != 0);
        Ok(())
    }
    fn image_supported(&self, offset: u64) -> bool {
        self.1.is_some_and(|r| {
            // X-T3 5.11 writes +0x38 when live view starts, DMA routed or not.
            if offset == 0x38 {
                return r.latch_unrouted;
            }
            matches!(offset, 0x140 | 0x144) || r.dma.is_some() || r.latch_unrouted
        })
    }
    pub fn streaming(&self) -> bool {
        let s = self.0.lock().unwrap();
        s.writes != 0 && s.registers.get(&0).copied().unwrap_or(1) & 1 == 0
    }
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    registers: BTreeMap<u64, u32>,
    response: [u32; 8],
    generation: u64,
    pending: Option<Vec<[u8; 8]>>,
    deadline: Option<u64>,
}
pub struct Command {
    base: u64,
    state: State,
    pub sensor: Sensor,
    latency_ticks: u64,
}
impl Command {
    pub fn new(base: u64, sensor: Sensor, latency_ticks: u64) -> Result<Self, String> {
        if base & 0xfff != 0 || latency_ticks == 0 {
            return Err("sensor command needs aligned base and positive latency".into());
        }
        Ok(Self {
            base,
            state: State::default(),
            sensor,
            latency_ticks,
        })
    }
    fn reg(&self, o: u64) -> u32 {
        self.state.registers.get(&o).copied().unwrap_or(0)
    }
    fn cancel(&mut self) {
        self.state.generation = self.state.generation.wrapping_add(1);
        self.state.pending = None;
        self.state.deadline = None;
        self.state.registers.insert(0x204, 0);
    }
    fn submit(&mut self, cx: &mut Cx<'_>) -> Result<(), String> {
        if self.state.pending.is_some() {
            return Err("sensor command submitted while busy".into());
        }
        let format = self.reg(0x208);
        if format & 0x7fffff00 != 0x10400
            || self.reg(0x21c) != 0
            || self.reg(0x214) != 0
            || self.reg(0x218) != 8
            || self.reg(0x20c) > 7
        {
            return Err("unsupported sensor command DMA descriptor".into());
        }
        let source = u64::from(self.reg(0x210)) | (u64::from(self.reg(0x20c)) << 32);
        let count = match format & 255 {
            0 => return Err("zero sensor packet count is unsupported".into()),
            n => n as usize,
        };
        let bytes = cx.ram_bytes(source, count * 8)?;
        let mut packets = Vec::with_capacity(count);
        for bytes in bytes.chunks_exact(8) {
            let packet: [u8; 8] = bytes.try_into().unwrap();
            if !matches!(packet[0], 0x81 | 0x82) {
                return Err(format!("unsupported sensor opcode {:02x}", packet[0]));
            }
            packets.push(packet);
        }
        let duration = self
            .latency_ticks
            .checked_mul(count as u64)
            .ok_or("sensor command duration overflow")?;
        let at = cx
            .now()
            .checked_add(duration)
            .ok_or("sensor command deadline overflow")?;
        self.state.pending = Some(packets);
        self.state.registers.insert(0x204, 1);
        self.state.generation = self.state.generation.wrapping_add(1);
        self.state.deadline = Some(at);
        cx.schedule(at, self.state.generation);
        Ok(())
    }
}
impl Device for Command {
    fn name(&self) -> &'static str {
        "imaging-sensor"
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let o = address.wrapping_sub(self.base);
        self.advance(cx);
        if size != 4 || o & 3 != 0 {
            cx.unmodeled();
            return 0;
        }
        match o {
            0 => u64::from(self.reg(0) | 1),
            0x4..=0x38 | 0x40..=0x94 | 0xc0..=0x114 | 0x140 | 0x144 => {
                if !self.sensor.image_supported(o) {
                    cx.unmodeled();
                    return 0;
                }
                u64::from(self.sensor.image_reg(o))
            }
            0x180..=0x1b0 | 0x200 | 0x204 | 0x208 | 0x20c | 0x210 | 0x214 | 0x218 | 0x21c => {
                u64::from(self.reg(o))
            }
            0x224..=0x240 => u64::from(self.state.response[((0x240 - o) / 4) as usize]),
            _ => {
                cx.unmodeled();
                0
            }
        }
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        let o = address.wrapping_sub(self.base);
        self.advance(cx);
        if size != 4 || o & 3 != 0 {
            cx.unmodeled();
            return;
        }
        let value = value as u32;
        match o {
            0 => {
                self.state.registers.insert(0, value | 1);
                if value & 1 == 0 {
                    self.cancel();
                }
            }
            0x200 => {
                if value > 1 {
                    cx.fail("invalid sensor command reset");
                    return;
                }
                self.cancel();
                self.state.registers.insert(o, 1);
            }
            0x204 => {
                if value == 0 {
                    self.cancel();
                } else if value == 1 {
                    if let Err(e) = self.submit(cx) {
                        cx.fail(e);
                    }
                } else {
                    cx.fail("invalid sensor command start");
                }
            }
            0x4..=0x38 | 0x40..=0x94 | 0xc0..=0x114 | 0x140 | 0x144 => {
                if !self.sensor.image_supported(o) {
                    cx.unmodeled();
                    return;
                }
                self.sensor.image_write(cx, o, value)
            }
            // Sensor control words outside the command descriptor: the
            // pending command was captured at submission and does not use
            // them, so they stay writable while it runs.
            0x180..=0x1b0 => {
                self.state.registers.insert(o, value);
                self.sensor.tg_write(o, value);
            }
            0x208 | 0x20c | 0x210 | 0x214 | 0x218 | 0x21c => {
                if self.state.pending.is_some() {
                    cx.fail(format!("reconfiguring active sensor command (+{o:#x} = {value:#x})"));
                } else {
                    self.state.registers.insert(o, value);
                }
            }
            _ => cx.unmodeled(),
        }
    }
    fn event(&mut self, cx: &mut Cx<'_>, token: u64) {
        if token == self.state.generation {
            self.advance(cx);
        }
    }
    fn next_deadline(&self, _: u64) -> Option<u64> {
        self.state.deadline
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        if !self.state.deadline.is_some_and(|at| at <= cx.now()) {
            return;
        }
        self.state.deadline = None;
        let Some(packets) = self.state.pending.take() else {
            return;
        };
        let mut sensor = self.sensor.0.lock().unwrap();
        for packet in packets {
            let address = u16::from_be_bytes([packet[1], packet[2]]);
            sensor.commands += 1;
            if packet[0] == 0x81 {
                sensor.registers.insert(address, packet[3]);
                sensor.writes += 1;
            } else {
                let (gate, mask) = self.sensor.2;
                if sensor.registers.get(&gate).copied().unwrap_or(0) & mask == 0 {
                    cx.fail(format!(
                        "sensor read while register {gate:#x} gate is closed"
                    ));
                    return;
                }
                for (i, response) in self.state.response.iter_mut().enumerate() {
                    *response = u32::from(
                        sensor
                            .registers
                            .get(&address.wrapping_add(i as u16))
                            .copied()
                            .unwrap_or(0),
                    ) << 16;
                }
                sensor.reads += 1;
            }
        }
        self.state.registers.insert(0x204, 0);
    }
    fn save(&self) -> Value {
        serde_json::json!({"command": self.state, "sensor": *self.sensor.0.lock().unwrap(), "command_latency_ticks": self.latency_ticks})
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value["command"].clone())
            .map_err(|e| format!("sensor command checkpoint: {e}"))?;
        let sensor = serde_json::from_value(value["sensor"].clone())
            .map_err(|e| format!("sensor checkpoint: {e}"))?;
        let latency = value["command_latency_ticks"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or("missing sensor command latency")?;
        if state.pending.is_some() != state.deadline.is_some() {
            return Err("sensor pending/deadline disagree".into());
        }
        self.state = state;
        *self.sensor.0.lock().unwrap() = sensor;
        self.latency_ticks = latency;
        for o in TG_WORDS {
            self.sensor.tg_write(o, self.reg(o));
        }
        // Mirroring restored words is not a generator start.
        self.sensor.take_tg_start();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::recording::RecordingHost;
    #[test]
    fn timing_input_causes_ack_without_granting_raw_dma_support() {
        let sensor = Sensor::new(
            Some(ImageRoutes {
                dma: None,
                timing: [100, 102],
                latch_unrouted: false,
                start_pulse: None,
            }),
            (0x51, 8),
        );
        let mut command = Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
        let mut host = RecordingHost::default();
        command.write(&mut Cx::new(&mut host, 3, 0xabad, 0), 0xfff6c140, 4, 15);
        sensor.timing(&mut Cx::new(&mut host, 0, 0, 0), true);
        assert!(host.irqs.iter().rev().find(|(i, _)| *i == 100).unwrap().1);
        assert_eq!(
            command.read(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c144, 4),
            3
        );
        command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c144, 4, 3);
        assert!(!host.irqs.iter().rev().find(|(i, _)| *i == 100).unwrap().1);
        sensor.timing(&mut Cx::new(&mut host, 3, 0x1234, 0), false);
        assert!(host.irqs.iter().rev().find(|(i, _)| *i == 102).unwrap().1);
        assert_eq!(
            command.read(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c144, 4),
            12
        );
        let saved = command.save();
        {
            let mut cx = Cx::new(&mut host, 0, 0, 0);
            command.write(&mut cx, 0xfff6c040, 4, 1);
            assert!(cx.is_unmodeled());
        }
        assert_eq!(command.save(), saved);
        assert!(host.ram.is_empty());
    }
    #[test]
    fn start_pulse_rises_and_falls_once_per_enabled_frame_start() {
        let sensor = Sensor::new(
            Some(ImageRoutes {
                dma: None,
                timing: [102, 100],
                latch_unrouted: true,
                start_pulse: Some((2, 101)),
            }),
            (0x51, 8),
        );
        let mut command = Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
        let mut host = RecordingHost::default();
        let pulses = |h: &RecordingHost| h.irqs.iter().filter(|(i, l)| *i == 101 && *l).count();
        // Cause 1 disabled: no pulse.
        command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c140, 4, 1);
        sensor.timing(&mut Cx::new(&mut host, 0, 0, 0), true);
        assert_eq!(pulses(&host), 0);
        // X-T3 enables causes 0..2: one rise and fall per frame start, none at the end.
        command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c140, 4, 7);
        sensor.timing(&mut Cx::new(&mut host, 0, 0, 0), true);
        sensor.timing(&mut Cx::new(&mut host, 0, 0, 0), false);
        sensor.timing(&mut Cx::new(&mut host, 0, 0, 0), true);
        assert_eq!(pulses(&host), 2);
        assert_eq!(host.irqs.iter().rev().find(|(i, _)| *i == 101), Some(&(101, false)));
    }
    #[test]
    fn packet_lists_read_gate_cancel_and_exact_deadline_are_program_independent() {
        fn experiment(cpu: usize, pc: u64, late: bool) -> Value {
            let sensor = Sensor::new(None, (0x51, 8));
            let mut command = Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
            let mut host = RecordingHost {
                ram_base: 0x1000,
                ram: vec![
                    0x81, 0, 0x51, 8, 0, 0, 0, 0, 0x81, 0, 0, 4, 0, 0, 0, 0, 0x81, 1, 0, 0xa5, 0,
                    0, 0, 0,
                ],
                ..Default::default()
            };
            for (offset, value) in [(0x208, 0x10403), (0x210, 0x1000), (0x218, 8), (0x204, 1)] {
                let mut cx = Cx::new(&mut host, cpu, pc, 0);
                command.write(&mut cx, 0xfff6c000 + offset, 4, value);
                assert!(cx.refusal().is_none());
            }
            let saved = command.save();
            assert_eq!(command.next_deadline(0), Some(51));
            host.now = 50;
            command.event(&mut Cx::new(&mut host, cpu, pc, 0), 1);
            assert_eq!(command.save(), saved);
            assert!(!sensor.streaming());
            // The transfer owns the submitted DMA packet bytes.
            host.ram.fill(0xff);
            host.now = if late { 1000 } else { 51 };
            command.event(&mut Cx::new(&mut host, cpu, pc, 0), 1);
            assert!(sensor.streaming());
            assert_eq!(sensor.0.lock().unwrap().commands, 3);
            host.ram[..8].copy_from_slice(&[0x82, 1, 0, 0, 0, 0, 0, 0]);
            for (offset, value) in [(0x208, 0x10401), (0x204, 1)] {
                command.write(
                    &mut Cx::new(&mut host, cpu, pc, 0),
                    0xfff6c000 + offset,
                    4,
                    value,
                );
            }
            host.now += 17;
            command.advance(&mut Cx::new(&mut host, cpu, pc, 0));
            assert_eq!(
                command.read(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff6c240, 4),
                0xa50000
            );
            // Cancel a pending write; its old event cannot apply it.
            host.ram[..8].copy_from_slice(&[0x81, 1, 0, 0x99, 0, 0, 0, 0]);
            command.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff6c204, 4, 1);
            command.write(&mut Cx::new(&mut host, cpu, pc, 0), 0xfff6c204, 4, 0);
            let saved = command.save();
            host.now += 17;
            command.event(&mut Cx::new(&mut host, cpu, pc, 0), 3);
            assert_eq!(command.save(), saved);
            assert_eq!(command.next_deadline(host.now), None);
            saved
        }
        let expected = experiment(0, 0, false);
        assert_eq!(experiment(3, 0xabad, true), expected);
    }
    #[test]
    fn control_words_stay_writable_while_a_command_runs_but_its_descriptor_does_not() {
        let sensor = Sensor::new(None, (0x51, 8));
        let mut command = Command::new(0xfff6c000, sensor, 17).unwrap();
        let mut host = RecordingHost {
            ram_base: 0x1000,
            ram: vec![0x81, 0, 0x51, 8, 0, 0, 0, 0],
            ..Default::default()
        };
        for (offset, value) in [(0x208, 0x10401), (0x210, 0x1000), (0x218, 8), (0x204, 1)] {
            command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c000 + offset, 4, value);
        }
        let mut cx = Cx::new(&mut host, 0, 0, 0);
        command.write(&mut cx, 0xfff6c184, 4, 0);
        assert!(cx.refusal().is_none());
        assert_eq!(command.read(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c184, 4), 0);
        let mut cx = Cx::new(&mut host, 0, 0, 0);
        command.write(&mut cx, 0xfff6c208, 4, 0x10402);
        assert!(cx.refusal().is_some());
    }
    #[test]
    fn dma_command_waits_for_guest_time_and_restores_pending_transport() {
        let sensor = Sensor::new(None, (0x59, 4));
        let mut command = Command::new(0xff948000, sensor.clone(), 100_000).unwrap();
        let mut host = RecordingHost {
            ram_base: 0x1000,
            ram: vec![0x81, 0, 0x59, 4, 0, 0, 0, 0],
            ..Default::default()
        };
        {
            let mut cx = Cx::new(&mut host, 3, 0xabad, 0);
            for (o, v) in [(0x208, 0x10401), (0x210, 0x1000), (0x218, 8), (0x204, 1)] {
                command.write(&mut cx, 0xff948000 + o, 4, v);
            }
            assert!(cx.refusal().is_none());
        }
        let saved = command.save();
        let token = saved["command"]["generation"].as_u64().unwrap();
        host.now = 99_999;
        command.event(&mut Cx::new(&mut host, 0, 0, 0), token);
        assert_eq!(command.save(), saved);
        assert_eq!(sensor.0.lock().unwrap().writes, 0);
        command.restore(&saved).unwrap();
        host.now = 100_000;
        command.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert_eq!(sensor.0.lock().unwrap().registers[&0x59], 4);
        assert_eq!(command.next_deadline(host.now), None);
        host.ram[0] = 0x82;
        command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xff948204, 4, 1);
        host.now = 200_000;
        command.advance(&mut Cx::new(&mut host, 0, 0, 0));
        assert_eq!(
            command.read(&mut Cx::new(&mut host, 0, 0, 0), 0xff948240, 4),
            4 << 16
        );
        assert_eq!(sensor.0.lock().unwrap().reads, 1);
    }
    #[test]
    fn timing_generator_words_give_clocks_and_whole_ticks() {
        let v79 = [1, 0x0001_1b30, 0x0002_1c08, 0x0003_01a2, 0x0003_01a4];
        assert_eq!(TgCadence::clocks(v79), Some(3_000_000));
        assert_eq!(TgCadence::clocks([1, 2104, 2224, 540, 542]), Some(1_201_200));
        assert_eq!(TgCadence::clocks([0, 2104, 2224, 540, 542]), None);
        assert_eq!(TgCadence::clocks([1, 2104, 0, 540, 542]), None);
        assert_eq!(TgCadence::clocks([1, 2104, 2224, 0, 542]), None);
        // More A lines than lines: all lines are A lines.
        assert_eq!(TgCadence::clocks([1, 9000, 2224, 540, 542]), Some(540 * 2224));
        assert!(TgCadence::new(0, 54_000_000).is_err());
        assert!(TgCadence::new(72_000_000, 0).is_err());
        let c = TgCadence::new(72_000_000, 54_000_000).unwrap();
        assert_eq!(c.ticks(3_000_000, 0), Some((2_250_000, 0)));
        assert_eq!(c.ticks(1_201_200, 0), Some((900_900, 0)));
        // 3 clocks = 2.25 ticks: the quarter ticks add up, none is lost.
        let mut rest = 0;
        let mut periods = vec![];
        for _ in 0..4 {
            let (p, r) = c.ticks(3, rest).unwrap();
            periods.push(p);
            rest = r;
        }
        assert_eq!((periods, rest), (vec![2, 2, 2, 3], 0));
    }
    #[test]
    fn timing_generator_words_are_mirrored_and_restored_and_timed_only_when_enabled() {
        let sensor = Sensor::new(None, (0x51, 8));
        let mut command = Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
        let mut host = RecordingHost::default();
        let words = [1u32, 6960, 7176, 418, 420];
        for (o, w) in TG_WORDS.into_iter().zip(words) {
            command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c000 + o, 4, u64::from(w));
        }
        assert_eq!(sensor.tg_cadence(), None);
        assert_eq!(sensor.tg_words(), words);
        // A checkpoint taken with the law off restores into a sensor with it on.
        let saved = command.save();
        let cadence = TgCadence::new(72_000_000, 54_000_000).unwrap();
        let on = Sensor::new(None, (0x51, 8));
        on.use_tg_cadence(cadence);
        assert_eq!(on.tg_cadence(), Some((cadence, None)));
        let mut back = Command::new(0xfff6c000, on.clone(), 17).unwrap();
        back.restore(&saved).unwrap();
        assert_eq!(on.tg_cadence(), Some((cadence, Some(3_000_000))));
        assert_eq!(back.save(), saved);
        back.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c184, 4, 0);
        assert_eq!(on.tg_cadence(), Some((cadence, None)));
    }
    #[test]
    fn a_generator_stop_is_seen_only_with_the_cadence_law_on() {
        let mut host = RecordingHost::default();
        let words = [1u32, 6960, 7176, 418, 420];
        let run = |command: &mut Command, host: &mut RecordingHost, on: bool| {
            command.write(&mut Cx::new(host, 0, 0, 0), 0xfff6c184, 4, u64::from(on));
        };
        // Law off: never stopped, no stop to take.
        let off = Sensor::new(None, (0x51, 8));
        let mut command = Command::new(0xfff6c000, off.clone(), 17).unwrap();
        for (o, w) in TG_WORDS.into_iter().zip(words) {
            command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c000 + o, 4, u64::from(w));
        }
        run(&mut command, &mut host, false);
        assert!(!off.tg_stopped() && !off.tg_stop_pending() && !off.take_tg_stop());
        // Law on: never started counts as stopped; a restore of running
        // words is not a stop.
        let saved = {
            run(&mut command, &mut host, true);
            command.save()
        };
        let on = Sensor::new(None, (0x51, 8));
        on.use_tg_cadence(TgCadence::new(72_000_000, 54_000_000).unwrap());
        assert!(on.tg_stopped());
        let mut back = Command::new(0xfff6c000, on.clone(), 17).unwrap();
        back.restore(&saved).unwrap();
        assert!(!on.tg_stopped() && !on.tg_stop_pending());
        // 1 -> 0 is a stop, kept until taken, even across a restart.
        run(&mut back, &mut host, false);
        assert!(on.tg_stopped() && on.tg_stop_pending());
        run(&mut back, &mut host, true);
        assert!(!on.tg_stopped() && on.tg_stop_pending());
        assert!(on.take_tg_stop());
        assert!(!on.take_tg_stop());
        // 0 -> 0 is not a new stop.
        run(&mut back, &mut host, false);
        assert!(on.take_tg_stop());
        run(&mut back, &mut host, false);
        assert!(!on.tg_stop_pending());
    }
    #[test]
    fn the_gain_law_follows_the_spi_code_and_is_neutral_until_written() {
        let law = SensorExposure::new(0, None).unwrap();
        let mut registers = BTreeMap::from([(0, 1)]);
        assert_eq!(SensorExposure::code(&registers), None);
        assert_eq!(law.factor(&registers), 1.0);
        // 0xefc = 3836 is the firmware cap, 24 dB.
        registers.insert(0x30, 0xfc);
        registers.insert(0x31, 0xfe);
        assert_eq!(SensorExposure::code(&registers), Some(0xefc));
        assert!((law.factor(&registers) - 4095.0 / 259.0).abs() < 1e-12);
        assert!((20.0 * law.factor(&registers).log10() - 24.0).abs() < 0.03);
        // Codes above the cap scale like the cap.
        registers.insert(0x31, 0x0f);
        assert_eq!(law.factor(&registers), 4095.0 / 259.0);
        // Normalised by the base code: the base code itself is 1.
        let base = SensorExposure::new(2175, None).unwrap();
        registers.insert(0x30, 0x7f);
        registers.insert(0x31, 0x08);
        assert_eq!(base.factor(&registers), 1.0);
        registers.insert(0x30, 0);
        registers.insert(0x31, 0);
        assert!((base.factor(&registers) - 1920.0 / 4095.0).abs() < 1e-12);
        assert!(SensorExposure::new(0xefd, None).is_err());
        assert!(SensorExposure::new(0, Some(0.0)).is_err());
        assert!(SensorExposure::new(0, Some(f64::NAN)).is_err());
    }
    #[test]
    fn the_high_conversion_gain_bit_multiplies_only_when_declared() {
        // T44-ex160 / ex640: ISO 160 = code 1965, bit clear; ISO 640 = code
        // 2498, bit set. Two stops apart only with a ratio of about 3.
        let plain = SensorExposure::new(1965, None).unwrap();
        let dual = SensorExposure::new(1965, Some(3.0)).unwrap();
        let mut registers = BTreeMap::from([(0x30, 0xc2), (0x31, 0x09), (0x2f, 0x01)]);
        let iso640 = dual.factor(&registers);
        assert!((iso640 - 4.0).abs() < 0.01);
        assert!((plain.factor(&registers) - iso640 / 3.0).abs() < 1e-12);
        // The bit alone, before any gain code, stays neutral.
        assert_eq!(dual.factor(&BTreeMap::from([(0x2f, 0x11)])), 1.0);
        registers.insert(0x2f, 0x10);
        assert_eq!(dual.factor(&registers), plain.factor(&registers));
        registers.extend([(0x30, 0xad), (0x31, 0x07), (0x2f, 0x00)]);
        assert_eq!(dual.factor(&registers), 1.0);
    }
    #[test]
    fn the_gain_law_needs_the_raw_scene_and_scales_the_painted_raw() {
        let sensor = Sensor::new(None, (0x51, 8));
        let law = SensorExposure::new(0, None).unwrap();
        assert!(sensor.use_sensor_exposure(law).is_err());
        let scene = super::super::statistics::Statistics::new(0x100000, 0x7000).unwrap();
        sensor.paint_scene(scene.clone());
        sensor.use_sensor_exposure(law).unwrap();
        assert_eq!(sensor.exposure_factor(), 1.0);
        sensor.0.lock().unwrap().registers.insert(0x30, 0xff);
        sensor.0.lock().unwrap().registers.insert(0x31, 0x07);
        // Code 2047: twice the signal (4095 / 2048).
        let factor = sensor.exposure_factor();
        assert!((factor - 4095.0 / 2048.0).abs() < 1e-12);
        let at = |bytes: &[u8]| u16::from_le_bytes([bytes[0], bytes[1]]);
        // Default scene: linear 2048 << 2 + 1024.
        assert_eq!(at(&scene.paint_raw(6, 6)), 2048 * 4 + 1024);
        let scaled = (f64::from(2048u32 * 4) * factor).round() as u16 + 1024;
        assert_eq!(at(&scene.paint_raw_scaled(6, 6, factor)), scaled.min(16383));
    }
    #[test]
    fn the_shutter_law_inverts_the_firmware_shs_and_is_neutral_until_written() {
        // T60c, binary fe4fde9b: C = 0x20c5920(0) = 6 at HA 418.
        let shutter = SensorShutter::new(11450.75, 6, 0xffdc, 2872).unwrap();
        let mut registers = BTreeMap::new();
        // No SHS byte: neutral, whatever VMAX says.
        assert_eq!(shutter.lines(&registers, 0xffdc), None);
        assert_eq!(shutter.factor(&registers, 0xffdc), 1.0);
        registers.insert(0x21, 3);
        assert_eq!(shutter.factor(&registers, 3156), 1.0);
        // Live view 0x8c: n 3, SHS 19, VMAX word 0xffdc (frame of 2872
        // lines): 11450.75 lines, P8 / 8 = 91608 / 8 = 11451 (within 1 line).
        registers.extend([(0x22, 0), (0x25, 19), (0x26, 0)]);
        let live = shutter.lines(&registers, 0xffdc).unwrap();
        assert_eq!(live, 4.0 * 2872.0 + 0.75 - 38.0);
        assert!((live - 91608.0 / 8.0).abs() <= 1.0);
        assert_eq!(shutter.factor(&registers, 0xffdc), 1.0);
        // After S1, 0x8b: n 3, SHS 3, VMAX 3156: P8 / 8 = 100944 / 8 = 12618.
        registers.insert(0x25, 3);
        let s1 = shutter.lines(&registers, 3156).unwrap();
        assert_eq!(s1, 4.0 * 3156.0 + 0.75 - 6.0);
        assert!((s1 - 100944.0 / 8.0).abs() <= 1.0);
        // Still capture tables 0x1c: SHS 0x13 and n 0 as the firmware leaves
        // them, VMAX 6028: the law reads them as they are.
        registers.extend([(0x21, 0), (0x25, 0x13)]);
        assert_eq!(shutter.lines(&registers, 6028), Some(6028.0 + 0.75 - 38.0));
        // Twice the SHS lines off: the signal follows the lines.
        let law = SensorShutter::new(1000.0, 0, 0xffdc, 2872).unwrap();
        registers.insert(0x25, 0);
        let full = law.factor(&registers, 2000);
        registers.insert(0x25, 250);
        assert_eq!(law.factor(&registers, 2000), full * 0.75);
        // High byte of SHS counts; the lines never go below an eighth.
        registers.extend([(0x25, 0), (0x26, 0x10)]);
        assert_eq!(law.lines(&registers, 2000), Some(SHUTTER_LINES_MIN));
        // VMAX 0 (never written): neutral.
        assert_eq!(law.factor(&registers, 0), 1.0);
        assert!(SensorShutter::new(0.0, 6, 0xffdc, 2872).is_err());
        assert!(SensorShutter::new(f64::NAN, 6, 0xffdc, 2872).is_err());
        assert!(SensorShutter::new(1.0, 6, 0xffdc, 0).is_err());
    }
    #[test]
    fn without_the_shutter_the_law_is_the_gain_law_bit_for_bit() {
        let gain = SensorExposure::new(1965, Some(3.0)).unwrap();
        let both = gain.with_shutter(SensorShutter::new(11450.75, 6, 0xffdc, 2872).unwrap()).unwrap();
        let mut registers = BTreeMap::from([(0x30, 0xc2), (0x31, 0x09), (0x2f, 0x01)]);
        for vmax in [0, 3156, 0xffdc] {
            assert_eq!(gain.factor_at(&registers, vmax).to_bits(), gain.factor(&registers).to_bits());
        }
        registers.extend([(0x21, 3), (0x25, 19)]);
        assert_eq!(both.factor_at(&registers, 0xffdc), gain.factor(&registers));
        registers.insert(0x25, 3);
        let ratio = both.factor_at(&registers, 3156) / gain.factor(&registers);
        assert!((ratio - 12618.75 / 11450.75).abs() < 1e-12);
        // The definition field: absent = None, unknown fields refused.
        let json = |text: &str| serde_json::from_str::<SensorExposure>(text);
        assert_eq!(json(r#"{"gain_ref": 1965}"#).unwrap().shutter, None);
        let with = json(
            r#"{"gain_ref": 1965, "shutter": {"ref_lines": 11450.75, "offset_eighths": 6,
                "free_run_vmax": 65500, "free_run_lines": 2872}}"#,
        )
        .unwrap();
        assert_eq!(with.shutter, both.shutter);
        assert!(json(r#"{"gain_ref": 1965, "shutter": {"ref_lines": 1.0}}"#).is_err());
        assert!(gain.with_shutter(SensorShutter { ref_lines: -1.0, ..both.shutter.unwrap() }).is_err());
    }
    #[test]
    fn the_shutter_law_follows_the_vmax_word_the_firmware_writes() {
        let sensor = Sensor::new(None, (0x51, 8));
        let scene = super::super::statistics::Statistics::new(0x100000, 0x7000).unwrap();
        sensor.paint_scene(scene.clone());
        let law = SensorExposure::new(0, None)
            .unwrap()
            .with_shutter(SensorShutter::new(1000.0, 0, 0xffdc, 2000).unwrap())
            .unwrap();
        sensor.use_sensor_exposure(law).unwrap();
        sensor.0.lock().unwrap().registers.extend([(0x30, 0), (0x31, 0), (0x25, 0)]);
        // No VMAX word yet: gain alone (code 0 against code 0 = 1).
        assert_eq!(sensor.exposure_factor(), 1.0);
        let mut host = RecordingHost::default();
        let mut command = Command::new(0xfff6c000, sensor.clone(), 17).unwrap();
        command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c194, 4, 0x2_0000 | 500);
        assert_eq!(sensor.exposure_factor(), 0.5);
        command.write(&mut Cx::new(&mut host, 0, 0, 0), 0xfff6c194, 4, 0x2_ffdc);
        assert_eq!(sensor.exposure_factor(), 2.0);
        let at = |bytes: &[u8]| u16::from_le_bytes([bytes[0], bytes[1]]);
        // The painted RAW scales by that factor.
        let scaled = (f64::from(2048u32 * 4) * 2.0).round() as u16 + 1024;
        assert_eq!(at(&scene.paint_raw_scaled(6, 6, sensor.exposure_factor())), scaled.min(16383));
    }
    #[test]
    fn a_restart_delay_needs_the_cadence_law_and_marks_only_real_starts() {
        let mut host = RecordingHost::default();
        let run = |command: &mut Command, host: &mut RecordingHost, on: bool| {
            command.write(&mut Cx::new(host, 0, 0, 0), 0xfff6c184, 4, u64::from(on));
        };
        // Law off: refused, nothing recorded.
        let off = Sensor::new(None, (0x51, 8));
        assert!(off.set_tg_restart_delay(3).is_err());
        assert_eq!(off.tg_restart_delay(), 0);
        let mut command = Command::new(0xfff6c000, off.clone(), 17).unwrap();
        run(&mut command, &mut host, true);
        assert!(!off.tg_start_pending());
        // Law on, no delay: a start is not recorded (identity).
        let on = Sensor::new(None, (0x51, 8));
        on.use_tg_cadence(TgCadence::new(72_000_000, 54_000_000).unwrap());
        let mut command = Command::new(0xfff6c000, on.clone(), 17).unwrap();
        run(&mut command, &mut host, true);
        assert!(!on.tg_start_pending());
        // With a delay: only 0 -> 1 is a start, kept until taken.
        on.set_tg_restart_delay(3).unwrap();
        assert_eq!(on.tg_restart_delay(), 3);
        run(&mut command, &mut host, true);
        assert!(!on.tg_start_pending());
        run(&mut command, &mut host, false);
        assert!(!on.tg_start_pending());
        run(&mut command, &mut host, true);
        assert!(on.tg_start_pending());
        assert!(on.take_tg_start());
        assert!(!on.take_tg_start());
        // Turning the delay off drops a start not taken yet.
        run(&mut command, &mut host, false);
        run(&mut command, &mut host, true);
        on.set_tg_restart_delay(0).unwrap();
        assert!(!on.tg_start_pending());
        // Restoring running words is not a start.
        on.set_tg_restart_delay(2).unwrap();
        let saved = command.save();
        let back_sensor = Sensor::new(None, (0x51, 8));
        back_sensor.use_tg_cadence(TgCadence::new(72_000_000, 54_000_000).unwrap());
        back_sensor.set_tg_restart_delay(2).unwrap();
        let mut back = Command::new(0xfff6c000, back_sensor.clone(), 17).unwrap();
        back.restore(&saved).unwrap();
        assert!(!back_sensor.tg_stopped() && !back_sensor.tg_start_pending());
    }
}
