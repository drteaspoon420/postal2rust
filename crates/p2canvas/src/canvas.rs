//! The Canvas: UE2's immediate-mode 2D drawing state and operations.
//!
//! Field names and semantics follow the script-visible `Engine.Canvas` class so VM natives can
//! map 1:1 onto this struct: script reads/writes `OrgX`, `CurX`, `DrawColor`, ... directly, and the
//! natives (`DrawTile` 466, `DrawText` 465, `StrLen` 464, `TextSize` 470, `DrawTileClipped` 468,
//! `DrawTextClipped` 469, `DrawActor` 467) call the methods below.
//!
//! Behaviour notes (observed from the shipped engine and scripts):
//! - `DrawTile` draws at (OrgX+CurX, OrgY+CurY), then advances `CurX += XL + SpaceX` and grows `CurYL`.
//!   Nothing is drawn for `Style == STY_None` or a fully transparent `DrawColor` (alpha 0).
//! - `DrawText` word-wraps against `ClipX`. Each line is drawn at `CurX` (or centred in `ClipX` when
//!   `bCenter`), after which `CurX` resets to 0 and `CurY` moves down by the line height. At the end
//!   `CurX += widest line`, `CurYL = max(CurYL, total height)`, and with `CR` the cursor moves to the
//!   start of the next row (`CurY += CurYL`). Text after a line feed is not printed.
//! - Character advance is `(glyph width + SpaceX + Font.Kerning) * FontScaleX`, truncated to an
//!   integer per character. Line height is `glyph height + SpaceY` scaled by `FontScaleY`.
//! - `TextSize` measures one unwrapped line (the last character contributes only its width), skipping
//!   `ESC` colour codes (ESC + 3 bytes). `StrLen` measures with wrapping and leaves the cursor unchanged.
//! - `DrawTextClipped` draws one unwrapped line at the cursor, clipped to `ClipX/ClipY`, without moving
//!   the cursor. With `bCheckHotKey`, `&x` draws `x` underlined and `&&` draws `&`.

use crate::font::CanvasFont;
use std::sync::Arc;
use upkg::library::Obj;

/// `Actor.ERenderStyle` values used by `Canvas.Style`.
pub mod style {
    pub const NONE: u8 = 0;
    pub const NORMAL: u8 = 1;
    pub const MASKED: u8 = 2;
    pub const TRANSLUCENT: u8 = 3;
    pub const MODULATED: u8 = 4;
    pub const ALPHA: u8 = 5;
    pub const PARTICLE: u8 = 6;
}

/// Colour as script sees it (R, G, B, A bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba(pub [u8; 4]);

impl Rgba {
    pub const WHITE: Rgba = Rgba([255, 255, 255, 255]);
    pub fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba([r, g, b, a])
    }
}

/// One textured quad queued for rendering, in screen pixels (origin top-left, Y down).
#[derive(Clone)]
pub struct DrawItem {
    pub material: Obj,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Texture coordinates normalised by the material's size (U/USize ...), before modifiers.
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    pub color: Rgba,
    pub style: u8,
    pub no_smooth: bool,
    pub z: f32,
}

/// Supplies material sizes (USize/VSize after following modifiers) to the canvas.
pub trait MaterialSizer {
    fn material_size(&self, mat: &Obj) -> (i32, i32);
}

/// A sizer for tests: every material is 256x256.
pub struct FixedSizer(pub i32, pub i32);
impl MaterialSizer for FixedSizer {
    fn material_size(&self, _mat: &Obj) -> (i32, i32) {
        (self.0, self.1)
    }
}

#[derive(Clone)]
pub struct Canvas {
    pub font: Option<Arc<CanvasFont>>,
    pub font_scale_x: f32,
    pub font_scale_y: f32,
    pub space_x: f32,
    pub space_y: f32,
    pub org_x: f32,
    pub org_y: f32,
    pub clip_x: f32,
    pub clip_y: f32,
    pub cur_x: f32,
    pub cur_y: f32,
    pub z: f32,
    pub style: u8,
    pub cur_yl: f32,
    pub draw_color: Rgba,
    pub b_center: bool,
    pub b_no_smooth: bool,
    /// Viewport size in pixels (script `SizeX`/`SizeY`).
    pub size_x: i32,
    pub size_y: i32,
    /// Seconds used to animate materials (the level's TimeSeconds).
    pub time: f32,
    pub items: Vec<DrawItem>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new(640, 480)
    }
}

impl Canvas {
    /// A canvas with the class defaults (`Style=STY_Normal`, `DrawColor=(127,127,127,255)`,
    /// font scale 1, `Z=1`) covering a `w` x `h` viewport.
    pub fn new(w: i32, h: i32) -> Self {
        Self {
            font: None,
            font_scale_x: 1.0,
            font_scale_y: 1.0,
            space_x: 0.0,
            space_y: 0.0,
            org_x: 0.0,
            org_y: 0.0,
            clip_x: w as f32,
            clip_y: h as f32,
            cur_x: 0.0,
            cur_y: 0.0,
            z: 1.0,
            style: style::NORMAL,
            cur_yl: 0.0,
            draw_color: Rgba([127, 127, 127, 255]),
            b_center: false,
            b_no_smooth: false,
            size_x: w,
            size_y: h,
            time: 0.0,
            items: Vec::new(),
        }
    }

    /// Starts a frame: the engine re-initialises the canvas for the viewport before HUD drawing.
    pub fn begin_frame(&mut self, w: i32, h: i32, time: f32) {
        let font = self.font.take();
        *self = Self { font, time, ..Self::new(w, h) };
    }

    /// Script `event Reset()`: restores defaults (but keeps the clip and viewport size).
    pub fn reset(&mut self, default_font: Option<Arc<CanvasFont>>) {
        let d = Self::new(self.size_x, self.size_y);
        self.font = default_font;
        self.font_scale_x = d.font_scale_x;
        self.font_scale_y = d.font_scale_y;
        self.space_x = d.space_x;
        self.space_y = d.space_y;
        self.org_x = d.org_x;
        self.org_y = d.org_y;
        self.cur_x = d.cur_x;
        self.cur_y = d.cur_y;
        self.style = d.style;
        self.draw_color = d.draw_color;
        self.cur_yl = d.cur_yl;
        self.b_center = false;
        self.b_no_smooth = false;
        self.z = 1.0;
    }

    pub fn set_pos(&mut self, x: f32, y: f32) {
        self.cur_x = x;
        self.cur_y = y;
    }

    pub fn set_origin(&mut self, x: f32, y: f32) {
        self.org_x = x;
        self.org_y = y;
    }

    pub fn set_clip(&mut self, x: f32, y: f32) {
        self.clip_x = x;
        self.clip_y = y;
    }

    /// Script `SetDrawColor`: an alpha of 0 means opaque.
    pub fn set_draw_color(&mut self, r: u8, g: u8, b: u8, a: u8) {
        self.draw_color = Rgba([r, g, b, if a == 0 { 255 } else { a }]);
    }

    fn push_tile(&mut self, mat: &Obj, size: (i32, i32), x: f32, y: f32, xl: f32, yl: f32, u: f32, v: f32, ul: f32, vl: f32) {
        if self.draw_color.0[3] == 0 {
            return;
        }
        let (us, vs) = (size.0.max(1) as f32, size.1.max(1) as f32);
        self.items.push(DrawItem {
            material: mat.clone(),
            x,
            y,
            w: xl,
            h: yl,
            u0: u / us,
            v0: v / vs,
            u1: (u + ul) / us,
            v1: (v + vl) / vs,
            color: self.draw_color,
            style: self.style,
            no_smooth: self.b_no_smooth,
            z: self.z,
        });
    }

    /// Native 466 `DrawTile(Mat, XL, YL, U, V, UL, VL)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_tile(&mut self, sizer: &dyn MaterialSizer, mat: Option<&Obj>, xl: f32, yl: f32, u: f32, v: f32, ul: f32, vl: f32) {
        let Some(mat) = mat else { return };
        if self.style != style::NONE {
            let size = sizer.material_size(mat);
            self.push_tile(mat, size, self.org_x + self.cur_x, self.org_y + self.cur_y, xl, yl, u, v, ul, vl);
        }
        self.cur_x += xl + self.space_x;
        self.cur_yl = self.cur_yl.max(yl);
    }

    /// Native 468 `DrawTileClipped`: like DrawTile but trimmed (with matching UVs) to `0..ClipX`, `0..ClipY`
    /// relative to the origin.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_tile_clipped(&mut self, sizer: &dyn MaterialSizer, mat: Option<&Obj>, mut xl: f32, mut yl: f32, mut u: f32, mut v: f32, mut ul: f32, mut vl: f32) {
        let Some(mat) = mat else { return };
        if self.cur_x < 0.0 {
            let c = self.cur_x * ul / xl;
            u -= c;
            ul += c;
            xl += self.cur_x;
            self.cur_x = 0.0;
        }
        if self.cur_y < 0.0 {
            let c = self.cur_y * vl / yl;
            v -= c;
            vl += c;
            yl += self.cur_y;
            self.cur_y = 0.0;
        }
        if xl > self.clip_x - self.cur_x {
            let c = (xl - (self.clip_x - self.cur_x)) * ul / xl;
            ul -= c;
            xl -= xl - (self.clip_x - self.cur_x);
        }
        if yl > self.clip_y - self.cur_y {
            let c = (yl - (self.clip_y - self.cur_y)) * vl / yl;
            vl -= c;
            yl -= yl - (self.clip_y - self.cur_y);
        }
        if xl <= 0.0 || yl <= 0.0 {
            return;
        }
        self.draw_tile(sizer, Some(mat), xl, yl, u, v, ul, vl);
    }

    /// Script `DrawRect(Tex, RectX, RectY)`: the whole texture stretched over a rectangle.
    pub fn draw_rect(&mut self, sizer: &dyn MaterialSizer, tex: Option<&Obj>, rx: f32, ry: f32) {
        let Some(t) = tex else { return };
        let (us, vs) = sizer.material_size(t);
        self.draw_tile(sizer, Some(t), rx, ry, 0.0, 0.0, us as f32, vs as f32);
    }

    /// Script `DrawIcon(Tex, Scale)`.
    pub fn draw_icon(&mut self, sizer: &dyn MaterialSizer, tex: Option<&Obj>, scale: f32) {
        let Some(t) = tex else { return };
        let (us, vs) = sizer.material_size(t);
        self.draw_tile(sizer, Some(t), us as f32 * scale, vs as f32 * scale, 0.0, 0.0, us as f32, vs as f32);
    }

    /// Script `DrawPattern(Tex, XL, YL, Scale)`: a tile whose texture is anchored to the origin.
    pub fn draw_pattern(&mut self, sizer: &dyn MaterialSizer, tex: Option<&Obj>, xl: f32, yl: f32, scale: f32) {
        let (u, v) = ((self.cur_x - self.org_x) * scale, (self.cur_y - self.org_y) * scale);
        self.draw_tile(sizer, tex, xl, yl, u, v, xl * scale, yl * scale);
    }

    /// Native 467 `DrawActor`: 3D actor rendering into the HUD is not implemented yet.
    pub fn draw_actor(&mut self) {}

    // ---------------------------------------------------------------- text

    /// Glyph size in texels after remapping (and the out-of-range '_' fallback used for drawing).
    fn glyph_for_draw(font: &CanvasFont, c: char) -> Option<usize> {
        let idx = font.remap(c);
        if idx < font.chars.len() {
            return Some(idx);
        }
        let us = font.remap('_');
        (us < font.chars.len()).then_some(us)
    }

    fn advance(&self, font: &CanvasFont, w: i32) -> i32 {
        ((w as f32 + self.space_x + font.kerning as f32) * self.font_scale_x) as i32
    }

    /// Draws one line of text at (x, y) relative to the origin; returns the width drawn.
    /// `clip` trims glyphs to `0..ClipX` x `0..ClipY`; `hotkey` enables `&x` underlining.
    fn draw_string(&mut self, sizer: &dyn MaterialSizer, font: &Arc<CanvasFont>, x: i32, y: i32, text: &[char], clip: bool, hotkey: bool) -> i32 {
        let mut drawn = 0i32;
        let mut underline_next = false;
        let mut underline_w = 0i32;
        let mut i = 0;
        while i < text.len() {
            let mut ch = text[i];
            let mut is_hotkey = false;
            if hotkey {
                if underline_next {
                    ch = '_';
                }
                if text[i] == '&' {
                    let Some(&n) = text.get(i + 1) else { return drawn };
                    if n != '&' {
                        is_hotkey = true;
                        ch = n;
                    }
                }
            }
            let Some(idx) = Self::glyph_for_draw(font, ch) else {
                i += 1;
                continue;
            };
            let g = font.chars[idx];
            let page = font.pages.get(g.page as usize).cloned().flatten();
            let Some(page) = page else {
                i += 1;
                continue;
            };
            let mut w = g.u_size;
            if underline_next && underline_w <= w {
                w = underline_w;
            }
            let (mut gx, mut gy) = (x + drawn, y);
            let (mut su, mut sv, mut sw, mut sh) = (g.start_u, g.start_v, w, g.v_size);
            let mut visible = true;
            if clip {
                visible = gx + w > 0 && gx as f32 <= self.clip_x && sh + gy > 0 && gy as f32 <= self.clip_y;
                if visible {
                    if gx < 0 {
                        su -= gx;
                        sw += gx;
                        gx = 0;
                    }
                    if gy < 0 {
                        sv -= gy;
                        sh += gy;
                        gy = 0;
                    }
                    if self.clip_x < (sw + gx) as f32 {
                        sw = (self.clip_x - gx as f32) as i32;
                    }
                    if self.clip_y < (sh + gy) as f32 {
                        sh = (self.clip_y - gy as f32) as i32;
                    }
                }
            }
            if visible && sw > 0 && sh > 0 {
                let size = sizer.material_size(&page);
                let px = (gx as f32 + self.org_x).round();
                let py = (gy as f32 + self.org_y).round();
                let pw = (sw as f32 * self.font_scale_x).round();
                let ph = (sh as f32 * self.font_scale_y).round();
                self.push_tile(&page, size, px, py, pw, ph, su as f32, sv as f32, sw as f32, sh as f32);
            }
            if underline_next {
                w = underline_w;
            }
            underline_next = is_hotkey;
            if is_hotkey {
                // Draw the underline over the same cell next, without advancing.
                underline_w = g.u_size;
                i += 1;
                continue;
            }
            drawn += self.advance(font, w);
            i += 1;
        }
        drawn
    }

    /// Word-wrapped print (shared by DrawText and StrLen). Returns (XL, YL).
    fn wrapped_print(&mut self, sizer: &dyn MaterialSizer, draw: bool, font: &Arc<CanvasFont>, center: bool, text: &str) -> (i32, i32) {
        let (mut xl, mut yl) = (0i32, 0i32);
        if self.clip_x < 0.0 || self.clip_y < 0.0 {
            return (0, 0);
        }
        let chars: Vec<char> = text.chars().collect();
        let mut start = 0usize;
        loop {
            let line = &chars[start..];
            let (mut clean_end, mut clean_xl, mut clean_yl) = (0usize, 0i32, 0i32);
            let (mut test_xl, mut test_yl) = (self.cur_x as i32, 0i32);
            let mut got_word = false;
            let mut i = 0usize;
            while i < line.len() && line[i] != '\n' {
                let (cw, chh) = font.char_size(line[i]);
                test_xl += self.advance(font, cw);
                let h = (self.space_y as i32 + chh).max(test_yl);
                test_yl = (h as f32 * self.font_scale_y) as i32;
                if test_xl as f32 > self.clip_x {
                    break;
                }
                i += 1;
                let brk = i >= line.len() || line[i] == ' ' || line[i] == '\n';
                if brk || !got_word {
                    clean_end = i;
                    clean_xl = test_xl;
                    clean_yl = test_yl;
                    got_word = got_word || brk;
                }
            }
            if clean_end == 0 {
                break;
            }
            let on_screen = self.org_y + self.cur_y < self.size_y as f32 && 0.0 < clean_yl as f32 + self.org_y + self.cur_y;
            if draw && on_screen {
                let lx = if center { ((self.clip_x - clean_xl as f32) * 0.5 + self.cur_x) as i32 } else { self.cur_x as i32 };
                let seg: Vec<char> = line[..clean_end].to_vec();
                let w = self.draw_string(sizer, font, lx, self.cur_y as i32, &seg, false, false);
                self.cur_x = (lx + w) as f32;
            }
            self.cur_x = 0.0;
            self.cur_y += clean_yl as f32;
            yl += clean_yl;
            xl = xl.max(clean_xl);
            start += clean_end;
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
            if start >= chars.len() {
                break;
            }
        }
        (xl, yl)
    }

    /// Native 465 `DrawText(Text, optional CR)`.
    pub fn draw_text(&mut self, sizer: &dyn MaterialSizer, text: &str, cr: bool) {
        if self.style == style::NONE {
            return;
        }
        let Some(font) = self.font.clone() else { return };
        let (xl, yl) = self.wrapped_print(sizer, true, &font, self.b_center, text);
        self.cur_x += xl as f32;
        self.cur_yl = self.cur_yl.max(yl as f32);
        if cr {
            self.cur_x = 0.0;
            self.cur_y += self.cur_yl;
            self.cur_yl = 0.0;
        }
    }

    /// Native 469 `DrawTextClipped(Text, optional bCheckHotKey)`.
    pub fn draw_text_clipped(&mut self, sizer: &dyn MaterialSizer, text: &str, check_hotkey: bool) {
        let Some(font) = self.font.clone() else { return };
        let chars: Vec<char> = text.chars().collect();
        let (x, y) = (self.cur_x as i32, self.cur_y as i32);
        self.draw_string(sizer, &font, x, y, &chars, true, check_hotkey);
    }

    /// Native 464 `StrLen(String, out XL, out YL)`: wrapped size; the cursor is left unchanged.
    pub fn str_len(&mut self, text: &str) -> (f32, f32) {
        let Some(font) = self.font.clone() else { return (0.0, 0.0) };
        let (cx, cy) = (self.cur_x, self.cur_y);
        self.cur_x = 0.0;
        self.cur_y = 0.0;
        let (xl, yl) = self.wrapped_print(&FixedSizer(1, 1), false, &font, false, text);
        self.cur_x = cx as i32 as f32;
        self.cur_y = cy as i32 as f32;
        (xl as f32, yl as f32)
    }

    /// Native 470 `TextSize(String, out XL, out YL)`: single-line size, no wrapping.
    pub fn text_size(&self, text: &str) -> (f32, f32) {
        let Some(font) = &self.font else { return (0.0, 0.0) };
        let chars: Vec<char> = text.chars().collect();
        let (mut xl, mut yl) = (0i32, 0i32);
        let mut skip = 0;
        for (i, &c) in chars.iter().enumerate() {
            if c == '\u{1b}' {
                skip = 3;
                continue;
            }
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let (w, h) = font.char_size(c);
            let adv = if i + 1 == chars.len() { w as f32 } else { w as f32 + self.space_x + font.kerning as f32 };
            xl += (adv * self.font_scale_x) as i32;
            yl = yl.max((h as f32 * self.font_scale_y) as i32);
        }
        (xl as f32, yl as f32)
    }

    /// `WrapStringToArray`-style helper: splits `text` at `eol` markers, then word-wraps each piece
    /// so that no line is wider than `width` (measured with `TextSize`). Words wider than the
    /// line are split by character.
    pub fn wrap_string_to_array(&self, text: &str, width: f32, eol: &str) -> Vec<String> {
        let mut out = Vec::new();
        let pieces: Vec<&str> = if eol.is_empty() { vec![text] } else { text.split(eol).collect() };
        for piece in pieces {
            let mut line = String::new();
            for word in piece.split(' ') {
                let cand = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if self.text_size(&cand).0 <= width {
                    line = cand;
                    continue;
                }
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                // Break over-long words by character.
                let mut w = String::new();
                for c in word.chars() {
                    w.push(c);
                    if self.text_size(&w).0 > width && w.chars().count() > 1 {
                        w.pop();
                        out.push(std::mem::take(&mut w));
                        w.push(c);
                    }
                }
                line = w;
            }
            out.push(line);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use upkg::font::FontChar;

    /// Every printable character is 8x16 texels, space included.
    fn mono() -> Arc<CanvasFont> {
        let chars = (0..256).map(|i| FontChar { start_u: (i % 16) * 8, start_v: (i / 16) * 16, u_size: 8, v_size: 16, page: 0 }).collect();
        Arc::new(CanvasFont::synthetic("mono", chars, 0))
    }

    fn canvas() -> Canvas {
        let mut c = Canvas::new(640, 480);
        c.font = Some(mono());
        c
    }

    #[test]
    fn text_size_single_line() {
        let c = canvas();
        assert_eq!(c.text_size("Hello"), (40.0, 16.0));
        assert_eq!(c.text_size(""), (0.0, 0.0));
    }

    #[test]
    fn text_size_spacing_and_scale() {
        let mut c = canvas();
        c.space_x = 2.0;
        // The last character does not get SpaceX.
        assert_eq!(c.text_size("abc").0, 8.0 + 2.0 + 8.0 + 2.0 + 8.0);
        c.space_x = 0.0;
        c.font_scale_x = 1.5;
        c.font_scale_y = 2.0;
        assert_eq!(c.text_size("ab"), (24.0, 32.0));
    }

    #[test]
    fn text_size_skips_colour_codes() {
        let c = canvas();
        assert_eq!(c.text_size("\u{1b}\u{ff}\u{10}\u{10}ab").0, 16.0);
    }

    #[test]
    fn str_len_wraps_and_keeps_cursor() {
        let mut c = canvas();
        c.clip_x = 100.0;
        c.set_pos(5.0, 7.0);
        // "aaaa bbbb cccc": 14 chars, wraps after 12 chars (96 px); words: "aaaa bbbb" = 72px fits, + " cccc" = 112 > 100.
        let (xl, yl) = c.str_len("aaaa bbbb cccc");
        assert_eq!((xl, yl), (72.0, 32.0));
        assert_eq!((c.cur_x, c.cur_y), (5.0, 7.0));
    }

    #[test]
    fn draw_text_cursor_update() {
        let mut c = canvas();
        c.set_pos(10.0, 20.0);
        c.draw_text(&FixedSizer(256, 256), "abc", false);
        // One line: CurX = 0 + XL (which starts from the old CurX), CurY moved down by the line.
        assert_eq!(c.cur_x, 34.0);
        assert_eq!(c.cur_y, 36.0);
        assert_eq!(c.cur_yl, 16.0);
        c.draw_text(&FixedSizer(256, 256), "d", true);
        assert_eq!(c.cur_x, 0.0);
        assert_eq!(c.cur_yl, 0.0);
    }

    #[test]
    fn draw_text_stops_at_line_feed() {
        let mut c = canvas();
        let (xl, yl) = c.str_len("ab\ncdefgh");
        assert_eq!((xl, yl), (16.0, 16.0));
    }

    #[test]
    fn draw_text_style_none_draws_nothing_and_does_not_move() {
        let mut c = canvas();
        c.style = style::NONE;
        c.draw_text(&FixedSizer(1, 1), "abc", false);
        assert_eq!((c.cur_x, c.cur_y), (0.0, 0.0));
    }

    #[test]
    fn long_word_breaks_at_clip() {
        let mut c = canvas();
        c.clip_x = 40.0;
        // No word break inside: the first character run that fits is used.
        let (xl, yl) = c.str_len("abcdefghij");
        assert_eq!(xl, 40.0);
        assert_eq!(yl, 32.0);
    }

    #[test]
    fn wrap_string_to_array_splits() {
        let c = canvas();
        let lines = c.wrap_string_to_array("one two three|four", 64.0, "|");
        assert_eq!(lines, vec!["one two", "three", "four"]);
    }

    #[test]
    fn set_draw_color_zero_alpha_is_opaque() {
        let mut c = canvas();
        c.set_draw_color(1, 2, 3, 0);
        assert_eq!(c.draw_color, Rgba([1, 2, 3, 255]));
    }
}
