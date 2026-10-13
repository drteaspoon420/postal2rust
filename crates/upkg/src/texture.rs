//! Texture decoding: mip chain extraction and conversion to RGBA8.
//!
//! The format numbering and mip layout were determined from the data in the shipped .utx files
//! (sizes of each mip blob match the block-compression math for the stored format id).

use crate::package::{ObjRef, Package};
use crate::props::{Properties, begin_object};
use crate::reader::Reader;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TexFormat {
    P8,
    Rgba7,
    Rgb16,
    Dxt1,
    Rgb8,
    Rgba8,
    NoData,
    Dxt3,
    Dxt5,
    L8,
    G16,
    Other(u8),
}

impl TexFormat {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::P8,
            1 => Self::Rgba7,
            2 => Self::Rgb16,
            3 => Self::Dxt1,
            4 => Self::Rgb8,
            5 => Self::Rgba8,
            6 => Self::NoData,
            7 => Self::Dxt3,
            8 => Self::Dxt5,
            9 => Self::L8,
            10 => Self::G16,
            o => Self::Other(o),
        }
    }

    /// Byte size of a w*h image in this format (block formats round up to 4x4 blocks).
    pub fn data_size(self, w: u32, h: u32) -> Option<usize> {
        let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
        let px = w as usize * h as usize;
        Some(match self {
            Self::P8 | Self::L8 => px,
            Self::Rgb16 | Self::G16 => px * 2,
            Self::Rgb8 => px * 3,
            Self::Rgba8 | Self::Rgba7 => px * 4,
            Self::Dxt1 => bw * bh * 8,
            Self::Dxt3 | Self::Dxt5 => bw * bh * 16,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Mip<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct Texture<'a> {
    pub name: String,
    pub format: TexFormat,
    pub width: u32,
    pub height: u32,
    pub masked: bool,
    pub alpha_texture: bool,
    pub palette: Option<Vec<[u8; 4]>>,
    pub mips: Vec<Mip<'a>>,
    pub props: Properties,
}

/// Reads a Palette object's colour table (BGRA in file, returned as RGBA).
pub fn read_palette(pkg: &Package, export: usize) -> Result<Vec<[u8; 4]>> {
    let (mut r, _props) = begin_object(pkg, export)?;
    r.array(|r| {
        let b = r.bytes(4)?;
        Ok([b[2], b[1], b[0], b[3]])
    })
}

pub fn read_texture(pkg: &Package, export: usize) -> Result<Texture<'_>> {
    let (mut r, props) = begin_object(pkg, export)?;
    let format = TexFormat::from_byte(props.byte("Format").unwrap_or(0));
    let width = props.int("USize").unwrap_or(0) as u32;
    let height = props.int("VSize").unwrap_or(0) as u32;
    let palette = match props.object("Palette") {
        Some(ObjRef::Export(p)) => Some(read_palette(pkg, p)?),
        _ => None,
    };
    let mips = read_mips(&mut r)?;
    Ok(Texture {
        name: pkg.name(pkg.exports[export].name).to_string(),
        format,
        width,
        height,
        masked: props.bool("bMasked").unwrap_or(false),
        alpha_texture: props.bool("bAlphaTexture").unwrap_or(false),
        palette,
        mips,
        props,
    })
}

fn read_mips<'a>(r: &mut Reader<'a>) -> Result<Vec<Mip<'a>>> {
    let n = r.count()?;
    let mut mips = Vec::with_capacity(n);
    for _ in 0..n {
        let data = r.lazy_bytes()?;
        let width = r.i32()? as u32;
        let height = r.i32()? as u32;
        let _ubits = r.u8()?;
        let _vbits = r.u8()?;
        mips.push(Mip { width, height, data });
    }
    Ok(mips)
}

impl Texture<'_> {
    /// Decodes a mip level to tightly packed RGBA8.
    pub fn decode_rgba(&self, level: usize) -> Result<(u32, u32, Vec<u8>)> {
        let mip = self.mips.get(level).ok_or(Error::Corrupt("missing mip"))?;
        if mip.data.is_empty() {
            // Seen on animated-texture frames that only chain to the next frame via AnimNext.
            return Err(Error::Unsupported("texture has no pixel data".into()));
        }
        let (w, h) = (mip.width.max(1), mip.height.max(1));
        let need = self.format.data_size(w, h).ok_or_else(|| Error::Unsupported(format!("texture format {:?}", self.format)))?;
        if mip.data.len() < need {
            return Err(Error::Corrupt("mip data shorter than format requires"));
        }
        let px = (w * h) as usize;
        let d = mip.data;
        let mut out = vec![0u8; px * 4];
        match self.format {
            TexFormat::P8 => {
                let pal = self.palette.as_deref().ok_or(Error::Corrupt("P8 texture without palette"))?;
                for i in 0..px {
                    let idx = d[i] as usize;
                    let mut c = pal.get(idx).copied().unwrap_or([255, 0, 255, 255]);
                    // Index 0 is the transparent colour for masked textures.
                    c[3] = if self.masked && idx == 0 { 0 } else { 255 };
                    out[i * 4..i * 4 + 4].copy_from_slice(&c);
                }
            }
            TexFormat::L8 => {
                for i in 0..px {
                    out[i * 4..i * 4 + 4].copy_from_slice(&[d[i], d[i], d[i], 255]);
                }
            }
            TexFormat::Rgba8 | TexFormat::Rgba7 => {
                for i in 0..px {
                    let s = &d[i * 4..i * 4 + 4];
                    out[i * 4..i * 4 + 4].copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                }
            }
            TexFormat::Rgb8 => {
                for i in 0..px {
                    let s = &d[i * 3..i * 3 + 3];
                    out[i * 4..i * 4 + 4].copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            TexFormat::G16 => {
                for i in 0..px {
                    let v = d[i * 2 + 1];
                    out[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            TexFormat::Dxt1 | TexFormat::Dxt3 | TexFormat::Dxt5 => decode_bc(self.format, d, w, h, &mut out),
            f => return Err(Error::Unsupported(format!("texture format {f:?}"))),
        }
        Ok((w, h, out))
    }
}

/// Replaces the alpha of an RGBA8 image (`dst`, `dw` x `dh`) with the alpha of another RGBA8
/// image (`src`, any size, sampled nearest). UE2 Shader Opacity: the opacity texture's alpha
/// is the material alpha regardless of the diffuse's own.
pub fn merge_alpha(dst: &mut [u8], dw: u32, dh: u32, src: &[u8], sw: u32, sh: u32) {
    if sw == 0 || sh == 0 || src.len() < (sw * sh * 4) as usize {
        return;
    }
    for y in 0..dh {
        let sy = ((y as u64 * sh as u64) / dh.max(1) as u64) as u32;
        for x in 0..dw {
            let sx = ((x as u64 * sw as u64) / dw.max(1) as u64) as u32;
            let (di, si) = (((y * dw + x) * 4 + 3) as usize, ((sy * sw + sx) * 4 + 3) as usize);
            if di < dst.len() {
                dst[di] = src[si];
            }
        }
    }
}

/// Decodes raw pixel data of a given format (e.g. a lightmap atlas stored outside a Texture
/// object) to RGBA8.
pub fn decode_raw(format: TexFormat, data: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let need = format.data_size(w, h).ok_or_else(|| Error::Unsupported(format!("texture format {format:?}")))?;
    if data.len() < need {
        return Err(Error::Corrupt("pixel data shorter than format requires"));
    }
    let mut out = vec![0u8; (w * h * 4) as usize];
    match format {
        TexFormat::Dxt1 | TexFormat::Dxt3 | TexFormat::Dxt5 => decode_bc(format, data, w, h, &mut out),
        TexFormat::Rgba8 => {
            for (o, s) in out.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                o.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        f => return Err(Error::Unsupported(format!("texture format {f:?}"))),
    }
    Ok(out)
}

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [((r * 527 + 23) >> 6) as u8, ((g * 259 + 33) >> 6) as u8, ((b * 527 + 23) >> 6) as u8]
}

/// Standard S3TC block decoding (BC1/BC2/BC3), written from the public format specification.
fn decode_bc(format: TexFormat, d: &[u8], w: u32, h: u32, out: &mut [u8]) {
    let bs = if format == TexFormat::Dxt1 { 8 } else { 16 };
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    for by in 0..bh {
        for bx in 0..bw {
            let off = ((by * bw + bx) as usize) * bs;
            let blk = &d[off..off + bs];
            let (alpha_part, color) = if bs == 16 { (&blk[..8], &blk[8..]) } else { (&blk[..0], blk) };
            let c0 = u16::from_le_bytes([color[0], color[1]]);
            let c1 = u16::from_le_bytes([color[2], color[3]]);
            let (p0, p1) = (rgb565(c0), rgb565(c1));
            let mut pal = [[0u8; 4]; 4];
            pal[0] = [p0[0], p0[1], p0[2], 255];
            pal[1] = [p1[0], p1[1], p1[2], 255];
            let mix = |a: u8, b: u8, wa: u32, wb: u32, div: u32| ((a as u32 * wa + b as u32 * wb) / div) as u8;
            if c0 > c1 || format != TexFormat::Dxt1 {
                for k in 0..3 {
                    pal[2][k] = mix(p0[k], p1[k], 2, 1, 3);
                    pal[3][k] = mix(p0[k], p1[k], 1, 2, 3);
                }
                pal[2][3] = 255;
                pal[3][3] = 255;
            } else {
                for k in 0..3 {
                    pal[2][k] = mix(p0[k], p1[k], 1, 1, 2);
                }
                pal[2][3] = 255;
                pal[3] = [0, 0, 0, 0];
            }
            let bits = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
            // Alpha table for BC3.
            let mut a5 = [0u8; 8];
            let mut abits = 0u64;
            if format == TexFormat::Dxt5 {
                let (a0, a1) = (alpha_part[0] as u32, alpha_part[1] as u32);
                a5[0] = a0 as u8;
                a5[1] = a1 as u8;
                if a0 > a1 {
                    for i in 1..7 {
                        a5[i + 1] = (((7 - i as u32) * a0 + i as u32 * a1) / 7) as u8;
                    }
                } else {
                    for i in 1..5 {
                        a5[i + 1] = (((5 - i as u32) * a0 + i as u32 * a1) / 5) as u8;
                    }
                    a5[6] = 0;
                    a5[7] = 255;
                }
                for (i, &b) in alpha_part[2..8].iter().enumerate() {
                    abits |= (b as u64) << (8 * i);
                }
            }
            for py in 0..4 {
                for px in 0..4 {
                    let (x, y) = (bx * 4 + px, by * 4 + py);
                    if x >= w || y >= h {
                        continue;
                    }
                    let i = (py * 4 + px) as usize;
                    let mut c = pal[((bits >> (2 * i)) & 3) as usize];
                    match format {
                        TexFormat::Dxt3 => {
                            let nib = (alpha_part[i / 2] >> ((i % 2) * 4)) & 0xF;
                            c[3] = nib * 17;
                        }
                        TexFormat::Dxt5 => c[3] = a5[((abits >> (3 * i)) & 7) as usize],
                        _ => {}
                    }
                    let o = ((y * w + x) * 4) as usize;
                    out[o..o + 4].copy_from_slice(&c);
                }
            }
        }
    }
}
