//! A register page whose job is acknowledged through a word of another block.
//!
//! X-T3 5.11 playback engine at 0xfa000000 (driver 0x1079240, handler
//! 0x10784c4 on GIC 145, level): +0x00 bit 16 reads set (ready) and writing
//! bit 0 starts a job. After the declared ticks the job ends: the level line
//! rises and bit 0 still reads set. The handler's acknowledge (0x1079280) sets
//! bit 3 of 0xfff4aa10, waits for +0x00 bit 0 to clear and clears bit 3 again;
//! setting the acknowledge bit clears bit 0 and lowers the line. Every other
//! word of the page, and the acknowledge word, read back their last write.
//!
//! X-T3 5.11 playback engines at 0xfff7e000 (driver 0x1091674.., GIC 132) and
//! 0xfff7f000 (0x109250c.., GIC 133) acknowledge inside the page instead: a
//! status word (+0x10c, +0x0e4) reads 0x01000100 once out of reset, gains bit 0
//! when a job ends, and writing 1 clears that bit (handlers 0x10921dc,
//! 0x1092f1c). The `status` option describes that word.
//!
//! With the `resize` option, a finished job also scales a packed 4:2:2
//! picture (Y0 Y1 Cb Cr per pixel pair, the JPEG decoder's output format) from
//! one RAM window to another, as the 0xfff7e000 engine does in playback
//! (driver 0x1091734: sizes w-1 | (h-1) << 16 at +0x0c (source) and +0x18
//! (output); steps at +0x1c / +0x20, 2^(bits 31..16) x (bits 15..0) / 0x1000
//! source pixels per output pixel as in [`crate::development::step`] (X-T3
//! 5.11 playback of a 3120 x 2080 shot to 720 x 480: 0x21155 = 4.333); source
//! address and pitch at +0xc4 / +0xd0, output at +0xd8 / +0xe4). The scaling is
//! nearest neighbour, a declared approximation: the driver programs no
//! interpolation-kernel register, so the engine's filter is fixed and unknown.
//! The words at +0x24, +0x28, +0x34..+0x3c and the 31 words at +0x40..+0xb8 (17 bits each,
//! 0x10000 = 1.0) are not filter coefficients but the lens distortion
//! correction, a 31-point radial table (0x11131d4); X-T3 5.11 programs the
//! identity there whenever no correction is active (every job measured so
//! far), and this model does not apply it. Without `resize`, what the engines
//! compute is not modeled.
//!
//! With `paired_rows` (opt-in), a format word selects, for the source (bits
//! 1..0) and the output (bits 5..4), format 3: two picture lines per pitch
//! row, the second one 2 x width bytes after the first. X-T3 5.11 programs
//! +0x08 = 0x133 for the 4K movie resize and 0x100 for stills; 0x1dad1e0
//! doubles the source line length when the format is 3 (<< 2 instead of << 1),
//! and only that layout fits the rings: 2636 source lines in 1318 of the 2088
//! rows of 0x4968000 at 0x9000, 2160 output lines in exactly the 1080 rows of
//! 0x16ad000 at 0x5600. Rows are ceil(h / 2), bounded by the declared ring
//! length; the line layout inside a row (two 4:2:2 lines) is a hypothesis.
//! With `tiles` (opt-in), a format 3 row is read and written instead as
//! Y0L2 2 x 2 blocks of 10-bit samples, one little-endian 64-bit word per
//! block (Linux drm_fourcc.h, DRM_FORMAT_Y0L2): Y0 bits 9..0, Cb 19..10,
//! Y1 29..20, A0 30, A1 31, Y2 41..32, Cr 51..42, Y3 61..52, A2 62, A3 63.
//! The 4K output is the Mali-V61 encoder's frame: the 5.11 driver (0x109a248)
//! gives it format 0x121, one plane, pitch 4 x stride, exactly when its
//! bit-depth flag is set (0x1098ecc: "bit_depth", "video_codec"), and the
//! output ring holds 1080 rows of that pitch. Declared: Y0 Y1 are the upper
//! line, Y2 Y3 the lower one; alpha bits are written 0; 8-bit packed samples
//! are widened by << 2 and narrowed by >> 2; a block takes its chroma from
//! its upper left pixel; an odd output height repeats the last line.
//! With `origin` (opt-in), a source position is offset by (x, y) pixels
//! before it is clamped to the source size: the X-T3 5.11 4K window starts at
//! the CMOS mode's left margin (mode 0x73 of 0x222aae0: 9) since no register
//! carries the start (0x1e747f8 writes only the end, +0x2c / +0x30).
//!
//! In a still capture (X-T3 5.11) the same engine is programmed with source
//! 0x80000000 and output 0xc0000000. These are DRAM channels 2 and 3
//! (0x108e658 maps 0x80000000 and 0xc0000000 as normal memory), so by default
//! the job copies there like anywhere else. The opt-in `linked_bit31` keeps
//! the earlier hypothesis (a direct link between engines): a job whose source
//! or output has bit 31 set copies nothing (counted as `chained`).
//!
//! With the `develop` option (opt-in, needs `resize`), a resizing job whose
//! source lies in the declared still picture first fills that picture from
//! the sensor's RAW, a declared stand-in for the development engines whose
//! data movement is not modeled (X-T3 5.11: RAW at 0x473d8fd0, lines packed at
//! 12552 bytes, picture B at 0x80000000 read by the still's first resize).
//! Each 16-bit sample of the window gives the luma `sample >> shift`; chroma
//! is neutral (0x80). Developing again gives the same picture.
//! With `color` (the X-Trans site, line and sample in
//! [`crate::development::XTRANS`], of the RAW's first sample), the window is
//! developed in color by [`crate::development::develop`] instead: nearest
//! site of each color, black 1024, sRGB, BT.601 full-range 4:2:2. The X-T3
//! 5.11 table (unit 6, 0x22073f8) is that tile from site (5, 1).
//! Three opt-in fields extend `develop` to the 4K movie (X-T3 5.11, mode 4
//! pools of 0x2229984): `window` scales a RAW window of that size from
//! `origin` onto the picture by nearest neighbour; `ring` pairs the rings,
//! picture buffer k (the resize source's index in the picture) developing
//! from RAW buffer k (RAW 0x370e000 apart in MemYc, picture 0x4968000 apart
//! in MemResz); `y0l2` writes format 3 Y0L2 rows (needs `tiles`, and only a
//! job whose source format is 3 develops), 10-bit luma (sample << 2) >>
//! `shift`, chroma 512 when grey. A declared stand-in for the firmware's RAW
//! to YC step (service 0x1293), whose window, scaling and filter are not
//! modeled; the pairing of buffer k with buffer k is a hypothesis.
//! `paired` writes format 3 rows of two packed 4:2:2 8-bit lines instead
//! (needs `paired_rows` without `tiles`). The 5.11 code settles the 4K
//! picture as Y0L2: RECC's output code 3 (0x80802, 0x1e89358 doubles the
//! pitch for it alone) is 10-bit 4:2:0 in line pairs (0x11563bc), and the
//! Mali-V61 format 0x121 is 10-bit 4:2:0 in one plane; the bit positions
//! are drm_fourcc.h's. Only the spatial order of the four luma samples (Y0
//! Y1 upper, Y2 Y3 lower) and the upper left chroma stay declared. `paired`
//! is kept as a witness layout, not the 4K movie's.
use crate::development::{YcLayout, developed_line, grey_line, y0l2_block, y0l2_sample, yc_row};
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub base: u64,
    pub size: u64,
    /// (offset, mask) that always reads set.
    pub ready: (u64, u32),
    /// (offset, mask) whose write starts a job and reads set until acknowledged.
    pub start: (u64, u32),
    /// (address, mask) of the acknowledge bit, outside the page.
    #[serde(default)]
    pub acknowledge: Option<(u64, u32)>,
    /// (offset, bits that always read set, done bit cleared by writing 1).
    #[serde(default)]
    pub status: Option<(u64, u32, u32)>,
    /// Register offsets of a packed 4:2:2 scaling done when a job ends.
    #[serde(default)]
    pub resize: Option<Resize>,
    /// Grey development of the still picture before it is resized (opt-in).
    #[serde(default)]
    pub develop: Option<Develop>,
    pub irq: u32,
    pub ticks: u64,
}

/// Where a resizing page keeps its parameters (offsets in the page).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resize {
    /// Source size, w - 1 | (h - 1) << 16.
    pub source_size: u64,
    /// Output size, same layout.
    pub output_size: u64,
    /// Horizontal and vertical steps: 2^(bits 31..16) x Q12 (bits 15..0).
    pub ratio: (u64, u64),
    /// Source address and pitch in bytes.
    pub source: (u64, u64),
    /// Output address and pitch in bytes.
    pub output: (u64, u64),
    /// Copy nothing when the source or output has bit 31 set (opt-in).
    #[serde(default)]
    pub linked_bit31: bool,
    /// Format 3, two lines per pitch row (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_rows: Option<PairedRows>,
    /// Source window origin (x, y) in pixels, added to every source position (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<(u64, u64)>,
}

/// Where the format word is and how many rows each buffer holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairedRows {
    /// Offset of the format word: source bits 1..0, output bits 5..4; 3 pairs lines.
    pub format: u64,
    /// Rows of the source and output buffers (0: no bound).
    pub rows: (u64, u64),
    /// Format 3 rows hold Y0L2 2 x 2 blocks of 10-bit samples (opt-in).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tiles: bool,
}

/// The format whose pitch rows hold two picture lines.
const PAIRED: u32 = 3;


/// Where the RAW lies and which picture it develops into.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Develop {
    /// RAW: address, line pitch in bytes, width and height in 16-bit samples.
    pub raw: (u64, u64, u64, u64),
    /// Window origin (x, y) in the RAW.
    pub origin: (u64, u64),
    /// Picture: base and length in bytes; a resize source inside it develops.
    pub picture: (u64, u64),
    /// Right shift from a RAW sample to 8-bit luma.
    pub shift: u32,
    /// Develop in color: X-Trans site (line, sample) of the RAW's first sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<(usize, usize)>,
    /// RAW window (width, height) from `origin`, scaled onto the picture by nearest neighbour (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<(u64, u64)>,
    /// Paired rings (RAW buffer stride, picture buffer stride): picture buffer k develops from RAW buffer k (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<(u64, u64)>,
    /// The picture is format 3 Y0L2 rows, two lines of 10-bit samples per pitch row (opt-in, needs `tiles`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub y0l2: bool,
    /// The picture is format 3 rows of two packed 4:2:2 lines, the second 2 x width bytes after the first
    /// (opt-in, needs `paired_rows` without `tiles`; only a job whose source format is 3 develops).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paired: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    words: BTreeMap<u64, u32>,
    acknowledge: u32,
    deadline: Option<u64>,
    done: bool,
    jobs: u64,
    /// The page's words at each of the first `HISTORY` starts, for reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<BTreeMap<u64, u32>>,
    /// Pictures scaled by the `resize` option.
    #[serde(default, skip_serializing_if = "is_zero")]
    resized: u64,
    /// Resizing jobs whose source or output had bit 31 set: nothing copied.
    #[serde(default, skip_serializing_if = "is_zero")]
    chained: u64,
    /// Pictures developed by the `develop` option.
    #[serde(default, skip_serializing_if = "is_zero")]
    developed: u64,
    /// Resizes with a source or output in format 3 (`paired_rows`).
    #[serde(default, skip_serializing_if = "is_zero")]
    paired: u64,
}

/// Address bit that marks a link between engines rather than a RAM window.
const LINKED: u32 = 0x8000_0000;

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Number of job starts whose words a report keeps.
const HISTORY: usize = 32;

pub struct JobAck {
    options: Options,
    state: State,
    /// Device name with the page base, so that several pages stay apart in reports.
    name: &'static str,
}

impl JobAck {
    pub fn new(options: Options) -> Result<Self, String> {
        let o = &options;
        if o.size == 0
            || o.base & 3 != 0
            || o.size & 3 != 0
            || o.ready.0 >= o.size
            || o.start.0 >= o.size
            || o.start.1 == 0
            || o.acknowledge.is_none() == o.status.is_none()
            || o.acknowledge.is_some_and(|(address, mask)| {
                mask == 0 || address & 3 != 0 || (o.base..o.base + o.size).contains(&address)
            })
            || o.status.is_some_and(|(offset, constant, done)| {
                offset >= o.size || offset & 3 != 0 || done == 0 || constant & done != 0
            })
            || o.resize.as_ref().is_some_and(|r| {
                [r.source_size, r.output_size, r.ratio.0, r.ratio.1, r.source.0, r.source.1, r.output.0, r.output.1]
                    .iter()
                    .chain(r.paired_rows.as_ref().map(|p| &p.format))
                    .any(|&offset| offset >= o.size || offset & 3 != 0)
            })
            || o.develop.as_ref().is_some_and(|d| {
                o.resize.is_none()
                    || d.shift > 16
                    || d.color.is_some_and(|site| site.0 >= 6 || site.1 >= 6)
                    || d.raw.1 < d.raw.2 * 2
                    || d.origin.0 >= d.raw.2
                    || d.origin.1 >= d.raw.3
                    || d.picture.1 == 0
                    || d.window.is_some_and(|(ww, wh)| {
                        ww == 0
                            || wh == 0
                            || d.origin.0 + ww > d.raw.2
                            || d.origin.1 + wh > d.raw.3
                            || (d.color.is_some() && (ww % 2 != 0 || ww < 6 || wh < 6))
                    })
                    || d.ring.is_some_and(|(raw, picture)| raw == 0 || picture == 0)
                    || (d.y0l2
                        && !o.resize.as_ref().is_some_and(|r| r.paired_rows.as_ref().is_some_and(|p| p.tiles)))
                    || (d.paired
                        && (d.y0l2
                            || !o.resize.as_ref().is_some_and(|r| r.paired_rows.as_ref().is_some_and(|p| !p.tiles))))
            })
            || !(32..512).contains(&o.irq)
            || o.ticks == 0
        {
            return Err("Invalid acknowledged job page".into());
        }
        let name = Box::leak(format!("acknowledged-job-page@{:#x}", options.base).into_boxed_str());
        Ok(Self { options, state: State::default(), name })
    }
    /// The page and the remote acknowledge word, for the board's address map.
    pub fn windows(&self) -> Vec<(u64, u64)> {
        let o = &self.options;
        let mut windows = vec![(o.base, o.base + o.size)];
        windows.extend(o.acknowledge.map(|(address, _)| (address, address + 4)));
        windows
    }
    /// Scales the programmed picture (nearest neighbour, packed 4:2:2).
    /// Returns whether a side was in format 3.
    fn resize(&self, cx: &mut Cx<'_>, r: &Resize) -> Result<bool, String> {
        let size = |offset| {
            let v = self.word(offset);
            ((v & 0xffff) as usize + 1, (v >> 16) as usize + 1)
        };
        let (sw, sh) = size(r.source_size);
        let (ow, oh) = size(r.output_size);
        let (spitch, opitch) = (self.word(r.source.1) as usize, self.word(r.output.1) as usize);
        // Per side: lines per row and buffer rows (0: no bound).
        let (sp, op) = match &r.paired_rows {
            Some(p) => {
                let format = self.word(p.format);
                let side = |shift: u32, rows: u64| {
                    if (format >> shift) & 3 == PAIRED { (2, rows as usize) } else { (1, 0) }
                };
                (side(0, p.rows.0), side(4, p.rows.1))
            }
            None => ((1, 0), (1, 0)),
        };
        if sw * 2 * sp.0 > spitch || ow * 2 * op.0 > opitch || sw % 2 != 0 || ow % 2 != 0 {
            return Err(format!("resize: {sw}x{sh} pitch {spitch} -> {ow}x{oh} pitch {opitch}"));
        }
        // Rows a side uses: ceil(h / lines per row), bounded by its buffer.
        let rows = |h: usize, (lines, bound): (usize, usize)| {
            let n = h.div_ceil(lines);
            if bound > 0 { n.min(bound) } else { n }
        };
        let (srows, orows) = (rows(sh, sp), rows(oh, op));
        // Row and byte offset of a picture line.
        let place = |y: usize, lines: usize, w: usize| (y / lines, y % lines * w * 2);
        // Source position of an output index: Q12 step scaled by 2^(bits 31..16).
        let step = |offset| {
            let v = self.word(offset);
            u64::from(v & 0xffff) << (v >> 16).min(8)
        };
        let (hx, hy) = (step(r.ratio.0), step(r.ratio.1));
        let at = |i: usize, step: u64, limit: usize| ((i as u64 * step) >> 12).min(limit as u64 - 1) as usize;
        // Source column and line of an output index, offset by the declared origin.
        let (ox, oy) = r.origin.map_or((0, 0), |(x, y)| (x as usize, y as usize));
        let sx = |i: usize| (at(i, hx, sw) + ox).min(sw - 1);
        let sy = |i: usize| (at(i, hy, sh) + oy).min(sh - 1);
        let source = cx.ram_bytes(self.word(r.source.0).into(), spitch * (srows - 1) + sw * 2 * sp.0)?;
        let output = u64::from(self.word(r.output.0));
        if r.paired_rows.as_ref().is_some_and(|p| p.tiles) && (sp.0 > 1 || op.0 > 1) {
            // 10-bit Y, Cb, Cr of a source pixel: Y0L2 rows or packed 4:2:2 lines.
            let sample = |x: usize, y: usize| {
                if sp.0 > 1 {
                    y0l2_sample(&source[(y / 2).min(srows - 1) * spitch..], x, y % 2)
                } else {
                    let row = &source[y * spitch..];
                    let p = x / 2 * 4;
                    let wide = |b: u8| u16::from(b) << 2;
                    (wide(row[p + x % 2]), wide(row[p + 2]), wide(row[p + 3]))
                }
            };
            if op.0 > 1 {
                let mut row = vec![0u8; ow * 4];
                for orow in 0..orows {
                    for block in 0..ow / 2 {
                        let mut luma = [0u16; 4];
                        let mut chroma = (0, 0);
                        for (k, l) in luma.iter_mut().enumerate() {
                            let y = (orow * 2 + k / 2).min(oh - 1);
                            let (v, cb, cr) = sample(sx(block * 2 + k % 2), sy(y));
                            *l = v;
                            if k == 0 {
                                chroma = (cb, cr);
                            }
                        }
                        let word = y0l2_block(luma, chroma.0, chroma.1);
                        row[block * 8..block * 8 + 8].copy_from_slice(&word.to_le_bytes());
                    }
                    cx.ram_write(output + (orow * opitch) as u64, &row)?;
                }
            } else {
                let mut line = vec![0u8; ow * 2];
                for y in 0..oh {
                    for pair in 0..ow / 2 {
                        let (y0, cb, cr) = sample(sx(pair * 2), sy(y));
                        let (y1, _, _) = sample(sx(pair * 2 + 1), sy(y));
                        let narrow = |v: u16| (v >> 2) as u8;
                        line[pair * 4..pair * 4 + 4].copy_from_slice(&[narrow(y0), narrow(y1), narrow(cb), narrow(cr)]);
                    }
                    cx.ram_write(output + (y * opitch) as u64, &line)?;
                }
            }
            return Ok(true);
        }
        let mut line = vec![0u8; ow * 2];
        for y in 0..oh {
            let (orow, oat) = place(y, op.0, ow);
            if orow >= orows {
                break;
            }
            let (srow, sat) = place(sy(y), sp.0, sw);
            let row = &source[srow.min(srows - 1) * spitch + sat..];
            for pair in 0..ow / 2 {
                let (x0, x1) = (sx(pair * 2), sx(pair * 2 + 1));
                let p0 = x0 / 2 * 4;
                line[pair * 4] = row[p0 + x0 % 2];
                line[pair * 4 + 1] = row[x1 / 2 * 4 + x1 % 2];
                line[pair * 4 + 2] = row[p0 + 2];
                line[pair * 4 + 3] = row[p0 + 3];
            }
            cx.ram_write(output + (orow * opitch + oat) as u64, &line)?;
        }
        Ok(sp.0 > 1 || op.0 > 1)
    }
    /// Fills the resize source from the RAW when it lies in the picture.
    /// Returns whether it did.
    fn develop(&self, cx: &mut Cx<'_>, r: &Resize, d: &Develop) -> Result<bool, String> {
        if d.window.is_some() || d.ring.is_some() || d.y0l2 || d.paired {
            return self.develop_scaled(cx, r, d);
        }
        let source = u64::from(self.word(r.source.0));
        if !(d.picture.0..d.picture.0 + d.picture.1).contains(&source) {
            return Ok(false);
        }
        let v = self.word(r.source_size);
        let (w, h) = ((v & 0xffff) as u64 + 1, (v >> 16) as u64 + 1);
        let pitch = u64::from(self.word(r.source.1));
        let (raw, raw_pitch, raw_w, raw_h) = d.raw;
        if w % 2 != 0
            || w * 2 > pitch
            || d.origin.0 + w > raw_w
            || d.origin.1 + h > raw_h
            || source + pitch * (h - 1) + w * 2 > d.picture.0 + d.picture.1
        {
            return Err(format!("develop: {w}x{h} pitch {pitch} outside the RAW or the picture"));
        }
        if let Some(site) = d.color {
            if site.0 >= 6 || site.1 >= 6 {
                return Err("develop: X-Trans site outside the tile".into());
            }
            let stride = w as usize * 2;
            let mut window = Vec::with_capacity(stride * h as usize);
            for y in 0..h {
                let row = raw + (d.origin.1 + y) * raw_pitch + d.origin.0 * 2;
                window.extend_from_slice(&cx.ram_bytes(row, stride)?);
            }
            let phase = (
                (site.0 + d.origin.1 as usize) % 6,
                (site.1 + d.origin.0 as usize) % 6,
            );
            let lines =
                crate::development::develop(&window, stride, h as usize, w as usize, phase, None)?;
            for (y, line) in lines.iter().enumerate() {
                cx.ram_write(source + y as u64 * pitch, line)?;
            }
            return Ok(true);
        }
        let mut line = vec![0x80u8; w as usize * 2];
        for y in 0..h {
            let row = raw + (d.origin.1 + y) * raw_pitch + d.origin.0 * 2;
            let samples = cx.ram_bytes(row, w as usize * 2)?;
            for pair in 0..w as usize / 2 {
                for k in 0..2 {
                    let i = (pair * 2 + k) * 2;
                    let sample = u16::from_le_bytes([samples[i], samples[i + 1]]);
                    line[pair * 4 + k] = (u32::from(sample) >> d.shift).min(255) as u8;
                }
            }
            cx.ram_write(source + y * pitch, &line)?;
        }
        Ok(true)
    }
    /// [`Self::develop`] through a scaled RAW window, paired rings or Y0L2 rows.
    fn develop_scaled(&self, cx: &mut Cx<'_>, r: &Resize, d: &Develop) -> Result<bool, String> {
        let source = u64::from(self.word(r.source.0));
        if !(d.picture.0..d.picture.0 + d.picture.1).contains(&source) {
            return Ok(false);
        }
        if (d.y0l2 || d.paired)
            && !r.paired_rows.as_ref().is_some_and(|p| p.tiles == d.y0l2 && self.word(p.format) & 3 == PAIRED)
        {
            return Ok(false);
        }
        // Picture buffer k and its end; RAW buffer k.
        let (k, end) = match d.ring {
            Some((_, stride)) => {
                let k = (source - d.picture.0) / stride;
                (k, (d.picture.0 + (k + 1) * stride).min(d.picture.0 + d.picture.1))
            }
            None => (0, d.picture.0 + d.picture.1),
        };
        let raw = d.raw.0 + d.ring.map_or(0, |(stride, _)| k * stride);
        let v = self.word(r.source_size);
        let (w, h) = ((v & 0xffff) as u64 + 1, (v >> 16) as u64 + 1);
        let pitch = u64::from(self.word(r.source.1));
        let (_, raw_pitch, raw_w, raw_h) = d.raw;
        let (ww, wh) = d.window.unwrap_or((w, h));
        let (lines, row_bytes) = if d.y0l2 || d.paired { (2, w * 4) } else { (1, w * 2) };
        let rows = h.div_ceil(lines);
        if w % 2 != 0
            || row_bytes > pitch
            || d.origin.0 + ww > raw_w
            || d.origin.1 + wh > raw_h
            || (d.color.is_some() && (ww % 2 != 0 || ww < 6 || wh < 6))
            || source + pitch * (rows - 1) + row_bytes > end
        {
            return Err(format!("develop: {w}x{h} pitch {pitch} from {ww}x{wh} outside the RAW or the picture"));
        }
        let at = |y: u64| raw + (d.origin.1 + y) * raw_pitch + d.origin.0 * 2;
        let developed = match d.color {
            Some(site) => {
                let stride = ww as usize * 2;
                let mut window = Vec::with_capacity(stride * wh as usize);
                for y in 0..wh {
                    window.extend_from_slice(&cx.ram_bytes(at(y), stride)?);
                }
                let phase = ((site.0 + d.origin.1 as usize) % 6, (site.1 + d.origin.0 as usize) % 6);
                Some(crate::development::develop(&window, stride, wh as usize, ww as usize, phase, None)?)
            }
            None => None,
        };
        // 10-bit Y, Cb, Cr of each pixel of picture line y (nearest window sample).
        let line = |cx: &Cx<'_>, y: u64| -> Result<Vec<[u16; 3]>, String> {
            let wy = y * wh / h;
            Ok(match &developed {
                Some(lines) => developed_line(&lines[wy as usize], ww, w),
                None => grey_line(&cx.ram_bytes(at(wy), ww as usize * 2)?, ww, w, d.shift),
            })
        };
        if d.y0l2 {
            for n in 0..rows {
                let (upper, lower) = (line(cx, 2 * n)?, line(cx, (2 * n + 1).min(h - 1))?);
                cx.ram_write(source + n * pitch, &yc_row(YcLayout::Y0l2, &upper, &lower))?;
            }
        } else {
            for y in 0..h {
                let out = yc_row(YcLayout::Lines, &line(cx, y)?, &[]);
                let place = if d.paired { y / 2 * pitch + y % 2 * w * 2 } else { y * pitch };
                cx.ram_write(source + place, &out)?;
            }
        }
        Ok(true)
    }
    fn finish(&mut self, cx: &mut Cx<'_>) {
        let o = &self.options;
        self.state.done = false;
        let word = self.word(o.start.0) & !o.start.1;
        self.state.words.insert(o.start.0, word);
        cx.set_irq(o.irq, false);
    }
    fn word(&self, offset: u64) -> u32 {
        self.state.words.get(&offset).copied().unwrap_or(0)
    }
}

impl Device for JobAck {
    fn name(&self) -> &'static str {
        self.name
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let o = &self.options;
        if size != 4 || address & 3 != 0 {
            cx.unmodeled();
            return 0;
        }
        if o.acknowledge.is_some_and(|(ack, _)| ack == address) {
            return u64::from(self.state.acknowledge);
        }
        let offset = address - o.base;
        if let Some((status, constant, done)) = o.status {
            if offset == status {
                return u64::from(constant | if self.state.done { done } else { 0 });
            }
        }
        let mut value = self.word(offset);
        if offset == o.ready.0 {
            value |= o.ready.1;
        }
        u64::from(value)
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        if size != 4 || address & 3 != 0 || value > u64::from(u32::MAX) {
            cx.unmodeled();
            return;
        }
        let value = value as u32;
        let o = self.options.clone();
        if let Some((_, mask)) = o.acknowledge.filter(|&(ack, _)| ack == address) {
            self.state.acknowledge = value;
            if value & mask != 0 && self.state.done {
                self.finish(cx);
            }
            return;
        }
        let offset = address - o.base;
        if let Some((_, _, done)) = o.status.filter(|&(status, _, _)| status == offset) {
            if value & done != 0 && self.state.done {
                self.finish(cx);
            }
            return;
        }
        let started = offset == o.start.0
            && value & o.start.1 != 0
            && self.word(offset) & o.start.1 == 0
            && self.state.deadline.is_none()
            && !self.state.done;
        self.state.words.insert(offset, value);
        if started {
            if self.state.history.len() < HISTORY {
                self.state.history.push(self.state.words.clone());
            }
            let at = cx.now() + o.ticks;
            self.state.deadline = Some(at);
            cx.schedule(at, 0);
        }
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        if self.state.deadline.is_some_and(|at| at <= cx.now()) {
            self.state.deadline = None;
            if let (Some(r), Some(d)) = (self.options.resize.clone(), self.options.develop.clone()) {
                match self.develop(cx, &r, &d) {
                    Ok(true) => self.state.developed += 1,
                    Ok(false) => {}
                    Err(e) => {
                        return cx.fail(format!("acknowledged job page {:#x}: {e}", self.options.base));
                    }
                }
            }
            if let Some(r) = self.options.resize.clone() {
                if r.linked_bit31 && (self.word(r.source.0) | self.word(r.output.0)) & LINKED != 0 {
                    self.state.chained += 1;
                } else {
                    match self.resize(cx, &r) {
                        Ok(paired) => self.state.paired += u64::from(paired),
                        Err(e) => {
                            return cx.fail(format!("acknowledged job page {:#x}: {e}", self.options.base));
                        }
                    }
                    self.state.resized += 1;
                }
            }
            self.state.done = true;
            self.state.jobs += 1;
            cx.set_irq(self.options.irq, true);
        }
    }
    fn event(&mut self, cx: &mut Cx<'_>, _token: u64) {
        self.advance(cx);
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        self.state.deadline
    }
    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap_or(Value::Null)
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        self.state = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::recording::RecordingHost;

    #[test]
    fn a_started_job_raises_its_level_line_until_the_remote_acknowledge() {
        let mut page = JobAck::new(Options {
            base: 0xfa000000,
            size: 0x1000,
            ready: (0, 0x10000),
            start: (0, 1),
            acknowledge: Some((0xfff4aa10, 8)),
            status: None,
            resize: None,
            develop: None,
            irq: 145,
            ticks: 50,
        })
        .unwrap();
        let mut h = RecordingHost::default();
        let rd = |p: &mut JobAck, h: &mut RecordingHost, a| p.read(&mut Cx::new(h, 0, 0, 0), a, 4);
        assert_eq!(rd(&mut page, &mut h, 0xfa000000), 0x10000);
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfa00001c, 4, 0x1234);
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfa000000, 4, 0x10001);
        assert!(h.irqs.is_empty());
        h.now = 50;
        page.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(h.irqs.last(), Some(&(145, true)));
        assert_eq!(rd(&mut page, &mut h, 0xfa000000) & 1, 1);
        // 0x1079280: set ack bit 3, wait for bit 0 to clear, clear bit 3.
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfff4aa10, 4, 8);
        assert_eq!(rd(&mut page, &mut h, 0xfa000000) & 1, 0);
        assert_eq!(h.irqs.last(), Some(&(145, false)));
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfff4aa10, 4, 0);
        assert_eq!(rd(&mut page, &mut h, 0xfa00001c), 0x1234);
    }

    #[test]
    fn a_status_word_reads_out_of_reset_and_its_done_bit_is_written_one_to_clear() {
        let mut page = JobAck::new(Options {
            base: 0xfff7e000,
            size: 0x1000,
            ready: (0, 1),
            start: (4, 1),
            acknowledge: None,
            status: Some((0x10c, 0x0100_0100, 1)),
            resize: None,
            develop: None,
            irq: 132,
            ticks: 50,
        })
        .unwrap();
        assert_eq!(page.windows(), vec![(0xfff7e000, 0xfff7f000)]);
        let mut h = RecordingHost::default();
        let rd = |p: &mut JobAck, h: &mut RecordingHost, a| p.read(&mut Cx::new(h, 0, 0, 0), a, 4);
        // 0x1091674: out of reset, then +0x000 bit 0.
        assert_eq!(rd(&mut page, &mut h, 0xfff7e10c), 0x0100_0100);
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfff7e000, 4, 0);
        assert_eq!(rd(&mut page, &mut h, 0xfff7e000) & 1, 1);
        // 0x109170c: +0x004 |= 1.
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfff7e004, 4, 1);
        h.now = 50;
        page.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(h.irqs.last(), Some(&(132, true)));
        assert_eq!(rd(&mut page, &mut h, 0xfff7e10c), 0x0100_0101);
        // 0x10921dc: +0x10c = 1.
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfff7e10c, 4, 1);
        assert_eq!(h.irqs.last(), Some(&(132, false)));
        assert_eq!(rd(&mut page, &mut h, 0xfff7e10c), 0x0100_0100);
        assert_eq!(rd(&mut page, &mut h, 0xfff7e004) & 1, 0);
        // A second job starts again.
        page.write(&mut Cx::new(&mut h, 0, 0, 0), 0xfff7e004, 4, 1);
        h.now = 100;
        page.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(h.irqs.last(), Some(&(132, true)));
        assert!(JobAck::new(Options { status: None, ..page.options.clone() }).is_err());
    }

    #[test]
    fn a_resizing_job_scales_a_packed_picture_when_it_ends() {
        use fujimod_emulator_devices::test_host::{TestHost, advance, write};
        let mut page = JobAck::new(Options {
            base: 0xfff7e000,
            size: 0x1000,
            ready: (0, 1),
            start: (4, 1),
            acknowledge: None,
            status: Some((0x10c, 0x0100_0100, 1)),
            resize: Some(Resize {
                source_size: 0x0c,
                output_size: 0x18,
                ratio: (0x1c, 0x20),
                source: (0xc4, 0xd0),
                output: (0xd8, 0xe4),
                linked_bit31: true,
                paired_rows: None,
                origin: None,
            }),
            develop: None,
            irq: 132,
            ticks: 10,
        })
        .unwrap();
        let mut h = TestHost::new();
        // 8x4 source, Y = 10 * x + y, Cb = 100 + pair, Cr = 200 + y.
        let source: Vec<u8> = (0..4u8)
            .flat_map(|y| (0..4u8).flat_map(move |p| [20 * p + y, 20 * p + 10 + y, 100 + p, 200 + y]))
            .collect();
        h.ram.splice(0x1000..0x1000 + source.len(), source);
        let w = |p: &mut JobAck, h: &mut TestHost, o: u64, v: u32| write(p, h, 0xfff7e000 + o, v);
        w(&mut page, &mut h, 0x0c, 3 << 16 | 7);
        w(&mut page, &mut h, 0x18, 1 << 16 | 3);
        // Halve first, then step 1.0: 8x4 -> 4x2.
        w(&mut page, &mut h, 0x1c, 0x1_1000);
        w(&mut page, &mut h, 0x20, 0x1_1000);
        w(&mut page, &mut h, 0xc4, 0x1000);
        w(&mut page, &mut h, 0xd0, 16);
        w(&mut page, &mut h, 0xd8, 0x2000);
        w(&mut page, &mut h, 0xe4, 12);
        w(&mut page, &mut h, 0x04, 1);
        advance(&mut page, &mut h, 10);
        // Output x 0..4 reads source x 0, 2, 4, 6; output y 0..2 reads y 0, 2.
        assert_eq!(&h.ram[0x2000..0x2008], &[0, 20, 100, 200, 40, 60, 102, 200]);
        assert_eq!(&h.ram[0x200c..0x2014], &[2, 22, 100, 202, 42, 62, 102, 202]);
        assert_eq!(page.state.resized, 1);
        // A step of 2^2 x 0x400 / 0x1000 = 1.0 keeps x 0..4 (bits above 16 count).
        w(&mut page, &mut h, 0x10c, 1);
        w(&mut page, &mut h, 0x1c, 0x2_0400);
        w(&mut page, &mut h, 0x20, 0x2_0800);
        w(&mut page, &mut h, 0x04, 0);
        w(&mut page, &mut h, 0x04, 1);
        advance(&mut page, &mut h, 20);
        assert_eq!(&h.ram[0x2000..0x2008], &[0, 10, 100, 200, 20, 30, 101, 200]);
        // Step y 2.0: output line 1 reads source line 2.
        assert_eq!(&h.ram[0x200c..0x2014], &[2, 12, 100, 202, 22, 32, 101, 202]);
        assert_eq!(page.state.resized, 2);
        // Capture: source 0x80000000, output 0xc0000000. Nothing is copied
        // and the job still ends with its line raised.
        w(&mut page, &mut h, 0x10c, 1);
        let before = h.ram.clone();
        w(&mut page, &mut h, 0xc4, 0x8000_0000);
        w(&mut page, &mut h, 0xd8, 0xc000_0000);
        w(&mut page, &mut h, 0x04, 0);
        w(&mut page, &mut h, 0x04, 1);
        advance(&mut page, &mut h, 30);
        assert_eq!(h.ram, before);
        assert_eq!((page.state.resized, page.state.chained), (2, 1));
        assert!(page.state.done);
        // By default the same addresses are DRAM and the picture is copied.
        w(&mut page, &mut h, 0x10c, 1);
        page.options.resize.as_mut().unwrap().linked_bit31 = false;
        w(&mut page, &mut h, 0x04, 0);
        w(&mut page, &mut h, 0x04, 1);
        advance(&mut page, &mut h, 40);
        assert_eq!((page.state.resized, page.state.chained), (3, 1));
    }

    #[test]
    fn a_format_3_resize_puts_two_lines_in_each_pitch_row() {
        use fujimod_emulator_devices::test_host::{TestHost, advance, write};
        let paired = |rows| Resize {
            source_size: 0x0c,
            output_size: 0x18,
            ratio: (0x1c, 0x20),
            source: (0xc4, 0xd0),
            output: (0xd8, 0xe4),
            linked_bit31: false,
            paired_rows: Some(PairedRows { format: 0x08, rows, tiles: false }),
            origin: None,
        };
        let page = |rows| {
            JobAck::new(Options {
                base: 0xfff7e000,
                size: 0x1000,
                ready: (0, 1),
                start: (4, 1),
                acknowledge: None,
                status: Some((0x10c, 0x0100_0100, 1)),
                resize: Some(paired(rows)),
                develop: None,
                irq: 132,
                ticks: 10,
            })
        };
        // The format word must be a word of the page.
        let mut bad = paired((0, 0));
        bad.paired_rows = Some(PairedRows { format: 0x1002, rows: (0, 0), tiles: false });
        assert!(
            JobAck::new(Options { resize: Some(bad), ..page((0, 0)).unwrap().options.clone() }).is_err()
        );
        // 4 x 5 source in format 3: 3 rows of 16 bytes, line y at row y / 2,
        // byte 8 * (y % 2). Y = 10 * y + x, Cb = 100 + y, Cr = 200 + y.
        let line = |y: u8| [10 * y, 10 * y + 1, 100 + y, 200 + y, 10 * y + 2, 10 * y + 3, 100 + y, 200 + y];
        let run = |rows, format: u32, opitch: u32| {
            let mut p = page(rows).unwrap();
            let mut h = TestHost::new();
            for y in 0..5u8 {
                let at = 0x1000 + usize::from(y / 2) * 16 + usize::from(y % 2) * 8;
                h.ram[at..at + 8].copy_from_slice(&line(y));
            }
            let w = |p: &mut JobAck, h: &mut TestHost, o: u64, v: u32| write(p, h, 0xfff7e000 + o, v);
            w(&mut p, &mut h, 0x08, format);
            w(&mut p, &mut h, 0x0c, 4 << 16 | 3);
            w(&mut p, &mut h, 0x18, 4 << 16 | 3);
            w(&mut p, &mut h, 0x1c, 0x1000);
            w(&mut p, &mut h, 0x20, 0x1000);
            w(&mut p, &mut h, 0xc4, 0x1000);
            w(&mut p, &mut h, 0xd0, 16);
            w(&mut p, &mut h, 0xd8, 0x2000);
            w(&mut p, &mut h, 0xe4, opitch);
            w(&mut p, &mut h, 0x04, 1);
            advance(&mut p, &mut h, 10);
            assert_eq!(p.state.resized, 1);
            (p.state.paired, h.ram[0x2000..0x2000 + 48].to_vec())
        };
        // 0x133 (4K movie): both sides paired, a 1:1 job copies the 3 rows.
        let (n, out) = run((0, 0), 0x133, 16);
        assert_eq!(n, 1);
        for y in 0..5u8 {
            let at = usize::from(y / 2) * 16 + usize::from(y % 2) * 8;
            assert_eq!(out[at..at + 8], line(y), "line {y}");
        }
        assert_eq!(out[40..48], [0; 8], "odd height: the last row's second line stays");
        // Output ring of 1 row: lines 2.. are not written.
        let (_, out) = run((0, 1), 0x133, 16);
        assert_eq!((out[0..8].to_vec(), out[8..16].to_vec()), (line(0).to_vec(), line(1).to_vec()));
        assert_eq!(out[16..48], [0; 32]);
        // Source ring of 1 row: lines 2.. read the last row of the ring.
        let (_, out) = run((1, 0), 0x133, 16);
        assert_eq!((out[16..24].to_vec(), out[24..32].to_vec()), (line(0).to_vec(), line(1).to_vec()));
        assert_eq!(out[32..40], line(0));
        // 0x103: paired source, one output line per row (pitch 8).
        let (n, out) = run((0, 0), 0x103, 8);
        assert_eq!(n, 1);
        for y in 0..5u8 {
            let at = usize::from(y) * 8;
            assert_eq!(out[at..at + 8], line(y), "line {y}");
        }
        // 0x100 (still): nothing paired, line y read at y * pitch as before.
        let (n, out) = run((0, 0), 0x100, 16);
        assert_eq!(n, 0);
        assert_eq!(out[16..24], line(2), "source row 1 holds line 2 first");
    }

    #[test]
    fn a_y0l2_resize_keeps_two_lines_in_each_64_bit_block() {
        use fujimod_emulator_devices::test_host::{TestHost, advance, write};
        // Field positions of drm_fourcc.h Y0L2: Y0 Cb Y1 A0 A1 | Y2 Cr Y3 A2 A3.
        let b = y0l2_block([1, 2, 3, 4], 5, 6);
        assert_eq!(b, 1 | 5 << 10 | 2 << 20 | 3 << 32 | 6 << 42 | 4 << 52);
        assert_eq!(y0l2_sample(&b.to_le_bytes(), 1, 1), (4, 5, 6));
        let run = |tiles, origin, format: u32, spitch: u32, opitch: u32, src: &[u8]| {
            let resize = Resize {
                source_size: 0x0c,
                output_size: 0x18,
                ratio: (0x1c, 0x20),
                source: (0xc4, 0xd0),
                output: (0xd8, 0xe4),
                linked_bit31: false,
                paired_rows: Some(PairedRows { format: 0x08, rows: (0, 0), tiles }),
                origin,
            };
            let mut p = JobAck::new(Options {
                base: 0xfff7e000,
                size: 0x1000,
                ready: (0, 1),
                start: (4, 1),
                acknowledge: None,
                status: Some((0x10c, 0x0100_0100, 1)),
                resize: Some(resize),
                develop: None,
                irq: 132,
                ticks: 10,
            })
            .unwrap();
            let mut h = TestHost::new();
            h.ram[0x1000..0x1000 + src.len()].copy_from_slice(src);
            let w = |p: &mut JobAck, h: &mut TestHost, o: u64, v: u32| write(p, h, 0xfff7e000 + o, v);
            w(&mut p, &mut h, 0x08, format);
            w(&mut p, &mut h, 0x0c, 3 << 16 | 3);
            w(&mut p, &mut h, 0x18, 3 << 16 | 3);
            w(&mut p, &mut h, 0x1c, 0x1000);
            w(&mut p, &mut h, 0x20, 0x1000);
            w(&mut p, &mut h, 0xc4, 0x1000);
            w(&mut p, &mut h, 0xd0, spitch);
            w(&mut p, &mut h, 0xd8, 0x2000);
            w(&mut p, &mut h, 0xe4, opitch);
            w(&mut p, &mut h, 0x04, 1);
            advance(&mut p, &mut h, 10);
            assert_eq!(p.state.resized, 1);
            (p.state.paired, h.ram[0x2000..0x2000 + 32].to_vec())
        };
        // 4 x 4 Y0L2 picture, 2 rows of 2 blocks: Y = 100 * y + x + 1, block n = 2 * row + column
        // with Cb = 500 + n, Cr = 600 + n.
        let mut src = Vec::new();
        for row in 0..2u16 {
            for col in 0..2u16 {
                let y = |line: u16, k: u16| 100 * (2 * row + line) + 2 * col + k + 1;
                let n = 2 * row + col;
                src.extend(y0l2_block([y(0, 0), y(0, 1), y(1, 0), y(1, 1)], 500 + n, 600 + n).to_le_bytes());
            }
        }
        // 0x133, 1:1: the blocks are copied.
        let (n, out) = run(true, None, 0x133, 16, 16, &src);
        assert_eq!((n, &out[..]), (1, &src[..]));
        // 0x103: Y0L2 source to packed 4:2:2 lines (pitch 8), samples >> 2.
        let (_, out) = run(true, None, 0x103, 16, 8, &src);
        for y in 0..4u16 {
            let n = y / 2 * 2;
            let want = [100 * y + 1, 100 * y + 2, 500 + n, 600 + n].map(|v| (v >> 2) as u8);
            assert_eq!(out[usize::from(y) * 8..usize::from(y) * 8 + 4], want, "line {y}");
        }
        // Origin (1, 1): output (0, 0) is source (1, 1), output (3, 3) clamps to source (3, 3),
        // output block 1 takes the chroma of source (3, 1), in source block 1.
        let (_, out) = run(true, Some((1, 1)), 0x133, 16, 16, &src);
        assert_eq!(y0l2_sample(&out, 0, 0), (102, 500, 600));
        assert_eq!(y0l2_sample(&out[16..], 3, 1).0, 304);
        assert_eq!(y0l2_sample(&out, 2, 0), (104, 501, 601));
        // Without tiles, origin (2, 0) on packed lines (0x100, pitch 8): pixels 2, 3 come first.
        let packed: Vec<u8> = (0..32).collect();
        let (n, out) = run(false, Some((2, 0)), 0x100, 8, 8, &packed);
        assert_eq!(n, 0);
        assert_eq!(out[0..8], [4, 5, 6, 7, 5, 5, 6, 7]);
    }

    #[test]
    fn a_scaled_y0l2_development_fills_the_paired_ring_buffer() {
        use fujimod_emulator_devices::test_host::{TestHost, advance, write};
        let develop = Develop {
            raw: (0x3000, 16, 8, 4),
            origin: (0, 0),
            picture: (0x1000, 0x400),
            shift: 4,
            color: None,
            window: Some((8, 4)),
            ring: Some((0x400, 0x200)),
            y0l2: true,
            paired: false,
        };
        let options = |develop: Develop, tiles| Options {
            base: 0xfff7e000,
            size: 0x1000,
            ready: (0, 1),
            start: (4, 1),
            acknowledge: None,
            status: Some((0x10c, 0x0100_0100, 1)),
            resize: Some(Resize {
                source_size: 0x0c,
                output_size: 0x18,
                ratio: (0x1c, 0x20),
                source: (0xc4, 0xd0),
                output: (0xd8, 0xe4),
                linked_bit31: false,
                paired_rows: Some(PairedRows { format: 0x08, rows: (0, 0), tiles }),
                origin: None,
            }),
            develop: Some(develop),
            irq: 132,
            ticks: 10,
        };
        assert!(JobAck::new(options(develop.clone(), false)).is_err(), "y0l2 needs tiles");
        assert!(JobAck::new(options(Develop { window: Some((9, 4)), ..develop.clone() }, true)).is_err());
        assert!(JobAck::new(options(Develop { ring: Some((0, 0x200)), ..develop.clone() }, true)).is_err());
        assert!(JobAck::new(options(Develop { paired: true, ..develop.clone() }, true)).is_err(), "y0l2 or paired");
        assert!(JobAck::new(options(Develop { y0l2: false, paired: true, ..develop.clone() }, true)).is_err());
        let run = |develop: Develop, format: u32, source: u32, pitch: u32| {
            let tiles = !develop.paired;
            let mut page = JobAck::new(options(develop, tiles)).unwrap();
            let mut h = TestHost::new();
            // RAW buffer 0: 7 everywhere; buffer 1 (0x3400): 1000 + 40 * line + 4 * sample.
            for y in 0..4u64 {
                for x in 0..8u64 {
                    let at = (0x3000 + y * 16 + x * 2) as usize;
                    h.ram[at..at + 2].copy_from_slice(&7u16.to_le_bytes());
                    h.ram[at + 0x400..at + 0x402].copy_from_slice(&((1000 + 40 * y + 4 * x) as u16).to_le_bytes());
                }
            }
            let w = |p: &mut JobAck, h: &mut TestHost, o: u64, v: u32| write(p, h, 0xfff7e000 + o, v);
            // 4 x 2 picture copied 1:1 to 0x2000.
            w(&mut page, &mut h, 0x08, format);
            w(&mut page, &mut h, 0x0c, 1 << 16 | 3);
            w(&mut page, &mut h, 0x18, 1 << 16 | 3);
            w(&mut page, &mut h, 0x1c, 0x1000);
            w(&mut page, &mut h, 0x20, 0x1000);
            w(&mut page, &mut h, 0xc4, source);
            w(&mut page, &mut h, 0xd0, pitch);
            w(&mut page, &mut h, 0xd8, 0x2000);
            w(&mut page, &mut h, 0xe4, pitch);
            w(&mut page, &mut h, 0x04, 1);
            advance(&mut page, &mut h, 10);
            assert_eq!(page.state.resized, 1);
            (page.state.developed, h.ram[0x1000..0x1400].to_vec(), h.ram[0x2000..0x2020].to_vec())
        };
        // Buffer 1 (source 0x1200) from RAW buffer 1, every second sample of every second line,
        // luma (sample << 2) >> 4, neutral chroma 512; buffer 0 untouched.
        let (n, picture, out) = run(develop.clone(), 0x133, 0x1200, 16);
        assert_eq!(n, 1);
        let row = &picture[0x200..0x210];
        assert_eq!(u64::from_le_bytes(row[..8].try_into().unwrap()), y0l2_block([250, 252, 270, 272], 512, 512));
        assert_eq!(u64::from_le_bytes(row[8..].try_into().unwrap()), y0l2_block([254, 256, 274, 276], 512, 512));
        assert!(picture[..0x200].iter().all(|&b| b == 0));
        assert_eq!(out[..16], row[..], "the 1:1 Y0L2 resize copies the developed row");
        // Buffer 0 from RAW buffer 0: luma 7 >> 2 = 1.
        let (_, picture, _) = run(develop.clone(), 0x133, 0x1000, 16);
        assert_eq!(y0l2_sample(&picture, 3, 1), (1, 512, 512));
        // A job whose source is not format 3 (stills, 0x100) develops nothing.
        let (n, picture, _) = run(develop.clone(), 0x100, 0x1200, 16);
        assert_eq!(n, 0);
        assert!(picture.iter().all(|&b| b == 0));
        // Without y0l2: packed 4:2:2 lines, 8-bit luma sample >> 4, chroma 0x80.
        let (n, picture, _) = run(Develop { y0l2: false, ..develop.clone() }, 0x100, 0x1200, 8);
        assert_eq!(n, 1);
        let lines = [62, 63, 0x80, 0x80, 63, 64, 0x80, 0x80, 67, 68, 0x80, 0x80, 68, 69, 0x80, 0x80];
        assert_eq!(picture[0x200..0x210], lines);
        // Paired: the same two lines in one pitch row (pitch 32), only for a format 3 source.
        let paired = Develop { y0l2: false, paired: true, ..develop };
        let (n, picture, out) = run(paired.clone(), 0x133, 0x1200, 32);
        assert_eq!(n, 1);
        assert_eq!(picture[0x200..0x210], lines);
        assert_eq!(out[..16], lines, "the 1:1 paired resize copies both lines");
        let (n, _, _) = run(paired, 0x100, 0x1200, 32);
        assert_eq!(n, 0);
    }

    #[test]
    fn a_developing_resize_fills_its_source_picture_from_the_raw_first() {
        use fujimod_emulator_devices::test_host::{TestHost, advance, write};
        let resize = Resize {
            source_size: 0x0c,
            output_size: 0x18,
            ratio: (0x1c, 0x20),
            source: (0xc4, 0xd0),
            output: (0xd8, 0xe4),
            linked_bit31: false,
            paired_rows: None,
            origin: None,
        };
        let develop = Develop { raw: (0x3000, 16, 6, 3), origin: (1, 1), picture: (0x1000, 0x40), shift: 2, color: None, window: None, ring: None, y0l2: false, paired: false };
        let options = Options {
            base: 0xfff7e000,
            size: 0x1000,
            ready: (0, 1),
            start: (4, 1),
            acknowledge: None,
            status: Some((0x10c, 0x0100_0100, 1)),
            resize: Some(resize),
            develop: Some(develop.clone()),
            irq: 132,
            ticks: 10,
        };
        assert!(JobAck::new(Options { resize: None, ..options.clone() }).is_err());
        assert!(
            JobAck::new(Options { develop: Some(Develop { origin: (6, 0), ..develop }), ..options.clone() })
                .is_err()
        );
        let mut page = JobAck::new(options).unwrap();
        let mut h = TestHost::new();
        // RAW 6 x 3 samples at a 16-byte pitch: sample = 40 * row + 4 * column.
        for y in 0..3u64 {
            for x in 0..6u64 {
                let at = (0x3000 + y * 16 + x * 2) as usize;
                h.ram[at..at + 2].copy_from_slice(&((40 * y + 4 * x) as u16).to_le_bytes());
            }
        }
        let w = |p: &mut JobAck, h: &mut TestHost, o: u64, v: u32| write(p, h, 0xfff7e000 + o, v);
        // 4 x 2 picture at 0x1000 (pitch 8), copied 1:1 to 0x2000 (pitch 8).
        w(&mut page, &mut h, 0x0c, 1 << 16 | 3);
        w(&mut page, &mut h, 0x18, 1 << 16 | 3);
        w(&mut page, &mut h, 0x1c, 0x1000);
        w(&mut page, &mut h, 0x20, 0x1000);
        w(&mut page, &mut h, 0xc4, 0x1000);
        w(&mut page, &mut h, 0xd0, 8);
        w(&mut page, &mut h, 0xd8, 0x2000);
        w(&mut page, &mut h, 0xe4, 8);
        w(&mut page, &mut h, 0x04, 1);
        advance(&mut page, &mut h, 10);
        // Window from (1, 1): luma = sample >> 2, neutral chroma.
        assert_eq!(&h.ram[0x1000..0x1008], &[11, 12, 0x80, 0x80, 13, 14, 0x80, 0x80]);
        assert_eq!(&h.ram[0x1008..0x1010], &[21, 22, 0x80, 0x80, 23, 24, 0x80, 0x80]);
        assert_eq!(&h.ram[0x2000..0x2008], &h.ram[0x1000..0x1008].to_vec()[..]);
        assert_eq!((page.state.developed, page.state.resized), (1, 1));
        // A source outside the picture is resized as it is.
        w(&mut page, &mut h, 0x10c, 1);
        w(&mut page, &mut h, 0xc4, 0x2000);
        w(&mut page, &mut h, 0xd8, 0x2800);
        w(&mut page, &mut h, 0x04, 0);
        w(&mut page, &mut h, 0x04, 1);
        advance(&mut page, &mut h, 20);
        assert_eq!((page.state.developed, page.state.resized), (1, 2));
    }

    #[test]
    fn a_color_development_follows_the_x_trans_sites_of_the_raw() {
        use crate::development::XTRANS;
        use fujimod_emulator_devices::test_host::{TestHost, advance, write};
        let resize = Resize {
            source_size: 0x0c,
            output_size: 0x18,
            ratio: (0x1c, 0x20),
            source: (0xc4, 0xd0),
            output: (0xd8, 0xe4),
            linked_bit31: false,
            paired_rows: None,
            origin: None,
        };
        // RAW 14 x 8 at a 32-byte pitch; window 12 x 6 from (1, 1).
        let develop = |color| Develop {
            raw: (0x3000, 32, 14, 8),
            origin: (1, 1),
            picture: (0x1000, 0x200),
            shift: 6,
            color,
            window: None,
            ring: None,
            y0l2: false,
            paired: false,
        };
        let run = |color: Option<(usize, usize)>, paint: (usize, usize), red: bool| {
            let options = Options {
                base: 0xfff7e000,
                size: 0x1000,
                ready: (0, 1),
                start: (4, 1),
                acknowledge: None,
                status: Some((0x10c, 0x0100_0100, 1)),
                resize: Some(resize.clone()),
                develop: Some(develop(color)),
                irq: 132,
                ticks: 10,
            };
            let mut page = JobAck::new(options).unwrap();
            let mut h = TestHost::new();
            for y in 0..8usize {
                for x in 0..14usize {
                    let site = XTRANS[(y + paint.0) % 6][(x + paint.1) % 6];
                    // Grey: every site at mid level; red: only red sites lit.
                    let sample: u16 = if !red || site == 0 { 1024 + 8192 } else { 1024 };
                    let at = 0x3000 + y * 32 + x * 2;
                    h.ram[at..at + 2].copy_from_slice(&sample.to_le_bytes());
                }
            }
            let w = |p: &mut JobAck, h: &mut TestHost, o: u64, v: u32| write(p, h, 0xfff7e000 + o, v);
            w(&mut page, &mut h, 0x0c, 5 << 16 | 11);
            w(&mut page, &mut h, 0x18, 5 << 16 | 11);
            w(&mut page, &mut h, 0x1c, 0x1000);
            w(&mut page, &mut h, 0x20, 0x1000);
            w(&mut page, &mut h, 0xc4, 0x1000);
            w(&mut page, &mut h, 0xd0, 24);
            w(&mut page, &mut h, 0xd8, 0x2000);
            w(&mut page, &mut h, 0xe4, 24);
            w(&mut page, &mut h, 0x04, 1);
            advance(&mut page, &mut h, 10);
            assert_eq!(page.state.developed, 1);
            h.ram[0x1000..0x1000 + 24 * 6].to_vec()
        };
        // A neutral RAW develops neutral, whatever the phase.
        let grey = run(Some((5, 1)), (5, 1), false);
        for pair in grey.chunks_exact(4) {
            assert_eq!(pair[0], pair[1]);
            assert!(pair[0] > 150, "mid level is bright in sRGB");
            assert_eq!((pair[2], pair[3]), (128, 128));
        }
        // Only red sites lit: red pixels (Cr high, Cb low), at the right phase.
        let red = run(Some((5, 1)), (5, 1), true);
        for pair in red.chunks_exact(4) {
            assert!(pair[3] > 200 && pair[2] < 128, "{pair:?}");
        }
        // X-Trans repeats itself shifted by (3, 3): the phase is defined modulo it.
        assert_eq!(run(Some((2, 4)), (5, 1), true), red);
        // Developed one sample off, the same RAW reads other colors.
        assert_ne!(run(Some((5, 2)), (5, 1), true), red);
        // A site outside the 6 x 6 tile is refused.
        let options = Options {
            base: 0xfff7e000,
            size: 0x1000,
            ready: (0, 1),
            start: (4, 1),
            acknowledge: Some((0xfff7f000, 1)),
            status: None,
            resize: Some(resize.clone()),
            develop: Some(develop(Some((6, 0)))),
            irq: 132,
            ticks: 10,
        };
        assert!(JobAck::new(options.clone()).is_err());
        assert!(JobAck::new(Options { develop: Some(develop(Some((5, 1)))), ..options }).is_ok());
    }
}
