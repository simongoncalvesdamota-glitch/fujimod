//! A Mali-V61 style video processing unit, reduced to what the X-T3 5.11
//! movie encoder driver needs (opt-in, absent by default).
//!
//! Driver: unit 5, 0x1097000..0x109e000; register base read from 0x180aff8
//! (0xffe00000), interrupt GIC 138 (handler 0x1098e6c, level).
//!
//! Registers (offsets in the page):
//! - +0x10 CORELSID reads 0xffff (no session on any core) and +0x14
//!   JOBQUEUE reads 0x0f0f0f0f (empty) out of reset; writing +0x50 (reset)
//!   restores both. The reset wait 0x109be80 and the queue code 0x109bd80 /
//!   0x109be28 only need that, the queue word otherwise keeps its last write.
//! - +0x18 bit 0: a message for the host is pending. The handler reads it and
//!   writes 0 to the session's +0x1c (0xffe0021c); that clears the bit and
//!   lowers the line.
//! - Session window 0x200 + 0x40 * lsid: +0x04 MMU_CTRL (table address >> 10,
//!   low bits 3), +0x18 TERMINATE always reads 0 (the driver waits for it),
//!   +0x20 IRQHOST: writing bit 0 rings the doorbell.
//! - Every other word reads back its last write.
//!
//! A doorbell is served `ticks` later. The model walks the session's two-level
//! table (index va >> 22, then (va >> 12) & 0x3ff; entry & 0x3ffffffc << 10)
//! to find the seven 4 KiB queue pages at the VPU address `queues`: host
//! pages 0 (messages), 2 (input buffers), 4 (output buffers) and VPU pages 1,
//! 3, 5 (their answers). Each page: u16 read and write positions at +0 / +2,
//! then a ring of 0x3fc words at +0x10; a message is code | bytes << 16 then
//! its payload words. A host page's +2 is where the driver writes, the
//! matching VPU page's +2 where the model has read; the VPU page's +0 is
//! where the model writes, the host page's +0 where the driver has read.
//!
//! Answers: JOB (0x3f1) gets 2001 SWITCHED_IN, each SET_OPTION (0x3f2) gets
//! 2003. Output buffers (3002, 0x28 bytes) that come before any frame are the
//! stream headers. With `h264`, each frame (3001, 0x48 bytes) whose handle
//! matches an output buffer is encoded as one IDR picture (a flat grey
//! picture, every macroblock I_16x16 DC without residual): at the first
//! frame the header buffers come back first (SPS then PPS, flags 0x80, tag =
//! the frame's handle), then the frame's buffer (flags 0x11) with 2006
//! OUTPUT, then the frame itself with 2005 INPUT. Each NAL starts with
//! 00 00 00 01 and fills one buffer at offset 0. Every answer that does not
//! fit its ring waits for the next pass, in order. With `go_state_change`,
//! GO (1001) also gets 2011 STATE_CHANGE when encoding: the 5.11 driver sends
//! GO after the options and its parameter buffers (3003 on page 2), and only
//! then its frames. With `stop_state_change`, STOP (1002) gets 2011
//! STATE_CHANGE (stopped), encoding or decoding: at the end of a recording the
//! 5.11 driver sends STOP and calls back the end of compression only on it.
//!
//! With `decode` (playback, the 5.11 driver's decode branch, 0x109c2ac and
//! its handlers): bitstream buffers (3002, 0x28 bytes) come on page 2 and go
//! back on page 3 with 2005, frame buffers (3001, 0x48 bytes) come on page 4
//! and go back on page 5 with 2006. GO (1001) gets 2011 STATE_CHANGE. Once a
//! bitstream and a frame are held, 2015 SEQ_PARAMETERS (the driver does not
//! read its payload; it answers 1004 OUTPUT_FLUSH) and the bitstreams come
//! back (2005). Each bitstream whose bytes hold an H.264 slice (NAL type 1 or
//! 5, after a start code) owes a picture; the one with flags bit 0 (end of
//! stream) owes one only if the session owed none yet. 1004 returns the held
//! frames with flags 0, then 2008 OUT_FLUSHED. After that flush, an owed
//! picture goes to the next held frame `picture_ticks` later: the frame is
//! filled with 0x80 (luma rows x stride at +0x1c, half as many chroma rows at
//! +0x20 unless the format is the one-plane 0x121) and returned with flags 4
//! and 2006 (the driver only takes a frame whose +0x10 is not 0, once its
//! state reached 3). A JOB starts the session over. No H.264 is decoded.
use fujimod_emulator_devices::{Cx, Device};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub base: u64,
    pub size: u64,
    pub irq: u32,
    /// Delay from a doorbell to the pass that answers it.
    pub ticks: u64,
    /// VPU address of queue page 0; pages 1..6 follow every 4 KiB.
    pub queues: u64,
    /// Encode each frame as a flat grey H.264 IDR picture.
    #[serde(default)]
    pub h264: Option<H264>,
    /// Answer the decode (playback) sequence with flat grey pictures.
    #[serde(default, skip_serializing_if = "is_false")]
    pub decode: bool,
    /// Decode: delay from the frame given back after the flush to its picture
    /// (0 = `PICTURE_TICKS`). The driver drops a picture that comes before it
    /// is ready for it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub picture_ticks: u64,
    /// Encode: answer GO (1001) with 2011 STATE_CHANGE.
    #[serde(default, skip_serializing_if = "is_false")]
    pub go_state_change: bool,
    /// Encode and decode: answer STOP (1002) with 2011 STATE_CHANGE (stopped).
    /// The 5.11 driver (state 6, 0x109ad44) waits for it before calling back
    /// the end of compression (0x1f01fd0).
    #[serde(default, skip_serializing_if = "is_false")]
    pub stop_state_change: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct H264 {
    pub width: u32,
    pub height: u32,
    /// level_idc of the SPS (51 = level 5.1).
    pub level: u8,
}

/// Message codes.
const JOB: u32 = 0x3f1;
const SET_OPTION: u32 = 0x3f2;
const FRAME: u32 = 3001;
const STREAM: u32 = 3002;
const SWITCHED_IN: u32 = 2001;
const OPTION_DONE: u32 = 2003;
const INPUT: u32 = 2005;
const OUTPUT: u32 = 2006;
/// Decode only.
const GO: u32 = 1001;
const STOP: u32 = 1002;
const OUTPUT_FLUSH: u32 = 1004;
const OUT_FLUSHED: u32 = 2008;
const STATE_CHANGE: u32 = 2011;
const SEQ_PARAMETERS: u32 = 2015;
/// STATE_CHANGE payload (not read by the 5.11 driver).
const RUNNING: u32 = 2;
const STOPPED: u32 = 0;
/// Frame flags of a returned picture; the encoder path sends frames with 4.
const PICTURE: u32 = 4;
/// Frame format of the one-plane frames (chroma address 0).
const ONE_PLANE: u32 = 0x121;
/// Largest grey fill of one frame, in bytes.
const FILL_MAX: u64 = 64 << 20;
/// Default `picture_ticks`: 10 ms at 54 MHz. The 5.11 player drops a decoded
/// picture that comes after its target vsync, so the delay must stay well
/// under one frame of the film (1 801 800 ticks at 29.97 fps, 900 900 at
/// 59.94 fps). The former 2 000 000 (27 fps) left the counter at "0s".
const PICTURE_TICKS: u64 = 540_000;
/// Bitstream flag of the last buffer of the stream (bit 0,
/// MVE_BUFFER_BITSTREAM_FLAG_EOS).
const END_OF_STREAM: u32 = 1;
/// Most bytes of one bitstream buffer read to look for a slice.
const SCAN_MAX: usize = 1 << 20;
/// Bitstreams described in the state.
const PROBES: usize = 8;

/// Ring length in words, and the page offset of the ring.
const RING: u32 = 0x3fc;
const DATA: u64 = 0x10;
/// Session windows.
const SESSIONS: u64 = 0x200;
const SESSION: u64 = 0x40;
const MMU_CTRL: u64 = 0x04;
const TERMINATE: u64 = 0x18;
const IRQ_ACK: u64 = 0x1c;
const DOORBELL: u64 = 0x20;
const CORELSID: u64 = 0x10;
const JOBQUEUE: u64 = 0x14;
const IRQVE: u64 = 0x18;
const RESET: u64 = 0x50;
/// Number of logged messages a report keeps.
const HISTORY: usize = 256;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    words: BTreeMap<u64, u32>,
    /// +0x18 bit 0.
    pending: bool,
    deadline: Option<u64>,
    /// Sessions whose doorbell rang (bit per lsid).
    rung: u32,
    /// Output buffers waiting for a frame (payload words).
    streams: Vec<Vec<u32>>,
    /// Header buffers, received before any frame.
    headers: Vec<Vec<u32>>,
    /// Frames waiting for their output buffer.
    frames: Vec<Vec<u32>>,
    frames_seen: u64,
    headers_sent: bool,
    encoded: u64,
    /// Answers that did not fit: (VPU page, code, payload), for this lsid.
    outbox: VecDeque<(u8, u32, Vec<u32>)>,
    outbox_lsid: u32,
    received: u64,
    sent: u64,
    /// (ticks, page, header) of the first messages each way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<(u64, u8, u32)>,
    /// Decode: bitstream buffers not returned yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bitstreams: Vec<Vec<u32>>,
    /// Decode: frame buffers waiting for a picture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    outputs: Vec<Vec<u32>>,
    #[serde(default, skip_serializing_if = "is_false")]
    sequence_sent: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    flushed: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    decoded: u64,
    /// Decode: pictures owed (bitstreams holding a slice, already returned).
    #[serde(default, skip_serializing_if = "is_zero")]
    owed: u64,
    /// Decode: pictures owed since the JOB (an end of stream owes one only
    /// while this is 0).
    #[serde(default, skip_serializing_if = "is_zero")]
    session_owed: u64,
    /// Decode: bitstreams read that hold a slice, and those that do not.
    #[serde(default, skip_serializing_if = "is_zero")]
    slices: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    no_slice: u64,
    /// Decode: what the first bitstreams held (offset, filled, address and
    /// the first bytes, or why they could not be read).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    probes: Vec<String>,
    /// Decode: when the next picture goes out, and for which lsid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    picture_at: Option<(u64, u32)>,
    /// With `decode`: what the current session does (0 not known yet, ENCODING,
    /// DECODING), set by its first buffer; a JOB starts over.
    #[serde(default, skip_serializing_if = "is_zero")]
    mode: u64,
}

/// Session modes (`State::mode`).
const ENCODING: u64 = 1;
const DECODING: u64 = 2;

fn is_false(v: &bool) -> bool {
    !*v
}
fn is_zero(v: &u64) -> bool {
    *v == 0
}

pub struct MaliV61 {
    options: Options,
    state: State,
    name: &'static str,
    /// SPS, PPS and the two IDR pictures (idr_pic_id 0 and 1), start codes included.
    nals: Option<[Vec<u8>; 4]>,
}

impl MaliV61 {
    pub fn new(options: Options) -> Result<Self, String> {
        let o = &options;
        if o.size < SESSIONS + 4 * SESSION
            || o.base & 0xfff != 0
            || o.size & 3 != 0
            || o.queues & 0xfff != 0
            || !(32..512).contains(&o.irq)
            || o.ticks == 0
            || o.h264.as_ref().is_some_and(|h| {
                h.width == 0 || h.height == 0 || h.width > 8192 || h.height > 8192 || h.level == 0
            })
        {
            return Err("Invalid Mali-V61 options".into());
        }
        let nals = o.h264.as_ref().map(|h| {
            [sps(h.width, h.height, h.level), pps(), idr(h.width, h.height, 0), idr(h.width, h.height, 1)]
        });
        let name = Box::leak(format!("mali-v61@{:#x}", options.base).into_boxed_str());
        Ok(Self { options, state: State::default(), name, nals })
    }
    pub fn windows(&self) -> Vec<(u64, u64)> {
        vec![(self.options.base, self.options.base + self.options.size)]
    }
    fn word(&self, offset: u64) -> u32 {
        match self.state.words.get(&offset) {
            Some(&v) => v,
            None if offset == CORELSID => 0xffff,
            None if offset == JOBQUEUE => 0x0f0f_0f0f,
            None => 0,
        }
    }
    fn session(offset: u64) -> Option<(u32, u64)> {
        let o = offset.checked_sub(SESSIONS)?;
        (o < 4 * SESSION).then(|| ((o / SESSION) as u32, o % SESSION))
    }
    /// Physical address of a VPU address through the session's table.
    fn translate(&self, cx: &mut Cx<'_>, lsid: u32, va: u64) -> Result<u64, String> {
        let ctrl = self.word(SESSIONS + SESSION * u64::from(lsid) + MMU_CTRL);
        let entry = |cx: &mut Cx<'_>, table: u64, index: u64| -> Result<u64, String> {
            let e = read32(cx, table + index * 4)?;
            if e & 0x3fff_fffc == 0 {
                return Err(format!("VA {va:#x}: no entry at {:#x}", table + index * 4));
            }
            Ok(u64::from(e & 0x3fff_fffc) << 10)
        };
        if ctrl & 0x3fff_fffc == 0 {
            return Err(format!("VA {va:#x}: session {lsid} has no table"));
        }
        let l0 = u64::from(ctrl & 0x3fff_fffc) << 10;
        let l1 = entry(cx, l0, (va >> 22) & 0x3ff)?;
        let page = entry(cx, l1, (va >> 12) & 0x3ff)?;
        Ok(page | (va & 0xfff))
    }
    fn page(&self, cx: &mut Cx<'_>, lsid: u32, index: u64) -> Result<u64, String> {
        self.translate(cx, lsid, self.options.queues + index * 0x1000)
    }
    /// Takes the next message of a host page; returns [header, payload...].
    fn take(&mut self, cx: &mut Cx<'_>, lsid: u32, host: u8) -> Result<Option<Vec<u32>>, String> {
        let (h, v) = (self.page(cx, lsid, host.into())?, self.page(cx, lsid, u64::from(host) + 1)?);
        let read = u32::from(read16(cx, v + 2)?);
        let write = u32::from(read16(cx, h + 2)?);
        if read >= RING || write >= RING {
            return Err(format!("page {host}: positions {read:#x} / {write:#x}"));
        }
        if read == write {
            return Ok(None);
        }
        let header = read32(cx, h + DATA + 4 * u64::from(read))?;
        let n = ((header >> 16) + 3) / 4;
        let mut message = vec![header];
        let mut pos = (read + 1) % RING;
        for _ in 0..n {
            message.push(read32(cx, h + DATA + 4 * u64::from(pos))?);
            pos = (pos + 1) % RING;
        }
        write16(cx, v + 2, pos as u16)?;
        self.state.received += 1;
        self.log(cx.now(), host, header);
        Ok(Some(message))
    }
    /// Writes a message to a VPU page; false if it does not fit.
    fn put(&mut self, cx: &mut Cx<'_>, lsid: u32, vpu: u8, code: u32, payload: &[u32]) -> Result<bool, String> {
        let (h, v) = (self.page(cx, lsid, u64::from(vpu) - 1)?, self.page(cx, lsid, vpu.into())?);
        let write = u32::from(read16(cx, v)?);
        let read = u32::from(read16(cx, h)?);
        if read >= RING || write >= RING {
            return Err(format!("page {vpu}: positions {read:#x} / {write:#x}"));
        }
        let free = (read + RING - write - 1) % RING;
        if payload.len() as u32 + 1 > free {
            return Ok(false);
        }
        let header = code | (payload.len() as u32 * 4) << 16;
        let mut pos = write;
        for w in std::iter::once(&header).chain(payload) {
            write32(cx, v + DATA + 4 * u64::from(pos), *w)?;
            pos = (pos + 1) % RING;
        }
        write16(cx, v, pos as u16)?;
        self.state.sent += 1;
        self.log(cx.now(), vpu, header);
        Ok(true)
    }
    /// Sends in order: behind anything already waiting.
    fn send(&mut self, cx: &mut Cx<'_>, lsid: u32, vpu: u8, code: u32, payload: Vec<u32>) -> Result<bool, String> {
        if self.state.outbox.is_empty() && self.put(cx, lsid, vpu, code, &payload)? {
            return Ok(true);
        }
        self.state.outbox_lsid = lsid;
        self.state.outbox.push_back((vpu, code, payload));
        Ok(false)
    }
    fn flush(&mut self, cx: &mut Cx<'_>) -> Result<bool, String> {
        let lsid = self.state.outbox_lsid;
        let mut any = false;
        while let Some((vpu, code, payload)) = self.state.outbox.front().cloned() {
            if !self.put(cx, lsid, vpu, code, &payload)? {
                break;
            }
            self.state.outbox.pop_front();
            any = true;
        }
        Ok(any)
    }
    fn log(&mut self, now: u64, page: u8, header: u32) {
        if self.state.history.len() < HISTORY {
            self.state.history.push((now, page, header));
        }
    }
    /// Writes bytes at a VPU address, page by page.
    fn write_va(&self, cx: &mut Cx<'_>, lsid: u32, va: u64, bytes: &[u8]) -> Result<(), String> {
        let mut done = 0;
        while done < bytes.len() {
            let at = va + done as u64;
            let n = ((0x1000 - (at & 0xfff)) as usize).min(bytes.len() - done);
            let pa = self.translate(cx, lsid, at)?;
            cx.ram_write(pa, &bytes[done..done + n])?;
            done += n;
        }
        Ok(())
    }
    /// Reads bytes at a VPU address, page by page.
    fn read_va(&self, cx: &mut Cx<'_>, lsid: u32, va: u64, size: usize) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(size);
        while out.len() < size {
            let at = va + out.len() as u64;
            let n = ((0x1000 - (at & 0xfff)) as usize).min(size - out.len());
            let pa = self.translate(cx, lsid, at)?;
            out.extend(cx.ram_bytes(pa, n)?);
        }
        Ok(out)
    }
    /// Fills an output buffer with one NAL and returns it with 2006 OUTPUT.
    fn output(&mut self, cx: &mut Cx<'_>, lsid: u32, mut buffer: Vec<u32>, nal: usize, tag: (u32, u32), flags: u32) -> Result<bool, String> {
        let Some(nals) = self.nals.as_ref() else { return Ok(false) };
        let bytes = nals[nal].clone();
        let alloc = buffer[6] as usize;
        if bytes.len() > alloc {
            return Err(format!("NAL of {} bytes in a buffer of {alloc}", bytes.len()));
        }
        // Payload words (after the header word): handle 1..2, tag 3..4, flags 5,
        // allocated 6, offset 7, filled 8, VPU address 9.
        self.write_va(cx, lsid, buffer[9].into(), &bytes)?;
        (buffer[3], buffer[4]) = tag;
        buffer[5] = flags;
        buffer[7] = 0;
        buffer[8] = bytes.len() as u32;
        let payload = buffer[1..].to_vec();
        self.send(cx, lsid, 1, OUTPUT, Vec::new())?;
        self.send(cx, lsid, 5, STREAM, payload)
    }
    /// Decode: fills a frame with 0x80 and returns it with 2006 OUTPUT.
    /// Payload words: handle 1..2, flags 5, rows 6 (low half), format 7, luma
    /// 8, chroma 9, luma stride 14, chroma stride 15. The 5.11 driver
    /// (0x109a248, 0x109a314) sends either format 0x201 (two planes: chroma
    /// at luma + rows x stride, same stride, half the rows) or format 0x121
    /// (one plane, stride 4 bytes per pixel, chroma 0): each byte of 0x80 is
    /// a neutral grey either way. A one-plane frame is filled over rows x
    /// stride, the extent the model always used.
    fn picture(&mut self, cx: &mut Cx<'_>, lsid: u32, mut frame: Vec<u32>) -> Result<(), String> {
        let rows = u64::from(frame[6] & 0xffff);
        let chroma = if frame[7] & 0xffff == ONE_PLANE { 0 } else { frame[9] };
        let planes = [(frame[8], frame[14], rows), (chroma, frame[15], rows / 2)];
        for (va, stride, n) in planes {
            let bytes = u64::from(stride) * n;
            if va == 0 || bytes == 0 {
                continue;
            }
            if bytes > FILL_MAX {
                cx.log(&format!("mali-v61: frame plane of {bytes} bytes not filled"));
                continue;
            }
            if let Err(e) = self.write_va(cx, lsid, va.into(), &vec![0x80; bytes as usize]) {
                cx.log(&format!("mali-v61: frame plane not filled: {e}"));
            }
        }
        frame[5] = PICTURE;
        self.state.decoded += 1;
        self.send(cx, lsid, 1, OUTPUT, Vec::new())?;
        self.send(cx, lsid, 5, FRAME, frame[1..].to_vec())?;
        Ok(())
    }
    /// With `decode`, a JOB: the driver starts a session over, whose mode is
    /// not known yet.
    fn new_session(&mut self) {
        let s = &mut self.state;
        (s.bitstreams, s.outputs) = (Vec::new(), Vec::new());
        (s.sequence_sent, s.flushed, s.owed, s.picture_at, s.mode) = (false, false, 0, None, 0);
        s.session_owed = 0;
    }
    /// The mode the first waiting buffer shows (0 while none waits): encoding
    /// sends frames (3001) on page 2 and output buffers (3002) on page 4,
    /// decoding sends bitstreams (3002) on page 2 and frames (3001) on page 4.
    /// Other messages (3003 parameters) tell nothing.
    fn sniff(&self, cx: &mut Cx<'_>, lsid: u32) -> Result<u64, String> {
        for (host, encoding, decoding) in [(2u8, FRAME, STREAM), (4, STREAM, FRAME)] {
            let (h, v) = (self.page(cx, lsid, host.into())?, self.page(cx, lsid, u64::from(host) + 1)?);
            let (mut read, write) = (u32::from(read16(cx, v + 2)?), u32::from(read16(cx, h + 2)?));
            if read >= RING || write >= RING {
                return Err(format!("page {host}: positions {read:#x} / {write:#x}"));
            }
            while read != write {
                let header = read32(cx, h + DATA + 4 * u64::from(read))?;
                match header & 0xffff {
                    c if c == encoding => return Ok(ENCODING),
                    c if c == decoding => return Ok(DECODING),
                    _ => read = (read + 1 + ((header >> 16) + 3) / 4) % RING,
                }
            }
        }
        Ok(0)
    }
    /// Decode: one pass for a session whose doorbell rang.
    fn serve_decode(&mut self, cx: &mut Cx<'_>, lsid: u32) -> Result<(), String> {
        while let Some(m) = self.take(cx, lsid, 0)? {
            match m[0] & 0xffff {
                JOB => {
                    self.new_session();
                    self.send(cx, lsid, 1, SWITCHED_IN, Vec::new())?;
                }
                SET_OPTION => {
                    self.send(cx, lsid, 1, OPTION_DONE, Vec::new())?;
                }
                GO => {
                    self.send(cx, lsid, 1, STATE_CHANGE, vec![RUNNING])?;
                }
                STOP if self.options.stop_state_change => {
                    self.send(cx, lsid, 1, STATE_CHANGE, vec![STOPPED])?;
                }
                OUTPUT_FLUSH => {
                    for mut f in std::mem::take(&mut self.state.outputs) {
                        f[5] = 0;
                        self.send(cx, lsid, 1, OUTPUT, Vec::new())?;
                        self.send(cx, lsid, 5, FRAME, f[1..].to_vec())?;
                    }
                    self.send(cx, lsid, 1, OUT_FLUSHED, Vec::new())?;
                    self.state.flushed = true;
                }
                _ => {}
            }
        }
        if self.state.mode == 0 {
            self.state.mode = self.sniff(cx, lsid)?;
        }
        if self.state.mode == ENCODING {
            return Ok(());
        }
        while let Some(m) = self.take(cx, lsid, 4)? {
            if m[0] & 0xffff == FRAME && m.len() == 1 + 0x48 / 4 {
                self.state.outputs.push(m);
            }
        }
        while let Some(m) = self.take(cx, lsid, 2)? {
            if m[0] & 0xffff == STREAM && m.len() == 1 + 0x28 / 4 {
                self.state.bitstreams.push(m);
            }
        }
        if !self.state.sequence_sent && !self.state.bitstreams.is_empty() && !self.state.outputs.is_empty() {
            self.state.sequence_sent = true;
            // Informative only: the two u16 at +0x14, luma stride, frames held.
            let f = &self.state.outputs[0];
            let payload = vec![f[6] & 0xffff, f[6] >> 16, f[14], self.state.outputs.len() as u32];
            self.send(cx, lsid, 1, SEQ_PARAMETERS, payload)?;
        }
        // Bitstreams are read as soon as the sequence is known: each one that
        // holds a slice owes a picture until a frame comes back after the
        // flush. The end of the stream owes one only if nothing was owed yet
        // (a buffer the model could not read).
        while self.state.sequence_sent && !self.state.bitstreams.is_empty() {
            let b = self.state.bitstreams.remove(0);
            let (slice, probe) = self.has_slice(cx, lsid, &b);
            let s = &mut self.state;
            if s.probes.len() < PROBES {
                s.probes.push(probe);
            }
            if slice {
                s.slices += 1;
            } else {
                s.no_slice += 1;
            }
            if slice || (b[5] & END_OF_STREAM != 0 && s.session_owed == 0) {
                s.owed += 1;
                s.session_owed += 1;
            }
            self.send(cx, lsid, 1, INPUT, Vec::new())?;
            self.send(cx, lsid, 3, STREAM, b[1..].to_vec())?;
        }
        self.plan_picture(cx, lsid);
        Ok(())
    }
    /// Decode: whether a bitstream buffer holds an H.264 slice (NAL type 1 or
    /// 5). Payload words: flags 5, offset 7, filled 8, VPU address 9. The
    /// driver sends Annex B (it puts a start code over each length and puts
    /// the length back on 2005); 4-byte lengths are read too.
    fn has_slice(&self, cx: &mut Cx<'_>, lsid: u32, b: &[u32]) -> (bool, String) {
        let n = (b[8] as usize).min(SCAN_MAX);
        let head = format!("flags {:#x} offset {:#x} filled {:#x} at {:#x}", b[5], b[7], b[8], b[9]);
        if n == 0 || b[9] == 0 {
            return (false, head);
        }
        match self.read_va(cx, lsid, u64::from(b[9]) + u64::from(b[7]), n) {
            Ok(bytes) => {
                let types = nal_types(&bytes, n == b[8] as usize);
                let first: String = bytes.iter().take(8).map(|x| format!("{x:02x}")).collect();
                (types.iter().any(|&t| t == 1 || t == 5), format!("{head}: {first} types {types:?}"))
            }
            Err(e) => {
                cx.log(&format!("mali-v61: bitstream not read: {e}"));
                (false, format!("{head}: {e}"))
            }
        }
    }
    /// Decode: schedules the next owed picture once a frame is held after the flush.
    fn plan_picture(&mut self, cx: &mut Cx<'_>, lsid: u32) {
        let s = &self.state;
        if !s.flushed || s.owed == 0 || s.outputs.is_empty() || s.picture_at.is_some() {
            return;
        }
        let delay = if self.options.picture_ticks == 0 { PICTURE_TICKS } else { self.options.picture_ticks };
        let at = cx.now() + delay;
        self.state.picture_at = Some((at, lsid));
        cx.schedule(at, 0);
    }
    /// Decode: sends the owed picture whose time has come.
    fn due_picture(&mut self, cx: &mut Cx<'_>) -> Result<(), String> {
        let Some((at, lsid)) = self.state.picture_at else { return Ok(()) };
        if at > cx.now() {
            return Ok(());
        }
        self.state.picture_at = None;
        if self.state.owed > 0 && !self.state.outputs.is_empty() {
            let f = self.state.outputs.remove(0);
            self.state.owed -= 1;
            self.picture(cx, lsid, f)?;
        }
        self.plan_picture(cx, lsid);
        Ok(())
    }
    /// One pass for a session whose doorbell rang.
    fn serve(&mut self, cx: &mut Cx<'_>, lsid: u32) -> Result<(), String> {
        // With `decode`, each session is served by its mode: decoding until its
        // first buffer shows it encodes.
        if self.options.decode && self.state.mode != ENCODING {
            self.serve_decode(cx, lsid)?;
            if self.state.mode != ENCODING {
                return Ok(());
            }
        }
        while let Some(m) = self.take(cx, lsid, 0)? {
            match m[0] & 0xffff {
                JOB if self.options.decode => {
                    // A new session, maybe a playback: decide again.
                    self.new_session();
                    self.send(cx, lsid, 1, SWITCHED_IN, Vec::new())?;
                    return self.serve(cx, lsid);
                }
                JOB => {
                    self.send(cx, lsid, 1, SWITCHED_IN, Vec::new())?;
                }
                SET_OPTION => {
                    self.send(cx, lsid, 1, OPTION_DONE, Vec::new())?;
                }
                GO if self.options.go_state_change => {
                    self.send(cx, lsid, 1, STATE_CHANGE, vec![RUNNING])?;
                }
                STOP if self.options.stop_state_change => {
                    self.send(cx, lsid, 1, STATE_CHANGE, vec![STOPPED])?;
                }
                _ => {}
            }
        }
        let had_frames = self.state.frames_seen > 0;
        while let Some(m) = self.take(cx, lsid, 2)? {
            if m[0] & 0xffff == FRAME && m.len() == 1 + 0x48 / 4 {
                self.state.frames_seen += 1;
                self.state.frames.push(m);
            }
        }
        while let Some(m) = self.take(cx, lsid, 4)? {
            if m[0] & 0xffff != STREAM || m.len() != 1 + 0x28 / 4 {
                continue;
            }
            let matches = self.state.frames.iter().any(|f| f[1] == m[1] && f[2] == m[2]);
            if !had_frames && !matches {
                self.state.headers.push(m);
            } else {
                self.state.streams.push(m);
            }
        }
        if self.nals.is_none() {
            return Ok(());
        }
        let mut i = 0;
        while i < self.state.frames.len() {
            let frame = self.state.frames[i].clone();
            let Some(j) = self.state.streams.iter().position(|s| s[1] == frame[1] && s[2] == frame[2]) else {
                i += 1;
                continue;
            };
            let stream = self.state.streams.remove(j);
            self.state.frames.remove(i);
            let tag = (frame[1], frame[2]);
            if !self.state.headers_sent {
                self.state.headers_sent = true;
                let headers = std::mem::take(&mut self.state.headers);
                if headers.len() < 2 {
                    cx.log(&format!("mali-v61: {} header buffer(s), PPS not returned", headers.len()));
                }
                for (k, h) in headers.into_iter().take(2).enumerate() {
                    self.output(cx, lsid, h, k, tag, 0x80)?;
                }
            }
            let nal = 2 + (self.state.encoded & 1) as usize;
            self.output(cx, lsid, stream, nal, tag, 0x11)?;
            self.state.encoded += 1;
            self.send(cx, lsid, 1, INPUT, Vec::new())?;
            self.send(cx, lsid, 3, FRAME, frame[1..].to_vec())?;
        }
        Ok(())
    }
}

fn read32(cx: &mut Cx<'_>, address: u64) -> Result<u32, String> {
    let b = cx.ram_bytes(address, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn read16(cx: &mut Cx<'_>, address: u64) -> Result<u16, String> {
    let b = cx.ram_bytes(address, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}
fn write32(cx: &mut Cx<'_>, address: u64, value: u32) -> Result<(), String> {
    cx.ram_write(address, &value.to_le_bytes())
}
fn write16(cx: &mut Cx<'_>, address: u64, value: u16) -> Result<(), String> {
    cx.ram_write(address, &value.to_le_bytes())
}

/// Bit writer for the H.264 syntax elements (Exp-Golomb codes).
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    used: u32,
}
impl Bits {
    fn u(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            if self.used % 8 == 0 {
                self.bytes.push(0);
            }
            if (v >> i) & 1 != 0 {
                *self.bytes.last_mut().unwrap() |= 0x80 >> (self.used % 8);
            }
            self.used += 1;
        }
    }
    fn ue(&mut self, v: u32) {
        let v = v + 1;
        let n = 32 - v.leading_zeros();
        self.u(n - 1, 0);
        self.u(n, v);
    }
    fn se(&mut self, v: i32) {
        self.ue(if v > 0 { 2 * v as u32 - 1 } else { (-2 * v) as u32 });
    }
    /// rbsp_trailing_bits.
    fn finish(mut self) -> Vec<u8> {
        self.u(1, 1);
        while self.used % 8 != 0 {
            self.u(1, 0);
        }
        self.bytes
    }
}

/// H.264 NAL unit types of a buffer: along 4-byte lengths that end exactly
/// at the buffer's end (big-endian as in the file, or little-endian as the
/// 5.11 driver writes them over the first one), or else after each 00 00 01
/// start code (a length of 1 reads as a start code, so lengths are tried
/// first). With `complete` false (the buffer was read in part), lengths may
/// run past the end, and are used only when no start code is found.
fn nal_types(bytes: &[u8], complete: bool) -> Vec<u8> {
    let walk = |little: bool| -> (Vec<u8>, bool, bool) {
        let mut types = Vec::new();
        let mut at = 0usize;
        while at + 4 < bytes.len() {
            let word = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
            let len = if little { u32::from_le_bytes(word) } else { u32::from_be_bytes(word) } as usize;
            let header = bytes[at + 4];
            if len == 0 || header & 0x80 != 0 {
                return (Vec::new(), false, false);
            }
            types.push(header & 0x1f);
            at = at.saturating_add(4).saturating_add(len);
        }
        let ends = at == bytes.len() && !types.is_empty();
        let runs_past = !complete && at > bytes.len() && !types.is_empty();
        (types, ends, runs_past)
    };
    let (big, little) = (walk(false), walk(true));
    for (lengths, ends, _) in [&big, &little] {
        if *ends {
            return lengths.clone();
        }
    }
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
            if bytes[i + 3] & 0x80 == 0 {
                types.push(bytes[i + 3] & 0x1f);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    if types.is_empty() {
        for (lengths, _, runs_past) in [big, little] {
            if runs_past {
                return lengths;
            }
        }
    }
    types
}

/// Start code, NAL header and the payload with emulation prevention bytes.
fn nal(header: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, header];
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

/// Baseline SPS (constraint sets 0 and 1), POC type 2, one reference frame.
fn sps(width: u32, height: u32, level: u8) -> Vec<u8> {
    let mut b = Bits::default();
    b.u(8, 66);
    b.u(8, 0xc0);
    b.u(8, level.into());
    b.ue(0); // seq_parameter_set_id
    b.ue(0); // log2_max_frame_num_minus4
    b.ue(2); // pic_order_cnt_type
    b.ue(1); // max_num_ref_frames
    b.u(1, 0); // gaps_in_frame_num_value_allowed_flag
    let (mw, mh) = (width.div_ceil(16), height.div_ceil(16));
    b.ue(mw - 1);
    b.ue(mh - 1);
    b.u(1, 1); // frame_mbs_only_flag
    b.u(1, 1); // direct_8x8_inference_flag
    let crop = ((mw * 16 - width) / 2, (mh * 16 - height) / 2);
    if crop != (0, 0) {
        b.u(1, 1);
        b.ue(0);
        b.ue(crop.0);
        b.ue(0);
        b.ue(crop.1);
    } else {
        b.u(1, 0);
    }
    b.u(1, 0); // vui_parameters_present_flag
    nal(0x67, &b.finish())
}

/// PPS: CAVLC, one slice group, no deblocking control.
fn pps() -> Vec<u8> {
    let mut b = Bits::default();
    b.ue(0);
    b.ue(0);
    b.u(1, 0);
    b.u(1, 0);
    b.ue(0);
    b.ue(0);
    b.ue(0);
    b.u(1, 0);
    b.u(2, 0);
    b.se(0);
    b.se(0);
    b.se(0);
    b.u(1, 0);
    b.u(1, 0);
    b.u(1, 0);
    nal(0x68, &b.finish())
}

/// IDR slice: every macroblock I_16x16, DC prediction, no residual.
fn idr(width: u32, height: u32, id: u32) -> Vec<u8> {
    let mut b = Bits::default();
    b.ue(0); // first_mb_in_slice
    b.ue(7); // slice_type I
    b.ue(0); // pic_parameter_set_id
    b.u(4, 0); // frame_num
    b.ue(id & 1); // idr_pic_id
    b.u(1, 0); // no_output_of_prior_pics_flag
    b.u(1, 0); // long_term_reference_flag
    b.se(0); // slice_qp_delta
    for _ in 0..width.div_ceil(16) * height.div_ceil(16) {
        // mb_type ue(3), intra_chroma_pred_mode ue(0), mb_qp_delta se(0), DC coeff_token '1'.
        b.u(8, 0x27);
    }
    nal(0x65, &b.finish())
}

impl Device for MaliV61 {
    fn name(&self) -> &'static str {
        self.name
    }
    fn read(&mut self, cx: &mut Cx<'_>, address: u64, size: u8) -> u64 {
        if size != 4 || address & 3 != 0 {
            cx.unmodeled();
            return 0;
        }
        let offset = address - self.options.base;
        if offset == IRQVE {
            return u64::from(self.word(offset) & !1 | u32::from(self.state.pending));
        }
        if let Some((_, TERMINATE)) = Self::session(offset) {
            return 0;
        }
        u64::from(self.word(offset))
    }
    fn write(&mut self, cx: &mut Cx<'_>, address: u64, size: u8, value: u64) {
        if size != 4 || address & 3 != 0 || value > u64::from(u32::MAX) {
            cx.unmodeled();
            return;
        }
        let value = value as u32;
        let offset = address - self.options.base;
        self.state.words.insert(offset, value);
        if offset == RESET && value & 1 != 0 {
            self.state.words.remove(&CORELSID);
            self.state.words.remove(&JOBQUEUE);
            if self.state.pending {
                self.state.pending = false;
                cx.set_irq(self.options.irq, false);
            }
            return;
        }
        match Self::session(offset) {
            Some((_, IRQ_ACK)) if value == 0 => {
                if self.state.pending {
                    self.state.pending = false;
                    cx.set_irq(self.options.irq, false);
                }
                // Answers still waiting for room: try again later.
                if !self.state.outbox.is_empty() && self.state.deadline.is_none() {
                    let at = cx.now() + self.options.ticks;
                    self.state.deadline = Some(at);
                    cx.schedule(at, 0);
                }
            }
            Some((lsid, DOORBELL)) if value & 1 != 0 => {
                self.state.rung |= 1 << lsid;
                if self.state.deadline.is_none() {
                    let at = cx.now() + self.options.ticks;
                    self.state.deadline = Some(at);
                    cx.schedule(at, 0);
                }
            }
            _ => {}
        }
    }
    fn advance(&mut self, cx: &mut Cx<'_>) {
        let now = cx.now();
        let pass = self.state.deadline.is_some_and(|at| at <= now);
        let picture = self.state.picture_at.is_some_and(|(at, _)| at <= now);
        if !pass && !picture {
            return;
        }
        let before = self.state.sent;
        let mut result = Ok(());
        if pass {
            self.state.deadline = None;
            result = self.flush(cx).map(|_| ());
            let rung = std::mem::take(&mut self.state.rung);
            for lsid in 0..4 {
                if result.is_ok() && rung & (1 << lsid) != 0 {
                    result = self.serve(cx, lsid);
                }
            }
        }
        if result.is_ok() && picture {
            result = self.due_picture(cx);
        }
        if let Err(e) = result {
            return cx.fail(format!("Mali-V61 {:#x}: {e}", self.options.base));
        }
        if self.state.sent != before && !self.state.pending {
            self.state.pending = true;
            cx.set_irq(self.options.irq, true);
        }
    }
    fn event(&mut self, cx: &mut Cx<'_>, _token: u64) {
        self.advance(cx);
    }
    fn next_deadline(&self, _now: u64) -> Option<u64> {
        match (self.state.deadline, self.state.picture_at) {
            (Some(d), Some((p, _))) => Some(d.min(p)),
            (d, p) => d.or(p.map(|(at, _)| at)),
        }
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
    use fujimod_emulator_devices::Host;

    const BASE: u64 = 0xffe0_0000;
    const RAM: u64 = 0x1000_0000;
    /// Two-level table at RAM: L0 at +0, L1 at +0x1000, pages from +0x10000.
    const PAGES: u64 = RAM + 0x10000;
    const QUEUES: u64 = 0x1007_9000;
    const STREAM_VA: u64 = 0x2000_1000;
    const HEADER_VA: u64 = 0x2000_0000;

    fn host() -> RecordingHost {
        let mut h = RecordingHost { ram: vec![0; 0x100000], ram_base: RAM, ..Default::default() };
        // Map QUEUES..+7 pages and the stream buffers.
        let map = |h: &mut RecordingHost, va: u64, pa: u64, l1: u64| {
            let l0e = ((l1 >> 10) as u32) | 3;
            h.ram_write(RAM + (va >> 22) * 4, &l0e.to_le_bytes()).unwrap();
            let e = ((pa >> 10) as u32) | 3;
            h.ram_write(l1 + ((va >> 12) & 0x3ff) * 4, &e.to_le_bytes()).unwrap();
        };
        for p in 0..7 {
            map(&mut h, QUEUES + p * 0x1000, PAGES + p * 0x1000, RAM + 0x1000);
        }
        for p in 0..4 {
            map(&mut h, HEADER_VA + p * 0x1000, PAGES + 0x8000 + p * 0x1000, RAM + 0x2000);
        }
        h
    }
    fn vpu(h264: bool) -> MaliV61 {
        MaliV61::new(Options {
            base: BASE,
            size: 0x1000,
            irq: 138,
            ticks: 10,
            queues: QUEUES,
            h264: h264.then_some(H264 { width: 64, height: 48, level: 51 }),
            decode: false,
            picture_ticks: 0,
            go_state_change: false,
            stop_state_change: false,
        })
        .unwrap()
    }
    fn wr(v: &mut MaliV61, h: &mut RecordingHost, off: u64, value: u32) {
        v.write(&mut Cx::new(h, 0, 0, 0), BASE + off, 4, value.into());
    }
    fn rd(v: &mut MaliV61, h: &mut RecordingHost, off: u64) -> u32 {
        v.read(&mut Cx::new(h, 0, 0, 0), BASE + off, 4) as u32
    }
    /// The driver's post: message at the host page's write position.
    fn post(h: &mut RecordingHost, page: u64, code: u32, payload: &[u32]) {
        let p = PAGES + page * 0x1000;
        let mut w = u16::from_le_bytes(h.bytes(p + 2, 2).unwrap().try_into().unwrap()) as u64;
        for x in std::iter::once(code | (payload.len() as u32 * 4) << 16).chain(payload.iter().copied()) {
            h.ram_write(p + 0x10 + 4 * w, &x.to_le_bytes()).unwrap();
            w = (w + 1) % 0x3fc;
        }
        h.ram_write(p + 2, &(w as u16).to_le_bytes()).unwrap();
    }
    /// The driver's read of a VPU page: all messages, advancing the host page's +0.
    fn drain(h: &mut RecordingHost, page: u64) -> Vec<Vec<u32>> {
        let (v, hp) = (PAGES + page * 0x1000, PAGES + (page - 1) * 0x1000);
        let rd16 = |h: &RecordingHost, a| u16::from_le_bytes(h.bytes(a, 2).unwrap().try_into().unwrap()) as u64;
        let rd32 = |h: &RecordingHost, a| u32::from_le_bytes(h.bytes(a, 4).unwrap().try_into().unwrap());
        let (mut r, w) = (rd16(h, hp), rd16(h, v));
        let mut out = vec![];
        while r != w {
            let hd = rd32(h, v + 0x10 + 4 * r);
            let mut m = vec![hd];
            r = (r + 1) % 0x3fc;
            for _ in 0..((hd >> 16) + 3) / 4 {
                m.push(rd32(h, v + 0x10 + 4 * r));
                r = (r + 1) % 0x3fc;
            }
            out.push(m);
        }
        h.ram_write(hp, &(r as u16).to_le_bytes()).unwrap();
        out
    }
    fn ring(v: &mut MaliV61, h: &mut RecordingHost) {
        wr(v, h, 0x220, 1);
        h.now += 10;
        v.advance(&mut Cx::new(h, 0, 0, 0));
    }

    #[test]
    fn reset_terminate_and_queue_words_read_what_the_driver_waits_for() {
        let mut h = host();
        let mut v = vpu(false);
        assert_eq!(rd(&mut v, &mut h, 0x14), 0x0f0f_0f0f);
        wr(&mut v, &mut h, 0x14, 0x0f0f_0f30);
        assert_eq!(rd(&mut v, &mut h, 0x14), 0x0f0f_0f30);
        wr(&mut v, &mut h, 0x10, 0);
        wr(&mut v, &mut h, 0x50, 1);
        assert_eq!(rd(&mut v, &mut h, 0x10), 0xffff);
        assert_eq!(rd(&mut v, &mut h, 0x14), 0x0f0f_0f0f);
        wr(&mut v, &mut h, 0x218, 1);
        assert_eq!(rd(&mut v, &mut h, 0x218), 0);
        wr(&mut v, &mut h, 0x230, 0x33);
        assert_eq!(rd(&mut v, &mut h, 0x230), 0x33);
    }

    #[test]
    fn job_and_options_are_answered_after_the_doorbell_and_the_line_is_acknowledged() {
        let mut h = host();
        let mut v = vpu(false);
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        post(&mut h, 0, JOB, &[4, 0]);
        post(&mut h, 0, SET_OPTION, &[1, 0x10]);
        post(&mut h, 0, SET_OPTION, &[5, 1]);
        post(&mut h, 0, 0x3e9, &[]);
        wr(&mut v, &mut h, 0x220, 1);
        assert!(h.irqs.is_empty());
        h.now += 10;
        v.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(h.irqs.last(), Some(&(138, true)));
        assert_eq!(rd(&mut v, &mut h, 0x18) & 1, 1);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2001, 2003, 2003]);
        wr(&mut v, &mut h, 0x21c, 0);
        assert_eq!(h.irqs.last(), Some(&(138, false)));
        assert_eq!(rd(&mut v, &mut h, 0x18) & 1, 0);
        // Nothing new: no line.
        ring(&mut v, &mut h);
        assert_eq!(h.irqs.last(), Some(&(138, false)));
    }

    #[test]
    fn a_frame_comes_back_after_its_headers_and_its_idr_picture() {
        let mut h = host();
        let mut v = vpu(true);
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        // Two header buffers before any frame.
        for k in 0..2u32 {
            let va = HEADER_VA as u32 + 0x40 * k;
            post(&mut h, 4, STREAM, &[0x4b0d_a000 + k, 0, 0, 0, 0, 0x1000, 0, 0, va, 0]);
        }
        ring(&mut v, &mut h);
        assert!(drain(&mut h, 1).is_empty());
        let mut frame = vec![0u32; 0x48 / 4];
        frame[0] = 7;
        frame[2] = 7;
        post(&mut h, 2, FRAME, &frame);
        post(&mut h, 4, STREAM, &[7, 0, 0, 0, 0, 0x3000, 0, 0, STREAM_VA as u32, 0]);
        ring(&mut v, &mut h);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2006, 2006, 2006, 2005]);
        let out = drain(&mut h, 5);
        assert_eq!(out.len(), 3);
        let (s, p, i) = (&out[0], &out[1], &out[2]);
        assert_eq!((s[1], s[3], s[5], s[7]), (0x4b0d_a000, 7, 0x80, 0));
        assert_eq!((p[1], p[3], p[5]), (0x4b0d_a001, 7, 0x80));
        assert_eq!((i[1], i[3], i[5], i[7]), (7, 7, 0x11, 0));
        let sps = h.bytes(PAGES + 0x8000, s[8] as usize).unwrap();
        assert_eq!(&sps[..5], &[0, 0, 0, 1, 0x67]);
        let pps = h.bytes(PAGES + 0x8040, p[8] as usize).unwrap();
        assert_eq!(pps, [0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80]);
        let idr = h.bytes(PAGES + 0x9000, i[8] as usize).unwrap();
        assert_eq!(&idr[..5], &[0, 0, 0, 1, 0x65]);
        assert_eq!(drain(&mut h, 3), [std::iter::once(3001 | 0x48 << 16).chain(frame).collect::<Vec<_>>()]);
    }

    #[test]
    fn decode_answers_go_the_sequence_the_flush_and_returns_a_grey_picture() {
        let mut h = host();
        let mut v = MaliV61::new(Options { decode: true, picture_ticks: 100, ..vpu(false).options }).unwrap();
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        post(&mut h, 0, JOB, &[4, 0]);
        post(&mut h, 0, SET_OPTION, &[1, 0x10]);
        post(&mut h, 0, GO, &[]);
        // Payload index 4 holds the flags (bit 0 ends the stream), 7 the bytes
        // filled, 8 the address. These two hold no slice: the end of the
        // stream owes the session's first picture.
        let bitstream = |k: u32, flags: u32| vec![0x4b0d_a000 + k, 0, 0x4b0d_a000 + k, 0, flags, 0x1000, 0, 0x200, HEADER_VA as u32, 0];
        post(&mut h, 2, STREAM, &bitstream(0, 0));
        post(&mut h, 2, STREAM, &bitstream(1, END_OF_STREAM));
        ring(&mut v, &mut h);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0] & 0xffff).collect();
        assert_eq!(codes, [2001, 2003, 2011]);
        // A frame: 16 rows, luma at STREAM_VA (stride 64), chroma 0x400 after it.
        let mut frame = vec![0u32; 0x48 / 4];
        (frame[0], frame[2], frame[5]) = (9, 9, 16 | 64 << 16);
        (frame[7], frame[8], frame[13], frame[14]) = (STREAM_VA as u32, STREAM_VA as u32 + 0x400, 64, 64);
        post(&mut h, 4, FRAME, &frame);
        ring(&mut v, &mut h);
        let answers = drain(&mut h, 1);
        assert_eq!(answers[0], [SEQ_PARAMETERS | 16 << 16, 16, 64, 64, 1]);
        let codes: Vec<u32> = answers[1..].iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2005, 2005]);
        let back = drain(&mut h, 3);
        assert_eq!(back[1], std::iter::once(STREAM | 0x28 << 16).chain(bitstream(1, END_OF_STREAM)).collect::<Vec<_>>());
        // Output flush: the frame comes back empty, then 2008.
        post(&mut h, 0, OUTPUT_FLUSH, &[]);
        ring(&mut v, &mut h);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2006, 2008]);
        assert_eq!(drain(&mut h, 5)[0][5], 0);
        assert!(h.bytes(PAGES + 0x9000, 1).unwrap() == [0]);
        // After the flush the frame comes back: the owed picture follows after the delay.
        assert_eq!(v.state.owed, 1);
        post(&mut h, 4, FRAME, &frame);
        // A buffer with a slice owes a picture; a second end of stream does not.
        h.ram_write(PAGES + 0x8300, &[0, 0, 0, 1, 0x65, 0x88, 0x84]).unwrap();
        let mut slice = bitstream(2, 0);
        (slice[7], slice[8]) = (7, HEADER_VA as u32 + 0x300);
        post(&mut h, 2, STREAM, &slice);
        post(&mut h, 2, STREAM, &bitstream(3, END_OF_STREAM));
        ring(&mut v, &mut h);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2005, 2005]);
        assert_eq!(drain(&mut h, 3).len(), 2);
        assert_eq!((v.state.owed, v.state.slices, v.state.no_slice), (2, 1, 3));
        let at = v.state.picture_at.unwrap().0;
        assert_eq!(v.next_deadline(h.now), Some(at));
        h.now = at - 1;
        v.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert!(drain(&mut h, 1).is_empty());
        h.now = at;
        v.advance(&mut Cx::new(&mut h, 0, 0, 0));
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2006]);
        assert_eq!((v.state.owed, v.state.picture_at), (1, None));
        let out = drain(&mut h, 5);
        assert_eq!((out[0][1], out[0][3], out[0][5]), (9, 9, PICTURE));
        assert!(h.bytes(PAGES + 0x9000, 0x400).unwrap().iter().all(|&b| b == 0x80));
        assert!(h.bytes(PAGES + 0x9400, 0x200).unwrap().iter().all(|&b| b == 0x80));
        assert_eq!(h.bytes(PAGES + 0x9600, 1).unwrap(), [0]);
        assert_eq!(v.state.decoded, 1);
        // Without the option, the same messages get no decode answer.
        let mut h = host();
        let mut v = vpu(false);
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        post(&mut h, 0, GO, &[]);
        post(&mut h, 2, STREAM, &bitstream(0, END_OF_STREAM));
        post(&mut h, 4, FRAME, &frame);
        ring(&mut v, &mut h);
        assert!(drain(&mut h, 1).is_empty());
        assert!(v.save().get("bitstreams").is_none());
    }

    #[test]
    fn encode_answers_go_only_with_the_option() {
        for (option, expected) in [(false, vec![2001, 2003]), (true, vec![2001, 2003, 2011])] {
            let mut h = host();
            let mut v = MaliV61::new(Options { go_state_change: option, ..vpu(true).options }).unwrap();
            wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
            post(&mut h, 0, JOB, &[4, 0]);
            post(&mut h, 0, SET_OPTION, &[1, 0x10]);
            post(&mut h, 0, GO, &[]);
            post(&mut h, 2, 3003, &[0, 0]);
            ring(&mut v, &mut h);
            let answers = drain(&mut h, 1);
            let codes: Vec<u32> = answers.iter().map(|m| m[0] & 0xffff).collect();
            assert_eq!(codes, expected);
            if option {
                assert_eq!(answers[2], [STATE_CHANGE | 4 << 16, RUNNING]);
            }
            assert_eq!(v.save().get("go_state_change"), None);
            assert_eq!((v.state.frames_seen, v.state.encoded), (0, 0));
        }
    }

    #[test]
    fn stop_is_answered_only_with_the_option() {
        for decode in [false, true] {
            for (option, expected) in [(false, vec![2001]), (true, vec![2001, 2011])] {
                let mut h = host();
                let mut v = MaliV61::new(Options { decode, stop_state_change: option, ..vpu(true).options }).unwrap();
                wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
                post(&mut h, 0, JOB, &[4, 0]);
                post(&mut h, 0, STOP, &[]);
                ring(&mut v, &mut h);
                let answers = drain(&mut h, 1);
                let codes: Vec<u32> = answers.iter().map(|m| m[0] & 0xffff).collect();
                assert_eq!(codes, expected, "decode {decode}, option {option}");
                if option {
                    assert_eq!(answers[1], [STATE_CHANGE | 4 << 16, STOPPED]);
                }
                assert_eq!(v.state.received, 2);
                assert_eq!(v.save().get("stop_state_change"), None);
            }
        }
    }

    #[test]
    fn with_decode_each_session_follows_its_first_buffer() {
        let mut h = host();
        let mut v = MaliV61::new(Options { decode: true, ..vpu(true).options }).unwrap();
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        // A recording: parameters (3003) tell nothing, the header buffers on page 4 do.
        post(&mut h, 0, JOB, &[4, 0]);
        post(&mut h, 0, SET_OPTION, &[1, 0x10]);
        post(&mut h, 0, GO, &[]);
        post(&mut h, 2, 3003, &[0, 0]);
        ring(&mut v, &mut h);
        assert_eq!(v.state.mode, 0);
        for k in 0..2u32 {
            let va = HEADER_VA as u32 + 0x40 * k;
            post(&mut h, 4, STREAM, &[0x4b0d_a000 + k, 0, 0, 0, 0, 0x1000, 0, 0, va, 0]);
        }
        ring(&mut v, &mut h);
        assert_eq!(v.state.mode, ENCODING);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0] & 0xffff).collect();
        assert_eq!(codes, [2001, 2003, 2011]);
        let mut frame = vec![0u32; 0x48 / 4];
        (frame[0], frame[2]) = (7, 7);
        post(&mut h, 2, FRAME, &frame);
        post(&mut h, 4, STREAM, &[7, 0, 0, 0, 0, 0x3000, 0, 0, STREAM_VA as u32, 0]);
        ring(&mut v, &mut h);
        assert_eq!(v.state.encoded, 1);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0]).collect();
        assert_eq!(codes, [2006, 2006, 2006, 2005]);
        // Then a playback: the JOB starts over and its bitstream makes it decode.
        post(&mut h, 0, JOB, &[4, 0]);
        post(&mut h, 0, GO, &[]);
        post(&mut h, 2, STREAM, &[0x4b0d_b000, 0, 0x4b0d_b000, 0, END_OF_STREAM, 0x1000, 0, 0x200, HEADER_VA as u32, 0]);
        let mut out = vec![0u32; 0x48 / 4];
        (out[0], out[2], out[5]) = (9, 9, 16 | 64 << 16);
        post(&mut h, 4, FRAME, &out);
        ring(&mut v, &mut h);
        assert_eq!(v.state.mode, DECODING);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0] & 0xffff).collect();
        assert_eq!(codes, [2001, 2011, 2015, 2005]);
        assert_eq!((v.state.encoded, v.state.owed), (1, 1));
        assert_eq!(v.save().get("mode"), Some(&serde_json::json!(DECODING)));
    }

    #[test]
    fn slices_are_found_after_start_codes_or_along_lengths() {
        // SPS, PPS, IDR slice (Annex B, 3- and 4-byte start codes).
        assert_eq!(nal_types(&[0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88], true), [7, 8, 5]);
        // Lengths: a P slice then an SEI of one byte (its length reads as a
        // start code), ending at the buffer's end.
        assert_eq!(nal_types(&[0, 0, 0, 2, 0x41, 0x9a, 0, 0, 0, 1, 0x06], true), [1, 6]);
        // Lengths that do not end at the buffer's end, unless it was read in part.
        assert!(nal_types(&[0, 0, 0, 2, 0x41, 0x9a, 0, 0], true).is_empty());
        assert_eq!(nal_types(&[0, 0, 0, 9, 0x25, 0x88, 0x84], false), [5]);
        // Zeros, a forbidden bit.
        assert!(nal_types(&[0; 64], true).is_empty());
        // As the 5.11 driver hands an IDR sample of the test movie: its
        // length (8168 - 4) little-endian, then the NAL; the 4-byte header
        // buffers before it are zeros.
        let mut sample = vec![0xe4, 0x1f, 0, 0, 0x65, 0x88, 0x84, 0x93];
        sample.resize(0x1fe8, 0x5a);
        assert_eq!(nal_types(&sample, true), [5]);
        assert_eq!(nal_types(&sample[..0x100], false), [5]);
        assert!(nal_types(&[0, 0, 0, 0], true).is_empty());
        assert!(nal_types(&[0, 0, 1, 0xe5, 0x88], true).is_empty());
    }

    #[test]
    fn playback_owes_a_picture_per_slice_and_the_pictures_follow_one_another() {
        let mut h = host();
        let mut v = MaliV61::new(Options { decode: true, picture_ticks: 100, ..vpu(false).options }).unwrap();
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        post(&mut h, 0, JOB, &[4, 0]);
        post(&mut h, 0, GO, &[]);
        // Three samples without an end of stream: an IDR access unit, then two
        // P slices (the last one with a length instead of a start code).
        let units: [&[u8]; 3] = [&[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88], &[0, 0, 0, 1, 0x41, 0x9a], &[0, 0, 0, 2, 0x41, 0x9a]];
        for (k, u) in units.iter().enumerate() {
            let at = 0x100 * (k as u64 + 1);
            h.ram_write(PAGES + 0x8000 + at, u).unwrap();
            post(&mut h, 2, STREAM, &[k as u32, 0, k as u32, 0, 0, 0x100, 0, u.len() as u32, (HEADER_VA + at) as u32, 0]);
        }
        // Two one-plane frames (format 0x121, 16 rows of 64 bytes); the
        // chroma word is ignored for this format.
        let frame = |handle: u32, luma: u64| {
            let mut f = vec![0u32; 0x48 / 4];
            (f[0], f[2], f[5], f[6]) = (handle, handle, 16 | 64 << 16, ONE_PLANE);
            (f[7], f[8], f[13]) = (luma as u32, STREAM_VA as u32 + 0x400, 64);
            f
        };
        post(&mut h, 4, FRAME, &frame(9, STREAM_VA));
        post(&mut h, 4, FRAME, &frame(10, STREAM_VA + 0x800));
        ring(&mut v, &mut h);
        let codes: Vec<u32> = drain(&mut h, 1).iter().map(|m| m[0] & 0xffff).collect();
        assert_eq!(codes, [2001, 2011, 2015, 2005, 2005, 2005]);
        assert_eq!((v.state.owed, v.state.slices, v.state.no_slice), (3, 3, 0));
        drain(&mut h, 3);
        post(&mut h, 0, OUTPUT_FLUSH, &[]);
        ring(&mut v, &mut h);
        drain(&mut h, 1);
        drain(&mut h, 5);
        // The frames come back: one picture each, `picture_ticks` apart.
        post(&mut h, 4, FRAME, &frame(9, STREAM_VA));
        post(&mut h, 4, FRAME, &frame(10, STREAM_VA + 0x800));
        ring(&mut v, &mut h);
        let mut handles = vec![];
        for _ in 0..2 {
            h.now = v.state.picture_at.unwrap().0;
            v.advance(&mut Cx::new(&mut h, 0, 0, 0));
            handles.extend(drain(&mut h, 5).iter().map(|m| (m[1], m[5])));
        }
        assert_eq!(handles, [(9, PICTURE), (10, PICTURE)]);
        assert_eq!((v.state.owed, v.state.decoded, v.state.picture_at), (1, 2, None));
        assert!(h.bytes(PAGES + 0x9000, 0x400).unwrap().iter().all(|&b| b == 0x80));
        assert!(h.bytes(PAGES + 0x9400, 0x400).unwrap().iter().all(|&b| b == 0));
        assert!(h.bytes(PAGES + 0x9800, 0x400).unwrap().iter().all(|&b| b == 0x80));
        // The driver shows the first one and gives it back: the third picture.
        post(&mut h, 4, FRAME, &frame(9, STREAM_VA));
        ring(&mut v, &mut h);
        h.now = v.state.picture_at.unwrap().0;
        v.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(drain(&mut h, 5)[0][1], 9);
        assert_eq!((v.state.owed, v.state.decoded, v.state.session_owed), (0, 3, 3));
    }

    #[test]
    fn the_nals_match_the_reference_generator() {
        // outils/gen-h264-idr-gris.py 1920 1080: SPS, PPS and IDR size.
        assert_eq!(sps(1920, 1080, 51), [0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x33, 0xda, 0x01, 0xe0, 0x08, 0x9f, 0x95]);
        assert_eq!(sps(3840, 2160, 51), [0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x33, 0xda, 0x00, 0xf0, 0x01, 0x0f, 0x90]);
        assert_eq!(pps(), [0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80]);
        assert_eq!(idr(1920, 1080, 0).len(), 8168);
        assert_eq!(idr(3840, 2160, 1).len(), 32408);
    }

    #[test]
    fn answers_that_do_not_fit_wait_in_order() {
        let mut h = host();
        let mut v = vpu(false);
        wr(&mut v, &mut h, 0x204, ((RAM >> 10) as u32) | 3);
        // Driver has not read page 1: leave room for 3 words only.
        let p0 = PAGES;
        h.ram_write(p0, &4u16.to_le_bytes()).unwrap();
        h.ram_write(PAGES + 0x1000, &0u16.to_le_bytes()).unwrap();
        for _ in 0..5 {
            post(&mut h, 0, SET_OPTION, &[5, 1]);
        }
        ring(&mut v, &mut h);
        assert_eq!(v.state.outbox.len(), 2);
        // The unread part of the ring (positions 4..0x3fc) holds empty words.
        let first = drain(&mut h, 1);
        assert_eq!(first.len(), 0x3f8 + 3);
        assert!(first[0x3f8..].iter().all(|m| m[0] == OPTION_DONE));
        wr(&mut v, &mut h, 0x21c, 0);
        h.now += 10;
        v.advance(&mut Cx::new(&mut h, 0, 0, 0));
        assert_eq!(drain(&mut h, 1).len(), 2);
        assert!(v.state.outbox.is_empty());
    }
}
