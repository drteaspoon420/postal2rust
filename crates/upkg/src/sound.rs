//! Sound objects and their embedded audio files.
//!
//! Layout determined from the shipped .uax packages and maps: after the tagged property list a
//! `Sound` export stores a name (the source file type, e.g. "WAV") followed by a lazy byte array
//! holding the complete original file. In POSTAL 2 that file is almost always a RIFF/WAVE.
//! `ProceduralSound` and `SoundGroup` are data-only subclasses: their behaviour lives entirely in
//! properties (base sound, pitch/volume randomisation, sub-sound lists) and their file payload is empty.
//!
//! Decoders here (PCM, IMA ADPCM, Microsoft ADPCM) were written from the public RIFF/WAVE and
//! ADPCM specifications. Compressed containers this crate does not decode (Ogg Vorbis, MP3) are
//! identified so callers can hand the raw bytes to a dedicated decoder.

use crate::package::{ObjRef, Package};
use crate::props::{Properties, Value, begin_object};
use crate::reader::Reader;
use crate::{Error, Result};

/// Sound-family classes with a meaning to the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundKind {
    /// A plain sound with an embedded audio file.
    Sound,
    /// Plays `BaseSound` with randomised pitch/volume.
    Procedural,
    /// Picks one of `Sounds` (optionally weighted by `Probabilities`).
    Group,
    /// Any other subclass (stored by class name in `Sound::class`).
    Other,
}

impl SoundKind {
    pub fn from_class(class: &str) -> Self {
        match class.to_ascii_lowercase().as_str() {
            "sound" => Self::Sound,
            "proceduralsound" => Self::Procedural,
            "soundgroup" => Self::Group,
            _ => Self::Other,
        }
    }

    /// True for class names this module knows how to read.
    pub fn is_sound_class(class: &str) -> bool {
        matches!(class.to_ascii_lowercase().as_str(), "sound" | "proceduralsound" | "soundgroup")
    }
}

/// Container of an embedded audio file, detected from its magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Container {
    Wav,
    Ogg,
    Mp3,
    Empty,
    Unknown,
}

pub fn detect_container(d: &[u8]) -> Container {
    if d.is_empty() {
        Container::Empty
    } else if d.len() >= 12 && &d[0..4] == b"RIFF" && &d[8..12] == b"WAVE" {
        Container::Wav
    } else if d.starts_with(b"OggS") {
        Container::Ogg
    } else if d.starts_with(b"ID3") || (d.len() >= 2 && d[0] == 0xFF && d[1] & 0xE0 == 0xE0) {
        Container::Mp3
    } else {
        Container::Unknown
    }
}

#[derive(Debug, Clone)]
pub struct Sound<'a> {
    pub name: String,
    pub class: String,
    pub kind: SoundKind,
    /// File type name stored with the object ("WAV" for every shipped sound so far).
    pub file_type: String,
    /// The embedded audio file, borrowed from the package buffer.
    pub data: &'a [u8],
    pub props: Properties,
}

impl Sound<'_> {
    pub fn container(&self) -> Container {
        detect_container(self.data)
    }

    /// `ProceduralSound.BaseSound`.
    pub fn base_sound(&self) -> Option<ObjRef> {
        self.props.object("BaseSound").filter(|r| *r != ObjRef::Null)
    }

    /// `SoundGroup.Sounds` (a dynamic array of object references).
    pub fn group_sounds(&self) -> Vec<ObjRef> {
        match self.props.get("Sounds") {
            Some(Value::Array { raw }) => {
                let mut r = Reader::new(raw);
                r.array(|r| Ok(ObjRef::from_raw(r.compact()?))).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// `SoundGroup.Probabilities` (dynamic array of floats), empty when unweighted.
    pub fn group_probabilities(&self) -> Vec<f32> {
        match self.props.get("Probabilities") {
            Some(Value::Array { raw }) => {
                let mut r = Reader::new(raw);
                r.array(|r| r.f32()).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    pub fn decode_pcm(&self) -> Result<Pcm> {
        decode_pcm(self.data)
    }
}

pub fn read_sound(pkg: &Package, export: usize) -> Result<Sound<'_>> {
    let (mut r, props) = begin_object(pkg, export)?;
    let class = pkg.export_class(export).to_string();
    let e = &pkg.exports[export];
    let end = e.offset + e.size;
    let (file_type, data) = if r.pos() < end {
        let ft = r.compact()?;
        let ft = pkg.names.get(ft as usize).cloned().ok_or(Error::Corrupt("sound file type name"))?;
        let data = r.lazy_bytes()?;
        (ft, data)
    } else {
        (String::new(), &[][..])
    };
    Ok(Sound {
        name: pkg.name(e.name).to_string(),
        kind: SoundKind::from_class(&class),
        class,
        file_type,
        data,
        props,
    })
}

/// Indices of every sound-family export in a package.
pub fn sound_exports(pkg: &Package) -> Vec<usize> {
    (0..pkg.exports.len()).filter(|&i| SoundKind::is_sound_class(pkg.export_class(i))).collect()
}

// ---------------------------------------------------------------------------------------------
// RIFF/WAVE

/// WAVE `fmt ` format tags seen or anticipated.
pub mod codec {
    pub const PCM: u16 = 0x0001;
    pub const MS_ADPCM: u16 = 0x0002;
    pub const IEEE_FLOAT: u16 = 0x0003;
    pub const IMA_ADPCM: u16 = 0x0011;
    pub const MP3: u16 = 0x0055;
    pub const EXTENSIBLE: u16 = 0xFFFE;

    pub fn name(tag: u16) -> &'static str {
        match tag {
            PCM => "PCM",
            MS_ADPCM => "MS-ADPCM",
            IEEE_FLOAT => "float",
            IMA_ADPCM => "IMA-ADPCM",
            MP3 => "MP3",
            EXTENSIBLE => "extensible",
            _ => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WavInfo<'a> {
    pub format_tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    /// Bytes after the 16-byte basic `fmt ` body (codec-specific, e.g. ADPCM coefficients).
    pub fmt_extra: &'a [u8],
    pub data: &'a [u8],
    /// Loop points in sample frames from a `smpl` chunk (start, end inclusive).
    pub loops: Vec<(u32, u32)>,
}

/// Parses the chunks of a RIFF/WAVE file.
pub fn parse_wav(d: &[u8]) -> Result<WavInfo<'_>> {
    if detect_container(d) != Container::Wav {
        return Err(Error::Unsupported("not a RIFF/WAVE file".into()));
    }
    let mut pos = 12;
    let mut fmt: Option<&[u8]> = None;
    let mut data: Option<&[u8]> = None;
    let mut loops = Vec::new();
    while pos + 8 <= d.len() {
        let id = &d[pos..pos + 4];
        let len = u32::from_le_bytes(d[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_start = pos + 8;
        // Tolerate a truncated final chunk (some tools write a data length past EOF).
        let body = &d[body_start..(body_start.saturating_add(len)).min(d.len())];
        match id {
            b"fmt " => fmt = Some(body),
            b"data" => data = Some(body),
            b"smpl" if body.len() >= 36 => {
                let n = u32::from_le_bytes(body[28..32].try_into().unwrap()) as usize;
                for k in 0..n {
                    let o = 36 + k * 24;
                    if o + 24 > body.len() {
                        break;
                    }
                    let s = u32::from_le_bytes(body[o + 8..o + 12].try_into().unwrap());
                    let e = u32::from_le_bytes(body[o + 12..o + 16].try_into().unwrap());
                    loops.push((s, e));
                }
            }
            _ => {}
        }
        pos = body_start.saturating_add(len).saturating_add(len & 1);
    }
    let fmt = fmt.ok_or(Error::Corrupt("WAVE without fmt chunk"))?;
    if fmt.len() < 16 {
        return Err(Error::Corrupt("WAVE fmt chunk too short"));
    }
    let u16_at = |o: usize| u16::from_le_bytes([fmt[o], fmt[o + 1]]);
    let mut format_tag = u16_at(0);
    let extra = if fmt.len() > 18 { &fmt[18..] } else { &[][..] };
    if format_tag == codec::EXTENSIBLE && extra.len() >= 8 {
        // WAVEFORMATEXTENSIBLE: the real tag is the first two bytes of the sub-format GUID.
        format_tag = u16::from_le_bytes([extra[6], extra[7]]);
    }
    Ok(WavInfo {
        format_tag,
        channels: u16_at(2),
        sample_rate: u32::from_le_bytes(fmt[4..8].try_into().unwrap()),
        block_align: u16_at(12),
        bits_per_sample: u16_at(14),
        fmt_extra: extra,
        data: data.ok_or(Error::Corrupt("WAVE without data chunk"))?,
        loops,
    })
}

// ---------------------------------------------------------------------------------------------
// PCM output

/// Decoded audio: interleaved signed 16-bit samples.
#[derive(Debug, Clone, Default)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
    /// Loop region in frames, if the source file declared one.
    pub loop_frames: Option<(u32, u32)>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }

    /// Serializes as a canonical 16-bit PCM RIFF/WAVE file.
    pub fn to_wav(&self) -> Vec<u8> {
        let data_len = self.samples.len() * 2;
        let mut o = Vec::with_capacity(44 + data_len);
        let ch = self.channels.max(1);
        o.extend_from_slice(b"RIFF");
        o.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        o.extend_from_slice(b"WAVEfmt ");
        o.extend_from_slice(&16u32.to_le_bytes());
        o.extend_from_slice(&codec::PCM.to_le_bytes());
        o.extend_from_slice(&ch.to_le_bytes());
        o.extend_from_slice(&self.sample_rate.to_le_bytes());
        o.extend_from_slice(&(self.sample_rate * ch as u32 * 2).to_le_bytes());
        o.extend_from_slice(&(ch * 2).to_le_bytes());
        o.extend_from_slice(&16u16.to_le_bytes());
        o.extend_from_slice(b"data");
        o.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in &self.samples {
            o.extend_from_slice(&s.to_le_bytes());
        }
        o
    }

    /// Samples converted to f32 in [-1, 1].
    pub fn to_f32(&self) -> Vec<f32> {
        self.samples.iter().map(|&s| s as f32 / 32768.0).collect()
    }
}

/// Decodes an embedded audio file (RIFF/WAVE with PCM, IMA ADPCM or MS ADPCM) to 16-bit PCM.
/// Ogg Vorbis and MP3 payloads return `Error::Unsupported` naming the container.
pub fn decode_pcm(file: &[u8]) -> Result<Pcm> {
    match detect_container(file) {
        Container::Wav => {}
        c => return Err(Error::Unsupported(format!("container {c:?}"))),
    }
    let w = parse_wav(file)?;
    let ch = w.channels;
    if ch == 0 || ch > 8 {
        return Err(Error::Corrupt("WAVE channel count"));
    }
    let samples = match (w.format_tag, w.bits_per_sample) {
        (codec::PCM, 8) => w.data.iter().map(|&b| ((b as i16) - 128) << 8).collect(),
        (codec::PCM, 16) => w.data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
        (codec::PCM, 24) => w.data.chunks_exact(3).map(|c| i16::from_le_bytes([c[1], c[2]])).collect(),
        (codec::PCM, 32) => w.data.chunks_exact(4).map(|c| i16::from_le_bytes([c[2], c[3]])).collect(),
        (codec::IEEE_FLOAT, 32) => w
            .data
            .chunks_exact(4)
            .map(|c| (f32::from_le_bytes(c.try_into().unwrap()).clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect(),
        (codec::IMA_ADPCM, 4) => decode_ima_adpcm(w.data, ch as usize, w.block_align as usize)?,
        (codec::MS_ADPCM, 4) => decode_ms_adpcm(w.data, ch as usize, w.block_align as usize, w.fmt_extra)?,
        (tag, bits) => return Err(Error::Unsupported(format!("WAVE codec {} ({tag:#06x}) {bits}-bit", codec::name(tag)))),
    };
    Ok(Pcm { sample_rate: w.sample_rate, channels: ch, samples, loop_frames: w.loops.first().copied() })
}

// ---------------------------------------------------------------------------------------------
// IMA ADPCM (WAVE format 0x11), from the public IMA/DVI ADPCM specification.

const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282,
    1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

struct ImaState {
    pred: i32,
    index: i32,
}

impl ImaState {
    fn step(&mut self, nib: u8) -> i16 {
        let step = IMA_STEPS[self.index as usize];
        let mut diff = step >> 3;
        if nib & 1 != 0 {
            diff += step >> 2;
        }
        if nib & 2 != 0 {
            diff += step >> 1;
        }
        if nib & 4 != 0 {
            diff += step;
        }
        if nib & 8 != 0 {
            diff = -diff;
        }
        self.pred = (self.pred + diff).clamp(-32768, 32767);
        self.index = (self.index + IMA_INDEX[nib as usize]).clamp(0, 88);
        self.pred as i16
    }
}

pub fn decode_ima_adpcm(data: &[u8], ch: usize, block_align: usize) -> Result<Vec<i16>> {
    if block_align < 4 * ch || (block_align - 4 * ch) % (4 * ch) != 0 {
        return Err(Error::Corrupt("IMA ADPCM block alignment"));
    }
    let per_block = (block_align - 4 * ch) * 2 / ch + 1;
    let mut out = Vec::with_capacity(data.len() / block_align * per_block * ch + per_block * ch);
    for block in data.chunks(block_align) {
        if block.len() < 4 * ch {
            break;
        }
        let mut st: Vec<ImaState> = (0..ch)
            .map(|c| ImaState { pred: i16::from_le_bytes([block[c * 4], block[c * 4 + 1]]) as i32, index: (block[c * 4 + 2] as i32).clamp(0, 88) })
            .collect();
        // A short final block decodes only the whole 4-byte groups it contains.
        let groups = (block.len() - 4 * ch) / (4 * ch);
        let frames = 1 + groups * 8;
        let base = out.len();
        out.resize(base + frames * ch, 0);
        for c in 0..ch {
            out[base + c] = st[c].pred as i16;
        }
        let body = &block[4 * ch..];
        for g in 0..groups {
            for c in 0..ch {
                let word = &body[(g * ch + c) * 4..(g * ch + c) * 4 + 4];
                for (k, &b) in word.iter().enumerate() {
                    for (h, nib) in [b & 0x0F, b >> 4].into_iter().enumerate() {
                        let frame = 1 + g * 8 + k * 2 + h;
                        out[base + frame * ch + c] = st[c].step(nib);
                    }
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Microsoft ADPCM (WAVE format 0x02), from the public Microsoft ADPCM specification.

const MS_ADAPT: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];
const MS_DEFAULT_COEFS: [(i32, i32); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];

pub fn decode_ms_adpcm(data: &[u8], ch: usize, block_align: usize, extra: &[u8]) -> Result<Vec<i16>> {
    let hdr = 7 * ch;
    if block_align <= hdr {
        return Err(Error::Corrupt("MS ADPCM block alignment"));
    }
    let mut coefs: Vec<(i32, i32)> = MS_DEFAULT_COEFS.to_vec();
    // extra: samplesPerBlock u16, numCoef u16, then numCoef pairs of i16.
    if extra.len() >= 4 {
        let n = u16::from_le_bytes([extra[2], extra[3]]) as usize;
        if n > 0 && extra.len() >= 4 + n * 4 {
            coefs = (0..n)
                .map(|i| {
                    let o = 4 + i * 4;
                    (i16::from_le_bytes([extra[o], extra[o + 1]]) as i32, i16::from_le_bytes([extra[o + 2], extra[o + 3]]) as i32)
                })
                .collect();
        }
    }
    let mut out = Vec::new();
    for block in data.chunks(block_align) {
        if block.len() < hdr {
            break;
        }
        let rd = |o: usize| i16::from_le_bytes([block[o], block[o + 1]]) as i32;
        let mut c1 = vec![0i32; ch];
        let mut c2 = vec![0i32; ch];
        let mut delta = vec![0i32; ch];
        let mut s1 = vec![0i32; ch];
        let mut s2 = vec![0i32; ch];
        for c in 0..ch {
            let p = (block[c] as usize).min(coefs.len() - 1);
            (c1[c], c2[c]) = coefs[p];
            delta[c] = rd(ch + c * 2);
            s1[c] = rd(3 * ch + c * 2);
            s2[c] = rd(5 * ch + c * 2);
        }
        // Header samples: the older one (sample2) comes first.
        for c in 0..ch {
            out.push(s2[c] as i16);
        }
        for c in 0..ch {
            out.push(s1[c] as i16);
        }
        let mut c = 0usize;
        for &b in &block[hdr..] {
            for nib in [b >> 4, b & 0x0F] {
                let signed = if nib & 8 != 0 { nib as i32 - 16 } else { nib as i32 };
                let pred = (s1[c] * c1[c] + s2[c] * c2[c]) >> 8;
                let v = (pred + signed * delta[c]).clamp(-32768, 32767);
                s2[c] = s1[c];
                s1[c] = v;
                delta[c] = ((MS_ADAPT[nib as usize] * delta[c]) >> 8).max(16);
                out.push(v as i16);
                c = (c + 1) % ch;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_roundtrip() {
        let pcm = Pcm { sample_rate: 22050, channels: 2, samples: vec![0, 1, -1, 32767, -32768, 5], loop_frames: None };
        let back = decode_pcm(&pcm.to_wav()).unwrap();
        assert_eq!(back.samples, pcm.samples);
        assert_eq!(back.sample_rate, 22050);
        assert_eq!(back.channels, 2);
    }

    #[test]
    fn ima_silence_block() {
        // Mono block: predictor 0, index 0, then 4 bytes of zero nibbles -> slowly ramping tiny values.
        let block = [0u8, 0, 0, 0, 0, 0, 0, 0];
        let s = decode_ima_adpcm(&block, 1, 8).unwrap();
        assert_eq!(s.len(), 9);
        assert_eq!(s[0], 0);
        assert!(s.iter().all(|v| v.abs() < 16));
    }
}
