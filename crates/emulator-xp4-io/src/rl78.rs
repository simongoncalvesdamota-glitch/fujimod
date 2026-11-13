//! Renesas RL78 flash loader, serial programming "Protocol A" (Renesas
//! application note R01AN0815), as the far end of a PL011 port.
//!
//! X-T3 5.11 updates reprogram the power microcontroller through port 0
//! (serial resource 5, table `0x13e58d4`: 115 200 baud, GPIO 10.5/10.6 for
//! RESET and TOOL0; driver `0x10a0c88`, commands built by `0x10a49dc`).
//!
//! Frames: a command is `01 LEN COM data… SUM 03`, a data frame
//! `02 LEN data… SUM 03` (`17` instead of `03` when more data frames follow);
//! LEN counts COM and the data bytes, 0 meaning 256, and
//! SUM = (0 − LEN − every byte after LEN) & 0xff. Answers are status frames
//! `02 LEN status… SUM 03`. Addresses are three bytes, low byte first.
//!
//! Model: bytes outside a frame (the mode-entry `00` or `3a`) get no answer.
//! A frame whose sum is wrong is answered `07`; an unknown command `04`.
//! - `9a` baud rate set: `06 20 00` (ACK, 32 MHz, full speed);
//! - `00` reset, `22` block erase (1 KiB block at the start address, erased
//!   to `ff`), `40` programming, `13` verify: `06`;
//! - `32` block blank check: `06` when every byte in range reads `ff`, else `1b`;
//! - `c0` silicon signature: `06`, then the declared 22-byte signature as a
//!   data frame;
//! - `b0` checksum: `06`, then a data frame with the 16-bit value
//!   0 − (sum of the bytes in range), low byte first (R01AN0815 fig. 3-28);
//! - every data frame after `40` or `13` is answered `06 06`; the bytes
//!   programmed are stored from the start address. The last verify frame
//!   reports every difference seen in the range: `06 1b` (note to fig. 3-17).
//!   After the last programming frame the loader also reports its internal
//!   verify: `06` (§3.4.4, fig. 3-13).
//!
//! The code flash is kept sparse, in 1 KiB blocks, up to the signature's code
//! flash end. What the microcontroller later does with its program, and the
//! time each operation takes, are not modeled.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const ETX: u8 = 0x03;
const ETB: u8 = 0x17;
const ACK: u8 = 0x06;
const SUM_ERROR: u8 = 0x07;
const COMMAND_ERROR: u8 = 0x04;
const PARAMETER_ERROR: u8 = 0x05;
const BLANK_ERROR: u8 = 0x1b;
const BLOCK: u32 = 1024;
/// Signature data frame length: device code (3), name (10), code flash end
/// (3), data flash end (3), firmware version (3).
pub const SIGNATURE: usize = 22;
/// Placeholder signature: the X-T3's power microcontroller part is not known.
/// Its code flash end, 0xbfff (48 KiB), is one the X-T3 5.11 updater accepts:
/// it compares end + 1 with the image sizes in the update header (16 or
/// 48 KiB) and stops before any erase when none matches (`0x10a4fa0`).
pub const DEFAULT_SIGNATURE: [u8; SIGNATURE] = [
    0x10, 0x00, 0x06, b'E', b'M', b'U', b'L', b'A', b'T', b'E', b'D', b' ', b' ', 0xff, 0xbf, 0x00,
    0xff, 0x1f, 0x0f, 0x01, 0x00, 0x00,
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
enum Stream {
    #[default]
    None,
    Program,
    Verify,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rl78 {
    signature: Vec<u8>,
    /// Bytes of the frame being received.
    frame: Vec<u8>,
    stream: Stream,
    /// Next address and last address of the programming or verify range.
    cursor: u32,
    end: u32,
    mismatch: bool,
    blocks: BTreeMap<u32, Vec<u8>>,
    /// Commands received, in order (bounded), for the report.
    commands: Vec<u8>,
    command_count: u64,
    data_frames: u64,
    ignored: u64,
    sum_errors: u64,
}

const KEPT_COMMANDS: usize = 256;

impl Rl78 {
    pub fn new(signature: [u8; SIGNATURE]) -> Self {
        Self {
            signature: signature.to_vec(),
            frame: Vec::new(),
            stream: Stream::None,
            cursor: 0,
            end: 0,
            mismatch: false,
            blocks: BTreeMap::new(),
            commands: Vec::new(),
            command_count: 0,
            data_frames: 0,
            ignored: 0,
            sum_errors: 0,
        }
    }
    fn code_end(&self) -> u32 {
        u32::from_le_bytes([self.signature[13], self.signature[14], self.signature[15], 0])
    }
    /// Checks a restored loader.
    pub fn valid(&self) -> bool {
        self.signature.len() == SIGNATURE
            && self.frame.len() <= 260
            && self.commands.len() <= KEPT_COMMANDS
            && self.blocks.iter().all(|(&b, v)| v.len() == BLOCK as usize && b * BLOCK <= self.code_end())
    }
    fn read(&self, address: u32) -> u8 {
        self.blocks
            .get(&(address / BLOCK))
            .map_or(0xff, |b| b[(address % BLOCK) as usize])
    }
    fn write(&mut self, address: u32, byte: u8) {
        self.blocks
            .entry(address / BLOCK)
            .or_insert_with(|| vec![0xff; BLOCK as usize])[(address % BLOCK) as usize] = byte;
    }
    /// One byte from the host; returns the bytes the loader sends back.
    pub fn receive(&mut self, byte: u8) -> Vec<u8> {
        if self.frame.is_empty() && byte != SOH && byte != STX {
            self.ignored += 1;
            return Vec::new();
        }
        self.frame.push(byte);
        if self.frame.len() < 2 {
            return Vec::new();
        }
        let len = match self.frame[1] {
            0 => 256,
            n => usize::from(n),
        };
        if self.frame.len() < len + 4 {
            return Vec::new();
        }
        let frame = std::mem::take(&mut self.frame);
        let body = &frame[2..2 + len];
        let sum = body
            .iter()
            .fold(0u8.wrapping_sub(frame[1]), |s, &b| s.wrapping_sub(b));
        let end = frame[len + 3];
        if sum != frame[len + 2] || !matches!(end, ETX | ETB) {
            self.sum_errors += 1;
            self.stream = Stream::None;
            return status(&[SUM_ERROR]);
        }
        if frame[0] == SOH {
            self.command(body[0], &body[1..])
        } else {
            self.data(body, end == ETX)
        }
    }
    fn command(&mut self, com: u8, data: &[u8]) -> Vec<u8> {
        if self.commands.len() < KEPT_COMMANDS {
            self.commands.push(com);
        }
        self.command_count += 1;
        self.stream = Stream::None;
        let address = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], 0]);
        let range = |s: &Self| -> Option<(u32, u32)> {
            (data.len() >= 6)
                .then(|| (address(0), address(3)))
                .filter(|&(a, b)| a <= b && b <= s.code_end())
        };
        match com {
            0x9a => status(&[ACK, 0x20, 0x00]),
            0x00 => status(&[ACK]),
            0x22 => {
                if data.len() < 3 || address(0) % BLOCK != 0 || address(0) > self.code_end() {
                    return status(&[PARAMETER_ERROR]);
                }
                self.blocks.remove(&(address(0) / BLOCK));
                status(&[ACK])
            }
            0x32 => match range(self) {
                Some((a, b)) if (a..=b).all(|x| self.read(x) == 0xff) => status(&[ACK]),
                Some(_) => status(&[BLANK_ERROR]),
                None => status(&[PARAMETER_ERROR]),
            },
            0x40 | 0x13 => match range(self) {
                Some((a, b)) => {
                    self.stream = if com == 0x40 { Stream::Program } else { Stream::Verify };
                    self.cursor = a;
                    self.end = b;
                    self.mismatch = false;
                    status(&[ACK])
                }
                None => status(&[PARAMETER_ERROR]),
            },
            0xc0 => {
                let mut out = status(&[ACK]);
                out.extend(status(&self.signature.clone()));
                out
            }
            0xb0 => match range(self) {
                Some((a, b)) => {
                    let value = (a..=b).fold(0u16, |s, x| s.wrapping_sub(u16::from(self.read(x))));
                    let mut out = status(&[ACK]);
                    out.extend(status(&value.to_le_bytes()));
                    out
                }
                None => status(&[PARAMETER_ERROR]),
            },
            _ => status(&[COMMAND_ERROR]),
        }
    }
    fn data(&mut self, body: &[u8], last: bool) -> Vec<u8> {
        self.data_frames += 1;
        let stream = self.stream;
        if stream == Stream::None
            || u64::from(self.cursor) + body.len() as u64 > u64::from(self.end) + 1
        {
            self.stream = Stream::None;
            return status(&[PARAMETER_ERROR]);
        }
        for &byte in body {
            if stream == Stream::Program {
                self.write(self.cursor, byte);
            } else if self.read(self.cursor) != byte {
                self.mismatch = true;
            }
            self.cursor += 1;
        }
        let differs = last && stream == Stream::Verify && self.mismatch;
        let mut out = status(&[ACK, if differs { BLANK_ERROR } else { ACK }]);
        if last {
            self.stream = Stream::None;
            if stream == Stream::Program {
                out.extend(status(&[ACK]));
            }
        }
        out
    }
}

/// A status or data frame from the loader: `02 LEN bytes SUM 03`.
fn status(bytes: &[u8]) -> Vec<u8> {
    let len = bytes.len() as u8;
    let sum = bytes.iter().fold(0u8.wrapping_sub(len), |s, &b| s.wrapping_sub(b));
    let mut out = vec![STX, len];
    out.extend_from_slice(bytes);
    out.extend([sum, ETX]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(com: u8, data: &[u8]) -> Vec<u8> {
        let mut body = vec![com];
        body.extend_from_slice(data);
        let mut f = status(&body);
        f[0] = SOH;
        f
    }
    fn feed(l: &mut Rl78, bytes: &[u8]) -> Vec<u8> {
        bytes.iter().flat_map(|&b| l.receive(b)).collect()
    }

    #[test]
    fn the_update_frames_get_the_application_note_answers() {
        let mut l = Rl78::new(DEFAULT_SIGNATURE);
        // Mode entry, then the frame the X-T3 sends first, sum and end included.
        assert_eq!(feed(&mut l, &[0x00]), Vec::<u8>::new());
        let baud = [0x01, 0x03, 0x9a, 0x00, 0x21, 0x42, 0x03];
        assert_eq!(command(0x9a, &[0x00, 0x21]), baud);
        assert_eq!(feed(&mut l, &baud[..6]), Vec::<u8>::new());
        assert_eq!(feed(&mut l, &baud[6..]), vec![0x02, 0x03, 0x06, 0x20, 0x00, 0xd7, 0x03]);
        assert_eq!(feed(&mut l, &[0x01, 0x01, 0x00, 0xff, 0x03]), vec![0x02, 0x01, 0x06, 0xf9, 0x03]);
        // A wrong sum is refused.
        assert_eq!(feed(&mut l, &[0x01, 0x01, 0x00, 0xfe, 0x03]), status(&[SUM_ERROR]));
        // Silicon signature: ACK then the 22-byte data frame.
        let sig = feed(&mut l, &command(0xc0, &[]));
        assert_eq!(&sig[..5], &[0x02, 0x01, 0x06, 0xf9, 0x03]);
        assert_eq!(&sig[5..7], &[0x02, 0x16]);
        assert_eq!(sig.len(), 5 + 26);
        assert_eq!(feed(&mut l, &command(0x55, &[])), status(&[COMMAND_ERROR]));
        assert_eq!(l.commands, vec![0x9a, 0x00, 0xc0, 0x55]);
        assert_eq!(l.ignored, 1);
        assert_eq!(l.sum_errors, 1);
    }

    #[test]
    fn erase_program_verify_and_checksum_agree() {
        let mut l = Rl78::new(DEFAULT_SIGNATURE);
        let range = [0x00, 0x04, 0x00, 0x07, 0x04, 0x00]; // 0x400..=0x407
        assert_eq!(feed(&mut l, &command(0x32, &[range.as_slice(), &[0]].concat())), status(&[ACK]));
        assert_eq!(feed(&mut l, &command(0x40, &range)), status(&[ACK]));
        let mut first = status(&[1, 2, 3, 4]);
        *first.last_mut().unwrap() = ETB;
        assert_eq!(feed(&mut l, &first), vec![0x02, 0x02, 0x06, 0x06, 0xf2, 0x03]);
        let last = status(&[5, 6, 7, 8]);
        let mut both = vec![0x02, 0x02, 0x06, 0x06, 0xf2, 0x03];
        both.extend([0x02, 0x01, 0x06, 0xf9, 0x03]);
        assert_eq!(feed(&mut l, &last), both);
        assert_eq!(feed(&mut l, &command(0x32, &[range.as_slice(), &[0]].concat())), status(&[BLANK_ERROR]));
        // Verify: a wrong byte in an intermediate frame is still answered
        // 06 06; the last frame reports it.
        assert_eq!(feed(&mut l, &command(0x13, &range)), status(&[ACK]));
        let mut wrong = status(&[1, 2, 3, 9]);
        *wrong.last_mut().unwrap() = ETB;
        assert_eq!(feed(&mut l, &wrong), status(&[ACK, ACK]));
        assert_eq!(feed(&mut l, &status(&[5, 6, 7, 8])), status(&[ACK, BLANK_ERROR]));
        assert_eq!(feed(&mut l, &command(0x13, &range)), status(&[ACK]));
        assert_eq!(feed(&mut l, &status(&[1, 2, 3, 4, 5, 6, 7, 8])), status(&[ACK, ACK]));
        // Checksum 0 − 36 = 0xffdc, low byte first.
        let ck = feed(&mut l, &command(0xb0, &range));
        assert_eq!(&ck[..5], &status(&[ACK])[..]);
        assert_eq!(&ck[5..9], &[0x02, 0x02, 0xdc, 0xff]);
        // Erasing the block makes it blank again; a checkpoint keeps the flash.
        assert_eq!(feed(&mut l, &command(0x22, &[0x00, 0x04, 0x00])), status(&[ACK]));
        assert_eq!(l.read(0x403), 0xff);
        assert_eq!(feed(&mut l, &command(0x40, &range)), status(&[ACK]));
        feed(&mut l, &status(&[9; 8]));
        let saved: Rl78 = serde_json::from_value(serde_json::to_value(&l).unwrap()).unwrap();
        assert!(saved.valid());
        assert_eq!(saved.read(0x407), 9);
        // Data beyond the range, or without a command, is refused.
        assert_eq!(feed(&mut l, &status(&[1])), status(&[PARAMETER_ERROR]));
    }
}
