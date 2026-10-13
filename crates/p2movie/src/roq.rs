//! id RoQ video decoding, written from the publicly documented format.
//!
//! A RoQ file is a sequence of chunks, each `u16 id, u32 size, u16 arg` followed by `size`
//! bytes. The first chunk is a signature (`0x1084`, size `0xFFFFFFFF`, arg = frames per second).
//!
//! * `0x1001` info: width, height (u16 each) and two unused words.
//! * `0x1002` codebook: `arg >> 8` 2x2 cells (0 means 256), each `Y0 Y1 Y2 Y3 U V` (raster
//!   order, one chroma pair per cell), then `arg & 0xFF` 4x4 cells (0 means 256 when bytes
//!   remain), each four indices into the 2x2 book (top-left, top-right, bottom-left, bottom-right).
//! * `0x1011` VQ frame: the image is walked in 16x16 macroblocks, each split into four 8x8
//!   blocks in raster order. A 16-bit little-endian flag word supplies 2-bit codes, most
//!   significant pair first, read whenever the previous word is exhausted. Codes:
//!   `0` keep the block from the previous frame, `1` motion: copy the block from the previous
//!   frame displaced by `(8 - (b >> 4) - mean_x, 8 - (b & 15) - mean_y)` where `b` is one byte
//!   and the means are the signed high/low bytes of the chunk arg, `2` one 4x4 codebook cell
//!   (scaled up 2x for an 8x8 block), `3` split into four quadrants, each coded the same way
//!   one level down (for 4x4 quadrants: `2` paints a 4x4 cell, `3` reads four 2x2 cell indices).
//! * `0x1020` / `0x1021` audio: mono / stereo DPCM at 22050 Hz. Each byte is a signed squared
//!   delta (`b < 128 ? b*b : -(b-128)^2`) added to a running 16-bit predictor whose start value
//!   comes from the arg (mono: the whole arg; stereo: high byte = left << 8, low byte = right << 8).

use std::fmt;

pub const SIG: u16 = 0x1084;
pub const INFO: u16 = 0x1001;
pub const QUAD_CODEBOOK: u16 = 0x1002;
pub const QUAD_VQ: u16 = 0x1011;
pub const JPEG: u16 = 0x1012;
pub const AUDIO_MONO: u16 = 0x1020;
pub const AUDIO_STEREO: u16 = 0x1021;

pub const AUDIO_RATE: u32 = 22050;

#[derive(Debug)]
pub enum RoqError {
    NotRoq,
    Truncated,
    Unsupported(String),
}

impl fmt::Display for RoqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoqError::NotRoq => write!(f, "not a RoQ file"),
            RoqError::Truncated => write!(f, "truncated RoQ data"),
            RoqError::Unsupported(s) => write!(f, "unsupported RoQ feature: {s}"),
        }
    }
}

impl std::error::Error for RoqError {}

#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    pub id: u16,
    pub arg: u16,
    /// Byte range of the body in the file.
    pub start: usize,
    pub len: usize,
}

/// Decoded audio from one chunk: interleaved i16 at [`AUDIO_RATE`].
#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub channels: u16,
    pub samples: Vec<i16>,
}

#[derive(Clone, Copy, Default)]
struct Cell2 {
    y: [u8; 4],
    u: u8,
    v: u8,
}

/// An event produced while walking the file.
pub enum Event {
    /// A new video frame is ready in [`Decoder::rgba`].
    Frame,
    Audio(AudioPacket),
}

/// Streaming decoder over a whole file held in memory.
pub struct Decoder {
    data: Vec<u8>,
    pos: usize,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    cb2: [Cell2; 256],
    cb4: [[u8; 4]; 256],
    /// Current and previous frames as planar 4:4:4 YUV.
    cur: [Vec<u8>; 3],
    prev: [Vec<u8>; 3],
    pub frame_index: u32,
    /// Chunks of unknown type skipped so far.
    pub skipped: u32,
}

impl Decoder {
    pub fn new(data: Vec<u8>) -> Result<Self, RoqError> {
        if data.len() < 8 || u16::from_le_bytes([data[0], data[1]]) != SIG {
            return Err(RoqError::NotRoq);
        }
        let fps = u16::from_le_bytes([data[6], data[7]]) as u32;
        let mut d = Self {
            data,
            pos: 8,
            fps: if fps == 0 { 30 } else { fps },
            width: 0,
            height: 0,
            cb2: [Cell2::default(); 256],
            cb4: [[0; 4]; 256],
            cur: Default::default(),
            prev: Default::default(),
            frame_index: 0,
            skipped: 0,
        };
        // Read ahead to the info chunk so the size is known before the first frame.
        let save = d.pos;
        while let Some(c) = d.read_chunk_header()? {
            d.pos = c.start + c.len;
            if c.id == INFO {
                d.set_info(c)?;
                break;
            }
        }
        d.pos = save;
        Ok(d)
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let data = std::fs::read(path)?;
        Self::new(data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Restarts at the first chunk (looping).
    pub fn rewind(&mut self) {
        self.pos = 8;
        self.frame_index = 0;
        for f in [&mut self.cur, &mut self.prev] {
            f[0].fill(0);
            f[1].fill(128);
            f[2].fill(128);
        }
    }

    fn read_chunk_header(&mut self) -> Result<Option<Chunk>, RoqError> {
        if self.pos + 8 > self.data.len() {
            return Ok(None);
        }
        let b = &self.data[self.pos..self.pos + 8];
        let id = u16::from_le_bytes([b[0], b[1]]);
        let len = u32::from_le_bytes([b[2], b[3], b[4], b[5]]) as usize;
        let arg = u16::from_le_bytes([b[6], b[7]]);
        let start = self.pos + 8;
        if start + len > self.data.len() {
            return Err(RoqError::Truncated);
        }
        self.pos = start + len;
        Ok(Some(Chunk { id, arg, start, len }))
    }

    fn set_info(&mut self, c: Chunk) -> Result<(), RoqError> {
        if c.len < 4 {
            return Err(RoqError::Truncated);
        }
        let b = &self.data[c.start..];
        let w = u16::from_le_bytes([b[0], b[1]]) as u32;
        let h = u16::from_le_bytes([b[2], b[3]]) as u32;
        if w == 0 || h == 0 || w % 16 != 0 || h % 16 != 0 {
            return Err(RoqError::Unsupported(format!("frame size {w}x{h}")));
        }
        if w != self.width || h != self.height {
            self.width = w;
            self.height = h;
            let n = (w * h) as usize;
            // Black.
            self.cur = [vec![0; n], vec![128; n], vec![128; n]];
            self.prev = self.cur.clone();
        }
        Ok(())
    }

    /// Advances to the next frame or audio packet. Returns None at the end of the file.
    pub fn next_event(&mut self) -> Result<Option<Event>, RoqError> {
        while let Some(c) = self.read_chunk_header()? {
            match c.id {
                INFO => self.set_info(c)?,
                QUAD_CODEBOOK => self.read_codebook(c)?,
                QUAD_VQ => {
                    self.decode_vq(c)?;
                    self.frame_index += 1;
                    return Ok(Some(Event::Frame));
                }
                AUDIO_MONO | AUDIO_STEREO => return Ok(Some(Event::Audio(self.decode_audio(c)))),
                JPEG => return Err(RoqError::Unsupported("JPEG keyframe".into())),
                _ => self.skipped += 1,
            }
        }
        Ok(None)
    }

    /// Decodes up to the next video frame (skipping audio). False at the end.
    pub fn next_frame(&mut self) -> Result<bool, RoqError> {
        loop {
            match self.next_event()? {
                Some(Event::Frame) => return Ok(true),
                Some(Event::Audio(_)) => continue,
                None => return Ok(false),
            }
        }
    }

    fn read_codebook(&mut self, c: Chunk) -> Result<(), RoqError> {
        let mut n2 = (c.arg >> 8) as usize;
        let mut n4 = (c.arg & 0xFF) as usize;
        if n2 == 0 {
            n2 = 256;
        }
        if n4 == 0 && n2 * 6 < c.len {
            n4 = 256;
        }
        if n2 * 6 + n4 * 4 > c.len {
            return Err(RoqError::Truncated);
        }
        let b = &self.data[c.start..c.start + c.len];
        for i in 0..n2 {
            let o = i * 6;
            self.cb2[i] = Cell2 { y: [b[o], b[o + 1], b[o + 2], b[o + 3]], u: b[o + 4], v: b[o + 5] };
        }
        let base = n2 * 6;
        for i in 0..n4 {
            let o = base + i * 4;
            self.cb4[i] = [b[o], b[o + 1], b[o + 2], b[o + 3]];
        }
        Ok(())
    }

    fn decode_vq(&mut self, c: Chunk) -> Result<(), RoqError> {
        if self.width == 0 {
            return Err(RoqError::Unsupported("frame before info chunk".into()));
        }
        // The new frame starts as a copy of the previous one ("keep" leaves it untouched).
        std::mem::swap(&mut self.cur, &mut self.prev);
        for p in 0..3 {
            self.cur[p].copy_from_slice(&self.prev[p]);
        }
        let mean_x = (c.arg >> 8) as u8 as i8 as i32;
        let mean_y = (c.arg & 0xFF) as u8 as i8 as i32;
        let end = c.start + c.len;
        let mut r = BitReader { pos: c.start, end, flags: 0, left: 0 };
        let (w, h) = (self.width as i32, self.height as i32);
        let (mut mx0, mut my0) = (0i32, 0i32);
        'outer: while r.pos < end {
            for k in 0..4 {
                let xp = mx0 + (k & 1) * 8;
                let yp = my0 + (k >> 1) * 8;
                let Some(code) = r.code(&self.data) else { break 'outer };
                match code {
                    0 => {}
                    1 => {
                        let Some(b) = r.byte(&self.data) else { break 'outer };
                        let dx = 8 - (b >> 4) as i32 - mean_x;
                        let dy = 8 - (b & 15) as i32 - mean_y;
                        self.motion(xp, yp, dx, dy, 8);
                    }
                    2 => {
                        let Some(b) = r.byte(&self.data) else { break 'outer };
                        let cell = self.cb4[b as usize];
                        for (q, &i2) in cell.iter().enumerate() {
                            let (qx, qy) = ((q as i32 & 1) * 4, (q as i32 >> 1) * 4);
                            self.paint2(xp + qx, yp + qy, self.cb2[i2 as usize], 2);
                        }
                    }
                    _ => {
                        for q in 0..4 {
                            let x = xp + (q & 1) * 4;
                            let y = yp + (q >> 1) * 4;
                            let Some(code) = r.code(&self.data) else { break 'outer };
                            match code {
                                0 => {}
                                1 => {
                                    let Some(b) = r.byte(&self.data) else { break 'outer };
                                    let dx = 8 - (b >> 4) as i32 - mean_x;
                                    let dy = 8 - (b & 15) as i32 - mean_y;
                                    self.motion(x, y, dx, dy, 4);
                                }
                                2 => {
                                    let Some(b) = r.byte(&self.data) else { break 'outer };
                                    let cell = self.cb4[b as usize];
                                    for (s, &i2) in cell.iter().enumerate() {
                                        self.paint2(x + (s as i32 & 1) * 2, y + (s as i32 >> 1) * 2, self.cb2[i2 as usize], 1);
                                    }
                                }
                                _ => {
                                    for s in 0..4 {
                                        let Some(b) = r.byte(&self.data) else { break 'outer };
                                        self.paint2(x + (s & 1) * 2, y + (s >> 1) * 2, self.cb2[b as usize], 1);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            mx0 += 16;
            if mx0 >= w {
                mx0 = 0;
                my0 += 16;
                if my0 >= h {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Paints a 2x2 cell at (x, y), each texel `scale` x `scale` pixels.
    fn paint2(&mut self, x: i32, y: i32, c: Cell2, scale: i32) {
        let w = self.width as i32;
        for j in 0..2 {
            for i in 0..2 {
                let yv = c.y[(j * 2 + i) as usize];
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = x + i * scale + sx;
                        let py = y + j * scale + sy;
                        if px < w && py < self.height as i32 {
                            let o = (py * w + px) as usize;
                            self.cur[0][o] = yv;
                            self.cur[1][o] = c.u;
                            self.cur[2][o] = c.v;
                        }
                    }
                }
            }
        }
    }

    /// Copies an n x n block from the previous frame at (x+dx, y+dy), clamped to the image.
    fn motion(&mut self, x: i32, y: i32, dx: i32, dy: i32, n: i32) {
        let (w, h) = (self.width as i32, self.height as i32);
        for j in 0..n {
            for i in 0..n {
                let (px, py) = (x + i, y + j);
                if px >= w || py >= h {
                    continue;
                }
                let sx = (px + dx).clamp(0, w - 1);
                let sy = (py + dy).clamp(0, h - 1);
                let (d, s) = ((py * w + px) as usize, (sy * w + sx) as usize);
                for p in 0..3 {
                    self.cur[p][d] = self.prev[p][s];
                }
            }
        }
    }

    fn decode_audio(&self, c: Chunk) -> AudioPacket {
        let b = &self.data[c.start..c.start + c.len];
        let delta = |x: u8| -> i32 {
            let x = x as i32;
            if x < 128 { x * x } else { -((x - 128) * (x - 128)) }
        };
        if c.id == AUDIO_MONO {
            let mut p = c.arg as i16 as i32;
            let samples = b
                .iter()
                .map(|&x| {
                    p = (p + delta(x)).clamp(-32768, 32767);
                    p as i16
                })
                .collect();
            AudioPacket { channels: 1, samples }
        } else {
            let mut pl = (c.arg & 0xFF00) as i16 as i32;
            let mut pr = ((c.arg & 0xFF) << 8) as i16 as i32;
            let mut samples = Vec::with_capacity(b.len());
            for pair in b.chunks_exact(2) {
                pl = (pl + delta(pair[0])).clamp(-32768, 32767);
                pr = (pr + delta(pair[1])).clamp(-32768, 32767);
                samples.push(pl as i16);
                samples.push(pr as i16);
            }
            AudioPacket { channels: 2, samples }
        }
    }

    /// The current frame as RGBA8 (BT.601 studio-range YCbCr).
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = vec![255u8; self.cur[0].len() * 4];
        self.write_rgba(&mut out);
        out
    }

    pub fn write_rgba(&self, out: &mut [u8]) {
        for (i, px) in out.chunks_exact_mut(4).enumerate() {
            let [r, g, b] = ycbcr_to_rgb(self.cur[0][i], self.cur[1][i], self.cur[2][i]);
            px[0] = r;
            px[1] = g;
            px[2] = b;
            px[3] = 255;
        }
    }

    /// Lists all chunks (for inspection tools).
    pub fn chunks(data: &[u8]) -> Vec<Chunk> {
        let mut v = Vec::new();
        let mut pos = 8;
        while pos + 8 <= data.len() {
            let id = u16::from_le_bytes([data[pos], data[pos + 1]]);
            let len = u32::from_le_bytes([data[pos + 2], data[pos + 3], data[pos + 4], data[pos + 5]]) as usize;
            let arg = u16::from_le_bytes([data[pos + 6], data[pos + 7]]);
            v.push(Chunk { id, arg, start: pos + 8, len });
            pos += 8 + len;
        }
        v
    }

    /// Frame count and duration in seconds (by counting VQ chunks).
    pub fn frame_count(&self) -> u32 {
        Self::chunks(&self.data).iter().filter(|c| c.id == QUAD_VQ).count() as u32
    }

    /// Decodes every audio chunk of the file up front: interleaved stereo i16 at [`AUDIO_RATE`]
    /// (mono is duplicated). Empty when the movie has no audio track (all POSTAL 2 movies; their
    /// soundtracks are separate Sound objects played alongside).
    pub fn all_audio(&self) -> Vec<i16> {
        let mut out = Vec::new();
        for c in Self::chunks(&self.data) {
            if c.start + c.len > self.data.len() {
                break;
            }
            if c.id == AUDIO_MONO || c.id == AUDIO_STEREO {
                let a = self.decode_audio(c);
                if a.channels == 2 {
                    out.extend_from_slice(&a.samples);
                } else {
                    out.extend(a.samples.iter().flat_map(|&s| [s, s]));
                }
            }
        }
        out
    }
}

pub fn ycbcr_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    // Full-range (JPEG) YCbCr.
    let y = y as f32;
    let cb = cb as f32 - 128.0;
    let cr = cr as f32 - 128.0;
    let r = y + 1.402 * cr;
    let g = y - 0.344_136 * cb - 0.714_136 * cr;
    let b = y + 1.772 * cb;
    [r.round().clamp(0.0, 255.0) as u8, g.round().clamp(0.0, 255.0) as u8, b.round().clamp(0.0, 255.0) as u8]
}

struct BitReader {
    pos: usize,
    end: usize,
    flags: u16,
    left: u8,
}

impl BitReader {
    fn byte(&mut self, d: &[u8]) -> Option<u8> {
        if self.pos >= self.end {
            return None;
        }
        self.pos += 1;
        Some(d[self.pos - 1])
    }

    fn code(&mut self, d: &[u8]) -> Option<u8> {
        if self.left == 0 {
            let lo = self.byte(d)?;
            let hi = self.byte(d)?;
            self.flags = u16::from_le_bytes([lo, hi]);
            self.left = 8;
        }
        self.left -= 1;
        Some(((self.flags >> (self.left * 2)) & 3) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: u16, arg: u16, body: &[u8]) -> Vec<u8> {
        let mut v = id.to_le_bytes().to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(&arg.to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn file(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0x84, 0x10, 0xFF, 0xFF, 0xFF, 0xFF, 30, 0];
        for c in chunks {
            v.extend_from_slice(c);
        }
        v
    }

    #[test]
    fn vq_paint_keep_and_motion() {
        // 16x16 frame; codebook: 2x2 cell 0 = grey 100, cell 1 = Y 200; 4x4 cell 0 = all cell 0,
        // 4x4 cell 1 = all cell 1.
        let mut cb = vec![100, 100, 100, 100, 128, 128, 200, 200, 200, 200, 128, 128];
        cb.extend_from_slice(&[0, 0, 0, 0, 1, 1, 1, 1]);
        let info = chunk(INFO, 0, &[16, 0, 16, 0, 8, 0, 4, 0]);
        let book = chunk(QUAD_CODEBOOK, 0x0202, &cb);
        // Frame 1: all four 8x8 blocks code 2 (paint 4x4 cell, scaled): codes 2,2,2,2 = 0b10101010 in
        // the top byte; remaining codes unused. Indices 0,1,0,1.
        let f1 = chunk(QUAD_VQ, 0, &[0x00, 0xAA, 0, 1, 0, 1]);
        // Frame 2: block 0 motion, blocks 1..3 keep: codes 1,0,0,0 = 0x4000. Motion byte 0x08 gives
        // dx = 8 - 0 = 8, dy = 8 - 8 = 0, so block 0 copies the block to its right (Y 200).
        let f2 = chunk(QUAD_VQ, 0, &[0x00, 0x40, 0x08]);
        let mut d = Decoder::new(file(&[info, book, f1, f2])).unwrap();
        assert_eq!((d.width, d.height), (16, 16));
        assert!(d.next_frame().unwrap());
        assert_eq!(d.cur[0][0], 100);
        assert_eq!(d.cur[0][8], 200);
        assert_eq!(d.cur[0][8 * 16], 100);
        assert_eq!(d.cur[0][8 * 16 + 8], 200);
        assert!(d.next_frame().unwrap());
        assert_eq!(d.cur[0][0], 200, "motion copied the right block");
        assert_eq!(d.cur[0][8], 200, "kept");
        assert_eq!(d.cur[0][8 * 16], 100, "kept");
        assert!(!d.next_frame().unwrap());
    }

    #[test]
    fn split_blocks() {
        let mut cb = Vec::new();
        for y in [10u8, 50, 90, 130] {
            cb.extend_from_slice(&[y, y, y, y, 128, 128]);
        }
        cb.extend_from_slice(&[0, 1, 2, 3]);
        let info = chunk(INFO, 0, &[16, 0, 16, 0, 0, 0, 0, 0]);
        let book = chunk(QUAD_CODEBOOK, 0x0401, &cb);
        // Block 0 split (3); its quadrants: 3 (four 2x2 indices), 2 (4x4 cell 0), 0, 0.
        // Flags word 1: [3, 3, 2, 0, 0, 0, 0, 0] = 0b11_11_10_00_00_00_00_00 = 0xF800.
        // Then remaining blocks 1..3 codes come from the same word (0,0,0).
        let f = chunk(QUAD_VQ, 0, &[0x00, 0xF8, 3, 2, 1, 0, 0]);
        let mut d = Decoder::new(file(&[info, book, f])).unwrap();
        assert!(d.next_frame().unwrap());
        let y = |x: usize, yy: usize| d.cur[0][yy * 16 + x];
        // Quadrant 0 (0..4,0..4): 2x2 cells 3,2,1,0.
        assert_eq!([y(0, 0), y(2, 0), y(0, 2), y(2, 2)], [130, 90, 50, 10]);
        // Quadrant 1 (4..8,0..4): 4x4 cell 0 -> 2x2 cells 0,1,2,3 unscaled.
        assert_eq!([y(4, 0), y(6, 0), y(4, 2), y(6, 2)], [10, 50, 90, 130]);
        // Untouched: black.
        assert_eq!(y(0, 4), 0);
    }

    #[test]
    fn dpcm_audio() {
        let info = chunk(INFO, 0, &[16, 0, 16, 0, 0, 0, 0, 0]);
        let mono = chunk(AUDIO_MONO, 1000u16, &[10, 128 + 20, 0]);
        let stereo = chunk(AUDIO_STEREO, 0x0102, &[1, 129]);
        let mut d = Decoder::new(file(&[info, mono, stereo])).unwrap();
        let Some(Event::Audio(a)) = d.next_event().unwrap() else { panic!() };
        assert_eq!(a.samples, vec![1100, 700, 700]);
        let Some(Event::Audio(a)) = d.next_event().unwrap() else { panic!() };
        assert_eq!(a.channels, 2);
        assert_eq!(a.samples, vec![256 + 1, 512 - 1]);
    }
}
