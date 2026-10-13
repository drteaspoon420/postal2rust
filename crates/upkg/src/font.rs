//! Font objects: bitmap glyph atlases used by the Canvas for HUD and menu text.
//!
//! Layout determined by requiring every Font export in the shipped packages to parse to its
//! exact export size (see `examples/ui_scan.rs`). After the (empty) property list a v118 /
//! licensee 2 Font stores:
//!
//! - `TArray<FFontCharacter>` : per character `StartU, StartV, USize, VSize` (i32) and a texture
//!   page index (u8). The array is indexed by the (UCS-2) character code.
//! - `TArray<UTexture*>` : the glyph pages (compact object references).
//! - `i32` : an extra licensee field (observed as the font's "Kerning" spacing in pixels).
//! - `TMap<u16, u16>` character remap followed by an `IsRemapped` bool (i32). When remapped,
//!   a character code is looked up in the map first, and unmapped characters draw as a space.

use crate::package::{ObjRef, Package};
use crate::props::begin_object;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FontChar {
    pub start_u: i32,
    pub start_v: i32,
    pub u_size: i32,
    pub v_size: i32,
    pub page: u8,
}

#[derive(Debug, Clone)]
pub struct Font {
    pub name: String,
    pub chars: Vec<FontChar>,
    pub pages: Vec<ObjRef>,
    /// Licensee extension: extra horizontal spacing between glyphs, in texels.
    pub kerning: i32,
    pub remap: Vec<(u16, u16)>,
    pub is_remapped: bool,
}

impl Font {
    /// Maps a character to the index into `chars`, applying the remap table when present.
    pub fn char_index(&self, c: char) -> usize {
        let code = c as u32;
        if self.is_remapped {
            let code = code as u16;
            return self.remap.iter().find(|(k, _)| *k == code).map(|(_, v)| *v as usize).unwrap_or(' ' as usize);
        }
        code as usize
    }

    /// Glyph for a character, falling back to nothing for codes past the table.
    pub fn glyph(&self, c: char) -> Option<&FontChar> {
        self.chars.get(self.char_index(c))
    }

    /// Height of the font in texels: the tallest glyph.
    pub fn max_height(&self) -> i32 {
        self.chars.iter().map(|c| c.v_size).max().unwrap_or(0)
    }
}

pub fn read_font(pkg: &Package, export: usize) -> Result<Font> {
    let (mut r, _props) = begin_object(pkg, export)?;
    let chars = r.array(|r| {
        Ok(FontChar { start_u: r.i32()?, start_v: r.i32()?, u_size: r.i32()?, v_size: r.i32()?, page: r.u8()? })
    })?;
    let pages = r.array(|r| Ok(ObjRef::from_raw(r.compact()?)))?;
    let kerning = r.i32()?;
    let remap = r.array(|r| Ok((r.u16()?, r.u16()?)))?;
    let is_remapped = r.i32()? != 0;
    let e = &pkg.exports[export];
    if r.pos() != e.offset + e.size {
        return Err(Error::Corrupt("font did not end at its export size"));
    }
    Ok(Font { name: pkg.name(e.name).to_string(), chars, pages, kerning, remap, is_remapped })
}
