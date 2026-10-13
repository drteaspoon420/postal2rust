//! Bitmap fonts as the Canvas sees them: glyph rectangles on texture pages, plus the
//! character remap table some fonts carry.

use std::collections::HashMap;
use std::sync::Arc;
use upkg::font::{FontChar, read_font};
use upkg::library::{Library, Obj};

pub struct CanvasFont {
    pub name: String,
    pub chars: Vec<FontChar>,
    /// Glyph pages (textures). `None` when a page reference did not resolve: glyphs on it are skipped.
    pub pages: Vec<Option<Obj>>,
    /// Extra horizontal spacing added after every character (0 in every shipped font).
    pub kerning: i32,
    remap: Option<HashMap<u16, u16>>,
}

impl CanvasFont {
    pub fn load(lib: &Library, obj: &Obj) -> Option<Arc<Self>> {
        let f = read_font(&obj.pkg, obj.export).ok()?;
        let pages = f.pages.iter().map(|p| lib.resolve(&obj.pkg, *p).filter(|o| o.class().eq_ignore_ascii_case("Texture"))).collect();
        let remap = f.is_remapped.then(|| f.remap.iter().copied().collect());
        Some(Arc::new(Self { name: f.name, chars: f.chars, pages, kerning: f.kerning, remap }))
    }

    /// A font without pages, for layout tests and measurement.
    pub fn synthetic(name: &str, chars: Vec<FontChar>, kerning: i32) -> Self {
        Self { name: name.into(), chars, pages: vec![], kerning, remap: None }
    }

    /// Character code after the font's remap table. Remapped fonts draw unmapped characters as a space.
    pub fn remap(&self, c: char) -> usize {
        let code = c as u32 as u16;
        match &self.remap {
            Some(m) => m.get(&code).copied().unwrap_or(b' ' as u16) as usize,
            None => code as usize,
        }
    }

    /// (width, height) of a character in texels; (0, 0) when it is past the glyph table.
    pub fn char_size(&self, c: char) -> (i32, i32) {
        match self.chars.get(self.remap(c)) {
            Some(g) => (g.u_size, g.v_size),
            None => (0, 0),
        }
    }

    /// Tallest glyph, a convenient line height.
    pub fn height(&self) -> i32 {
        self.chars.iter().map(|c| c.v_size).max().unwrap_or(0)
    }
}
