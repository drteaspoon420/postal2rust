//! Little-endian cursor over a package's bytes with the Unreal Engine 2 primitive encodings
//! (compact indices, FStrings, lazy/trans arrays).

use crate::{Error, Result};

#[derive(Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Eof(self.pos))?;
        if end > self.data.len() {
            return Err(Error::Eof(self.pos));
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    #[inline]
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    #[inline]
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    #[inline]
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    #[inline]
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    #[inline]
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }

    #[inline]
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    pub fn vec3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }

    /// Variable-length signed integer: first byte holds sign (0x80), continuation (0x40) and
    /// 6 value bits; following bytes hold a continuation bit (0x80) and 7 value bits.
    pub fn compact(&mut self) -> Result<i32> {
        let start = self.pos;
        let b0 = self.u8()?;
        let neg = b0 & 0x80 != 0;
        let mut v: i64 = (b0 & 0x3F) as i64;
        if b0 & 0x40 != 0 {
            let mut shift = 6;
            loop {
                let b = self.u8()?;
                v |= ((b & 0x7F) as i64) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
                if shift > 34 {
                    return Err(Error::BadCompact(start));
                }
            }
        }
        let v = if neg { -v } else { v };
        i32::try_from(v).map_err(|_| Error::BadCompact(start))
    }

    /// Non-negative compact used as an element count; rejects absurd values early.
    pub fn count(&mut self) -> Result<usize> {
        let at = self.pos;
        let n = self.compact()?;
        if n < 0 || n as usize > self.data.len() {
            return Err(Error::BadCount(at, n as i64));
        }
        Ok(n as usize)
    }

    /// FString: compact length including terminator; positive = Latin-1, negative = UTF-16LE.
    pub fn fstring(&mut self) -> Result<String> {
        let at = self.pos;
        let len = self.compact()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            let b = self.bytes(len as usize)?;
            let body = &b[..b.len() - 1];
            Ok(body.iter().map(|&c| c as char).collect())
        } else {
            let n = (-(len as i64)) as usize;
            if n * 2 > self.remaining() {
                return Err(Error::BadCount(at, len as i64));
            }
            let b = self.bytes(n * 2)?;
            let units: Vec<u16> = b.chunks_exact(2).take(n - 1).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(String::from_utf16_lossy(&units))
        }
    }

    /// TArray<T>: compact count followed by elements.
    pub fn array<T>(&mut self, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let n = self.count()?;
        let mut v = Vec::with_capacity(n.min(1 << 20));
        for _ in 0..n {
            v.push(f(self)?);
        }
        Ok(v)
    }

    /// TLazyArray<T> (package version >= 63): i32 absolute offset of the data end, then a TArray.
    pub fn lazy_array<T>(&mut self, f: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let _end = self.i32()?;
        self.array(f)
    }

    /// TLazyArray<BYTE> returned as a borrowed slice (no copy).
    pub fn lazy_bytes(&mut self) -> Result<&'a [u8]> {
        let _end = self.i32()?;
        let n = self.count()?;
        self.bytes(n)
    }
}
