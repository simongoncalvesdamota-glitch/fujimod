//! Scene-derived IBUF color sums, with the stock 16x16/64-byte-row layout.
//! Unmapped statistic fields remain explore accesses; see ae-pipeline.md.
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use super::sensor::{SensorExposure, SensorState};

#[derive(Clone, Copy)]
pub enum Planes {
    Color,
    Rgb,
    Ae,
    /// Zone grid (X-T3 5.11 destination 0x244): word (row, column) at
    /// row * 0x40 + column * 4; high u16 = zone luminance, low u16 = 0.
    Luma { shift: u8 },
    /// Ten 0x20-byte window records (X-T3 5.11 destination 0x25c): `sites`
    /// in the first window, `rest` in each of the nine others.
    Records { sites: u32, rest: u32 },
    /// 256-class luminance histogram (X-T3 5.11 destination 0x2b4).
    Histogram { side: u16 },
    /// Optical-black block (X-T3 5.11 destinations 0x274 / 0x27c / 0x284):
    /// four u64 sums of black_level x samples. Without fixed `samples`, the
    /// window is read from the input-buffer words `window` (width - 1 in bits
    /// 0..5) and `window + 4` (height - 1 in bits 0..11), and each of its
    /// sites counts `weight` samples.
    Black {
        window: u64,
        weight: u32,
        samples: Option<u32>,
    },
}
impl Planes {
    /// Whether an unprogrammed destination withdraws the output's frame-cause
    /// bit (the stock colour outputs). The statistics outputs added by
    /// [`StatisticsOutputs`] never touch the input-buffer status.
    pub fn completes(self) -> bool {
        matches!(self, Planes::Color | Planes::Rgb | Planes::Ae)
    }
    /// The output with its sample count resolved from the input-buffer
    /// registers (`register` reads one word).
    pub fn resolve(self, register: impl Fn(u64) -> u32) -> Self {
        match self {
            Planes::Black {
                window,
                weight,
                samples: None,
            } => {
                let width = (register(window) & 0x3f) + 1;
                let height = (register(window + 4) & 0xfff) + 1;
                Planes::Black {
                    window,
                    weight,
                    samples: Some(width * height * weight),
                }
            }
            planes => planes,
        }
    }
}

/// X-T3 5.11 statistics outputs beyond the 0x254 colour sums (opt-in). Each
/// is written at the destination the firmware programs in its own
/// input-buffer register pair (low, low + 4), on the frame that serves enable
/// bit 0x100 of word 0, with no completion bit of its own. Every value comes
/// from the statistics scene (the one the RAW read-out paints), scaled by
/// the followed sensor gain; the black level is never scaled.
/// Layouts and reliability: spec-lot-sorties-statistiques.md.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatisticsOutputs {
    /// Zones of the 0x244 grid (1..=16 each; firmware 5.11 live view reads 1x1).
    #[serde(default = "grid_side")]
    pub grid_columns: usize,
    #[serde(default = "grid_side")]
    pub grid_rows: usize,
    /// Right shift of the black-free RAW14 zone luminance (0..=8).
    #[serde(default = "grid_shift")]
    pub grid_shift: u8,
    /// Sensor sites in the first 0x25c window (horizontal band 0); R, G and B
    /// take 2/9, 5/9 and 2/9 of them (X-Trans counts). Firmware 5.11 removes
    /// black x width x height x [2, 5, 2, 5] / 9 from each window sum
    /// (FUN_02082228, counts FUN_01f5c08c, at least 6 x 6) with the window
    /// sizes it keeps at 0x2e6b428 + 0xb0: 153 x 153 for window 0 in live
    /// view (run T58h). Any other count leaves black in the sum, which the
    /// AE reads as light (45864 sites held the live view at 1/32000).
    #[serde(default = "record_sites")]
    pub record_sites: u32,
    /// Sensor sites in each of the nine other 0x25c windows. Firmware 5.11
    /// programs them 1 x 1 in live view: one site gives no R, G or B sample
    /// with the integer shares, so their sums are 0.
    #[serde(default = "record_rest_sites")]
    pub record_rest_sites: u32,
    /// The 0x2b4 histogram samples `side` x `side` scene points (1..=1024).
    #[serde(default = "histogram_side")]
    pub histogram_side: u16,
    /// Samples summed per channel in each optical-black block (each sum is
    /// black_level x samples). None (default): the window the firmware
    /// programs in input-buffer words 0x1ac / 0x1b4 / 0x1bc (width) and
    /// 0x1b0 / 0x1b8 / 0x1c0 (height), as Runs photo § 18 reads them.
    #[serde(default)]
    pub ob_samples: Option<u32>,
    /// Samples per window site (1..=16, default 4: RAW14 black on the
    /// 16-bit base). Firmware 5.11 takes black = sum / (4 x width x height)
    /// (sum << 9 / area, then >> 11 in FUN_01db16b8): with 4, the black
    /// register 0xfff50184 follows the scene black (T48: 0x03ff03ff for
    /// 1024) instead of its 128 floor.
    #[serde(default = "ob_site_weight")]
    pub ob_site_weight: u32,
}
fn grid_side() -> usize {
    7
}
fn grid_shift() -> u8 {
    2
}
fn record_sites() -> u32 {
    153 * 153
}
fn record_rest_sites() -> u32 {
    1
}
fn histogram_side() -> u16 {
    64
}
fn ob_site_weight() -> u32 {
    4
}
impl Default for StatisticsOutputs {
    fn default() -> Self {
        Self {
            grid_columns: grid_side(),
            grid_rows: grid_side(),
            grid_shift: grid_shift(),
            record_sites: record_sites(),
            record_rest_sites: record_rest_sites(),
            histogram_side: histogram_side(),
            ob_samples: None,
            ob_site_weight: ob_site_weight(),
        }
    }
}
impl StatisticsOutputs {
    pub fn check(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.grid_columns) || !(1..=16).contains(&self.grid_rows) {
            return Err("statistics grid must be 1..16 zones a side".into());
        }
        if self.grid_shift > 8 {
            return Err("statistics grid shift must be 0..8".into());
        }
        if self.record_sites == 0 || self.record_rest_sites == 0 || self.ob_samples == Some(0) {
            return Err("statistics record sites and black samples must be positive".into());
        }
        if !(1..=16).contains(&self.ob_site_weight) {
            return Err("statistics black site weight must be 1..16".into());
        }
        if !(1..=1024).contains(&self.histogram_side) {
            return Err("statistics histogram side must be 1..1024".into());
        }
        Ok(())
    }
    /// (low destination register, columns, rows, planes) of each output, in
    /// write order: grid, records, histogram, then the three black blocks.
    pub fn outputs(&self) -> Vec<(u64, usize, usize, Planes)> {
        let black = |window| Planes::Black {
            window,
            weight: self.ob_site_weight,
            samples: self.ob_samples,
        };
        vec![
            (
                0x244,
                self.grid_columns,
                self.grid_rows,
                Planes::Luma {
                    shift: self.grid_shift,
                },
            ),
            (
                0x25c,
                1,
                1,
                Planes::Records {
                    sites: self.record_sites,
                    rest: self.record_rest_sites,
                },
            ),
            (
                0x2b4,
                1,
                1,
                Planes::Histogram {
                    side: self.histogram_side,
                },
            ),
            (0x274, 1, 1, black(0x1ac)),
            (0x27c, 1, 1, black(0x1b4)),
            (0x284, 1, 1, black(0x1bc)),
        ]
    }
}

/// Live-view picture of the statistics scene for the MOVDC outputs (opt-in).
/// Not an ISP model: each channel's black-free RAW14 signal (gain followed,
/// clipped like the RAW read-out) is divided by `white`, encoded with a pure
/// power law 1 / `gamma`, then converted with BT.601 limited-range
/// coefficients (Y 16..235, Cb / Cr 16..240). No demosaic, white balance,
/// colour matrix, tone curve or noise: the camera's live-view rendering is
/// unknown (50 %).
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewPicture {
    /// Black-free RAW14 signal shown as white (1..=16383). None (default):
    /// the full scale above the scene black (16383 - black_level).
    #[serde(default)]
    pub white: Option<u16>,
    /// Encoding exponent: value = linear ^ (1 / gamma), 1.0..=4.0 (default 2.2).
    #[serde(default = "view_gamma")]
    pub gamma: f64,
}
fn view_gamma() -> f64 {
    2.2
}
impl Default for ViewPicture {
    fn default() -> Self {
        Self {
            white: None,
            gamma: view_gamma(),
        }
    }
}
impl ViewPicture {
    pub fn check(&self) -> Result<(), String> {
        if self.white.is_some_and(|w| w == 0 || w > 16383) || !(1.0..=4.0).contains(&self.gamma) {
            return Err("MOVDC scene picture needs white 1..=16383 and gamma 1..=4".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Scene {
    name: String,
    width: usize,
    height: usize,
    rgb: Vec<[u16; 3]>,
    samples_per_cell: u32,
    #[serde(default = "black_level")]
    black_level: u16,
    #[serde(default = "ae_area")]
    ae_cell_area: u32,
    #[serde(default = "signal_shift")]
    signal_shift: u8,
}
fn black_level() -> u16 {
    1024
}
fn ae_area() -> u32 {
    120 * 78
}
fn signal_shift() -> u8 {
    2
}
impl Default for Scene {
    fn default() -> Self {
        Self {
            name: "mid-gray (linear12=2048)".into(),
            width: 1,
            height: 1,
            rgb: vec![[2048; 3]],
            samples_per_cell: 1024,
            black_level: black_level(),
            ae_cell_area: ae_area(),
            signal_shift: signal_shift(),
        }
    }
}
impl Scene {
    fn load(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("scene must be an object")?;
        if let Some(key) = object.keys().find(|key| {
            !matches!(
                key.as_str(),
                "name"
                    | "width"
                    | "height"
                    | "rgb"
                    | "luma"
                    | "samples_per_cell"
                    | "black_level"
                    | "ae_cell_area"
                    | "signal_shift"
            )
        }) {
            return Err(format!("unknown scene input {key}"));
        }
        let integer = |key: &str, fallback: usize| -> Result<usize, String> {
            value.get(key).map_or(Ok(fallback), |v| {
                v.as_i64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("scene {key} must be positive"))
            })
        };
        let width = integer("width", 1)?;
        let height = integer("height", 1)?;
        let count = width
            .checked_mul(height)
            .filter(|n| *n <= 1_048_576)
            .ok_or("scene dimensions exceed 1M pixels")?;
        let sample = |v: &Value| -> Result<u16, String> {
            v.as_i64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or("scene samples must be linear radiance (0..65535)".into())
        };
        let rgb = if let Some(values) = value.get("rgb").and_then(Value::as_array) {
            if values.len() != count * 3 {
                return Err("scene rgb requires width*height*3 samples".into());
            }
            values
                .chunks_exact(3)
                .map(|p| Ok([sample(&p[0])?, sample(&p[1])?, sample(&p[2])?]))
                .collect::<Result<Vec<_>, String>>()?
        } else if let Some(values) = value.get("luma").and_then(Value::as_array) {
            if values.len() != count {
                return Err("scene luma requires width*height samples".into());
            }
            values
                .iter()
                .map(|v| Ok([sample(v)?; 3]))
                .collect::<Result<Vec<_>, String>>()?
        } else {
            vec![[sample(value.get("luma").ok_or("scene requires luma or rgb")?)?; 3]; count]
        };
        let samples_per_cell = u32::try_from(integer("samples_per_cell", 1024)?)
            .ok()
            .filter(|n| *n <= u32::MAX / 16383)
            .ok_or("scene cell sum overflows u32")?;
        Ok(Self {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("scene")
                .to_owned(),
            width,
            height,
            rgb,
            samples_per_cell,
            black_level: value
                .get("black_level")
                .map(sample)
                .transpose()?
                .unwrap_or_else(black_level),
            ae_cell_area: u32::try_from(integer("ae_cell_area", ae_area() as usize)?)
                .ok()
                .filter(|n| *n <= 65536)
                .ok_or("scene AE cell area exceeds 65536")?,
            signal_shift: value
                .get("signal_shift")
                .map(|v| {
                    v.as_i64()
                        .and_then(|n| u8::try_from(n).ok())
                        .filter(|n| *n <= 2)
                        .ok_or("scene signal_shift must be 0..2".to_string())
                })
                .transpose()?
                .unwrap_or_else(signal_shift),
        })
    }
    /// One RAW14 sample per sensor site, as the statistics cells see it:
    /// signal << shift plus the black level, clipped to 14 bits.
    /// The sensor gain scales the signal, not the black level; a gain of 1
    /// keeps the exact integer law.
    fn raw(
        &self,
        channel: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        gain: f64,
    ) -> u16 {
        let px = x * self.width / width;
        let py = y * self.height / height;
        let signal = u32::from(self.rgb[py * self.width + px][channel]) << self.signal_shift;
        let signal = if gain == 1.0 {
            signal
        } else {
            (f64::from(signal) * gain).round().min(16383.0) as u32
        };
        (signal + u32::from(self.black_level)).min(16383) as u16
    }
    /// Mean RAW14 sample of each channel over zone (x, y) of a `columns` x
    /// `rows` split of the frame (same exact-area law as [`Scene::planes`]).
    fn zone(&self, x: usize, y: usize, columns: usize, rows: usize, gain: f64) -> [f64; 3] {
        let (left, right) = (x * self.width, (x + 1) * self.width);
        let (top, bottom) = (y * self.height, (y + 1) * self.height);
        let mut weighted = [0.0; 3];
        for py in top / rows..bottom.div_ceil(rows) {
            let dy = bottom.min((py + 1) * rows) - top.max(py * rows);
            for px in left / columns..right.div_ceil(columns) {
                let dx = right.min((px + 1) * columns) - left.max(px * columns);
                for channel in 0..3 {
                    let signal = f64::from(self.rgb[py * self.width + px][channel])
                        * f64::from(1u32 << self.signal_shift)
                        * gain;
                    let raw = (signal + f64::from(self.black_level)).clamp(0.0, 16383.0);
                    weighted[channel] += raw * (dx * dy) as f64;
                }
            }
        }
        weighted.map(|w| w / (self.width * self.height) as f64)
    }
    /// 0x244 grid: high u16 = (R + 2G + B) / 4 - black, shifted right.
    fn luma(&self, columns: usize, rows: usize, shift: u8, gain: f64) -> Vec<u8> {
        let mut bytes = vec![0; 0x400];
        for y in 0..rows {
            for x in 0..columns {
                let [r, g, b] = self.zone(x, y, columns, rows, gain);
                let luma = ((r + 2.0 * g + b) / 4.0 - f64::from(self.black_level)).max(0.0);
                let high = ((luma.round() as u32) >> shift).min(0xffff);
                let at = y * 0x40 + x * 4;
                bytes[at..at + 4].copy_from_slice(&(high << 16).to_le_bytes());
            }
        }
        bytes
    }
    /// 0x25c records: ten horizontal bands, u64 R, G, B sums (black
    /// included), +0x18 = 0; band 0 over `sites` sites, the others over
    /// `rest` sites each.
    fn records(&self, sites: u32, rest: u32, gain: f64) -> Vec<u8> {
        let mut bytes = vec![0; 10 * 0x20];
        for band in 0..10 {
            let mean = self.zone(0, band, 1, 10, gain);
            let band_sites = if band == 0 { sites } else { rest };
            for (channel, share) in [2u64, 5, 2].into_iter().enumerate() {
                let samples = u64::from(band_sites) * share / 9;
                let sum = (mean[channel] * samples as f64).round() as u64;
                let at = band * 0x20 + channel * 8;
                bytes[at..at + 8].copy_from_slice(&sum.to_le_bytes());
            }
        }
        bytes
    }
    /// 0x2b4 histogram: 256 u32 counts of ((R + 2G + B) / 4 - black) >> 6
    /// over `side` x `side` scene points (nearest pixel).
    fn histogram(&self, side: u16, gain: f64) -> Vec<u8> {
        let side = usize::from(side);
        let mut counts = [0u32; 256];
        for y in 0..side {
            for x in 0..side {
                let [r, g, b] = [0, 1, 2].map(|c| u32::from(self.raw(c, x, y, side, side, gain)));
                let luma = ((r + 2 * g + b) / 4).saturating_sub(u32::from(self.black_level));
                counts[((luma >> 6) as usize).min(255)] += 1;
            }
        }
        counts.iter().flat_map(|n| n.to_le_bytes()).collect()
    }
    /// Optical-black block: four u64 channel sums of black_level x samples.
    fn black(&self, samples: u32) -> Vec<u8> {
        let sum = u64::from(self.black_level) * u64::from(samples);
        (0..4).flat_map(|_| sum.to_le_bytes()).collect()
    }
    fn planes(&self, columns: usize, rows: usize, ae: bool, gain: f64) -> Vec<u8> {
        let mut bytes = vec![0; 0x1000];
        // The scene spans the configured grid, not the unused 16x16 padding.
        // Equal green channels are a virtual Bayer sampling choice.
        for y in 0..rows {
            for x in 0..columns {
                // Exact area weights in integer coordinates retain small bright
                // regions that a center-point sampler would miss entirely.
                let (left, right) = (x * self.width, (x + 1) * self.width);
                let (top, bottom) = (y * self.height, (y + 1) * self.height);
                let mut weighted = [0.0; 3];
                for py in top / rows..bottom.div_ceil(rows) {
                    let dy = bottom.min((py + 1) * rows) - top.max(py * rows);
                    for px in left / columns..right.div_ceil(columns) {
                        let dx = right.min((px + 1) * columns) - left.max(px * columns);
                        for channel in 0..3 {
                            let signal = f64::from(self.rgb[py * self.width + px][channel])
                                * f64::from(1u32 << self.signal_shift)
                                * gain;
                            let raw = (signal + f64::from(self.black_level)).clamp(0.0, 16383.0);
                            weighted[channel] += raw * (dx * dy) as f64;
                        }
                    }
                }
                for (plane, channel) in [0, 1, 2, 1].into_iter().enumerate() {
                    let samples = if ae {
                        self.ae_cell_area * [2, 5, 2, 5][plane] / 9
                    } else {
                        self.samples_per_cell
                    };
                    let raw = weighted[channel] / (self.width * self.height) as f64;
                    let sum = (raw * f64::from(samples)).round() as u32;
                    let at = plane * 0x400 + y * 64 + x * 4;
                    bytes[at..at + 4].copy_from_slice(&sum.to_le_bytes());
                }
            }
        }
        bytes
    }
}
#[derive(Serialize, Deserialize)]
struct State {
    scene: Scene,
    bytes: Vec<u8>,
    frames: u64,
    last_destination: u64,
}
/// Where the statistics read the sensor gain (opt-in, not checkpointed:
/// the gain registers live in the sensor state).
type GainSource = Option<(Arc<Mutex<SensorState>>, SensorExposure, Arc<AtomicU32>)>;
#[derive(Clone)]
pub struct Statistics {
    state: Arc<Mutex<State>>,
    gain: Arc<Mutex<GainSource>>,
    base: u64,
    bank_size: usize,
    /// Guest-visible SRAM bytes: two banks by default; a body can declare more
    /// (X-T3 5.11 copies 0xd800 still-capture bytes from the second bank).
    window: usize,
}
/// X-T3 5.11 X-Trans colour filter (unit 6, 0x22073f8): 0 = R, 1 = G, 2 = B.
pub const XTRANS: [u8; 36] = [
    1, 0, 1, 1, 2, 1, //
    2, 1, 2, 0, 1, 0, //
    1, 0, 1, 1, 2, 1, //
    1, 2, 1, 1, 0, 1, //
    0, 1, 0, 2, 1, 2, //
    1, 2, 1, 1, 0, 1, //
];
impl Statistics {
    /// The scene as a RAW14-in-LE16 mosaic of `width` x `height` sites: the
    /// scene is stretched over the frame (nearest pixel) and each site keeps
    /// the channel its X-Trans filter passes.
    pub fn paint_raw(&self, width: usize, height: usize) -> Vec<u8> {
        self.paint_raw_scaled(width, height, 1.0)
    }
    /// [`Statistics::paint_raw`] with the signal scaled by the sensor `gain`.
    pub fn paint_raw_scaled(&self, width: usize, height: usize, gain: f64) -> Vec<u8> {
        let state = self.state.lock().unwrap();
        let scene = &state.scene;
        let mut bytes = Vec::with_capacity(width * height * 2);
        for y in 0..height {
            for x in 0..width {
                let channel = usize::from(XTRANS[(y % 6) * 6 + x % 6]);
                bytes.extend_from_slice(&scene.raw(channel, x, y, width, height, gain).to_le_bytes());
            }
        }
        bytes
    }
    /// The scene as packed Y0,Y1,Cb,Cr rows of `width` (even) x `height`
    /// points, stretched like [`Statistics::paint_raw`] (nearest pixel), with
    /// the followed sensor gain. Chroma is the mean of each pixel pair.
    pub fn paint_view(&self, width: usize, height: usize, law: &ViewPicture) -> Vec<u8> {
        let gain = self.gain();
        let state = self.state.lock().unwrap();
        let scene = &state.scene;
        let black = u32::from(scene.black_level);
        let white = f64::from(law.white.map_or(16383u32.saturating_sub(black).max(1), u32::from));
        // Y', Cb, Cr of each scene pixel (the stretch only repeats them).
        let colours: Vec<[f64; 3]> = (0..scene.width * scene.height)
            .map(|i| {
                let (x, y) = (i % scene.width, i / scene.width);
                let v = |c: usize| {
                    let raw = u32::from(scene.raw(c, x, y, scene.width, scene.height, gain));
                    (f64::from(raw.saturating_sub(black)) / white)
                        .min(1.0)
                        .powf(1.0 / law.gamma)
                };
                let (r, g, b) = (v(0), v(1), v(2));
                let luma = 0.299 * r + 0.587 * g + 0.114 * b;
                [luma, (b - luma) / 1.772, (r - luma) / 1.402]
            })
            .collect();
        let byte = |v: f64, lo: f64, span: f64| (lo + span * v).round().clamp(0.0, 255.0) as u8;
        let mut bytes = Vec::with_capacity(width * height * 2);
        for y in 0..height {
            let py = y * scene.height / height;
            for x in (0..width.saturating_sub(1)).step_by(2) {
                let a = colours[py * scene.width + x * scene.width / width];
                let b = colours[py * scene.width + (x + 1) * scene.width / width];
                bytes.extend_from_slice(&[
                    byte(a[0], 16.0, 219.0),
                    byte(b[0], 16.0, 219.0),
                    byte((a[1] + b[1]) / 2.0, 128.0, 224.0),
                    byte((a[2] + b[2]) / 2.0, 128.0, 224.0),
                ]);
            }
        }
        bytes
    }
    pub fn new(base: u64, bank_size: usize) -> Result<Self, String> {
        if base & 31 != 0
            || !(0x1c60..=0x100000).contains(&bank_size)
            || base.checked_add(bank_size as u64 * 2).is_none()
        {
            return Err("invalid statistics SRAM wiring".into());
        }
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                scene: Scene::default(),
                bytes: vec![0; bank_size * 2],
                frames: 0,
                last_destination: 0,
            })),
            gain: Arc::default(),
            base,
            bank_size,
            window: bank_size * 2,
        })
    }

    /// Scale the cell sums by the gain latched in `sensor` (shared by every
    /// clone of these statistics).
    /// `tg_vmax` mirrors the timing generator VMAX word for the exposure
    /// time law.
    pub fn follow_exposure(
        &self,
        sensor: Arc<Mutex<SensorState>>,
        exposure: SensorExposure,
        tg_vmax: Arc<AtomicU32>,
    ) {
        *self.gain.lock().unwrap() = Some((sensor, exposure, tg_vmax));
    }

    /// The sensor gain factor now (1 when not followed). Takes the sensor
    /// lock alone, before any statistics lock.
    fn gain(&self) -> f64 {
        let source = self.gain.lock().unwrap().clone();
        source.map_or(1.0, |(sensor, exposure, tg_vmax)| {
            let vmax = tg_vmax.load(Ordering::Relaxed) as u16;
            exposure.factor_at(&sensor.lock().unwrap().registers, vmax)
        })
    }

    /// Widens the SRAM window beyond the two statistics banks (opt-in). The
    /// extra bytes are plain storage: they read back their last write.
    pub fn with_window(self, window: usize) -> Result<Self, String> {
        if window < self.bank_size * 2
            || window > 0x100000
            || window & 31 != 0
            || self.base.checked_add(window as u64).is_none()
        {
            return Err("invalid statistics SRAM window".into());
        }
        self.state.lock().unwrap().bytes.resize(window, 0);
        Ok(Self { window, ..self })
    }

    pub fn window(&self) -> usize {
        self.window
    }

    pub fn complete(
        &self,
        cx: &mut Cx<'_>,
        destination: u64,
        columns: usize,
        rows: usize,
        planes: Planes,
    ) -> Result<(), String> {
        if destination == 0 {
            return Ok(());
        }
        let gain = self.gain();
        let mut s = self.state.lock().unwrap();
        if !(1..=16).contains(&columns) || !(1..=16).contains(&rows) {
            return Err("statistics grid exceeds 16x16 hardware cells".into());
        }
        let bytes = match planes {
            Planes::Luma { shift } => s.scene.luma(columns, rows, shift, gain),
            Planes::Records { sites, rest } => s.scene.records(sites, rest, gain),
            Planes::Histogram { side } => s.scene.histogram(side, gain),
            Planes::Black { samples, .. } => s.scene.black(samples.unwrap_or(0)),
            Planes::Color | Planes::Rgb | Planes::Ae => {
                let mut bytes = s
                    .scene
                    .planes(columns, rows, matches!(planes, Planes::Ae), gain);
                if !matches!(planes, Planes::Color) {
                    bytes.truncate(0xc00);
                }
                bytes
            }
        };
        if destination >= self.base
            && destination
                .checked_add(bytes.len() as u64)
                .is_some_and(|end| end <= self.base + self.window as u64)
        {
            let at = (destination - self.base) as usize;
            s.bytes[at..at + bytes.len()].copy_from_slice(&bytes);
        } else {
            cx.ram_write(destination, &bytes)?;
        }
        // Frames count the stock colour outputs only (checkpointed as before).
        if planes.completes() {
            s.frames += 1;
            s.last_destination = destination;
        }
        Ok(())
    }
}
impl Device for Statistics {
    fn name(&self) -> &'static str {
        "imaging-statistics-sram"
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        let Some(at) = address
            .checked_sub(self.base)
            .and_then(|n| usize::try_from(n).ok())
        else {
            cx.unmodeled();
            return 0;
        };
        if !matches!(size, 1 | 2 | 4 | 8)
            || at
                .checked_add(size as usize)
                .is_none_or(|end| end > self.window)
        {
            cx.unmodeled();
            return 0;
        }
        // Only four color planes are decoded. Other fields (X-T3 5.11 reads
        // +0x2060.. after live view starts) read back the SRAM bytes, which
        // the engine never writes: they read their last host or guest write.
        let s = self.state.lock().unwrap();
        let mut bytes = [0; 8];
        bytes[..size as usize].copy_from_slice(&s.bytes[at..at + size as usize]);
        u64::from_le_bytes(bytes)
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        let Some(at) = address
            .checked_sub(self.base)
            .and_then(|n| usize::try_from(n).ok())
        else {
            cx.unmodeled();
            return;
        };
        if !matches!(size, 1 | 2 | 4 | 8)
            || at
                .checked_add(size as usize)
                .is_none_or(|end| end > self.window)
        {
            cx.unmodeled();
            return;
        }
        self.state.lock().unwrap().bytes[at..at + size as usize]
            .copy_from_slice(&value.to_le_bytes()[..size as usize]);
    }
    fn set_scene(&mut self, input: &Value) -> Result<bool, String> {
        let scene = Scene::load(input)?;
        self.state.lock().unwrap().scene = scene;
        Ok(true)
    }
    fn save(&self) -> Value {
        serde_json::to_value(&*self.state.lock().unwrap()).unwrap()
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value.clone())
            .map_err(|e| format!("statistics checkpoint: {e}"))?;
        if state.bytes.len() != self.window
            || state.scene.width.checked_mul(state.scene.height) != Some(state.scene.rgb.len())
        {
            return Err("statistics checkpoint dimensions disagree".into());
        }
        *self.state.lock().unwrap() = state;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn word(bytes: &[u8], plane: usize, x: usize, y: usize) -> u32 {
        let at = plane * 0x400 + y * 64 + x * 4;
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }
    #[test]
    fn a_wider_window_keeps_the_banks_and_adds_plain_storage() {
        let banks = Statistics::new(0xfff10000, 0x7000).unwrap();
        assert_eq!(banks.window(), 0xe000);
        assert!(banks.clone().with_window(0xd000).is_err());
        assert!(banks.clone().with_window(0x20010).is_err());
        let wide = Statistics::new(0xfff10000, 0x7000)
            .unwrap()
            .with_window(0x20000)
            .unwrap();
        assert_eq!(wide.window(), 0x20000);
        let saved = wide.save();
        assert_eq!(saved["bytes"].as_array().unwrap().len(), 0x20000);
        let mut narrow = Statistics::new(0xfff10000, 0x7000).unwrap();
        assert!(narrow.restore(&saved).is_err());
    }

    #[test]
    fn a_painted_raw_is_the_scene_seen_through_the_x_trans_filter() {
        let mut statistics = Statistics::new(0x100000, 0x7000).unwrap();
        // Left half red-ish, right half blue-ish; black 1024, shift 2.
        statistics
            .set_scene(&serde_json::json!({
                "width": 2, "height": 1,
                "rgb": [1000, 500, 100, 100, 500, 3000],
                "black_level": 1024, "signal_shift": 2
            }))
            .unwrap();
        let bytes = statistics.paint_raw(12, 6);
        assert_eq!(bytes.len(), 12 * 6 * 2);
        let at = |x: usize, y: usize| u16::from_le_bytes([bytes[(y * 12 + x) * 2], bytes[(y * 12 + x) * 2 + 1]]);
        let rgb = [[1000u16, 500, 100], [100, 500, 3000]];
        for y in 0..6 {
            for x in 0..12 {
                let channel = usize::from(XTRANS[(y % 6) * 6 + x % 6]);
                let expected = (u32::from(rgb[x / 6][channel]) * 4 + 1024).min(16383) as u16;
                assert_eq!(at(x, y), expected, "site ({x}, {y})");
            }
        }
        // Site (1, 0) is R in the left half, (10, 0) is B in the right half.
        assert_eq!(at(1, 0), 1000 * 4 + 1024);
        assert_eq!(at(10, 0), 3000 * 4 + 1024);
        assert_eq!(XTRANS.iter().filter(|&&c| c == 1).count(), 20);
    }

    #[test]
    fn area_average_keeps_a_small_bright_region() {
        let mut scene = Scene::default();
        scene.width = 20;
        scene.height = 20;
        scene.rgb = (0..400)
            .map(|i| {
                if i % 20 < 5 && i / 20 < 4 {
                    [4000; 3]
                } else {
                    [0; 3]
                }
            })
            .collect();
        scene.samples_per_cell = 1;
        scene.black_level = 0;
        scene.signal_shift = 0;
        assert_eq!(word(&scene.planes(1, 1, false, 1.0), 0, 0, 0), 200);
        assert_eq!(word(&scene.planes(7, 7, false, 1.0), 0, 1, 1), 1200);
        assert_eq!(word(&scene.planes(7, 7, false, 1.0), 0, 6, 6), 0);
    }
    #[test]
    fn subpixel_cells_preserve_rgb_and_padding() {
        let scene = Scene {
            rgb: vec![[1, 2, 3]],
            samples_per_cell: 1024,
            black_level: 0,
            signal_shift: 0,
            ..Scene::default()
        };
        let bytes = scene.planes(7, 7, false, 1.0);
        assert_eq!(
            [
                word(&bytes, 0, 6, 6),
                word(&bytes, 1, 6, 6),
                word(&bytes, 2, 6, 6),
                word(&bytes, 3, 6, 6)
            ],
            [1024, 2048, 3072, 2048]
        );
        assert_eq!(word(&bytes, 0, 7, 6), 0);
    }
    #[test]
    fn ae_xtrans_counts_include_black_pedestal() {
        let bytes = Scene::default().planes(16, 16, true, 1.0);
        assert_eq!(
            [
                word(&bytes, 0, 0, 0),
                word(&bytes, 1, 0, 0),
                word(&bytes, 2, 0, 0)
            ],
            [9216 * 2080, 9216 * 5200, 9216 * 2080]
        );
    }
    #[test]
    fn clipping_precedes_area_integration() {
        let scene = Scene {
            width: 2,
            rgb: vec![[0; 3], [65535; 3]],
            samples_per_cell: 2,
            black_level: 0,
            ..Scene::default()
        };
        assert_eq!(word(&scene.planes(1, 1, false, 1.0), 0, 0, 0), 16383);
    }

    #[test]
    fn followed_gain_scales_the_cells_and_a_unit_gain_keeps_them_bit_identical() {
        let statistics = Statistics::new(0x100000, 0x7000).unwrap();
        let scene = Scene::default();
        assert_eq!(statistics.gain(), 1.0);
        let sensor: Arc<Mutex<SensorState>> = Arc::default();
        statistics
            .clone()
            .follow_exposure(sensor.clone(), SensorExposure::new(0, None).unwrap(), Arc::default());
        // Unwritten gain registers: the stock cells.
        assert_eq!(statistics.gain(), 1.0);
        assert_eq!(scene.planes(16, 16, true, statistics.gain()), scene.planes(16, 16, true, 1.0));
        sensor.lock().unwrap().registers.insert(0x30, 0xff);
        sensor.lock().unwrap().registers.insert(0x31, 0x07);
        let gain = statistics.gain();
        assert!((gain - 4095.0 / 2048.0).abs() < 1e-12);
        // 2048 << 2 = 8192 doubles to 16380 (+1024 black, clipped at 16383).
        let expected = ((8192.0 * gain + 1024.0f64).min(16383.0) * 1024.0).round() as u32;
        assert_eq!(word(&scene.planes(1, 1, false, gain), 0, 0, 0), expected);
        // A small signal scales below the clip.
        let mut dim = Scene::default();
        dim.rgb = vec![[100; 3]];
        let expected = ((400.0 * gain + 1024.0) * 1024.0f64).round() as u32;
        assert_eq!(word(&dim.planes(1, 1, false, gain), 0, 0, 0), expected);
    }

    #[test]
    fn view_picture_shows_the_scene_with_the_gain_and_keeps_grey_neutral() {
        let law = ViewPicture::default();
        law.check().unwrap();
        assert!(ViewPicture { white: Some(0), ..law }.check().is_err());
        assert!(ViewPicture { gamma: 0.5, ..law }.check().is_err());
        let statistics = Statistics::new(0x100000, 0x7000).unwrap();
        // Default scene: black-free 8192 of 15359, (8192 / 15359) ^ (1 / 2.2).
        let grey = statistics.paint_view(4, 2, &law);
        assert_eq!(grey.len(), 4 * 2 * 2);
        let y = (16.0 + 219.0 * (8192.0f64 / 15359.0).powf(1.0 / 2.2)).round() as u8;
        assert_eq!(grey, [y, y, 128, 128].repeat(4));
        // Twice the gain clips at the full scale: white.
        let sensor: Arc<Mutex<SensorState>> = Arc::default();
        statistics.follow_exposure(sensor.clone(), SensorExposure::new(0, None).unwrap(), Arc::default());
        sensor.lock().unwrap().registers.insert(0x30, 0xff);
        sensor.lock().unwrap().registers.insert(0x31, 0x07);
        assert_eq!(statistics.paint_view(2, 1, &law), [235, 235, 128, 128]);
        // A dark left half and a red right half, stretched over 4 x 1.
        statistics.state.lock().unwrap().scene = Scene {
            width: 2,
            rgb: vec![[0; 3], [3000, 0, 0]],
            ..Scene::default()
        };
        sensor.lock().unwrap().registers.clear();
        let split = statistics.paint_view(4, 1, &law);
        assert_eq!(split[..4], [16, 16, 128, 128]);
        assert!(split[4] > 16 && split[4] == split[5]);
        assert!(split[6] < 128 && split[7] > 128); // Red: Cb low, Cr high.
        // A white point at the scene signal: full white.
        let white = ViewPicture { white: Some(3000 << 2), ..law };
        assert_eq!(statistics.paint_view(2, 1, &white)[0], 16);
        let red = statistics.paint_view(4, 1, &white);
        assert_eq!(red[4], (16.0 + 219.0 * 0.299f64).round() as u8);
    }

    #[test]
    fn statistics_outputs_follow_the_scene_and_the_gain_but_not_the_black() {
        let scene = Scene::default();
        let options = StatisticsOutputs::default();
        options.check().unwrap();
        let u32_at = |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        // 1x1 grid (firmware 5.11 live view): one zone, the whole frame.
        let one = scene.luma(1, 1, 2, 1.0);
        assert_eq!(one.len(), 0x400);
        assert_eq!(u32_at(&one, 0), 2048 << 16);
        assert_eq!(u32_at(&one, 4), 0);
        // Doubling the gain doubles the black-free luminance, not the black.
        let dim = Scene {
            rgb: vec![[100; 3]],
            ..Scene::default()
        };
        assert_eq!(u32_at(&dim.luma(1, 1, 2, 1.0), 0), 100 << 16);
        assert_eq!(u32_at(&dim.luma(1, 1, 2, 2.0), 0), 200 << 16);
        assert_eq!(scene.black(16)[..8], (1024u64 * 16).to_le_bytes());
        // A dark top half and a bright bottom half: the zones and bands see it.
        let split = Scene {
            height: 2,
            rgb: vec![[0; 3], [1000; 3]],
            ..Scene::default()
        };
        let grid = split.luma(1, 2, 0, 1.0);
        assert_eq!([u32_at(&grid, 0) >> 16, u32_at(&grid, 0x40) >> 16], [0, 4000]);
        let records = split.records(9, 9, 1.0);
        let r = |band: usize| u64::from_le_bytes(records[band * 0x20..band * 0x20 + 8].try_into().unwrap());
        assert_eq!([r(0), r(9)], [1024 * 2, 5024 * 2]);
        // Default counts follow the firmware windows: 153 x 153 sites in
        // band 0 (black removed exactly with its own 153 x 153 count), one
        // site, hence no sample, in the nine others.
        assert_eq!((options.record_sites, options.record_rest_sites), (23409, 1));
        let records = split.records(options.record_sites, options.record_rest_sites, 1.0);
        let at = |band: usize, channel: usize| {
            let at = band * 0x20 + channel * 8;
            u64::from_le_bytes(records[at..at + 8].try_into().unwrap())
        };
        assert_eq!([at(0, 0), at(0, 1), at(0, 2)], [1024 * 5202, 1024 * 13005, 1024 * 5202]);
        assert!((1..10).all(|band| (0..3).all(|channel| at(band, channel) == 0)));
        assert!(StatisticsOutputs { record_rest_sites: 0, ..options }.check().is_err());
        let histogram = split.histogram(4, 1.0);
        assert_eq!([u32_at(&histogram, 0), u32_at(&histogram, (4000 >> 6) * 4)], [8, 8]);
        // Window registers resolve the black samples; a fixed count wins.
        let registers = |o: u64| match o {
            0x1ac => 0x000c_0015,
            0x1b0 => 0x0114_03ff,
            _ => 0,
        };
        let resolved = |weight, samples| match (Planes::Black { window: 0x1ac, weight, samples }).resolve(registers) {
            Planes::Black { samples, .. } => samples,
            _ => None,
        };
        assert_eq!(resolved(4, None), Some(22 * 1024 * 4));
        assert_eq!(resolved(1, None), Some(22 * 1024));
        assert_eq!(resolved(4, Some(7)), Some(7));
        // Bad parameters are refused; the definition keys are closed.
        assert!(StatisticsOutputs { grid_columns: 17, ..options }.check().is_err());
        assert!(StatisticsOutputs { ob_samples: Some(0), ..options }.check().is_err());
        assert!(StatisticsOutputs { ob_site_weight: 0, ..options }.check().is_err());
        assert!(serde_json::from_value::<StatisticsOutputs>(serde_json::json!({"grid": 7})).is_err());
        let parsed: StatisticsOutputs =
            serde_json::from_value(serde_json::json!({"grid_columns": 1, "grid_rows": 1})).unwrap();
        assert_eq!((parsed.grid_columns, parsed.grid_rows, parsed.ob_site_weight), (1, 1, 4));
        assert_eq!(serde_json::from_value::<StatisticsOutputs>(serde_json::json!({})).unwrap(), options);
    }
}
