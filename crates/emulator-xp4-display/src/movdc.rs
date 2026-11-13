//! XP4 MOVDC output DMA. The packed422 source and clock are explicit scenario
//! inputs, not an ISP implementation or measured silicon timing.
use fujimod_emulator_imaging::frame;
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Default, Serialize, Deserialize)]
struct State {
    regs: Vec<u32>,
    reset_deadline: Option<u64>,
    frame_deadline: Option<u64>,
    pending: bool,
    frames: u64,
}
/// A computed processed-frame input: `(width, height)` -> packed Y0,Y1,Cb,Cr
/// rows of `width` * 2 bytes. Construction input, not state.
pub type Source = std::sync::Arc<dyn Fn(u32, u32) -> Vec<u8> + Send + Sync>;
pub struct Movdc {
    base: u64,
    irq: u32,
    reset_ticks: u64,
    period_ticks: u64,
    luma: u8,
    state: State,
    /// See [`Movdc::latch_configuration`]; construction input, not state.
    latch: bool,
    /// See [`Movdc::with_source`]; construction input, not state.
    source: Option<Source>,
    /// See [`Movdc::accept_yuv420`]; construction input, not state.
    yuv420: bool,
}
impl Movdc {
    pub fn new(
        base: u64,
        irq: u32,
        reset_ticks: u64,
        period_ticks: u64,
        luma: u8,
    ) -> Result<Self, String> {
        if ![0xfff72000, 0xfff73000].contains(&base)
            || !(32..512).contains(&irq)
            || reset_ticks == 0
            || period_ticks <= reset_ticks
            || !(16..=235).contains(&luma)
        {
            return Err("Invalid XP4 MOVDC wiring or source clock/luma".into());
        }
        Ok(Self {
            base,
            irq,
            reset_ticks,
            period_ticks,
            luma,
            state: State {
                regs: vec![0; 0x10c / 4],
                ..State::default()
            },
            latch: false,
            source: None,
            yuv420: false,
        })
    }
    fn r(&self, offset: usize) -> u32 {
        self.state.regs[offset / 4]
    }
    fn active(&self) -> bool {
        self.state.reset_deadline.is_none() && self.r(4) == 1 && self.r(0xd4) & 0x10000000 != 0
    }
    fn irq(&self, cx: &mut Cx<'_>) {
        cx.set_irq(
            self.irq,
            self.state.pending && self.r(0xd4) & 0x10000000 != 0,
        );
    }
    fn arm(&mut self, cx: &mut Cx<'_>) {
        if !self.active() {
            self.state.frame_deadline = None;
        } else if self.state.frame_deadline.is_none() {
            self.state.frame_deadline = cx.now().checked_add(self.period_ticks);
            if self.state.frame_deadline.is_none() {
                cx.fail("XP4 MOVDC deadline overflow");
            }
        }
        self.irq(cx);
    }
    fn produce(&mut self, cx: &mut Cx<'_>) -> Result<(), String> {
        // 011117ac selects +d4[18:16] by divisibility of the output width
        // by (8 << exponent) for this format. 01f31664/01f31aa8 select
        // +d8[17:16] with table 0140a944 = [3,3,3,2,1,0]. These are
        // horizontal transfer controls, not alternate pixel formats.
        // +d8[1:0] is 2 in live view and 3 when X-T3 5.11 shows an enlarged
        // crop (544x368 source window, rear dial at 1617M in run D1/T41).
        // The source window stays unmodeled: mode 3 paints the same declared
        // processed frame at the programmed output size as mode 2.
        // +08[1:0] is the output format written by 01055d74 (0x100 | format).
        // 011117ac's divisor base is 01111770's per-format factor
        // [8, 6, 16, 4] beside 01111818's row units [0x100, 0xc0, 0x200,
        // 0x80]: 2, 1.5, 4 and 1 bytes per pixel. Format 1 (X-T3 5.11 FHD
        // movie recording, 1920x1080) is read as 4:2:0 at 1.5 bytes per
        // pixel; it is accepted only with `accept_yuv420`.
        let exponent = (self.r(0xd4) >> 16) & 7;
        let selector = [3, 3, 3, 2, 1, 0].get(exponent as usize);
        let width = (self.r(0x18) & 0x7ff) + 1;
        let yuv420 = self.yuv420 && self.r(8) == 0x101;
        let factor = if yuv420 { 6 } else { 8 };
        if !(self.r(8) == 0x100 || yuv420)
            || selector.is_none_or(|selector| {
                !matches!(self.r(0xd8) ^ (0x10000000 | selector << 16), 2 | 3)
            })
            || self.r(0xd4) & !0x10070000 != 0
            || width % (factor << exponent) != 0
            || self.r(0x18) & !0x07ff07ff != 0
            || self.r(0xc0) > 3
            || self.r(0xc8) != 0
            || self.r(0xcc) != 0
        {
            return Err("Unsupported XP4 MOVDC output mode/size/address".into());
        }
        let size = self.r(0x18);
        let address = (u64::from(self.r(0xc0)) << 32) | u64::from(self.r(0xc4));
        if address == 0 || address & 7 != 0 {
            return Err("XP4 MOVDC needs a nonzero aligned DMA destination".into());
        }
        if yuv420 {
            return self.luma_plane(cx, address, width, (size >> 16) + 1);
        }
        match &self.source {
            Some(source) => {
                let height = (size >> 16) + 1;
                frame::copy(cx, address, width, height, self.r(0xd0), &source(width, height))?;
            }
            None => frame::paint(
                cx,
                address,
                width,
                (size >> 16) + 1,
                self.r(0xd0),
                self.luma,
            )?,
        }
        self.state.frames += 1;
        self.state.pending = true;
        self.irq(cx);
        Ok(())
    }
    /// Format 1: only the luma plane, `width` bytes per row every +d0 bytes,
    /// from the source's Y0,Y1 samples or the declared fixed luma. Where the
    /// chroma plane goes (no second address among the modeled words) is not
    /// known, so chroma is not written. Completion, interrupt and timing are
    /// those of the packed output.
    fn luma_plane(&mut self, cx: &mut Cx<'_>, address: u64, width: u32, height: u32) -> Result<(), String> {
        let stride = self.r(0xd0);
        if width & 1 != 0
            || width > 4096
            || height > 4096
            || stride < width
            || stride & 3 != 0
            || u64::from(stride) * u64::from(height) > 32 * 1024 * 1024
        {
            return Err("YUV picture geometry exceeds modeled bounds".into());
        }
        address
            .checked_add(u64::from(stride) * u64::from(height))
            .ok_or("YUV DMA address overflow")?;
        let plane: Vec<u8> = match &self.source {
            Some(source) => {
                let packed = source(width, height);
                if packed.len() as u64 != u64::from(width) * 2 * u64::from(height) {
                    return Err("YUV picture geometry exceeds modeled bounds".into());
                }
                packed.chunks_exact(4).flat_map(|p| [p[0], p[1]]).collect()
            }
            None => vec![self.luma; width as usize * height as usize],
        };
        for (y, row) in plane.chunks_exact(width as usize).enumerate() {
            cx.ram_write(address + y as u64 * u64::from(stride), row)?;
        }
        self.state.frames += 1;
        self.state.pending = true;
        self.irq(cx);
        Ok(())
    }
    fn known(offset: u64) -> bool {
        // This is the DMA front end with an explicit processed-frame input.
        // ISP coefficients, crop/filter controls, watermark fields and +e0
        // counters stay unmodeled and in the discovery ledger.
        matches!(
            offset,
            0 | 4 | 8 | 0x18 | 0xc0 | 0xc4 | 0xc8 | 0xcc | 0xd0 | 0xd4 | 0xd8 | 0xdc
        )
    }
    /// Configuration words outside the modeled front end (ISP coefficients,
    /// crop/filter and watermark fields X-T3 5.11 programs when live view
    /// starts): plain latches that change nothing in the produced frames.
    /// The +e0 counters stay unmodeled.
    fn latch(&self, offset: u64) -> bool {
        self.latch && offset & 3 == 0 && offset < 0x10c && offset != 0xe0 && !Self::known(offset)
    }
    /// Latch the configuration words outside the modeled front end instead of
    /// refusing them. The selected output and ISP modes are then not
    /// emulated: frames keep the declared processed-frame input.
    pub fn latch_configuration(mut self) -> Self {
        self.latch = true;
        self
    }
    /// Produce each frame from `source` at the programmed output size instead
    /// of the declared fixed luma (opt-in). Same registers, interrupt and
    /// timing; only the bytes written change.
    pub fn with_source(mut self, source: Source) -> Self {
        self.source = Some(source);
        self
    }
    /// Accept output format 1 (+08 = 0x101, X-T3 5.11 FHD movie recording)
    /// as 4:2:0 luma only (opt-in; default: refused like formats 2 and 3).
    /// Widths then divide by 6 << exponent, as the firmware chooses them;
    /// format 0 is unchanged. See [`Movdc::luma_plane`].
    pub fn accept_yuv420(mut self) -> Self {
        self.yuv420 = true;
        self
    }
}
impl Device for Movdc {
    fn name(&self) -> &'static str {
        if self.base == 0xfff72000 {
            "xp4-movdc0"
        } else {
            "xp4-movdc1"
        }
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        self.advance(cx);
        let offset = address.wrapping_sub(self.base);
        if size == 4 && self.latch(offset) {
            return u64::from(self.r(offset as usize));
        }
        if size != 4 || !Self::known(offset) {
            cx.unmodeled();
            return 0; // +e0's two 5-bit counters are unresolved.
        }
        match offset {
            0 => u64::from(self.state.reset_deadline.is_none()),
            0xdc => u64::from(self.state.pending) | if self.active() { 0 } else { 0x100 },
            _ => u64::from(self.r(offset as usize)),
        }
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        self.advance(cx);
        let offset = address.wrapping_sub(self.base);
        if size == 4 && self.latch(offset) {
            self.state.regs[offset as usize / 4] = value as u32;
            return;
        }
        if size != 4 || !Self::known(offset) {
            cx.unmodeled();
            return;
        }
        let value = value as u32;
        match offset {
            0 => {
                if value != 0 {
                    cx.unmodeled();
                    return;
                }
                self.state.regs[1] = 0;
                self.state.frame_deadline = None;
                self.state.pending = false;
                self.state.reset_deadline = cx.now().checked_add(self.reset_ticks);
                if self.state.reset_deadline.is_none() {
                    cx.fail("XP4 MOVDC reset deadline overflow");
                }
            }
            0xdc => {
                if value & 1 != 0 {
                    self.state.pending = false;
                }
            }
            _ => {
                if offset == 4 && value > 1 {
                    cx.unmodeled();
                    return;
                }
                if offset == 4 && value == 1 && self.state.reset_deadline.is_some() {
                    cx.fail("XP4 MOVDC started during reset");
                    return;
                }
                self.state.regs[offset as usize / 4] = value;
            }
        }
        self.arm(cx);
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        if self.state.reset_deadline.is_some_and(|at| at <= cx.now()) {
            self.state.reset_deadline = None;
        }
        while let Some(at) = self.state.frame_deadline.filter(|at| *at <= cx.now()) {
            self.state.frame_deadline = None;
            if let Err(error) = self.produce(cx) {
                cx.fail(error);
                return;
            }
            if self.active() {
                self.state.frame_deadline = at.checked_add(self.period_ticks);
                if self.state.frame_deadline.is_none() {
                    cx.fail("XP4 MOVDC deadline overflow");
                    return;
                }
            }
        }
    }
    fn next_deadline(&self, _: u64) -> Option<u64> {
        [self.state.reset_deadline, self.state.frame_deadline]
            .into_iter()
            .flatten()
            .min()
    }
    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap()
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let s: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if s.regs.len() != 0x10c / 4
            || s.regs[1] > 1
            || s.frame_deadline.is_some()
                != (s.regs[1] == 1
                    && s.regs[0xd4 / 4] & 0x10000000 != 0
                    && s.reset_deadline.is_none())
        {
            return Err("Invalid XP4 MOVDC checkpoint".into());
        }
        self.state = s;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::{Host, Record};
    struct Clock {
        now: u64,
        ram: Vec<u8>,
        irqs: Vec<(u64, u32, bool)>,
    }
    impl Host for Clock {
        fn now(&self) -> u64 {
            self.now
        }
        fn schedule(&mut self, _: usize, _: u64, _: u64) {
            unreachable!()
        }
        fn set_irq(&mut self, irq: u32, level: bool) {
            self.irqs.push((self.now, irq, level));
        }
        fn raise_irq(&mut self, _: usize, _: u32) -> Result<(), String> {
            unreachable!()
        }
        fn ram_read(&self, _: u64, _: &mut [u8]) -> Result<(), String> {
            unreachable!()
        }
        fn ram_write(&mut self, address: u64, bytes: &[u8]) -> Result<(), String> {
            self.ram
                .get_mut(address as usize..address as usize + bytes.len())
                .ok_or("test DMA bounds")?
                .copy_from_slice(bytes);
            Ok(())
        }
        fn record(&mut self, _: Record) {}
    }
    fn put(d: &mut Movdc, h: &mut Clock, pc: u64, offset: u64, value: u64) {
        let base = d.base;
        let mut cx = Cx::new(h, 3, pc, 0);
        d.write(&mut cx, base + offset, 4, value);
        assert_eq!(cx.refusal(), None);
    }
    fn setup(pc: u64, mode: u64) -> (Movdc, Clock) {
        let mut d = Movdc::new(0xfff73000, 108, 3, 20, 81).unwrap();
        let mut h = Clock {
            now: 10,
            ram: vec![0xaa; 256],
            irqs: vec![],
        };
        put(&mut d, &mut h, pc, 0, 0);
        assert_eq!(d.next_deadline(10), Some(13));
        h.now = 12;
        let before = d.save();
        d.advance(&mut Cx::new(&mut h, 1, pc, 0));
        assert_eq!(d.save(), before);
        h.now = 13;
        d.advance(&mut Cx::new(&mut h, 0, pc, 0));
        for (offset, value) in [
            (8, 0x100),
            (0x18, 0x1001f),
            (0xc4, 64),
            (0xd0, 80),
            (0xd8, mode),
            (0xd4, 0x10020000),
            (4, 1),
        ] {
            put(&mut d, &mut h, pc, offset, value);
        }
        (d, h)
    }
    fn experiment(pc: u64, early: bool) -> (Value, Vec<u8>, Vec<(u64, u32, bool)>) {
        let (mut d, mut h) = setup(pc, 0x10030002);
        assert_eq!(d.next_deadline(13), Some(33));
        if early {
            for at in 14..33 {
                h.now = at;
                let before = d.save();
                d.advance(&mut Cx::new(&mut h, 2, pc, 0));
                assert_eq!(d.save(), before);
            }
        }
        assert!(h.ram.iter().all(|v| *v == 0xaa));
        h.now = 33;
        d.advance(&mut Cx::new(&mut h, 0, pc, 0));
        assert_eq!(&h.ram[64..128], [81, 81, 128, 128].repeat(16));
        assert_eq!(&h.ram[144..208], &h.ram[64..128]);
        assert_eq!(&h.ram[128..144], &[0xaa; 16]);
        assert!(h.irqs.contains(&(33, 108, true)));
        assert_eq!(d.read(&mut Cx::new(&mut h, 0, pc, 0), 0xfff730dc, 4), 1);
        assert!(d.state.pending); // Status reads do not acknowledge.
        put(&mut d, &mut h, pc, 0xdc, 1);
        assert!(!d.state.pending);
        assert_eq!(d.next_deadline(33), Some(53));
        let before = d.save();
        for _ in 0..100 {
            assert_eq!(d.save(), before);
        }
        let mut restored = Movdc::new(0xfff73000, 108, 3, 20, 81).unwrap();
        restored.restore(&before).unwrap();
        assert_eq!(restored.save(), before);
        put(&mut d, &mut h, pc, 0xd4, 0x20000);
        assert_eq!(d.next_deadline(33), None);
        h.now = 100;
        d.advance(&mut Cx::new(&mut h, 1, pc, 0));
        assert_eq!(d.state.frames, 1);
        assert_eq!(d.read(&mut Cx::new(&mut h, 0, pc, 0), 0xfff730dc, 4), 0x100);
        (d.save(), h.ram, h.irqs)
    }
    #[test]
    fn dma_deadline_irq_ack_stop_and_observations_are_program_independent() {
        assert_eq!(experiment(0x1000, false), experiment(0x987654, true));
    }
    #[test]
    fn a_source_replaces_the_fixed_luma_and_keeps_timing_and_state() {
        let fixed = experiment(0x1000, false);
        let (mut d, mut h) = setup(0x1000, 0x10030002);
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        d = d.with_source(std::sync::Arc::new(move |width, height| {
            seen.lock().unwrap().push((width, height));
            (0..width * 2 * height).map(|i| i as u8).collect()
        }));
        h.now = 33;
        d.advance(&mut Cx::new(&mut h, 0, 0x1000, 0));
        assert_eq!(*asked.lock().unwrap(), [(32, 2)]);
        let picture: Vec<u8> = (0..128u32).map(|i| i as u8).collect();
        assert_eq!(&h.ram[64..128], &picture[..64]);
        assert_eq!(&h.ram[144..208], &picture[64..]);
        assert_eq!(&h.ram[128..144], &[0xaa; 16]);
        assert!(h.irqs.contains(&(33, 108, true)));
        assert_eq!(d.state.frames, 1);
        // Same checkpoint as the fixed source at the same point.
        let (mut f, mut g) = setup(0x1000, 0x10030002);
        g.now = 33;
        f.advance(&mut Cx::new(&mut g, 0, 0x1000, 0));
        assert_eq!(d.save(), f.save());
        assert_eq!(fixed, experiment(0x1000, false));
    }
    #[test]
    fn horizontal_transfer_exponent_and_selector_preserve_packed_rows() {
        for exponent in 0..6 {
            let (mut d, mut h) = setup(0, 0x10030002);
            let width = 8 << exponent;
            let stride = width * 2 + 16;
            h.ram = vec![0xaa; 64 + stride as usize * 2];
            put(&mut d, &mut h, 0, 0x18, 0x10000 | (width - 1));
            put(&mut d, &mut h, 0, 0xd0, stride);
            put(&mut d, &mut h, 0, 0xd4, 0x10000000 | exponent << 16);
            put(
                &mut d,
                &mut h,
                0,
                0xd8,
                0x10000002 | [3, 3, 3, 2, 1, 0][exponent as usize] << 16,
            );
            h.now = 33;
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.advance(&mut cx);
            assert_eq!(cx.refusal(), None);
            let row = [81, 81, 128, 128].repeat(width as usize / 2);
            assert_eq!(&h.ram[64..64 + row.len()], row);
            assert_eq!(
                &h.ram[64 + stride as usize..64 + stride as usize + row.len()],
                row
            );
            assert!(h.ram[64 + row.len()..64 + stride as usize]
                .iter()
                .all(|v| *v == 0xaa));
            assert_eq!(d.state.frames, 1);
        }
        for (offset, value) in [(0xd4, 0x10060000), (0xd4, 0x10040000), (0xd8, 0x10010002)] {
            let (mut d, mut h) = setup(0, 0x10030002);
            put(&mut d, &mut h, 0, offset, value);
            h.now = 33;
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.advance(&mut cx);
            assert!(cx.refusal().is_some());
            assert!(!d.state.pending);
            assert!(h.ram.iter().all(|v| *v == 0xaa));
        }
    }
    #[test]
    fn cropped_mode_three_paints_like_live_view_mode_two() {
        let run = |mode| {
            let (mut d, mut h) = setup(0, mode);
            h.now = 33;
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.advance(&mut cx);
            assert_eq!(cx.refusal(), None);
            assert_eq!(d.state.frames, 1);
            assert!(d.state.pending);
            d.state.regs[0xd8 / 4] = 0;
            (d.save(), h.ram, h.irqs)
        };
        assert_eq!(run(0x10030003), run(0x10030002));
        // Other +d8 bits stay refused, as do modes 0 and 1.
        for mode in [0x10030000, 0x10030001, 0x10030007, 0x10020003, 0x30030003] {
            let (mut d, mut h) = setup(0, mode);
            h.now = 33;
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.advance(&mut cx);
            assert!(cx.refusal().unwrap().contains("output mode"));
            assert_eq!(d.state.frames, 0);
        }
    }
    #[test]
    fn unselected_modes_refuse_before_dma_or_completion() {
        let (mut d, mut h) = setup(0, 0x10030001);
        for offset in [0x20, 0x40, 0xe0, 0xe4, 0x100] {
            let before = d.save();
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.write(&mut cx, 0xfff73000 + offset, 4, 0x1234);
            assert!(cx.is_unmodeled());
            assert_eq!(d.save(), before);
        }
        h.now = 33;
        let mut cx = Cx::new(&mut h, 0, 0, 0);
        d.advance(&mut cx);
        assert!(cx.refusal().unwrap().contains("output mode"));
        assert_eq!(d.state.frames, 0);
        assert!(!d.state.pending);
        assert!(h.ram.iter().all(|v| *v == 0xaa));
    }

    /// Format 1 setup: +08 = 0x101, `width` x 2 rows, exponent `exponent`.
    fn yuv420(d: Movdc, width: u32, exponent: u32, stride: u32) -> (Movdc, Clock) {
        output(d, 0x101, width, exponent, stride)
    }
    fn output(d: Movdc, format: u64, width: u32, exponent: u32, stride: u32) -> (Movdc, Clock) {
        let mut d = d;
        let mut h = Clock {
            now: 10,
            ram: vec![0xaa; 64 + stride as usize * 2 + 64],
            irqs: vec![],
        };
        put(&mut d, &mut h, 0, 0, 0);
        h.now = 13;
        d.advance(&mut Cx::new(&mut h, 1, 0, 0));
        for (offset, value) in [
            (8, format),
            (0x18, 0x10000 | u64::from(width - 1)),
            (0xc4, 64),
            (0xd0, u64::from(stride)),
            (0xd8, 0x10000002 | [3, 3, 3, 2, 1, 0][exponent as usize] << 16),
            (0xd4, 0x10000000 | u64::from(exponent) << 16),
            (4, 1),
        ] {
            put(&mut d, &mut h, 0, offset, value);
        }
        h.now = 33;
        (d, h)
    }
    #[test]
    fn format_one_stays_refused_without_the_option() {
        let (mut d, mut h) = yuv420(Movdc::new(0xfff73000, 108, 3, 20, 81).unwrap(), 192, 5, 200);
        let mut cx = Cx::new(&mut h, 0, 0, 0);
        d.advance(&mut cx);
        assert!(cx.refusal().unwrap().contains("output mode"));
        assert_eq!(d.state.frames, 0);
        assert!(h.ram.iter().all(|v| *v == 0xaa));
    }
    #[test]
    fn format_one_writes_the_luma_plane_only_with_the_firmware_divisor() {
        let new = || Movdc::new(0xfff73000, 108, 3, 20, 81).unwrap().accept_yuv420();
        // 1920 = 10 x (6 << 5): the X-T3 5.11 FHD recording output.
        for (width, exponent) in [(192, 5), (1920, 5), (96, 4), (12, 1)] {
            let stride = (width + 63) & !63;
            let (mut d, mut h) = yuv420(new(), width, exponent, stride);
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.advance(&mut cx);
            assert_eq!(cx.refusal(), None, "{width}");
            let w = width as usize;
            let s = stride as usize;
            assert!(h.ram[64..64 + w].iter().all(|v| *v == 81));
            assert!(h.ram[64 + s..64 + s + w].iter().all(|v| *v == 81));
            assert!(h.ram[64 + w..64 + s].iter().all(|v| *v == 0xaa));
            assert!(h.ram[..64].iter().chain(&h.ram[64 + 2 * s..]).all(|v| *v == 0xaa));
            assert!(h.irqs.contains(&(33, 108, true)));
            assert_eq!(d.state.frames, 1);
            assert!(d.state.pending);
        }
        // A width the factor 6 does not divide stays refused, as do formats 2 and 3.
        let (mut d, mut h) = yuv420(new(), 1920 - 64, 5, 1920);
        let mut cx = Cx::new(&mut h, 0, 0, 0);
        d.advance(&mut cx);
        assert!(cx.refusal().is_some());
        for format in [0x102, 0x103, 0x1] {
            let (mut d, mut h) = output(new(), format, 192, 5, 200);
            let mut cx = Cx::new(&mut h, 0, 0, 0);
            d.advance(&mut cx);
            assert!(cx.refusal().is_some(), "{format:#x}");
            assert_eq!(d.state.frames, 0);
        }
    }
    #[test]
    fn format_one_takes_the_luma_of_the_source() {
        let d = Movdc::new(0xfff73000, 108, 3, 20, 81)
            .unwrap()
            .with_source(std::sync::Arc::new(|width, height| {
                (0..width * 2 * height).map(|i| i as u8).collect()
            }))
            .accept_yuv420();
        let (mut d, mut h) = yuv420(d, 12, 1, 16);
        let mut cx = Cx::new(&mut h, 0, 0, 0);
        d.advance(&mut cx);
        assert_eq!(cx.refusal(), None);
        // Packed Y0,Y1,Cb,Cr: bytes 0,1,4,5,... of each 24-byte row.
        let y: Vec<u8> = (0..48u8).filter(|i| i % 4 < 2).collect();
        assert_eq!(&h.ram[64..76], &y[..12]);
        assert_eq!(&h.ram[80..92], &y[12..]);
        assert_eq!(&h.ram[76..80], &[0xaa; 4]);
    }
    #[test]
    fn the_option_leaves_format_zero_unchanged() {
        let plain = experiment(0x1000, false);
        let (mut d, mut h) = setup(0x1000, 0x10030002);
        d.yuv420 = true;
        h.now = 33;
        d.advance(&mut Cx::new(&mut h, 0, 0x1000, 0));
        let (mut f, mut g) = setup(0x1000, 0x10030002);
        g.now = 33;
        f.advance(&mut Cx::new(&mut g, 0, 0x1000, 0));
        assert_eq!((d.save(), &h.ram, &h.irqs), (f.save(), &g.ram, &g.irqs));
        assert_eq!(plain, experiment(0x1000, false));
    }
}
