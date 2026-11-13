//! X-Processor Pro register blocks with no work in flight.
//!
//! Several imaging blocks of X-T20 2.11 are configured from section 5 with
//! plain word writes, often through the table copier 0xc41d30, and read back
//! with read-modify-write updates. Some have handshakes that wait for a
//! status bit after a stop or start: 0xc3c0c0 writes 0 to 0xfff3b000 and
//! waits until bit 0 reads set; 0xc3a954 does the same for the units at
//! 0xfff55000 + 0x1000*n, and their stop 0xc3be40 waits for +0xd8 bit 8.
//! Each block is a register file: words read back their last write and start
//! at 0, and the declared idle bits always read set, as no work is ever in
//! flight. What the blocks compute and any data they move are not modeled.
//!
//! A block may declare [`Frames`]: while its start bit is set it finishes a
//! frame at every multiple of the frame period and raises one interrupt edge;
//! its stopped bit reads set only while the start bit is clear. The live-view
//! output units at 0xfff55000 + 0x1000*n start with +0xc8 bit 28 (0xc3a98c),
//! their stop clears it and waits for +0xd8 bit 8, and their edge handlers
//! (GIC 107 + n) report the frame to the imaging dispatcher.
//!
//! A live-view output unit may declare a uniform image: before each frame
//! interrupt it writes `lines` lines of packed 4:2:2 pixel pairs (Y0 Y1 Cb Cr)
//! to the buffer at +0xc0, +0xc4 pixels wide (two bytes per pixel). The
//! firmware moves +0xc0 through its buffers between frames (unit 1:
//! 0xa7d6000 + 0x180000*k, 720 pixels) and the display shows the buffer the
//! unit finished last. Other data the unit or the rest of the pipeline moves
//! is not modeled. Instead of the uniform image a unit may declare the
//! virtual post-ISP scene ([`fujimod_virtual_scene::PROFILE`]: color bars, a
//! ramp and a moving square, BT.601 limited 4:2:2), sampled over the whole
//! scene into the programmed width and `lines`, one phase per frame.
//!
//! A block may instead declare a [`Job`]: each write that sets its start bit
//! while no job is in flight finishes one job after the declared ticks. The
//! job sets the status bit, the interrupt line is high while a status bit is
//! set, and writing a status bit clears it, as for the face detection engine.
//! The block at 0xfff7f000 (driver 0x1558424..0x1558a94) starts with +0x04
//! bit 0 (0x15589c4); its handler 0x1558108 reads +0x44, writes 1 to it and
//! calls the job's callback. A job without a status bit instead declares a
//! stop bit: its interrupt stays high until a write clears that bit (the
//! development engine 0xfff98000, whose handler 0x1560744 writes 0 to +0x00
//! and waits for bit 0 through 0x1560de8). Idle bits may share the status
//! word when they do not overlap the status bit (0xfffc4000: +0x3c bit 20
//! idle, bit 0 done; 0xfffc3000: +0xdc 0x01000100 idle, bit 0 done, which
//! its handler 0xcce510 clears through 0xc6d580). A job with neither a status
//! nor a stop bit is a pulse job: when it finishes, its start bit reads clear
//! again and its edge line pulses once, with nothing to acknowledge (XP4
//! X-T3 5.11 0xfffa8000: 0x1091630 sets +0x04 bit 0 and returns, GIC 144's
//! handler 0x1090e24 reads no register; 0x1091150 instead polls bit 0 back
//! to 0). What a job computes is not modeled.
//!
//! A status job may also declare that its start bit clears when it ends:
//! X-T3 5.11 IIFP 0xfff6f000 (job 8, GIC 134, handler 0x1dbb344) starts with
//! +0x00 bit 0 (0x1dbc0b4) and acknowledges +0x84; 0x1dbb6c8 later writes
//! +0x00 back with a read-modify-write, which would start a second job if the
//! bit still read set.
//!
//! A block may declare [`Command`]s: writing exactly a command's value to its
//! word sets the declared field of a status word, which otherwise reads back
//! its last write. After a still, reccont takes the second DRAM controller
//! 0xfff0f000 through 0xc6c64c (+0xb8 = 0x6a020202, then waits for +0xbc bits
//! 5..0 = 0x27) and back through 0xc6c700 (0x01020202, waits for 0x20); these
//! are its only +0xb8 commands. What the states mean is not known.
//!
//! A job may declare [`Work`]: when it finishes, before its interrupt rises,
//! the block develops or scales a still from the registers the firmware
//! programmed, with the declared approximation of [`crate::development`].
//! A block may publish its words for another block to read: the development
//! engine reads the color unit 0xfff47000 this way when its film law is
//! declared ([`crate::development::FILM_PROFILE`]).
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Words a block publishes: its current register file.
pub type Published = Arc<Mutex<BTreeMap<u64, u32>>>;

/// Frame law of a block: (offset, mask) of the start and stopped bits, the
/// interrupt line and the frame period in ticks.
#[derive(Clone, Copy, Debug)]
pub struct Frames {
    pub start: (u64, u32),
    pub stopped: (u64, u32),
    pub irq: u32,
    pub period: u64,
}
/// Job law of a block: (offset, mask) of the start and status bits (or,
/// without a status bit, the stop bit that a write clears to acknowledge),
/// the level interrupt line and the ticks a job takes.
#[derive(Clone, Copy, Debug)]
pub struct Job {
    pub start: (u64, u32),
    pub status: Option<(u64, u32)>,
    pub stop: Option<(u64, u32)>,
    pub irq: u32,
    pub ticks: u64,
}
/// Command law of a block: writing `value` to `word` sets the bits `mask` of
/// the status word `status` to `state`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Command {
    pub word: u64,
    pub value: u32,
    pub status: u64,
    pub mask: u32,
    pub state: u32,
}
/// Data a job moves when it finishes (see [`crate::development`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Work {
    /// The development engine; the X-Trans site (line, sample) of the first
    /// source sample.
    Develop { phase: (u8, u8) },
    /// The resizer.
    Resize,
    /// X-T3 5.11 RECC 0xfff82000 (job 71, GIC 129), a declared stand-in for
    /// the development: see [`Recc`].
    Recc(Recc),
    /// X-T3 5.11 audio format converter 0xfffa8000 (job 0x71, GIC 144): see
    /// [`AUDIO_CONTROL`].
    Audio,
}
/// X-T3 5.11 audio format converter 0xfffa8000, as its driver programs it
/// (0x1091220, 0x1091630; register trace of a movie playback, run LV111i):
///
/// - +0x08: bit 0 converts (clear, +0x04 loads the firmware table addressed
///   by +0x0c/+0x10, 0x1091150); source sample format in bits 8..10 and
///   destination format in bits 24..26 (0x111a4bc: 1 = 16-bit, 2 = 24-bit
///   packed, 3 = 32-bit);
/// - source at +0x18 (high bits +0x14), line bytes +0x1c, lines +0x20;
///   destination at +0x28 (high bits +0x24), +0x2c, +0x30; +0x34 and +0x38
///   repeat the source and destination line bytes in both halves;
/// - movie playback, 720 stereo frames a job: 90 lines of 0x30 bytes
///   (24-bit packed) become 90 lines of 0x40 bytes (32-bit) that DMA
///   channel 7 sends to serial audio port 0. Recording goes the other way.
///
/// The declared stand-in converts each signed little-endian sample
/// left-justified ([`convert_audio`]): 24 to 32 bits shifts left by 8, 32 to
/// 24 keeps the top 24 bits. The firmware table (a microcode of 16-bit
/// slots) is not decoded: the law is declared, not measured. Other
/// geometries are left unconverted and counted.
pub const AUDIO_CONTROL: u64 = 0x08;
const AUDIO_SOURCE_HIGH: u64 = 0x14;
const AUDIO_SOURCE: u64 = 0x18;
const AUDIO_SOURCE_LINE: u64 = 0x1c;
const AUDIO_SOURCE_LINES: u64 = 0x20;
const AUDIO_DESTINATION_HIGH: u64 = 0x24;
const AUDIO_DESTINATION: u64 = 0x28;
const AUDIO_DESTINATION_LINE: u64 = 0x2c;
const AUDIO_DESTINATION_LINES: u64 = 0x30;
const AUDIO_SOURCE_PITCH: u64 = 0x34;
const AUDIO_DESTINATION_PITCH: u64 = 0x38;
/// Converts `source`, samples of `from` bytes, to samples of `to` bytes
/// (2, 3 or 4, signed little-endian), left-justified: the sample's top bits
/// stay its top bits, the low bits added are zero, the low bits dropped are
/// truncated.
pub fn convert_audio(source: &[u8], from: usize, to: usize) -> Vec<u8> {
    source
        .chunks_exact(from)
        .flat_map(|sample| {
            let mut slot = [0u8; 4];
            slot[4 - from..].copy_from_slice(sample);
            let mut out = [0u8; 4];
            out[..to].copy_from_slice(&slot[4 - to..]);
            out.into_iter().take(to)
        })
        .collect()
}
/// RECC as X-T3 5.11 programs it for a still (0x1dc3490; HLE-E5r register
/// log of the three jobs of one shot):
///
/// - mode +0x08 = 0x620000, twice: channel 1 reads the RAW at +0x0c (high
///   bits +0x10), pitch +0x14, from line +0x1c >> 16; +0x18 is the band
///   (lines << 16 | pixels: 2088 x 6256); channel 4 writes the picture at
///   +0x70 (high bits +0x74), pitch +0x80, from line +0x54 >> 16. The second
///   job sets both line words to 2088: two bands of one picture, written 64
///   lines and 128 pixels into B (the margins of the next job).
/// - mode +0x08 = 2: channel 2 reads +0x3c (high bits +0x40), pitch +0x4c,
///   size +0x50 (4176 x 6256), and channel 4 writes it to B itself.
///
/// The declared stand-in develops each band in color
/// ([`crate::development::develop`]) and copies the last job's picture as
/// it is (its own processing, distortion correction among others, is not
/// modeled). It writes only inside `area`; every job is logged (source,
/// destination, lines written).
///
/// With [`ReccMovie`] (opt-in, [`Block::with_recc_movie`]) it also models the
/// 4K movie mode, see there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recc {
    /// RAW: address and line pitch in bytes, for the X-Trans phase.
    pub raw: (u64, u64),
    /// X-Trans site (line, sample) in [`crate::development::XTRANS`] of the
    /// RAW's first sample.
    pub site: (u8, u8),
    /// Where RECC may write: address and length in bytes.
    pub area: (u64, u64),
}
const RECC_MODE: u64 = 0x08;
const RECC_SOURCE: u64 = 0x0c;
const RECC_SOURCE_HIGH: u64 = 0x10;
const RECC_SOURCE_PITCH: u64 = 0x14;
const RECC_BAND: u64 = 0x18;
const RECC_SOURCE_LINE: u64 = 0x1c;
const RECC_SECOND: u64 = 0x3c;
const RECC_SECOND_HIGH: u64 = 0x40;
const RECC_SECOND_PITCH: u64 = 0x4c;
const RECC_SECOND_SIZE: u64 = 0x50;
const RECC_DESTINATION_LINE: u64 = 0x54;
const RECC_DESTINATION: u64 = 0x70;
const RECC_DESTINATION_HIGH: u64 = 0x74;
const RECC_DESTINATION_PITCH: u64 = 0x80;
const RECC_DEVELOP: u32 = 0x62_0000;
const RECC_MOVIE: u32 = 0x42_2000;
const RECC_MOVIE_SOURCE_WIDTH: u64 = 0x20;
const RECC_MOVIE_LINES: u64 = 0x50;
const RECC_MOVIE_WIDTH: u64 = 0x58;
const RECC_MOVIE_OUTPUT: u64 = 0x6c;
/// RECC in the X-T3 5.11 4K movie (REC 4K, service 0x1293 "RECMB_SV_YC",
/// recfReccProcess 0x1e901d0; run RC4K-a register log): mode +0x08 =
/// 0x422000, two bands per picture (cores 3 then 0). Channel 1 reads the RAW
/// at +0x0c (high bits +0x10; the MemYc buffer + 0x360, where the sensor
/// read-out starts), pitch +0x14 (0x3600), from line +0x1c >> 16 (332 then
/// 2084), +0x24 >> 16 lines (1752 then 1760), +0x20 samples a line (6272:
/// 6252 rounded up to 0x20). Channel 4 writes the YC picture at +0x70 (high
/// bits +0x74; a MemResz buffer), pitch +0x80 (0x9000, two lines a row), from
/// line +0x54 >> 16 (0 then 1314), +0x50 >> 16 lines (1316 then 1320, the
/// first band 2 lines past its share), +0x58 pixels a line (4704). The
/// firmware scales by 3/4 down and across (0x1e717e0): 4704 / 6272.
///
/// The declared stand-in maps picture line `first_out + j` to RAW line
/// `first_in + j * 6272 / 4704` and pixel x to sample x * 6272 / 4704
/// (nearest neighbour, the engine's filter is not modeled), so that the
/// lines two bands share come out the same; it reads 16-bit samples (the
/// firmware's RAW is 12-bit packed, format 0x40101: a declared difference)
/// and writes `layout` rows: grey (luma (sample << 2) >> `shift`, chroma 512)
/// or, with `color`, the band developed by [`crate::development::develop`]
/// first. The row layout is the output code in +0x6c bits 1..0, as
/// 0x1dc2f74 writes it from the output format (0x11563bc converts its
/// pixels to bytes): 3 = 0x80802, 10-bit 4:2:0 in line pairs at twice the
/// descriptor's pitch, written as Y0L2 rows ([`YcLayout::Y0l2`]; the bit
/// order inside a block is drm_fourcc.h's, declared); 0 = 0x100402, 8-bit
/// 4:2:2, one line a row ([`YcLayout::Lines`], the still's layout). Codes 1
/// (8-bit 4:2:0) and 2 (10-bit 4:2:2, three samples a word) are not modeled:
/// such a band is logged and nothing is written.
///
/// [`YcLayout::Y0l2`]: crate::development::YcLayout::Y0l2
/// [`YcLayout::Lines`]: crate::development::YcLayout::Lines
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReccMovie {
    pub shift: u32,
    pub color: bool,
}
const RECC_COPY: u32 = 2;
/// RECC jobs kept in the log.
const RECC_LOG: usize = 64;
/// RECC jobs whose whole register file is kept.
const RECC_WORDS: usize = 8;
const DEVELOP_SOURCE: u64 = 0x08;
const DEVELOP_SOURCE_STRIDE: u64 = 0x0c;
const DEVELOP_SIZE: u64 = 0x14;
const DEVELOP_DESTINATION: u64 = 0x1c;
const DEVELOP_DESTINATION_STRIDE: u64 = 0x20;
const RESIZE_INPUT: u64 = 0x08;
const RESIZE_OUTPUT: u64 = 0x14;
const RESIZE_STEP_X: u64 = 0x18;
const RESIZE_STEP_Y: u64 = 0x1c;
const RESIZE_SOURCE: u64 = 0xc0;
const RESIZE_SOURCE_STRIDE: u64 = 0xc4;
const RESIZE_DESTINATION: u64 = 0xc8;
const RESIZE_DESTINATION_STRIDE: u64 = 0xcc;
const RESIZE_FORMAT: u64 = 0x100;
/// Packed 4:2:2 in and out, the format of a shot's jobs.
const RESIZE_PACKED_422: u32 = 0x1100_0002;
/// The same with bits 16 and 20 set: X-T20 2.11 playback (0x1548040) sets
/// them from request bytes +0x32 and +0x31 when it magnifies the decoded
/// picture to the display. Their effect is not modeled; the layout is
/// declared packed 4:2:2 as well.
const RESIZE_PACKED_422_PLAYBACK: u32 = 0x1111_0002;
/// Picture a live-view output unit writes at each frame.
#[derive(Clone, Debug, PartialEq)]
enum Picture {
    /// One packed pixel pair repeated.
    Uniform([u8; 4]),
    /// The declared virtual scene.
    Scene,
    /// A host RGB image: width, height and rows of red, green, blue bytes.
    Host(u64, u64, Arc<[u8]>),
    /// One 14-bit linear (R, G, B) developed through the film law and white
    /// balance of the color unit's programmed words, with the packed pair
    /// written while the unit is still unprogrammed. A unit not programmed
    /// yet completes no frame.
    Film([u16; 3], [u8; 4]),
}
/// Virtual scene size: its normalized coordinates.
const SCENE_WIDTH: u64 = 1024;
const SCENE_HEIGHT: u64 = 768;
/// The whole of a scene, unscaled.
fn whole(scene: fujimod_virtual_scene::Scene) -> fujimod_virtual_scene::Crop {
    fujimod_virtual_scene::Crop {
        x: 0,
        y: 0,
        width: scene.width,
        height: scene.height,
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    words: BTreeMap<u64, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    due: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    frames: u64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    status: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    jobs: u64,
    /// RECC jobs: (source, destination, lines developed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recc: Vec<(u64, u64, u64)>,
    /// The register file of the first RECC jobs, as each started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recc_words: Vec<BTreeMap<u64, u32>>,
    /// Audio converter jobs converted, and conversion jobs left unconverted
    /// (unmodeled format or geometry); table loads count in neither.
    #[serde(default, skip_serializing_if = "is_zero")]
    audio_converted: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    audio_skipped: u64,
}
const BUFFER: u64 = 0xc0;
const WIDTH: u64 = 0xc4;
fn is_zero(n: &u64) -> bool {
    *n == 0
}
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}
pub struct Block {
    base: u64,
    size: u64,
    /// (offset, mask) of status bits that always read set.
    idle: Vec<(u64, u32)>,
    frames: Option<Frames>,
    job: Option<Job>,
    work: Option<Work>,
    /// Lines and the Y0 Y1 Cb Cr pixel pair of a uniform live-view image.
    image: Option<(u32, Picture)>,
    /// (offset, value) of words that read a fixed value whatever is written.
    fixed: Vec<(u64, u32)>,
    commands: Vec<Command>,
    /// The start bit of a status job reads clear again once the job ends.
    start_clears: bool,
    /// Where this block publishes its words.
    published: Option<Published>,
    /// Report name naming the block by its base (opt-in, see `with_own_name`).
    own_name: Option<&'static str>,
    /// The color unit's words, for the film law of a development job, and
    /// the declared reference white balance.
    color: Option<(Published, [u32; 3])>,
    /// The still tone unit's words (matrix, curves and brightness table of the
    /// still law), for a development job.
    tone: Option<Published>,
    /// RECC only (opt-in): the published words of the tone unit and of the
    /// YCbCr unit whose firmware color replaces the declared sRGB.
    recc_color: Option<(Published, Published, bool)>,
    /// RECC only (opt-in): the published words of the LUT unit's control
    /// block and of the blocks of its table, 0x1000 bytes each in order.
    recc_lut3d: Option<(Published, Vec<Published>)>,
    /// RECC only (opt-in): also apply the YCbCr unit's 2x2 chroma matrix.
    recc_chroma_matrix: bool,
    /// RECC only (opt-in): also apply the tone unit's brightness gain.
    recc_brightness_gain: bool,
    /// RECC only (opt-in): also apply the YCbCr unit's luma bands.
    recc_luma_bands: bool,
    /// RECC only (opt-in): also apply the tone unit's white balance relative
    /// to these declared reference (R, G, B) gains.
    recc_white_balance: Option<[u32; 3]>,
    /// RECC only (opt-in): the 4K movie mode 0x422000.
    recc_movie: Option<ReccMovie>,
    /// The published integration value and scene-light scale the film
    /// picture's light follows.
    film_exposure: Option<crate::statistics::Exposure>,
    film_light: Option<crate::statistics::SceneLight>,
    /// A host picture the film develops pixel by pixel: width, height, rows
    /// of sRGB bytes.
    film_picture: Option<(u64, u64, Arc<[u8]>)>,
    /// The last developed picture and what it was developed from; derived
    /// output only, never part of the block's state.
    film_frame: Mutex<Option<FilmFrame>>,
    state: State,
}
/// One developed film picture and its inputs.
struct FilmFrame {
    words: BTreeMap<u64, u32>,
    factor: u64,
    width: u64,
    lines: u64,
    image: Vec<u8>,
}
impl Block {
    pub fn new(
        base: u64,
        size: u64,
        idle: Vec<(u64, u32)>,
        frames: Option<Frames>,
    ) -> Result<Self, String> {
        let bit = |(o, mask): (u64, u32)| o & 3 != 0 || o >= size || mask == 0;
        // A block is a word-aligned register file inside one 4 KiB page. It may
        // start after another model's window in the same page (XP4 0xfff4a520,
        // after the DMA request multiplexer at 0xfff4a500..0xfff4a520).
        if base & 3 != 0
            || size == 0
            || (base & 0xfff) + size > 0x1000
            || size & 3 != 0
            || idle.iter().any(|&b| bit(b))
            || frames.is_some_and(|f| {
                bit(f.start)
                    || bit(f.stopped)
                    || !(32..512).contains(&f.irq)
                    || f.period == 0
                    || idle.contains(&f.stopped)
            })
        {
            return Err("Invalid X-Processor Pro register block".into());
        }
        Ok(Self {
            base,
            size,
            idle,
            frames,
            job: None,
            work: None,
            image: None,
            fixed: Vec::new(),
            commands: Vec::new(),
            start_clears: false,
            published: None,
            own_name: None,
            color: None,
            tone: None,
            recc_color: None,
            recc_lut3d: None,
            recc_chroma_matrix: false,
            recc_brightness_gain: false,
            recc_luma_bands: false,
            recc_white_balance: None,
            recc_movie: None,
            film_exposure: None,
            film_light: None,
            film_picture: None,
            film_frame: Mutex::new(None),
            state: State::default(),
        })
    }
    /// One job per start, finished after the declared ticks.
    pub fn with_job(mut self, job: Job) -> Result<Self, String> {
        let bit = |(o, mask): (u64, u32)| o & 3 != 0 || o >= self.size || mask == 0;
        if self.frames.is_some()
            || bit(job.start)
            || job.status.is_some() && job.stop.is_some()
            || job.stop.is_some_and(bit)
            || job.status.is_some_and(|status| {
                bit(status)
                    || job.start.0 == status.0
                    || self
                        .idle
                        .iter()
                        .any(|&(o, mask)| o == status.0 && mask & status.1 != 0)
            })
            || !(32..512).contains(&job.irq)
            || job.ticks == 0
        {
            return Err("Invalid X-Processor Pro register block job".into());
        }
        self.job = Some(job);
        Ok(self)
    }
    /// The start bit of the block's status job clears when the job ends.
    pub fn with_start_clearing(mut self) -> Result<Self, String> {
        if !self.job.is_some_and(|j| j.status.is_some()) {
            return Err("Only a status job's start bit can clear itself".into());
        }
        self.start_clears = true;
        Ok(self)
    }
    /// Data the job moves when it finishes.
    pub fn with_work(mut self, work: Work) -> Result<Self, String> {
        let words = match work {
            Work::Develop { phase } if phase.0 < 6 && phase.1 < 6 => DEVELOP_DESTINATION_STRIDE,
            Work::Resize => RESIZE_FORMAT,
            Work::Develop { .. } => return Err("Invalid X-Trans phase".into()),
            Work::Recc(r) if r.site.0 < 6 && r.site.1 < 6 && r.raw.1 >= 12 && r.area.1 > 0 => {
                RECC_DESTINATION_PITCH
            }
            Work::Recc(_) => return Err("Invalid RECC geometry".into()),
            Work::Audio => AUDIO_DESTINATION_PITCH,
        };
        if self.job.is_none() || words >= self.size {
            return Err("Invalid X-Processor Pro register block work".into());
        }
        self.work = Some(work);
        Ok(self)
    }
    /// Report this block as `xpp-register-block@<base>` rather than under the
    /// name every plain block shares, so its words can be read after a run.
    pub fn with_own_name(mut self) -> Self {
        let name = format!("xpp-register-block@{:#x}", self.base);
        self.own_name = Some(Box::leak(name.into_boxed_str()));
        self
    }
    /// Publish this block's words to `words`.
    pub fn with_published(mut self, words: Published) -> Self {
        *words.lock().unwrap() = self.state.words.clone();
        self.published = Some(words);
        self
    }
    /// Develop with the film law and white balance of the color unit's
    /// published words, relative to the `reference` (R, G, B) gains.
    pub fn with_color(mut self, words: Published, reference: [u32; 3]) -> Result<Self, String> {
        if !matches!(self.work, Some(Work::Develop { .. })) || reference.contains(&0) {
            return Err("Only a development job reads a color unit".into());
        }
        self.color = Some((words, reference));
        Ok(self)
    }
    /// RECC: develop with the firmware color of the tone unit's and the YCbCr
    /// unit's published words, as they stand when each job starts (see
    /// [`crate::development::FirmwareColor`]); with `chroma_modes`, also
    /// honour the YCbCr unit's chroma mode fields
    /// ([`crate::development::FirmwareColor::with_chroma_modes`]).
    pub fn with_recc_color(
        mut self,
        tone: Published,
        ycbcr: Published,
        chroma_modes: bool,
    ) -> Result<Self, String> {
        if !matches!(self.work, Some(Work::Recc(_))) {
            return Err("Only RECC develops with the firmware color".into());
        }
        if Arc::ptr_eq(&tone, &ycbcr) {
            return Err("The tone and YCbCr units are two blocks".into());
        }
        self.recc_color = Some((tone, ycbcr, chroma_modes));
        Ok(self)
    }
    /// RECC with the firmware color: also apply the 3D lookup table of the
    /// LUT unit's published words, as they stand when each job starts (see
    /// [`crate::development::FirmwareColor::with_lut3d`]): `control` the
    /// block whose +0x00 bit 0 enables it, `table` the blocks that hold it,
    /// 0x1000 bytes each.
    pub fn with_recc_lut3d(mut self, control: Published, table: Vec<Published>) -> Result<Self, String> {
        if self.recc_color.is_none() {
            return Err("Only RECC with the firmware color reads a lookup table".into());
        }
        if table.len() as u64 * 0x1000 != crate::development::LUT_BYTES
            || table.iter().any(|t| Arc::ptr_eq(t, &control))
        {
            return Err("The lookup table spans eight blocks apart from its control block".into());
        }
        self.recc_lut3d = Some((control, table));
        Ok(self)
    }
    /// RECC with the firmware color: also apply the 2x2 chroma matrix of the
    /// YCbCr unit's published words (see
    /// [`crate::development::FirmwareColor::with_chroma_matrix`]).
    pub fn with_recc_chroma_matrix(mut self) -> Result<Self, String> {
        if self.recc_color.is_none() {
            return Err("Only RECC with the firmware color reads a chroma matrix".into());
        }
        self.recc_chroma_matrix = true;
        Ok(self)
    }
    /// RECC with the firmware color: also apply the brightness gain of the
    /// tone unit's published words (see
    /// [`crate::development::FirmwareColor::with_brightness_gain`]).
    pub fn with_recc_brightness_gain(mut self) -> Result<Self, String> {
        if self.recc_color.is_none() {
            return Err("Only RECC with the firmware color reads a brightness gain".into());
        }
        self.recc_brightness_gain = true;
        Ok(self)
    }
    /// RECC with the firmware color: also apply the luma bands of the YCbCr
    /// unit's published words (see
    /// [`crate::development::FirmwareColor::with_luma_bands`]).
    pub fn with_recc_luma_bands(mut self) -> Result<Self, String> {
        if self.recc_color.is_none() {
            return Err("Only RECC with the firmware color reads luma bands".into());
        }
        self.recc_luma_bands = true;
        Ok(self)
    }
    /// RECC: also model the 4K movie mode 0x422000 (opt-in, see [`ReccMovie`]).
    pub fn with_recc_movie(mut self, movie: ReccMovie) -> Result<Self, String> {
        if !matches!(self.work, Some(Work::Recc(_))) {
            return Err("Only RECC has a movie mode".into());
        }
        if movie.shift > 16 {
            return Err("Invalid RECC movie shift".into());
        }
        self.recc_movie = Some(movie);
        Ok(self)
    }
    /// RECC with the firmware color: also apply the white balance of the tone
    /// unit's published words, relative to the declared `reference` (R, G, B)
    /// gains at which the declared scene renders neutral (see
    /// [`crate::development::FirmwareColor::with_white_balance`]).
    pub fn with_recc_white_balance(mut self, reference: [u32; 3]) -> Result<Self, String> {
        if self.recc_color.is_none() {
            return Err("Only RECC with the firmware color reads a white balance".into());
        }
        if reference.contains(&0) {
            return Err("a declared reference white-balance gain is zero".into());
        }
        self.recc_white_balance = Some(reference);
        Ok(self)
    }
    /// Develop with the matrix and curves of the still tone unit's published
    /// words; widths, chroma, luma bands and white balance stay the color
    /// unit's.
    pub fn with_tone(mut self, words: Published) -> Result<Self, String> {
        if !matches!(self.work, Some(Work::Develop { .. })) || self.color.is_none() {
            return Err("Only a development job reads a still tone unit".into());
        }
        self.tone = Some(words);
        Ok(self)
    }
    fn publish(&self) {
        if let Some(words) = &self.published {
            *words.lock().unwrap() = self.state.words.clone();
        }
    }
    /// One audio converter job: None for a table load, Some(true) once
    /// converted, Some(false) for an unmodeled format or geometry.
    fn run_audio(&self, cx: &mut Cx<'_>) -> Result<Option<bool>, String> {
        let word = |o| self.state.words.get(&o).copied().unwrap_or(0);
        let control = word(AUDIO_CONTROL);
        if control & 1 == 0 {
            return Ok(None);
        }
        let width = |code: u32| match code {
            1 => Some(2usize),
            2 => Some(3),
            3 => Some(4),
            _ => None,
        };
        let (Some(from), Some(to)) = (width(control >> 8 & 7), width(control >> 24 & 7)) else {
            return Ok(Some(false));
        };
        let line = |o| word(o) as usize;
        let (source_line, lines) = (line(AUDIO_SOURCE_LINE), line(AUDIO_SOURCE_LINES));
        let destination_line = line(AUDIO_DESTINATION_LINE);
        let both = |o: u64, bytes: usize| {
            let w = word(o) as usize;
            w & 0x3ff == bytes && w >> 16 & 0x3ff == bytes
        };
        if source_line == 0
            || lines == 0
            || line(AUDIO_DESTINATION_LINES) != lines
            || source_line % from != 0
            || destination_line % to != 0
            || source_line / from != destination_line / to
            || !both(AUDIO_SOURCE_PITCH, source_line)
            || !both(AUDIO_DESTINATION_PITCH, destination_line)
        {
            return Ok(Some(false));
        }
        let address = |low, high| u64::from(word(high) & 3) << 32 | u64::from(word(low));
        let source = cx.ram_bytes(address(AUDIO_SOURCE, AUDIO_SOURCE_HIGH), source_line * lines)?;
        cx.ram_write(
            address(AUDIO_DESTINATION, AUDIO_DESTINATION_HIGH),
            &convert_audio(&source, from, to),
        )?;
        Ok(Some(true))
    }
    /// One RECC job: its log entry (source, destination, lines written).
    fn run_recc(&self, cx: &mut Cx<'_>, r: Recc) -> Result<(u64, u64, u64), String> {
        let word = |o| u64::from(self.state.words.get(&o).copied().unwrap_or(0));
        let address = |low, high| (word(high) & 3) << 32 | word(low);
        let size = |o| (word(o) >> 16, word(o) & 0xffff);
        let mode = word(RECC_MODE) as u32;
        if let (RECC_MOVIE, Some(movie)) = (mode, self.recc_movie) {
            return self.run_recc_movie(cx, r, movie);
        }
        let pitch = word(RECC_DESTINATION_PITCH);
        let (source, source_pitch, (lines, width), first) = match mode {
            RECC_DEVELOP => (
                address(RECC_SOURCE, RECC_SOURCE_HIGH),
                word(RECC_SOURCE_PITCH),
                size(RECC_BAND),
                (word(RECC_SOURCE_LINE) >> 16, word(RECC_DESTINATION_LINE) >> 16),
            ),
            RECC_COPY => (
                address(RECC_SECOND, RECC_SECOND_HIGH),
                word(RECC_SECOND_PITCH),
                size(RECC_SECOND_SIZE),
                (0, 0),
            ),
            _ => (0, 0, (0, 0), (0, 0)),
        };
        let destination = address(RECC_DESTINATION, RECC_DESTINATION_HIGH) + first.1 * pitch;
        let source = source + first.0 * source_pitch;
        let end = destination + pitch * lines.saturating_sub(1) + 2 * width;
        if source == 0
            || lines < 6
            || width < 6
            || pitch < 4 * width.div_ceil(2)
            || source_pitch < 2 * width
            || destination < r.area.0
            || end > r.area.0 + r.area.1
        {
            return Ok((source, destination, 0));
        }
        let bytes = cx.ram_bytes(source, (source_pitch * (lines - 1) + 2 * width) as usize)?;
        let rows = if mode == RECC_DEVELOP {
            let Some(offset) = source.checked_sub(r.raw.0) else {
                return Ok((source, destination, 0));
            };
            let phase = (
                (usize::from(r.site.0) + (offset / r.raw.1) as usize) % 6,
                (usize::from(r.site.1) + (offset % r.raw.1 / 2) as usize) % 6,
            );
            match &self.recc_color {
                None => crate::development::develop(
                    &bytes,
                    source_pitch as usize,
                    lines as usize,
                    width as usize,
                    phase,
                    None,
                )?,
                Some((tone, ycbcr, chroma_modes)) => {
                    let ycbcr = ycbcr.lock().unwrap();
                    let mut color =
                        crate::development::FirmwareColor::from_words(&tone.lock().unwrap(), &ycbcr)?;
                    if *chroma_modes {
                        color = color.with_chroma_modes(&ycbcr);
                    }
                    if self.recc_chroma_matrix {
                        color = color.with_chroma_matrix(&ycbcr);
                    }
                    if self.recc_brightness_gain {
                        color = color.with_brightness_gain(&tone.lock().unwrap());
                    }
                    if self.recc_luma_bands {
                        color = color.with_luma_bands(&ycbcr);
                    }
                    if let Some(reference) = self.recc_white_balance {
                        color = color.with_white_balance(&tone.lock().unwrap(), reference)?;
                    }
                    if let Some((control, blocks)) = &self.recc_lut3d {
                        let mut table = BTreeMap::new();
                        for (k, block) in blocks.iter().enumerate() {
                            for (o, w) in block.lock().unwrap().iter() {
                                table.insert(0x1000 * k as u64 + o, *w);
                            }
                        }
                        color = color.with_lut3d(&control.lock().unwrap(), &table);
                    }
                    crate::development::develop_firmware(
                        &bytes,
                        source_pitch as usize,
                        lines as usize,
                        width as usize,
                        phase,
                        &color,
                    )?
                }
            }
        } else {
            bytes
                .chunks(source_pitch as usize)
                .map(|row| row[..2 * width as usize].to_vec())
                .collect()
        };
        for (y, row) in rows.iter().enumerate() {
            cx.ram_write(destination + y as u64 * pitch, row)
                .map_err(|e| format!("RECC picture outside RAM: {e}"))?;
        }
        Ok((source, destination, lines))
    }
    /// One RECC band of the 4K movie (see [`ReccMovie`]): its log entry
    /// (first RAW line read, first picture row written, lines written).
    fn run_recc_movie(&self, cx: &mut Cx<'_>, r: Recc, m: ReccMovie) -> Result<(u64, u64, u64), String> {
        use crate::development::{YcLayout, developed_line, grey_line, yc_row};
        let word = |o| u64::from(self.state.words.get(&o).copied().unwrap_or(0));
        let address = |low, high| (word(high) & 3) << 32 | word(low);
        let source_pitch = word(RECC_SOURCE_PITCH);
        let source = address(RECC_SOURCE, RECC_SOURCE_HIGH) + (word(RECC_SOURCE_LINE) >> 16) * source_pitch;
        let window = word(RECC_MOVIE_SOURCE_WIDTH);
        let width = word(RECC_MOVIE_WIDTH);
        let lines = word(RECC_MOVIE_LINES) >> 16;
        let first = word(RECC_DESTINATION_LINE) >> 16;
        let pitch = word(RECC_DESTINATION_PITCH);
        let layout = match word(RECC_MOVIE_OUTPUT) & 3 {
            3 => YcLayout::Y0l2,
            0 => YcLayout::Lines,
            _ => return Ok((source, address(RECC_DESTINATION, RECC_DESTINATION_HIGH), 0)),
        };
        let per_row = layout.lines();
        let destination = address(RECC_DESTINATION, RECC_DESTINATION_HIGH) + first / per_row * pitch;
        let rows = lines / per_row;
        let row_bytes = width * 2 * per_row;
        if address(RECC_SOURCE, RECC_SOURCE_HIGH) == 0
            || window < 6
            || width < 2
            || width % 2 != 0
            || width > window
            || rows == 0
            || first % per_row != 0
            || lines % per_row != 0
            || pitch < row_bytes
            || source_pitch < 2 * window
            || (m.color && window % 2 != 0)
            || destination < r.area.0
            || destination + pitch * (rows - 1) + row_bytes > r.area.0 + r.area.1
        {
            return Ok((source, destination, 0));
        }
        // RAW line (from `source`) of band line j: the same step down as across.
        let raw_line = |j: u64| j * window / width;
        let developed = if m.color {
            let n = raw_line(lines - 1) + 1;
            if n < 6 {
                return Ok((source, destination, 0));
            }
            let bytes = cx.ram_bytes(source, (source_pitch * (n - 1) + 2 * window) as usize)?;
            let Some(offset) = source.checked_sub(r.raw.0) else {
                return Ok((source, destination, 0));
            };
            let phase = (
                (usize::from(r.site.0) + (offset / r.raw.1) as usize) % 6,
                (usize::from(r.site.1) + (offset % r.raw.1 / 2) as usize) % 6,
            );
            Some(crate::development::develop(
                &bytes,
                source_pitch as usize,
                n as usize,
                window as usize,
                phase,
                None,
            )?)
        } else {
            None
        };
        let line = |cx: &Cx<'_>, j: u64| -> Result<Vec<[u16; 3]>, String> {
            Ok(match &developed {
                Some(developed) => developed_line(&developed[raw_line(j) as usize], window, width),
                None => grey_line(
                    &cx.ram_bytes(source + raw_line(j) * source_pitch, 2 * window as usize)?,
                    window,
                    width,
                    m.shift,
                ),
            })
        };
        for n in 0..rows {
            let upper = line(cx, n * per_row)?;
            let lower = if per_row > 1 { line(cx, n * per_row + 1)? } else { Vec::new() };
            cx.ram_write(destination + n * pitch, &yc_row(layout, &upper, &lower))
                .map_err(|e| format!("RECC picture outside RAM: {e}"))?;
        }
        Ok((source, destination, lines))
    }
    fn run_work(&self, cx: &mut Cx<'_>, work: Work) -> Result<(), String> {
        let word = |o| self.state.words.get(&o).copied().unwrap_or(0);
        let address = |o| u64::from(word(o));
        let stride = |o| word(o) as usize * 2;
        let (rows, destination, destination_stride) = match work {
            Work::Recc(_) => return Err("RECC develops through its own path".into()),
            Work::Audio => return Err("The audio converter converts through its own path".into()),
            Work::Develop { phase } => {
                let (lines, width) = crate::development::size(word(DEVELOP_SIZE));
                let source_stride = stride(DEVELOP_SOURCE_STRIDE);
                let raw = cx.ram_bytes(
                    address(DEVELOP_SOURCE),
                    source_stride * (lines - 1) + 2 * width,
                )?;
                let phase = (usize::from(phase.0), usize::from(phase.1));
                let film = match &self.color {
                    Some((words, reference)) => {
                        let words = words.lock().unwrap();
                        let still = self.tone.as_ref().map(|tone| tone.lock().unwrap());
                        Some((
                            crate::development::film(still.as_deref(), &words)?,
                            crate::development::white_balance(&words, *reference)?,
                        ))
                    }
                    None => None,
                };
                let rows = crate::development::develop(
                    &raw,
                    source_stride,
                    lines,
                    width,
                    phase,
                    film.as_ref().map(|(film, factors)| (film, *factors)),
                )?;
                (
                    rows,
                    address(DEVELOP_DESTINATION),
                    stride(DEVELOP_DESTINATION_STRIDE),
                )
            }
            Work::Resize => {
                if ![RESIZE_PACKED_422, RESIZE_PACKED_422_PLAYBACK].contains(&word(RESIZE_FORMAT)) {
                    return Err(format!(
                        "Unmodeled resizer format {:#x}",
                        word(RESIZE_FORMAT)
                    ));
                }
                let (lines, width) = crate::development::size(word(RESIZE_INPUT));
                let (out_lines, out_width) = crate::development::size(word(RESIZE_OUTPUT));
                let (Some(step_x), Some(step_y)) = (
                    crate::development::step(word(RESIZE_STEP_X)),
                    crate::development::step(word(RESIZE_STEP_Y)),
                ) else {
                    return Err("Unmodeled resizer step".into());
                };
                let scale = crate::development::Scale {
                    stride: stride(RESIZE_SOURCE_STRIDE),
                    lines,
                    width,
                    out_lines,
                    out_width,
                    step_x,
                    step_y,
                };
                let source = cx.ram_bytes(
                    address(RESIZE_SOURCE),
                    scale.stride * (lines - 1) + 2 * width,
                )?;
                let rows = crate::development::resample(&source, scale)?;
                (
                    rows,
                    address(RESIZE_DESTINATION),
                    stride(RESIZE_DESTINATION_STRIDE),
                )
            }
        };
        if let Some(row) = rows.first().filter(|row| row.len() > destination_stride) {
            return Err(format!(
                "Still picture line of {} bytes wider than its destination stride {destination_stride} ({} lines to {destination:#x})",
                row.len(),
                rows.len()
            ));
        }
        for (y, row) in rows.iter().enumerate() {
            cx.ram_write(destination + (y * destination_stride) as u64, row)
                .map_err(|e| format!("Still picture outside RAM: {e}"))?;
        }
        Ok(())
    }
    /// Words that read a fixed value: ready words a driver compares for
    /// equality before a run (0xfffc3000 +0xdc and 0xfff7e000 +0x1c read
    /// 0x01000100 while the firmware writes 1 there).
    pub fn with_fixed(mut self, fixed: Vec<(u64, u32)>) -> Result<Self, String> {
        let job_offsets = self
            .job
            .map(|j| [Some(j.start.0), j.status.map(|s| s.0)])
            .unwrap_or_default();
        if fixed.iter().any(|&(o, _)| {
            o & 3 != 0
                || o >= self.size
                || self.idle.iter().any(|&(i, _)| i == o)
                || job_offsets.contains(&Some(o))
                || self
                    .frames
                    .is_some_and(|f| f.start.0 == o || f.stopped.0 == o)
        }) {
            return Err("Invalid X-Processor Pro register block fixed word".into());
        }
        self.fixed = fixed;
        Ok(self)
    }
    /// Commands that set a field of a status word. A command may name its own
    /// word as the status: the field takes the state right after the write, a
    /// self-clearing busy bit whose work completes at once (XP4 0xff505384:
    /// the firmware writes 1 and waits for bit 0 to read clear).
    pub fn with_commands(mut self, commands: Vec<Command>) -> Result<Self, String> {
        let word = |o: u64| o & 3 != 0 || o >= self.size;
        let special = |o: u64| {
            self.idle.iter().any(|&(i, _)| i == o)
                || self.fixed.iter().any(|&(f, _)| f == o)
                || self.job.is_some_and(|j| {
                    j.start.0 == o
                        || j.status.is_some_and(|s| s.0 == o)
                        || j.stop.is_some_and(|s| s.0 == o)
                })
                || self
                    .frames
                    .is_some_and(|f| f.start.0 == o || f.stopped.0 == o)
        };
        if commands.iter().any(|c| {
            word(c.word)
                || word(c.status)
                || c.mask == 0
                || c.state & !c.mask != 0
                || special(c.word)
                || special(c.status)
        }) {
            return Err("Invalid X-Processor Pro register block command".into());
        }
        self.commands = commands;
        Ok(self)
    }
    /// A uniform live-view image written at each frame: lines and Y, Cb, Cr.
    pub fn with_live_view_image(self, lines: u32, ycbcr: [u8; 3]) -> Result<Self, String> {
        let [y, cb, cr] = ycbcr;
        self.with_picture(lines, Picture::Uniform([y, y, cb, cr]))
    }
    /// A uniform scene, given as 14-bit linear light at the `reference` white
    /// balance, developed at each frame with the film law and white balance of
    /// the color unit's published `words` (the live-view pipeline reprograms
    /// the unit every frame). Until the firmware programs it, `unprogrammed`
    /// (Y, Cb, Cr) is written.
    pub fn with_live_view_film(
        mut self,
        lines: u32,
        linear: [u16; 3],
        unprogrammed: [u8; 3],
        words: Published,
        reference: [u32; 3],
    ) -> Result<Self, String> {
        if self.work.is_some() || reference.contains(&0) || linear.iter().any(|&v| v > 16383) {
            return Err("Invalid X-Processor Pro live-view film".into());
        }
        let [y, cb, cr] = unprogrammed;
        self = self.with_picture(lines, Picture::Film(linear, [y, y, cb, cr]))?;
        self.color = Some((words, reference));
        Ok(self)
    }
    /// The declared film light follows the published integration value, as the
    /// statistics scene's light does (`crate::statistics::Exposure`).
    pub fn with_film_exposure(
        mut self,
        exposure: crate::statistics::Exposure,
    ) -> Result<Self, String> {
        if !matches!(self.image, Some((_, Picture::Film(..)))) || exposure.reference == 0 {
            return Err("Invalid X-Processor Pro live-view film exposure".into());
        }
        self.film_exposure = Some(exposure);
        Ok(self)
    }
    /// The declared film light follows a published scene-light scale
    /// (`crate::statistics::SceneLight`).
    pub fn with_film_scene_light(
        mut self,
        light: crate::statistics::SceneLight,
    ) -> Result<Self, String> {
        if !matches!(self.image, Some((_, Picture::Film(..)))) {
            return Err("Invalid X-Processor Pro live-view film scene light".into());
        }
        self.film_light = Some(light);
        Ok(self)
    }
    /// A host picture developed through the film law in place of the uniform
    /// film light, scaled by the same exposure and scene light.
    pub fn with_film_picture(
        mut self,
        width: u64,
        height: u64,
        rgb: Vec<u8>,
    ) -> Result<Self, String> {
        let scene = fujimod_virtual_scene::Scene {
            width,
            height,
            frame: 0,
        };
        if !matches!(self.image, Some((_, Picture::Film(..))))
            || fujimod_virtual_scene::validate_crop(scene, whole(scene)).is_err()
            || width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(rgb.len() as u64)
        {
            return Err("Invalid X-Processor Pro live-view film picture".into());
        }
        self.film_picture = Some((width, height, rgb.into()));
        Ok(self)
    }
    /// The published exposure and scene-light factors applied to film light.
    fn film_factor(&self) -> f64 {
        let exposure = self.film_exposure.as_ref().map_or(1., |e| e.factor());
        let light = self.film_light.as_ref().map_or(1., |l| l.factor());
        exposure * light
    }
    /// The film picture developed with `words` at `width` x `lines`, reused
    /// while the words, the light and the size are unchanged. `None` while
    /// the unit is not programmed yet.
    fn developed_picture(
        &self,
        words: &BTreeMap<u64, u32>,
        reference: [u32; 3],
        width: u64,
        lines: u64,
    ) -> Option<Result<Vec<u8>, fujimod_virtual_scene::SceneError>> {
        let (w, h, rgb) = self.film_picture.as_ref()?;
        let factor = self.film_factor();
        let mut cached = self.film_frame.lock().unwrap();
        if let Some(frame) = cached.as_ref().filter(|f| {
            f.factor == factor.to_bits()
                && f.width == width
                && f.lines == lines
                && &f.words == words
        }) {
            return Some(Ok(frame.image.clone()));
        }
        let film = crate::development::film(None, words).ok()?;
        let balance = crate::development::white_balance(words, reference).ok()?;
        let light: Vec<u16> = (0..=255u8)
            .map(|v| {
                (crate::statistics::linear(v) * 16383. * factor)
                    .round()
                    .clamp(0., 16383.) as u16
            })
            .collect();
        let scene = fujimod_virtual_scene::Scene {
            width: *w,
            height: *h,
            frame: 0,
        };
        let develop = |x: u64, y: u64| {
            let i = ((y * w + x) * 3) as usize;
            let level =
                |c: usize| crate::development::balanced(light[usize::from(rgb[i + c])], balance[c]);
            film.develop([level(0), level(1), level(2)])
        };
        let image = fujimod_virtual_scene::output_pairs(
            scene,
            whole(scene),
            width,
            lines,
            None,
            |x0, x1, y| fujimod_emulator_imaging::film::Film::pack(develop(x0, y), develop(x1, y)),
        );
        if let Ok(image) = &image {
            *cached = Some(FilmFrame {
                words: words.clone(),
                factor: factor.to_bits(),
                width,
                lines,
                image: image.clone(),
            });
        }
        Some(image)
    }
    /// The declared film light scaled by the published factors, in the 14-bit
    /// sensor range.
    fn scaled_film_light(&self, linear: [u16; 3]) -> [u16; 3] {
        let factor = self.film_factor();
        linear.map(|value| (f64::from(value) * factor).round().clamp(0., 16383.) as u16)
    }
    /// The declared virtual scene written at each frame, over `lines` lines.
    pub fn with_live_view_scene(self, lines: u32) -> Result<Self, String> {
        self.with_picture(lines, Picture::Scene)
    }
    /// A host RGB image (post-ISP) sampled at each frame, over `lines` lines.
    pub fn with_live_view_host_image(
        self,
        lines: u32,
        width: u64,
        height: u64,
        rgb: Vec<u8>,
    ) -> Result<Self, String> {
        let scene = fujimod_virtual_scene::Scene {
            width,
            height,
            frame: 0,
        };
        if fujimod_virtual_scene::validate_crop(scene, whole(scene)).is_err()
            || width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(rgb.len() as u64)
        {
            return Err("Invalid X-Processor Pro live-view host image".into());
        }
        self.with_picture(lines, Picture::Host(width, height, rgb.into()))
    }
    fn with_picture(mut self, lines: u32, picture: Picture) -> Result<Self, String> {
        if self.frames.is_none() || self.size <= WIDTH || !(1..=4096).contains(&lines) {
            return Err("Invalid X-Processor Pro live-view image".into());
        }
        self.image = Some((lines, picture));
        Ok(self)
    }
    fn write_image(&self, cx: &mut Cx<'_>) {
        let Some((lines, picture)) = &self.image else {
            return;
        };
        let word = |o| self.state.words.get(&o).copied().unwrap_or(0);
        let (address, width) = (word(BUFFER), word(WIDTH));
        if width == 0 {
            return;
        }
        if width & 1 != 0 || width > 0x2000 {
            return cx.fail("Unsupported live-view line width");
        }
        let (width, lines) = (u64::from(width), u64::from(*lines));
        let image = match picture {
            Picture::Uniform(pair) => Ok(pair.repeat((width / 2 * lines) as usize)),
            Picture::Film(_, _)
                if self.film_picture.is_some()
                    && self
                        .color
                        .as_ref()
                        .is_some_and(|(w, _)| !w.lock().unwrap().is_empty()) =>
            {
                let (words, reference) = self.color.as_ref().unwrap();
                let words = words.lock().unwrap().clone();
                match self.developed_picture(&words, *reference, width, lines) {
                    Some(image) => image,
                    // A unit not programmed yet completes no frame; a unit
                    // between two words develops with the words as they are.
                    None => return,
                }
            }
            Picture::Film(linear, unprogrammed) => {
                let linear = self.scaled_film_light(*linear);
                let pair = match &self.color {
                    Some((words, reference)) if !words.lock().unwrap().is_empty() => {
                        let words = words.lock().unwrap();
                        let developed = crate::development::film(None, &words).and_then(|film| {
                            let factors = crate::development::white_balance(&words, *reference)?;
                            let level =
                                |c: usize| crate::development::balanced(linear[c], factors[c]);
                            Ok(film.develop([level(0), level(1), level(2)]))
                        });
                        match developed {
                            Ok(pixel) => fujimod_emulator_imaging::film::Film::pack(pixel, pixel),
                            // A unit not programmed yet completes no frame.
                            Err(_) => return,
                        }
                    }
                    _ => *unprogrammed,
                };
                Ok(pair.repeat((width / 2 * lines) as usize))
            }
            Picture::Scene => {
                let scene = fujimod_virtual_scene::Scene {
                    width: SCENE_WIDTH,
                    height: SCENE_HEIGHT,
                    frame: self.state.frames,
                };
                fujimod_virtual_scene::output(scene, whole(scene), width, lines, None)
            }
            Picture::Host(w, h, rgb) => {
                let scene = fujimod_virtual_scene::Scene {
                    width: *w,
                    height: *h,
                    frame: 0,
                };
                fujimod_virtual_scene::output_rgb(
                    scene,
                    whole(scene),
                    width,
                    lines,
                    None,
                    |x, y| {
                        let i = ((y * w + x) * 3) as usize;
                        [rgb[i], rgb[i + 1], rgb[i + 2]]
                    },
                )
            }
        };
        let Ok(image) = image else {
            return cx.fail("Live-view scene output refused");
        };
        if cx.ram_write(u64::from(address), &image).is_err() {
            cx.fail("Live-view buffer outside RAM");
        }
    }
    fn started(&self, frames: &Frames) -> bool {
        let (offset, mask) = frames.start;
        self.state.words.get(&offset).is_some_and(|w| w & mask != 0)
    }
    /// Register window size in bytes.
    pub fn window(&self) -> u64 {
        self.size
    }
    /// The stopped bit reads the run state, not the last write.
    fn stopped_mask(&self, offset: u64) -> u32 {
        self.frames
            .filter(|f| f.stopped.0 == offset)
            .map_or(0, |f| f.stopped.1)
    }
    fn offset(&self, address: u64, size: u8) -> Option<u64> {
        let offset = address.checked_sub(self.base)?;
        (size == 4 && offset & 3 == 0 && offset < self.size).then_some(offset)
    }
    /// A naturally aligned byte or halfword access to a word of the block:
    /// the word's address, the bit shift of the lane and the lane mask.
    fn lane(&self, address: u64, size: u8) -> Option<(u64, u32, u64)> {
        let offset = address.checked_sub(self.base)?;
        let mask = match size {
            1 => 0xff,
            2 => 0xffff,
            _ => return None,
        };
        (offset & u64::from(size - 1) == 0 && offset < self.size)
            .then(|| (address & !3, ((offset & 3) * 8) as u32, mask))
    }
}
impl Device for Block {
    fn name(&self) -> &'static str {
        if let Some(name) = self.own_name {
            name
        } else if matches!(self.work, Some(Work::Recc(_))) {
            "xpp-recc"
        } else {
            "xpp-register-block"
        }
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        // XP4 byte-wide controllers (0xfff3e000) and SDHCI halfword fields
        // read their lane of the word, with the word's idle and fixed bits.
        if let Some((word, shift, mask)) = self.lane(address, size) {
            return (self.read(cx, word, 4) >> shift) & mask;
        }
        let Some(offset) = self.offset(address, size) else {
            cx.unmodeled();
            return 0;
        };
        if let Some(&(_, value)) = self.fixed.iter().find(|(o, _)| *o == offset) {
            return u64::from(value);
        }
        let mut idle = self
            .idle
            .iter()
            .filter(|(o, _)| *o == offset)
            .fold(0, |bits, (_, mask)| bits | mask);
        if let Some((_, mask)) = self.job.and_then(|j| j.status).filter(|s| s.0 == offset) {
            return u64::from(self.state.status & mask | idle);
        }
        if let Some(frames) = self.frames.filter(|f| f.stopped.0 == offset) {
            idle = idle & !frames.stopped.1
                | if self.started(&frames) {
                    0
                } else {
                    frames.stopped.1
                };
        }
        u64::from(
            self.state.words.get(&offset).copied().unwrap_or(0) & !self.stopped_mask(offset) | idle,
        )
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        // A lane write updates its lane of the stored word, then acts as a
        // whole-word write of the merged value (commands compare that word).
        if let Some((word, shift, mask)) = self.lane(address, size) {
            let stored = u64::from(
                self.state
                    .words
                    .get(&(word - self.base))
                    .copied()
                    .unwrap_or(0),
            );
            return self.write(cx, word, 4, stored & !(mask << shift) | (value & mask) << shift);
        }
        if let (Some(job), Some(offset), Ok(value)) =
            (self.job, self.offset(address, size), u32::try_from(value))
        {
            if job
                .stop
                .is_some_and(|(o, mask)| o == offset && value & mask == 0)
                && self.state.status != 0
            {
                self.state.status = 0;
                cx.set_irq(job.irq, false);
            }
            if job.status.is_some_and(|s| s.0 == offset) {
                self.state.status &= !value;
                if self.state.status == 0 {
                    cx.set_irq(job.irq, false);
                }
                return;
            }
            if offset == job.start.0 && value & job.start.1 != 0 && self.state.due.is_none() {
                self.state.due = cx.now().checked_add(job.ticks);
                if self.state.due.is_none() {
                    cx.fail("Register block job deadline overflow");
                }
            }
        }
        match (self.offset(address, size), u32::try_from(value)) {
            (Some(offset), Ok(0)) => {
                self.state.words.remove(&offset);
            }
            (Some(offset), Ok(value)) => {
                self.state.words.insert(offset, value);
            }
            _ => return cx.unmodeled(),
        }
        if let Some(c) = self
            .commands
            .iter()
            .find(|c| Some(c.word) == self.offset(address, size) && u64::from(c.value) == value)
        {
            let word = self.state.words.get(&c.status).copied().unwrap_or(0) & !c.mask | c.state;
            if word == 0 {
                self.state.words.remove(&c.status);
            } else {
                self.state.words.insert(c.status, word);
            }
        }
        self.publish();
        if let Some(frames) = self.frames {
            self.state.due = match (self.started(&frames), self.state.due) {
                (false, _) => None,
                (true, Some(at)) => Some(at),
                (true, None) => (cx.now() / frames.period)
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(frames.period)),
            };
        }
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        if let Some(job) = self.job {
            if self.state.due.is_some_and(|at| at <= cx.now()) {
                self.state.due = None;
                if let Some(Work::Recc(r)) = self.work {
                    match self.run_recc(cx, r) {
                        Ok(entry) => {
                            if self.state.recc.len() == RECC_LOG {
                                self.state.recc.remove(0);
                            }
                            self.state.recc.push(entry);
                            if self.state.recc_words.len() < RECC_WORDS {
                                let words = self.state.words.clone();
                                self.state.recc_words.push(words);
                            }
                        }
                        Err(e) => return cx.fail(e),
                    }
                } else if let Some(Work::Audio) = self.work {
                    match self.run_audio(cx) {
                        Ok(Some(true)) => self.state.audio_converted += 1,
                        Ok(Some(false)) => self.state.audio_skipped += 1,
                        Ok(None) => {}
                        Err(e) => return cx.fail(e),
                    }
                } else if let Some(work) = self.work
                    && let Err(e) = self.run_work(cx, work)
                {
                    return cx.fail(e);
                }
                self.state.jobs += 1;
                if job.status.is_none() && job.stop.is_none() {
                    // Pulse job: the start bit clears, the edge line pulses.
                    self.clear_start(job.start);
                    cx.set_irq(job.irq, true);
                    cx.set_irq(job.irq, false);
                    return;
                }
                self.state.status |= job.status.map_or(1, |(_, mask)| mask);
                if self.start_clears {
                    self.clear_start(job.start);
                }
                cx.set_irq(job.irq, true);
            }
            return;
        }
        let (Some(frames), Some(due)) = (self.frames, self.state.due) else {
            return;
        };
        if cx.now() < due {
            return;
        }
        self.write_image(cx);
        cx.set_irq(frames.irq, true);
        cx.set_irq(frames.irq, false);
        self.state.frames += 1;
        let step = ((cx.now() - due) / frames.period + 1).checked_mul(frames.period);
        self.state.due = step.and_then(|step| due.checked_add(step));
        if self.state.due.is_none() {
            cx.fail("Register block frame deadline overflow");
        }
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        self.state.due
    }
    fn save(&self) -> Value {
        serde_json::to_value(&self.state).unwrap_or(Value::Null)
    }
    fn restore(&mut self, value: &Value) -> Result<(), String> {
        let state: State = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        self.check(&state)?;
        self.state = state;
        self.publish();
        Ok(())
    }
    fn checkpoint(&self) -> Result<fujimod_emulator_devices::checkpoint::Checkpoint, String> {
        fujimod_emulator_devices::checkpoint::Checkpoint::encode(self.name(), &self.state)
    }
    fn validate_checkpoint(
        &self,
        saved: &fujimod_emulator_devices::checkpoint::Checkpoint,
    ) -> Result<(), String> {
        if !saved.children().is_empty() || !saved.blobs().is_empty() {
            return Err("Register block checkpoint has unexpected parts".into());
        }
        self.check(&saved.decode(self.name())?)
    }
    /// The published words are shared with the color unit's reader and are
    /// refreshed in place from the restored register file.
    fn restore_checkpoint(
        &mut self,
        saved: &fujimod_emulator_devices::checkpoint::Checkpoint,
    ) -> Result<(), String> {
        self.validate_checkpoint(saved)?;
        self.state = saved.decode(self.name())?;
        self.publish();
        Ok(())
    }
    fn fingerprint(&self) -> Result<Value, String> {
        Ok(self.checkpoint()?.fingerprint())
    }
}
impl Block {
    fn clear_start(&mut self, (offset, mask): (u64, u32)) {
        let word = self.state.words.get(&offset).copied().unwrap_or(0) & !mask;
        if word == 0 {
            self.state.words.remove(&offset);
        } else {
            self.state.words.insert(offset, word);
        }
        self.publish();
    }
    fn check(&self, state: &State) -> Result<(), String> {
        let started = self.frames.is_some_and(|f| {
            let (offset, mask) = f.start;
            state.words.get(&offset).is_some_and(|w| w & mask != 0)
        });
        if state
            .words
            .iter()
            .any(|(o, v)| o & 3 != 0 || *o >= self.size || *v == 0)
            || self.frames.is_some() && started != state.due.is_some()
            || self.frames.is_none() && self.job.is_none() && state.due.is_some()
            || self.job.is_none() && (state.status != 0 || state.jobs != 0)
            || self.job.is_some_and(|j| match j.status {
                Some((offset, mask)) => {
                    state.status & !mask != 0 || state.words.contains_key(&offset)
                }
                None => state.status > 1,
            })
            || self
                .frames
                .zip(state.due)
                .is_some_and(|(f, at)| at % f.period != 0)
        {
            return Err("Invalid register block checkpoint".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujimod_emulator_devices::test_host::{TestHost, read, write};

    const BASE: u64 = 0xfff55000;

    #[test]
    fn words_read_back_and_idle_bits_read_set() {
        let mut unit = Block::new(BASE, 0x200, vec![(0, 1), (0xd8, 0x100)], None).unwrap();
        let mut h = TestHost::new();
        // 0xc3a954: stop, then wait for bit 0.
        write(&mut unit, &mut h, BASE, 0);
        assert_eq!(read(&mut unit, &mut h, BASE), 1);
        // 0xc3ab78 sets +0xd8 bit 0; 0xc3be40 waits for bit 8.
        write(&mut unit, &mut h, BASE + 0xd8, 1);
        assert_eq!(read(&mut unit, &mut h, BASE + 0xd8), 0x101);
        write(&mut unit, &mut h, BASE + 0xc0, 0xa7d_6000);
        let saved = unit.save();
        write(&mut unit, &mut h, BASE + 0xc0, 0);
        unit.restore(&saved).unwrap();
        assert_eq!(read(&mut unit, &mut h, BASE + 0xc0), 0xa7d_6000);
        let mut cx = h.cx();
        unit.read(&mut cx, BASE + 0x200, 4);
        assert!(cx.is_unmodeled());
        assert!(Block::new(BASE, 0x200, vec![(0x200, 1)], None).is_err());
    }

    #[test]
    fn x_t20_auto_mode_unit_reads_the_status_bits_its_driver_waits_for() {
        // 0xfffd4000: 0x155d0ec polls +0x00 bit 0, 0x155d40c polls +0x088 bits 0 and 16.
        let mut h = TestHost::new();
        let mut pipeline =
            Block::new(0xfffd4000, 0x1000, vec![(0, 1), (0x88, 1 << 16 | 1)], None).unwrap();
        write(&mut pipeline, &mut h, 0xfffd4000, 0);
        assert_eq!(read(&mut pipeline, &mut h, 0xfffd4000) & 1, 1);
        assert_eq!(
            read(&mut pipeline, &mut h, 0xfffd4088) & (1 << 16 | 1),
            1 << 16 | 1
        );
    }

    #[test]
    fn started_blocks_finish_a_frame_each_period() {
        use fujimod_emulator_devices::test_host::advance;
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 107,
            period: 10,
        };
        assert!(Block::new(BASE, 0x200, vec![(0xd8, 0x100)], Some(frames)).is_err());
        let mut unit = Block::new(BASE, 0x200, vec![(0, 1)], Some(frames)).unwrap();
        let mut h = TestHost::new();
        assert_eq!(read(&mut unit, &mut h, BASE + 0xd8), 0x100);
        assert_eq!(unit.next_deadline(0), None);
        h.now = 3;
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28 | 1 << 24);
        assert_eq!(read(&mut unit, &mut h, BASE + 0xd8), 0);
        assert_eq!(unit.next_deadline(3), Some(10));
        // 0xc3abe8 sets bit 0 for each frame; bit 8 keeps reading the run state.
        write(&mut unit, &mut h, BASE + 0xd8, 0x101);
        assert_eq!(read(&mut unit, &mut h, BASE + 0xd8), 1);
        advance(&mut unit, &mut h, 25);
        assert_eq!(unit.state.frames, 1);
        assert_eq!(unit.next_deadline(25), Some(30));
        let saved = unit.save();
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 24);
        assert_eq!(read(&mut unit, &mut h, BASE + 0xd8), 0x101);
        assert_eq!(unit.next_deadline(25), None);
        unit.restore(&saved).unwrap();
        assert_eq!(unit.next_deadline(25), Some(30));
        assert!(
            unit.restore(&serde_json::json!({ "words": [] , "due": 30 }))
                .is_err()
        );
    }

    #[test]
    fn a_started_job_finishes_once_and_holds_its_level_interrupt() {
        use fujimod_emulator_devices::test_host::advance;
        const ENGINE: u64 = 0xfff7f000;
        let job = Job {
            start: (0x04, 1),
            status: Some((0x44, 1)),
            stop: None,
            irq: 128,
            ticks: 100,
        };
        let block = || Block::new(ENGINE, 0x1000, vec![(0, 1), (0x40, 0x1_0001)], None).unwrap();
        assert!(block().with_job(Job { ticks: 0, ..job }).is_err());
        assert!(
            block()
                .with_job(Job {
                    status: Some((0x40, 1)),
                    ..job
                })
                .is_err()
        );
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 107,
            period: 10,
        };
        assert!(
            Block::new(ENGINE, 0x1000, vec![], Some(frames))
                .unwrap()
                .with_job(job)
                .is_err()
        );
        let mut engine = block().with_job(job).unwrap();
        let mut h = TestHost::new();
        // 0x1558468 writes 1 to the status; 0x15589c4 waits for +0x40 and
        // sets +0x04 bit 0.
        write(&mut engine, &mut h, ENGINE + 0x44, 1);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x40), 0x1_0001);
        h.now = 5;
        write(&mut engine, &mut h, ENGINE + 0x04, 1);
        assert_eq!(engine.next_deadline(5), Some(105));
        // A second start while the job runs does not move it.
        write(&mut engine, &mut h, ENGINE + 0x04, 1);
        assert_eq!(engine.next_deadline(5), Some(105));
        advance(&mut engine, &mut h, 104);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x44), 0);
        advance(&mut engine, &mut h, 105);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x44), 1);
        assert_eq!(engine.state.status, 1);
        assert_eq!(engine.next_deadline(105), None);
        let saved = engine.save();
        // 0x1558108 reads the status and writes 1 to acknowledge it.
        write(&mut engine, &mut h, ENGINE + 0x44, 1);
        assert_eq!(engine.state.status, 0);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x44), 0);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x04), 1);
        engine.restore(&saved).unwrap();
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x44), 1);
        assert!(block().restore(&saved).is_err());
        write(&mut engine, &mut h, ENGINE + 0x04, 1);
        advance(&mut engine, &mut h, 205);
        assert_eq!(engine.state.jobs, 2);
    }

    #[test]
    fn live_view_units_write_the_declared_image_each_frame() {
        use fujimod_emulator_devices::test_host::advance;
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 108,
            period: 10,
        };
        let block = || Block::new(BASE, 0x200, vec![(0, 1)], Some(frames)).unwrap();
        assert!(
            Block::new(BASE, 0x200, vec![], None)
                .unwrap()
                .with_live_view_image(480, [73, 114, 138])
                .is_err()
        );
        assert!(block().with_live_view_image(0, [73, 114, 138]).is_err());
        let mut unit = block().with_live_view_image(3, [73, 114, 138]).unwrap();
        let mut h = TestHost::new();
        write(&mut unit, &mut h, BASE + BUFFER, 0x1000);
        write(&mut unit, &mut h, BASE + WIDTH, 4);
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28);
        advance(&mut unit, &mut h, 10);
        assert_eq!(h.ram[0x1000..0x1008], [73, 73, 114, 138, 73, 73, 114, 138]);
        assert_eq!(h.ram[0x1014..0x1018], [73, 73, 114, 138]);
        assert_eq!(h.ram[0x1018], 0);
        write(&mut unit, &mut h, BASE + BUFFER, 0x2000);
        advance(&mut unit, &mut h, 20);
        assert_eq!(h.ram[0x2000..0x2004], [73, 73, 114, 138]);
        write(&mut unit, &mut h, BASE + WIDTH, 3);
        h.now = 30;
        let mut cx = h.cx();
        unit.advance(&mut cx);
        assert_eq!(cx.refusal(), Some("Unsupported live-view line width"));
    }

    #[test]
    fn a_live_view_film_picture_is_developed_pixel_by_pixel() {
        use crate::development::tests::provia;
        use fujimod_emulator_devices::test_host::advance;
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 108,
            period: 10,
        };
        let reference = [1138, 604, 1096];
        let words = Published::default();
        let film = || {
            Block::new(BASE, 0x200, vec![(0, 1)], Some(frames))
                .unwrap()
                .with_live_view_film(
                    1,
                    [1015, 764, 577],
                    [16, 128, 128],
                    words.clone(),
                    reference,
                )
                .unwrap()
        };
        // Only a film unit takes a picture, and only one of its declared size.
        assert!(
            Block::new(BASE, 0x200, vec![(0, 1)], Some(frames))
                .unwrap()
                .with_film_picture(2, 1, vec![0; 6])
                .is_err()
        );
        assert!(film().with_film_picture(2, 1, vec![0; 5]).is_err());
        // A dark left half and a bright right half.
        let mut unit = film()
            .with_film_picture(2, 1, vec![20, 20, 20, 230, 230, 230])
            .unwrap();
        let mut h = TestHost::new();
        write(&mut unit, &mut h, BASE + BUFFER, 0x1000);
        write(&mut unit, &mut h, BASE + WIDTH, 4);
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28);
        advance(&mut unit, &mut h, 10);
        // Unprogrammed, the declared pair.
        assert_eq!(h.ram[0x1000..0x1008], [16, 16, 128, 128, 16, 16, 128, 128]);
        let mut all = provia();
        all.insert(0x100, reference[0] | reference[1] << 16);
        all.insert(0x104, reference[2]);
        *words.lock().unwrap() = all;
        h.ram[0x1000..0x1008].fill(0);
        advance(&mut unit, &mut h, 20);
        let (dark, bright) = (h.ram[0x1000], h.ram[0x1004]);
        assert!(dark < bright, "{dark} {bright}");
        // A neutral picture stays near neutral at the reference white balance.
        for c in [h.ram[0x1006], h.ram[0x1007]] {
            assert!(c.abs_diff(128) <= 8, "{c}");
        }
        // The same inputs give the same frame.
        let first = h.ram[0x1000..0x1008].to_vec();
        h.ram[0x1000..0x1008].fill(0);
        advance(&mut unit, &mut h, 30);
        assert_eq!(h.ram[0x1000..0x1008], first[..]);
    }

    #[test]
    fn a_live_view_film_unit_develops_its_scene_with_the_color_units_words() {
        use crate::development::tests::provia;
        use fujimod_emulator_devices::test_host::advance;
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 108,
            period: 10,
        };
        let block = || Block::new(BASE, 0x200, vec![(0, 1)], Some(frames)).unwrap();
        let reference = [1138, 604, 1096];
        let words = Published::default();
        // The color unit's words with the gains of the reference white balance.
        let program = |matrix: Option<[u32; 6]>, gains: [u32; 3]| {
            let mut all = provia();
            for (i, word) in matrix.into_iter().flatten().enumerate() {
                all.insert(0x300 + 4 * i as u64, word);
            }
            all.insert(0x100, gains[0] | gains[1] << 16);
            all.insert(0x104, gains[2]);
            *words.lock().unwrap() = all;
        };
        assert!(
            block()
                .with_live_view_film(
                    3,
                    [1015, 764, 577],
                    [16, 128, 128],
                    words.clone(),
                    [0, 1, 1]
                )
                .is_err()
        );
        assert!(
            block()
                .with_live_view_film(3, [20000, 0, 0], [16, 128, 128], words.clone(), reference)
                .is_err()
        );
        let mut unit = block()
            .with_live_view_film(
                3,
                [1015, 764, 577],
                [16, 128, 128],
                words.clone(),
                reference,
            )
            .unwrap();
        let mut h = TestHost::new();
        write(&mut unit, &mut h, BASE + BUFFER, 0x1000);
        write(&mut unit, &mut h, BASE + WIDTH, 4);
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28);
        // Until the firmware programs the color unit the declared pair is written.
        advance(&mut unit, &mut h, 10);
        assert_eq!(h.ram[0x1000..0x1004], [16, 16, 128, 128]);
        let mut frame = |unit: &mut Block, h: &mut TestHost, now| {
            h.ram[0x1000..0x1100].fill(0);
            advance(unit, h, now);
            h.ram[0x1000..0x1004].to_vec()
        };
        program(None, reference);
        let provia_frame = frame(&mut unit, &mut h, 20);
        // PROVIA at the reference white balance gives the measured wall.
        assert_eq!(provia_frame, [73, 73, 114, 138]);
        // The same words give the same frame; the whole line is one pair.
        assert_eq!(frame(&mut unit, &mut h, 30), provia_frame);
        assert_eq!(h.ram[0x1004..0x1008], provia_frame[..]);
        // Another film matrix and another white balance each change it.
        let velvia = [0x1f71040b, 0x84, 0x04b61fb6, 0x1f94, 0x1f84002c, 0x450];
        program(Some(velvia), reference);
        let velvia_frame = frame(&mut unit, &mut h, 40);
        assert_ne!(velvia_frame, provia_frame);
        program(None, [3917, 604, 2776]);
        let warm_frame = frame(&mut unit, &mut h, 50);
        assert_ne!(warm_frame, provia_frame);
        assert_ne!(warm_frame, velvia_frame);
        // Unequal curve copies are a legitimate per-channel program: each
        // output channel develops through its own slot with the words the
        // unit holds, so the frame is not empty.
        program(None, reference);
        words.lock().unwrap().insert(0x440, 0);
        assert_ne!(frame(&mut unit, &mut h, 60), [0; 4]);
    }

    #[test]
    fn a_live_view_film_follows_the_published_exposure_and_scene_light() {
        use crate::statistics::{Exposure, LIGHT_UNIT, SceneLight};
        use fujimod_emulator_devices::test_host::advance;
        use std::sync::atomic::{AtomicU32, Ordering};
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 108,
            period: 10,
        };
        let reference = [1138, 604, 1096];
        let words = Published::default();
        let value = std::sync::Arc::new(AtomicU32::new(0));
        let light = std::sync::Arc::new(AtomicU32::new(LIGHT_UNIT));
        let block = || Block::new(BASE, 0x200, vec![(0, 1)], Some(frames)).unwrap();
        let film = || {
            block()
                .with_live_view_film(
                    3,
                    [1015, 764, 577],
                    [16, 128, 128],
                    words.clone(),
                    reference,
                )
                .unwrap()
                .with_film_exposure(Exposure {
                    value: value.clone(),
                    reference: 1566,
                })
                .unwrap()
                .with_film_scene_light(SceneLight {
                    value: light.clone(),
                })
                .unwrap()
        };
        let mut unit = film();
        // Not yet published: the declared light.
        assert_eq!(
            unit.scaled_film_light([1000, 2000, 16383]),
            [1000, 2000, 16383]
        );
        // Twice the reference doubles it, clamped to the 14-bit range.
        value.store(2 * 1566, Ordering::Relaxed);
        assert_eq!(
            unit.scaled_film_light([1000, 2000, 16383]),
            [2000, 4000, 16383]
        );
        // The scene light is a second scale on the same light.
        value.store(0, Ordering::Relaxed);
        light.store(LIGHT_UNIT / 2, Ordering::Relaxed);
        assert_eq!(
            unit.scaled_film_light([1000, 2000, 16383]),
            [500, 1000, 8192]
        );
        // The developed frame brightens with the exposure and keeps its
        // declared light while the value is 0.
        let mut all = crate::development::tests::provia();
        all.insert(0x100, reference[0] | reference[1] << 16);
        all.insert(0x104, reference[2]);
        *words.lock().unwrap() = all;
        let mut h = TestHost::new();
        write(&mut unit, &mut h, BASE + BUFFER, 0x1000);
        write(&mut unit, &mut h, BASE + WIDTH, 4);
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28);
        let frame = |unit: &mut Block, h: &mut TestHost, now| {
            h.ram[0x1000..0x1100].fill(0);
            advance(unit, h, now);
            h.ram[0x1000]
        };
        light.store(LIGHT_UNIT, Ordering::Relaxed);
        let declared = frame(&mut unit, &mut h, 10);
        value.store(2 * 1566, Ordering::Relaxed);
        let brighter = frame(&mut unit, &mut h, 20);
        assert!(brighter > declared, "{brighter} vs {declared}");
        value.store(0, Ordering::Relaxed);
        assert_eq!(frame(&mut unit, &mut h, 30), declared);
        // Only a film picture takes the two scales.
        let plain = block().with_live_view_image(3, [73, 114, 138]).unwrap();
        assert!(
            plain
                .with_film_exposure(Exposure {
                    value: value.clone(),
                    reference: 1566
                })
                .is_err()
        );
        assert!(
            block()
                .with_live_view_image(3, [73, 114, 138])
                .unwrap()
                .with_film_scene_light(SceneLight {
                    value: light.clone()
                })
                .is_err()
        );
    }

    #[test]
    fn a_live_view_scene_unit_writes_the_moving_virtual_scene() {
        use fujimod_emulator_devices::test_host::advance;
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 108,
            period: 10,
        };
        let mut unit = Block::new(BASE, 0x200, vec![(0, 1)], Some(frames))
            .unwrap()
            .with_live_view_scene(4)
            .unwrap();
        let mut h = TestHost::new();
        write(&mut unit, &mut h, BASE + BUFFER, 0x1000);
        write(&mut unit, &mut h, BASE + WIDTH, 8);
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28);
        advance(&mut unit, &mut h, 10);
        // The white and yellow bars at the top left (eight pixels sample one
        // bar each), BT.601 limited, chroma averaged over the pair.
        assert_eq!(h.ram[0x1000..0x1004], [235, 210, 72, 137]);
        let first = h.ram[0x1000..0x1040].to_vec();
        advance(&mut unit, &mut h, 20);
        // The ramp moves with the frame phase.
        assert_ne!(h.ram[0x1000..0x1040], first[..]);
        assert_eq!(h.ram[0x1040], 0);
    }

    #[test]
    fn a_live_view_host_image_unit_samples_the_image_into_the_buffer() {
        use fujimod_emulator_devices::test_host::advance;
        let frames = Frames {
            start: (0xc8, 1 << 28),
            stopped: (0xd8, 0x100),
            irq: 108,
            period: 10,
        };
        let unit = || Block::new(BASE, 0x200, vec![(0, 1)], Some(frames)).unwrap();
        let (red, blue) = ([255, 0, 0], [0, 0, 255]);
        assert!(
            unit()
                .with_live_view_host_image(2, 2, 1, red.to_vec())
                .is_err()
        );
        let mut unit = unit()
            .with_live_view_host_image(2, 2, 1, [red, blue].concat())
            .unwrap();
        let mut h = TestHost::new();
        write(&mut unit, &mut h, BASE + BUFFER, 0x1000);
        write(&mut unit, &mut h, BASE + WIDTH, 4);
        write(&mut unit, &mut h, BASE + 0xc8, 1 << 28);
        advance(&mut unit, &mut h, 10);
        // Each image pixel covers two output pixels, on both lines.
        let row = [[81, 81, 90, 240], [41, 41, 240, 110]].concat();
        assert_eq!(h.ram[0x1000..0x1010], [row.clone(), row].concat()[..]);
        assert_eq!(h.ram[0x1010], 0);
    }

    #[test]
    fn development_engine_jobs_share_idle_status_words_or_have_no_status() {
        use fujimod_emulator_devices::test_host::advance;
        // 0xfffc4000: start +0x00 bit 0, done +0x3c bit 0 beside idle bit 20.
        const ENGINE: u64 = 0xfffc4000;
        let job = Job {
            start: (0, 1),
            status: Some((0x3c, 1)),
            stop: None,
            irq: 132,
            ticks: 10,
        };
        let block = |idle| Block::new(ENGINE, 0x1000, idle, None).unwrap();
        assert!(block(vec![(0x3c, 1 << 20 | 1)]).with_job(job).is_err());
        let mut engine = block(vec![(0, 1 << 28), (0x3c, 1 << 20)])
            .with_job(job)
            .unwrap();
        let mut h = TestHost::new();
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x3c), 1 << 20);
        write(&mut engine, &mut h, ENGINE, 1);
        advance(&mut engine, &mut h, 10);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x3c), 1 << 20 | 1);
        write(&mut engine, &mut h, ENGINE + 0x3c, 1);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x3c), 1 << 20);
        // 0xfff98000: start +0x04 bit 0, no status; the handler stops it
        // (+0x00 = 0) to acknowledge interrupt 130.
        let job = Job {
            start: (4, 1),
            status: None,
            stop: Some((0, 1)),
            irq: 130,
            ticks: 10,
        };
        let block = || Block::new(0xfff98000, 0x1000, vec![(0, 1)], None).unwrap();
        assert!(block().with_job(Job { status: Some((8, 1)), ..job }).is_err());
        let mut engine = block().with_job(job).unwrap();
        write(&mut engine, &mut h, 0xfff98004, 1);
        assert_eq!(engine.next_deadline(10), Some(20));
        advance(&mut engine, &mut h, 20);
        assert_eq!(engine.state.status, 1);
        assert_eq!(engine.state.jobs, 1);
        assert_eq!(engine.next_deadline(20), None);
        let saved = engine.save();
        write(&mut engine, &mut h, 0xfff98000, 0);
        assert_eq!(engine.state.status, 0);
        assert_eq!(read(&mut engine, &mut h, 0xfff98000), 1);
        engine.restore(&saved).unwrap();
        assert_eq!(engine.state.status, 1);
    }

    #[test]
    fn recc_white_balance_needs_the_firmware_color_and_a_nonzero_reference() {
        let job = Job { start: (4, 1), status: None, stop: None, irq: 129, ticks: 10 };
        let recc = Recc { raw: (0x3000, 40), site: (5, 1), area: (0x1000, 0x1000) };
        let block = || {
            Block::new(0xfff82000, 0x1000, vec![(0, 1)], None)
                .unwrap()
                .with_job(job)
                .unwrap()
                .with_work(Work::Recc(recc))
                .unwrap()
        };
        let (tone, ycbcr): (Published, Published) = Default::default();
        assert!(block().with_recc_white_balance([9149, 4822, 9309]).is_err());
        let color = || block().with_recc_color(tone.clone(), ycbcr.clone(), false).unwrap();
        assert!(color().with_recc_white_balance([9149, 0, 9309]).is_err());
        let on = color().with_recc_white_balance([9149, 4822, 9309]).unwrap();
        assert_eq!(on.recc_white_balance, Some([9149, 4822, 9309]));
        assert_eq!(color().recc_white_balance, None);
    }

    #[test]
    fn recc_develops_with_the_firmware_color_words_as_they_stand_at_each_job() {
        use crate::development::XTRANS;
        use fujimod_emulator_devices::test_host::advance;
        const RECC: u64 = 0xfff82000;
        const TONE: u64 = 0xfff86000;
        const YCBCR: u64 = 0xfff87000;
        let job = Job { start: (4, 1), status: None, stop: None, irq: 129, ticks: 10 };
        let recc = Recc { raw: (0x3000, 40), site: (5, 1), area: (0x1000, 0x1000) };
        let block = || Block::new(RECC, 0x1000, vec![(0, 1)], None).unwrap().with_job(job).unwrap();
        let (tone_words, ycbcr_words): (Published, Published) = Default::default();
        let mut tone = Block::new(TONE, 0x1000, vec![], None).unwrap().with_published(tone_words.clone());
        let mut ycbcr =
            Block::new(YCBCR, 0x1000, vec![], None).unwrap().with_published(ycbcr_words.clone());
        assert!(block().with_recc_color(tone_words.clone(), ycbcr_words.clone(), false).is_err());
        assert!(block()
            .with_work(Work::Recc(recc))
            .unwrap()
            .with_recc_color(tone_words.clone(), tone_words.clone(), false)
            .is_err());
        let mut engine = block()
            .with_work(Work::Recc(recc))
            .unwrap()
            .with_recc_color(tone_words.clone(), ycbcr_words.clone(), false)
            .unwrap();
        let mut declared = block().with_work(Work::Recc(recc)).unwrap();
        let mut h = TestHost::new();
        // The firmware programs both units through their registers.
        let (t, c) = crate::development::tests::firmware_units();
        for (o, v) in &t {
            write(&mut tone, &mut h, TONE + o, *v);
        }
        for (o, v) in &c {
            write(&mut ycbcr, &mut h, YCBCR + o, *v);
        }
        assert_eq!(*tone_words.lock().unwrap(), t);
        // Red sites at a quarter of the range, green and blue sites at an
        // eighth: a reddish grey.
        for y in 0..20usize {
            for x in 0..16usize {
                let level = if XTRANS[(y + 5) % 6][(x + 1) % 6] == 0 { 4864 } else { 2944 };
                let at = 0x3000 + y * 40 + x * 2;
                h.ram[at..at + 2].copy_from_slice(&(level as u16).to_le_bytes());
            }
        }
        let w = |e: &mut Block, h: &mut TestHost, o: u64, v: u32| write(e, h, RECC + o, v);
        let band = |e: &mut Block, h: &mut TestHost, now: u64| {
            w(e, h, 0x08, 0x62_0000);
            w(e, h, 0x0c, 0x3000 + 2 * 40 + 4);
            w(e, h, 0x14, 40);
            w(e, h, 0x18, 6 << 16 | 12);
            w(e, h, 0x1c, 0);
            w(e, h, 0x54, 0);
            w(e, h, 0x70, 0x1000);
            w(e, h, 0x80, 32);
            w(e, h, 0x04, 1);
            advance(e, h, now);
            (0..6).map(|y| h.ram[0x1000 + y * 32..0x1000 + y * 32 + 24].to_vec()).collect::<Vec<_>>()
        };
        let declared_rows = band(&mut declared, &mut h, 10);
        let firmware = band(&mut engine, &mut h, 20);
        assert_ne!(firmware, declared_rows);
        let color = crate::development::FirmwareColor::from_words(&t, &c).unwrap();
        let linear = |s: u32| ((s - 1024) * 16383 / (16383 - 1024)) as u16;
        let [y, cb, cr] = color.pixel([linear(4864), linear(2944), linear(2944)]);
        assert!(cr > 128 && cb < 128);
        for row in &firmware {
            for pair in row.chunks_exact(4) {
                assert_eq!(pair, [y as u8, y as u8, cb as u8, cr as u8]);
            }
        }
        // A halved red curve, written before the next job, is the one it uses.
        for i in 0..16u64 {
            let o = 0x224 + 4 * i;
            write(&mut tone, &mut h, TONE + o, (t[&o] >> 1) & 0x7fff_7fff);
        }
        let halved = band(&mut engine, &mut h, 30);
        assert!(halved[0][3] < firmware[0][3], "{:?} {:?}", halved[0], firmware[0]);
        // The LUT unit: eight table blocks apart from the control block, and
        // only after the firmware color.
        let (control, table): (Published, Vec<Published>) =
            (Default::default(), (0..8).map(|_| Default::default()).collect());
        assert!(block()
            .with_work(Work::Recc(recc))
            .unwrap()
            .with_recc_lut3d(control.clone(), table.clone())
            .is_err());
        let colored = || {
            block()
                .with_work(Work::Recc(recc))
                .unwrap()
                .with_recc_color(tone_words.clone(), ycbcr_words.clone(), false)
                .unwrap()
        };
        assert!(colored().with_recc_lut3d(control.clone(), table[..7].to_vec()).is_err());
        let mut aliased = table.clone();
        aliased[3] = control.clone();
        assert!(colored().with_recc_lut3d(control.clone(), aliased).is_err());
        let mut lut = colored().with_recc_lut3d(control.clone(), table.clone()).unwrap();
        // Disabled (no words yet): the plain firmware color of the halved curve.
        assert_eq!(band(&mut lut, &mut h, 40), halved);
        // The firmware's monochrome table, enabled: grey, Cb = Cr = 128.
        let y = |r: u32, g: u32, b: u32| (306 * r + 601 * g + 117 * b) * 273 / 1024;
        let (c, words) = crate::development::tests::lut_words(|r, g, b| [y(r, g, b); 3]);
        *control.lock().unwrap() = c;
        for (o, v) in words {
            table[(o / 0x1000) as usize].lock().unwrap().insert(o % 0x1000, v);
        }
        let grey = band(&mut lut, &mut h, 50);
        for row in &grey {
            for pair in row.chunks_exact(4) {
                assert!(pair[2].abs_diff(128) <= 1 && pair[3].abs_diff(128) <= 1, "{pair:?}");
            }
        }
    }

    #[test]
    fn recc_develops_its_bands_then_copies_the_picture_into_place() {
        use crate::development::XTRANS;
        use fujimod_emulator_devices::test_host::advance;
        const RECC: u64 = 0xfff82000;
        let job = Job { start: (4, 1), status: None, stop: None, irq: 129, ticks: 10 };
        // RAW 16 x 20 at 0x3000, pitch 40; picture area 0x1000..0x2000.
        let recc = Recc { raw: (0x3000, 40), site: (5, 1), area: (0x1000, 0x1000) };
        let block = || Block::new(RECC, 0x1000, vec![(0, 1)], None).unwrap().with_job(job).unwrap();
        assert!(block().with_work(Work::Recc(Recc { site: (6, 0), ..recc })).is_err());
        let mut engine = block().with_work(Work::Recc(recc)).unwrap();
        assert_eq!(engine.name(), "xpp-recc");
        let mut h = TestHost::new();
        // Only red sites lit, the RAW's first sample at site (5, 1).
        for y in 0..20usize {
            for x in 0..16usize {
                let red = XTRANS[(y + 5) % 6][(x + 1) % 6] == 0;
                let at = 0x3000 + y * 40 + x * 2;
                h.ram[at..at + 2].copy_from_slice(&(if red { 9216u16 } else { 1024 }).to_le_bytes());
            }
        }
        let w = |e: &mut Block, h: &mut TestHost, o: u64, v: u32| write(e, h, RECC + o, v);
        // Two bands of 6 lines x 12 pixels from RAW line 2, sample 2, written
        // 2 lines and 2 pixels into the picture (pitch 32 at 0x1000).
        let band = |e: &mut Block, h: &mut TestHost, line: u32, now: u64| {
            w(e, h, 0x08, 0x62_0000);
            w(e, h, 0x0c, 0x3000 + 2 * 40 + 4);
            w(e, h, 0x14, 40);
            w(e, h, 0x18, 6 << 16 | 12);
            w(e, h, 0x1c, line << 16);
            w(e, h, 0x54, line << 16);
            w(e, h, 0x70, 0x1000 + 2 * 32 + 4);
            w(e, h, 0x80, 32);
            w(e, h, 0x04, 1);
            advance(e, h, now);
        };
        band(&mut engine, &mut h, 0, 10);
        band(&mut engine, &mut h, 6, 20);
        assert_eq!(
            engine.state.recc,
            vec![(0x3054, 0x1044, 6), (0x3054 + 6 * 40, 0x1044 + 6 * 32, 6)]
        );
        for y in 0..12 {
            let at = 0x1044 + y * 32;
            for pair in h.ram[at..at + 24].chunks_exact(4) {
                assert!(pair[3] > 200 && pair[2] < 128, "line {y}: {pair:?}");
            }
        }
        // The copy job moves the 12 x 12 picture to the start of the area.
        w(&mut engine, &mut h, 0x08, 2);
        w(&mut engine, &mut h, 0x0c, 0);
        w(&mut engine, &mut h, 0x3c, 0x1044);
        w(&mut engine, &mut h, 0x4c, 32);
        w(&mut engine, &mut h, 0x50, 12 << 16 | 12);
        w(&mut engine, &mut h, 0x1c, 0);
        w(&mut engine, &mut h, 0x54, 0);
        w(&mut engine, &mut h, 0x70, 0x1000);
        w(&mut engine, &mut h, 0x04, 1);
        let developed: Vec<Vec<u8>> =
            (0..12).map(|y| h.ram[0x1044 + y * 32..0x1044 + y * 32 + 24].to_vec()).collect();
        advance(&mut engine, &mut h, 30);
        assert_eq!(engine.state.recc[2], (0x1044, 0x1000, 12));
        for (y, row) in developed.iter().enumerate() {
            assert_eq!(&h.ram[0x1000 + y * 32..0x1000 + y * 32 + 24], &row[..]);
        }
        // A destination outside the area is logged, not written.
        w(&mut engine, &mut h, 0x70, 0x8000);
        w(&mut engine, &mut h, 0x04, 1);
        advance(&mut engine, &mut h, 40);
        assert_eq!(engine.state.recc[3], (0x1044, 0x8000, 0));
        assert_eq!(engine.state.jobs, 4);
        assert_eq!(engine.state.recc_words.len(), 4);
        let saved = engine.save();
        engine.restore(&saved).unwrap();
        assert_eq!(engine.state.recc.len(), 4);
    }

    #[test]
    fn recc_movie_scales_two_bands_of_the_raw_into_y0l2_rows() {
        use crate::development::y0l2_sample;
        use fujimod_emulator_devices::test_host::advance;
        const RECC: u64 = 0xfff82000;
        let job = Job { start: (4, 1), status: None, stop: None, irq: 129, ticks: 10 };
        let recc = Recc { raw: (0x3000, 32), site: (5, 1), area: (0x1000, 0x1000) };
        let movie = ReccMovie { shift: 2, color: false };
        let block = || Block::new(RECC, 0x1000, vec![(0, 1)], None).unwrap().with_job(job).unwrap();
        assert!(block().with_recc_movie(movie).is_err(), "RECC only");
        let recc_block = || block().with_work(Work::Recc(recc)).unwrap();
        assert!(recc_block().with_recc_movie(ReccMovie { shift: 17, ..movie }).is_err());
        // RAW 16 x 18 at 0x3000, pitch 32: sample = 50 * line + column.
        let raw = |h: &mut TestHost| {
            for y in 0..18usize {
                for x in 0..16usize {
                    let at = 0x3000 + y * 32 + x * 2;
                    h.ram[at..at + 2].copy_from_slice(&((50 * y + x) as u16).to_le_bytes());
                }
            }
        };
        let w = |e: &mut Block, h: &mut TestHost, o: u64, v: u32| write(e, h, RECC + o, v);
        // A band: RAW lines from `first_in`, 8 samples a line, onto `lines` picture lines of
        // 6 pixels from `first_out` (pitch 32 at 0x1000): the 3/4 step of the 4K movie.
        let band = |e: &mut Block, h: &mut TestHost, first_in: u32, first_out: u32, lines: u32, now: u64| {
            w(e, h, 0x08, 0x42_2000);
            w(e, h, 0x0c, 0x3000);
            w(e, h, 0x14, 32);
            w(e, h, 0x1c, first_in << 16);
            w(e, h, 0x20, 8);
            w(e, h, 0x24, (lines * 4 / 3) << 16);
            w(e, h, 0x50, lines << 16);
            w(e, h, 0x54, first_out << 16);
            w(e, h, 0x58, 6);
            w(e, h, 0x70, 0x1000);
            w(e, h, 0x80, 32);
            w(e, h, 0x04, 1);
            advance(e, h, now);
        };
        // Without the movie mode, RECC writes nothing in mode 0x422000.
        let mut plain = recc_block();
        let mut h = TestHost::new();
        raw(&mut h);
        band(&mut plain, &mut h, 2, 0, 8, 10);
        assert_eq!(plain.state.recc[0].2, 0);
        assert!(h.ram[0x1000..0x2000].iter().all(|&b| b == 0));
        // Y0L2: band 1 writes lines 0..8 from RAW line 2, band 2 lines 6..12 from RAW line 10;
        // the two lines they share come out the same.
        // Output code (+0x6c bits 1..0) 3: Y0L2 line pairs, as X-T3 5.11 programs it.
        let mut engine = recc_block().with_recc_movie(movie).unwrap();
        let mut h = TestHost::new();
        raw(&mut h);
        w(&mut engine, &mut h, 0x6c, 3);
        band(&mut engine, &mut h, 2, 0, 8, 10);
        let shared: Vec<u8> = h.ram[0x1000 + 3 * 32..0x1000 + 4 * 32].to_vec();
        band(&mut engine, &mut h, 10, 6, 6, 20);
        assert_eq!(engine.state.recc, vec![(0x3000 + 2 * 32, 0x1000, 8), (0x3000 + 10 * 32, 0x1000 + 3 * 32, 6)]);
        assert_eq!(h.ram[0x1000 + 3 * 32..0x1000 + 4 * 32], shared[..]);
        for y in 0..12usize {
            for x in 0..6usize {
                let row = &h.ram[0x1000 + y / 2 * 32..];
                let want = (50 * (2 + y * 8 / 6) + x * 8 / 6) as u16;
                assert_eq!(y0l2_sample(row, x, y % 2), (want, 512, 512), "line {y} pixel {x}");
            }
        }
        assert!(h.ram[0x1000 + 6 * 32..0x2000].iter().all(|&b| b == 0), "nothing past the picture");
        // Output code 0 (8-bit 4:2:2, the still's): one line a row, samples >> 2.
        // Code 1 (8-bit 4:2:0) is not modeled: logged, nothing written.
        let mut engine = recc_block().with_recc_movie(movie).unwrap();
        let mut h = TestHost::new();
        raw(&mut h);
        band(&mut engine, &mut h, 2, 1, 8, 10);
        assert_eq!(h.ram[0x1020..0x1024], [25, 25, 128, 128]);
        assert_eq!(h.ram[0x1040..0x1044], [37, 37, 128, 128]);
        w(&mut engine, &mut h, 0x6c, 1);
        let before = h.ram[0x1000..0x2000].to_vec();
        band(&mut engine, &mut h, 2, 1, 8, 20);
        assert_eq!(engine.state.recc[1], (0x3000 + 2 * 32, 0x1000, 0));
        assert_eq!(h.ram[0x1000..0x2000], before[..]);
        // A band whose rows leave the area (line 512: row 256 at 0x3000) is logged, not written.
        let mut engine = recc_block().with_recc_movie(movie).unwrap();
        let mut h = TestHost::new();
        raw(&mut h);
        w(&mut engine, &mut h, 0x6c, 3);
        band(&mut engine, &mut h, 2, 512, 8, 10);
        assert_eq!(engine.state.recc, vec![(0x3000 + 2 * 32, 0x3000, 0)]);
        assert!(h.ram[0x1000..0x2000].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_block_may_report_under_its_own_name_and_keeps_its_words() {
        let mut plain = Block::new(0xfff83000, 0x1000, vec![], None).unwrap();
        assert_eq!(plain.name(), "xpp-register-block");
        let mut named = Block::new(0xfff83000, 0x1000, vec![], None).unwrap().with_own_name();
        assert_eq!(named.name(), "xpp-register-block@0xfff83000");
        let mut h = TestHost::new();
        for block in [&mut plain, &mut named] {
            write(block, &mut h, 0xfff83004, 0x400);
            assert_eq!(read(block, &mut h, 0xfff83004), 0x400);
        }
        assert_eq!(plain.save(), named.save());
    }

    #[test]
    fn a_pulse_job_clears_its_start_bit_and_pulses_its_edge_line() {
        use fujimod_emulator_devices::test_host::advance;
        // X-T3 5.11 0xfffa8000: +0x00 bit 0 idle; +0x04 bit 0 starts a job
        // (0x1091630 returns at once, GIC 144; 0x1091150 polls bit 0 to 0).
        const ENGINE: u64 = 0xfffa8000;
        let job = Job { start: (4, 1), status: None, stop: None, irq: 144, ticks: 10 };
        let mut engine = Block::new(ENGINE, 0x1000, vec![(0, 1)], None).unwrap().with_job(job).unwrap();
        let mut h = TestHost::new();
        write(&mut engine, &mut h, ENGINE + 0x10, 0x13eef80);
        write(&mut engine, &mut h, ENGINE + 4, 1);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 4), 1);
        advance(&mut engine, &mut h, 10);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 4), 0);
        assert_eq!(read(&mut engine, &mut h, ENGINE + 0x10), 0x13eef80);
        assert_eq!(read(&mut engine, &mut h, ENGINE), 1);
        assert_eq!(engine.state.jobs, 1);
        assert_eq!(engine.state.status, 0);
        assert_eq!(engine.next_deadline(10), None);
    }

    #[test]
    fn a_development_job_and_a_resizer_job_move_the_programmed_pictures() {
        use fujimod_emulator_devices::test_host::advance;
        let mut h = TestHost::new();
        let job = Job {
            start: (4, 1),
            status: None,
            stop: Some((0, 1)),
            irq: 130,
            ticks: 10,
        };
        let block = |base| Block::new(base, 0x1000, vec![(0, 1)], None).unwrap();
        assert!(block(0xfff98000).with_work(Work::Resize).is_err());
        assert!(
            block(0xfff98000)
                .with_job(job)
                .unwrap()
                .with_work(Work::Develop { phase: (6, 0) })
                .is_err()
        );
        // A gray X-Trans frame of 12 lines x 12 samples, stride 32 bytes.
        let gray = crate::imaging::BLACK + (crate::imaging::WHITE - crate::imaging::BLACK) / 5;
        h.ram[0x1000..0x1000 + 32 * 12].copy_from_slice(&gray.to_le_bytes().repeat(16 * 12));
        let mut engine = block(0xfff98000)
            .with_job(job)
            .unwrap()
            .with_work(Work::Develop { phase: (2, 2) })
            .unwrap();
        for (offset, value) in [
            (0x08, 0x1000),
            (0x0c, 16),
            (0x14, 0xb_000b),
            (0x1c, 0x4000),
            (0x20, 16),
        ] {
            write(&mut engine, &mut h, 0xfff98000 + offset, value);
        }
        write(&mut engine, &mut h, 0xfff98004, 1);
        advance(&mut engine, &mut h, 10);
        assert_eq!(engine.state.status, 1);
        let developed = h.ram[0x4000..0x4000 + 32 * 12].to_vec();
        let pixel = &developed[..4];
        assert!(pixel[0] > 100 && pixel[0] == pixel[1] && pixel[2] == 128 && pixel[3] == 128);
        assert!(
            developed
                .chunks(32)
                .all(|line| line[..24].chunks(4).all(|p| p == pixel))
        );
        // The resizer halves it: 12 x 12 to 6 x 6 at 0x8000, stride 12 bytes.
        let mut resizer = block(0xfffc3000)
            .with_job(Job { irq: 131, ..job })
            .unwrap()
            .with_work(Work::Resize)
            .unwrap();
        for (offset, value) in [
            (0x08, 0xb_000b),
            (0x14, 0x5_0005),
            (0x18, 0x1_1000),
            (0x1c, 0x1_1000),
            (0xc0, 0x4000),
            (0xc4, 16),
            (0xc8, 0x8000),
            (0xcc, 6),
        ] {
            write(&mut resizer, &mut h, 0xfffc3000 + offset, value);
        }
        write(&mut resizer, &mut h, 0xfffc3004, 1);
        h.now = 20;
        let mut cx = h.cx();
        resizer.advance(&mut cx);
        assert!(cx.refusal().is_some_and(|f| f.contains("format")));
        drop(cx);
        write(&mut resizer, &mut h, 0xfffc3100, RESIZE_PACKED_422);
        write(&mut resizer, &mut h, 0xfffc3004, 1);
        advance(&mut resizer, &mut h, 40);
        let scaled = &h.ram[0x8000..0x8000 + 12 * 6];
        assert!(scaled.chunks(4).all(|p| p == pixel));
        // Playback's format magnifies: 6x6 to 12x12 at half-pixel steps.
        h.ram[0x8000..0x8000 + 24 * 12].fill(0);
        for (offset, value) in [
            (0x08, 0x5_0005),
            (0x14, 0xb_000b),
            (0x18, 0x800),
            (0x1c, 0x800),
            (0xc0, 0x4000),
            (0xcc, 12),
            (0x100, RESIZE_PACKED_422_PLAYBACK),
        ] {
            write(&mut resizer, &mut h, 0xfffc3000 + offset, value);
        }
        write(&mut resizer, &mut h, 0xfffc3004, 1);
        advance(&mut resizer, &mut h, 60);
        let magnified = &h.ram[0x8000..0x8000 + 24 * 12];
        assert!(magnified.chunks(4).all(|p| p == pixel));
    }

    #[test]
    fn commands_set_their_status_field() {
        let dram = |commands| {
            Block::new(0xfff0f000, 0x1000, vec![(0x14c, 8)], None)?.with_commands(commands)
        };
        let enter = Command {
            word: 0xb8,
            value: 0x6a02_0202,
            status: 0xbc,
            mask: 0x3f,
            state: 0x27,
        };
        let leave = Command {
            value: 0x0102_0202,
            state: 0x20,
            ..enter
        };
        assert!(
            dram(vec![Command {
                state: 0x40,
                ..enter
            }])
            .is_err()
        );
        assert!(
            dram(vec![Command {
                status: 0x14c,
                ..enter
            }])
            .is_err()
        );
        let mut block = dram(vec![enter, leave]).unwrap();
        let base = 0xfff0f000;
        let mut h = TestHost::new();
        write(&mut block, &mut h, base + 0xbc, 0x100);
        write(&mut block, &mut h, base + 0xb8, 0x6a02_0202);
        assert_eq!(read(&mut block, &mut h, base + 0xbc), 0x127);
        assert_eq!(read(&mut block, &mut h, base + 0xb8), 0x6a02_0202);
        write(&mut block, &mut h, base + 0xb8, 0x6a02_0203);
        assert_eq!(read(&mut block, &mut h, base + 0xbc), 0x127);
        write(&mut block, &mut h, base + 0xb8, 0x0102_0202);
        let saved = block.save();
        assert_eq!(read(&mut block, &mut h, base + 0xbc), 0x120);
        let mut other = dram(vec![enter, leave]).unwrap();
        other.restore(&saved).unwrap();
        assert_eq!(read(&mut other, &mut h, base + 0xbc), 0x120);
    }

    #[test]
    fn fixed_words_read_their_value_whatever_is_written() {
        let block = || Block::new(0xfffc3000, 0x1000, vec![(0, 1)], None).unwrap();
        assert!(block().with_fixed(vec![(0, 1)]).is_err());
        let mut engine = block().with_fixed(vec![(0xdc, 0x0100_0100)]).unwrap();
        let mut h = TestHost::new();
        write(&mut engine, &mut h, 0xfffc30dc, 1);
        assert_eq!(read(&mut engine, &mut h, 0xfffc30dc), 0x0100_0100);
        write(&mut engine, &mut h, 0xfffc30d8, 4);
        assert_eq!(read(&mut engine, &mut h, 0xfffc30d8), 4);
    }

    #[test]
    fn a_status_job_may_clear_its_start_bit_when_it_ends() {
        use fujimod_emulator_devices::test_host::advance;
        // X-T3 5.11 IIFP 0xfff6f000: start +0x00 bit 0 beside idle bit 31,
        // done +0x84 bit 0, GIC 134.
        const IIFP: u64 = 0xfff6f000;
        let job = Job {
            start: (0, 1),
            status: Some((0x84, 1)),
            stop: None,
            irq: 134,
            ticks: 10,
        };
        let block = || Block::new(IIFP, 0x1000, vec![(0, 0x8000_0000)], None).unwrap();
        assert!(block().with_start_clearing().is_err());
        let pulse = Job { status: None, ..job };
        assert!(block().with_job(pulse).unwrap().with_start_clearing().is_err());
        let mut iifp = block().with_job(job).unwrap().with_start_clearing().unwrap();
        let mut h = TestHost::new();
        write(&mut iifp, &mut h, IIFP, 0x10 | 1);
        assert_eq!(read(&mut iifp, &mut h, IIFP), 0x8000_0011);
        advance(&mut iifp, &mut h, 10);
        assert_eq!(iifp.state.status, 1);
        assert_eq!(read(&mut iifp, &mut h, IIFP), 0x8000_0010);
        // 0x1dbb6c8's read-modify-write does not start a second job.
        let word = read(&mut iifp, &mut h, IIFP) as u32;
        write(&mut iifp, &mut h, IIFP, word & !0x8000_0000 | 0x20);
        assert_eq!(iifp.next_deadline(10), None);
        write(&mut iifp, &mut h, IIFP + 0x84, 1);
        assert_eq!(iifp.state.status, 0);
        assert_eq!(iifp.state.jobs, 1);
        // Without the option the start bit stays set, as before.
        let mut held = block().with_job(job).unwrap();
        write(&mut held, &mut h, IIFP, 1);
        advance(&mut held, &mut h, 20);
        assert_eq!(read(&mut held, &mut h, IIFP), 0x8000_0001);
    }

    #[test]
    fn audio_converter_left_justifies_24_bit_playback_samples_into_32_bit_slots() {
        use fujimod_emulator_devices::test_host::advance;
        const A: u64 = 0xfffa8000;
        let job = Job { start: (4, 1), status: None, stop: None, irq: 144, ticks: 10 };
        let block = || Block::new(A, 0x1000, vec![(0, 1)], None).unwrap().with_job(job).unwrap();
        assert!(Block::new(A, 0x1000, vec![(0, 1)], None).unwrap().with_work(Work::Audio).is_err());
        let mut converter = block().with_work(Work::Audio).unwrap();
        let mut plain = block();
        let mut h = TestHost::new();
        // Two lines of eight stereo frames, 24-bit packed: frame n has left
        // n + 1 and right 0xa00000 + n (negative samples).
        for n in 0..16u32 {
            let at = 0x2000 + 6 * n as usize;
            h.ram[at..at + 3].copy_from_slice(&(n + 1).to_le_bytes()[..3]);
            h.ram[at + 3..at + 6].copy_from_slice(&(0xa0_0000 + n).to_le_bytes()[..3]);
        }
        let w = |e: &mut Block, h: &mut TestHost, o: u64, v: u32| write(e, h, A + o, v);
        // 0x1091150: loading the firmware table converts nothing.
        for (o, v) in [(0x0c, 0), (0x10, 0x013e_ef80), (0x08, 0), (0x04, 1)] {
            w(&mut converter, &mut h, o, v);
        }
        advance(&mut converter, &mut h, 10);
        assert!(h.ram[0x4000..0x4080].iter().all(|&b| b == 0));
        // 0x1091220 then 0x1091630, as run LV111i traced the first job (two
        // lines instead of 90).
        let start = |e: &mut Block, h: &mut TestHost, lines: u32| {
            for (o, v) in [
                (0x08, 0x0300_0201),
                (0x14, 0),
                (0x18, 0x2000),
                (0x1c, 0x30),
                (0x20, lines),
                (0x24, 0),
                (0x28, 0x4000),
                (0x2c, 0x40),
                (0x30, 2),
                (0x34, 0x0030_0030),
                (0x38, 0x0040_0040),
                (0x3c, 1),
                (0x40, 5),
                (0x44, 0x25),
                (0x04, 1),
            ] {
                w(e, h, o, v);
            }
        };
        start(&mut plain, &mut h, 2);
        advance(&mut plain, &mut h, 30);
        assert!(h.ram[0x4000..0x4080].iter().all(|&b| b == 0));
        start(&mut converter, &mut h, 2);
        advance(&mut converter, &mut h, 50);
        let slot = |h: &TestHost, at: usize| u32::from_le_bytes(h.ram[at..at + 4].try_into().unwrap());
        for n in 0..16u32 {
            let at = 0x4000 + 8 * n as usize;
            assert_eq!(slot(&h, at), (n + 1) << 8);
            assert_eq!(slot(&h, at + 4), (0xa0_0000 + n) << 8);
        }
        assert_eq!(slot(&h, 0x4080), 0);
        assert_eq!((converter.state.audio_converted, converter.state.audio_skipped), (1, 0));
        // Lines that do not match: left alone and counted.
        h.ram[0x4000..0x4080].fill(0);
        start(&mut converter, &mut h, 3);
        advance(&mut converter, &mut h, 70);
        assert!(h.ram[0x4000..0x4080].iter().all(|&b| b == 0));
        assert_eq!((converter.state.audio_converted, converter.state.audio_skipped), (1, 1));
        let saved = converter.save();
        converter.restore(&saved).unwrap();
        assert_eq!(converter.state.audio_skipped, 1);
        // The other widths, and recording's 32 to 24 bits.
        assert_eq!(convert_audio(&[0x22, 0x33, 0x44], 3, 4), [0, 0x22, 0x33, 0x44]);
        assert_eq!(convert_audio(&[0x33, 0x44], 2, 4), [0, 0, 0x33, 0x44]);
        assert_eq!(convert_audio(&[0x11, 0x22, 0x33, 0x44], 4, 3), [0x22, 0x33, 0x44]);
        assert_eq!(convert_audio(&[0x11, 0x22, 0x33, 0x44], 4, 2), [0x33, 0x44]);
        assert_eq!(convert_audio(&[0x22, 0x33, 0x44, 0x55], 2, 3), [0, 0x22, 0x33, 0, 0x44, 0x55]);
    }
}
