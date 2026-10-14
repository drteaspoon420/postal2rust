//! Boot splash: the original shows `System\Postal2Splash.bmp` (the red WARNING disclaimer) in a
//! small borderless window centred on the desktop while the engine boots, and closes it when the
//! game window appears. Same here: [`show`] opens it on its own thread before the world boots,
//! [`close`] takes it down once the game window has drawn its first frame.
//!
//! Windows only (plain user32/gdi32 calls); elsewhere both functions do nothing.
use std::path::Path;

/// Decodes an uncompressed 8/24/32-bit BMP into top-down BGRA rows.
pub fn decode_bmp(b: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?));
    if b.get(0..2)? != b"BM" || u32_at(30)? != 0 {
        return None;
    }
    let off = u32_at(10)? as usize;
    let hdr = u32_at(14)? as usize;
    let w = u32_at(18)? as i32;
    let h = u32_at(22)? as i32;
    let bpp = u16_at(28)?;
    let (w, top_down, h) = (w.unsigned_abs(), h < 0, h.unsigned_abs());
    let colors = match u32_at(46)? {
        0 if bpp <= 8 => 1usize << bpp,
        n => n as usize,
    };
    let pal: Vec<[u8; 4]> = (0..if bpp <= 8 { colors } else { 0 }).map(|i| {
        let o = 14 + hdr + i * 4;
        [b[o], b[o + 1], b[o + 2], 255]
    }).collect();
    let stride = ((w as usize * bpp as usize + 31) / 32) * 4;
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        let src_row = if top_down { y } else { h as usize - 1 - y };
        let row = b.get(off + src_row * stride..off + src_row * stride + stride)?;
        for x in 0..w as usize {
            let px = match bpp {
                8 => *pal.get(row[x] as usize)?,
                24 => [row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 255],
                32 => [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], 255],
                _ => return None,
            };
            out[(y * w as usize + x) * 4..][..4].copy_from_slice(&px);
        }
    }
    Some((w, h, out))
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::OnceLock;

    type Hwnd = isize;
    #[repr(C)]
    struct WndClassW {
        style: u32,
        wnd_proc: unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize,
        cls_extra: i32,
        wnd_extra: i32,
        instance: isize,
        icon: isize,
        cursor: isize,
        background: isize,
        menu_name: *const u16,
        class_name: *const u16,
    }
    #[repr(C)]
    struct Msg {
        hwnd: Hwnd,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: [i32; 2],
        private: u32,
    }
    #[repr(C)]
    struct PaintStruct {
        hdc: isize,
        erase: i32,
        rc: [i32; 4],
        restore: i32,
        inc_update: i32,
        reserved: [u8; 32],
    }
    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_ppm: i32,
        y_ppm: i32,
        clr_used: u32,
        clr_important: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(c: *const WndClassW) -> u16;
        fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: Hwnd, menu: isize, inst: isize, param: *mut core::ffi::c_void) -> Hwnd;
        fn DefWindowProcW(h: Hwnd, m: u32, w: usize, l: isize) -> isize;
        fn GetMessageW(m: *mut Msg, h: Hwnd, min: u32, max: u32) -> i32;
        fn TranslateMessage(m: *const Msg) -> i32;
        fn DispatchMessageW(m: *const Msg) -> isize;
        fn PostMessageW(h: Hwnd, m: u32, w: usize, l: isize) -> i32;
        fn DestroyWindow(h: Hwnd) -> i32;
        fn PostQuitMessage(code: i32);
        fn GetSystemMetrics(i: i32) -> i32;
        fn BeginPaint(h: Hwnd, ps: *mut PaintStruct) -> isize;
        fn EndPaint(h: Hwnd, ps: *const PaintStruct) -> i32;
        fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
        fn UpdateWindow(h: Hwnd) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn SetDIBitsToDevice(hdc: isize, x: i32, y: i32, w: u32, h: u32, sx: i32, sy: i32, start: u32, lines: u32, bits: *const core::ffi::c_void, bmi: *const BitmapInfoHeader, usage: u32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }

    const WM_DESTROY: u32 = 0x0002;
    const WM_CLOSE: u32 = 0x0010;
    const WM_PAINT: u32 = 0x000F;
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_TOPMOST: u32 = 0x0000_0008;

    static HWND: AtomicIsize = AtomicIsize::new(0);
    static IMAGE: OnceLock<(u32, u32, Vec<u8>)> = OnceLock::new();

    unsafe extern "system" fn wnd_proc(h: Hwnd, m: u32, w: usize, l: isize) -> isize {
        unsafe {
            match m {
                WM_PAINT => {
                    let mut ps: PaintStruct = std::mem::zeroed();
                    let dc = BeginPaint(h, &mut ps);
                    if let Some((iw, ih, px)) = IMAGE.get() {
                        let bmi = BitmapInfoHeader { size: 40, width: *iw as i32, height: -(*ih as i32), planes: 1, bit_count: 32, compression: 0, size_image: 0, x_ppm: 0, y_ppm: 0, clr_used: 0, clr_important: 0 };
                        SetDIBitsToDevice(dc, 0, 0, *iw, *ih, 0, 0, 0, *ih, px.as_ptr().cast(), &bmi, 0);
                    }
                    EndPaint(h, &ps);
                    0
                }
                WM_CLOSE => {
                    DestroyWindow(h);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(h, m, w, l),
            }
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn show(image: (u32, u32, Vec<u8>)) {
        let (w, h) = (image.0 as i32, image.1 as i32);
        if IMAGE.set(image).is_err() {
            return;
        }
        std::thread::spawn(move || unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let class = wide("P2RustSplash");
            let wc = WndClassW { style: 0, wnd_proc, cls_extra: 0, wnd_extra: 0, instance: inst, icon: 0, cursor: 0, background: 0, menu_name: std::ptr::null(), class_name: class.as_ptr() };
            RegisterClassW(&wc);
            let (sw, sh) = (GetSystemMetrics(0), GetSystemMetrics(1));
            let title = wide("POSTAL 2");
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, class.as_ptr(), title.as_ptr(), WS_POPUP, (sw - w) / 2, (sh - h) / 2, w, h, 0, 0, inst, std::ptr::null_mut());
            if hwnd == 0 {
                return;
            }
            HWND.store(hwnd, Ordering::SeqCst);
            ShowWindow(hwnd, 4); // SW_SHOWNOACTIVATE
            UpdateWindow(hwnd);
            let mut msg: Msg = std::mem::zeroed();
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    }

    pub fn close() {
        let h = HWND.swap(0, Ordering::SeqCst);
        if h != 0 {
            unsafe { PostMessageW(h, WM_CLOSE, 0, 0) };
        }
    }
}

/// Shows the boot splash from `System\Postal2Splash.bmp` (no-op when missing, when not on
/// Windows, with `P2GAME_NO_SPLASH`, or in automated runs: any of the test hooks
/// P2GAME_EXEC / P2GAME_PILOT / P2GAME_SHOTS / P2GAME_EXIT / P2GAME_DEMO_GOTO set, since the
/// topmost splash window would cover the user's desktop).
pub fn show(system_dir: &Path) {
    const AUTOMATED: [&str; 6] = ["P2GAME_NO_SPLASH", "P2GAME_EXEC", "P2GAME_PILOT", "P2GAME_SHOTS", "P2GAME_EXIT", "P2GAME_DEMO_GOTO"];
    if AUTOMATED.iter().any(|k| std::env::var_os(k).is_some()) {
        return;
    }
    let Ok(b) = std::fs::read(upkg::ci::join(system_dir, "Postal2Splash.bmp")) else { return };
    let Some(img) = decode_bmp(&b) else { return };
    #[cfg(windows)]
    imp::show(img);
    #[cfg(not(windows))]
    let _ = img;
}

/// Closes the boot splash (safe to call any number of times).
pub fn close() {
    #[cfg(windows)]
    imp::close();
}

#[cfg(test)]
mod tests {
    use super::decode_bmp;

    #[test]
    fn decodes_8bit_bottom_up() {
        // 2x2, 8-bit, palette: 0 = red, 1 = green; bottom row first.
        let mut b = Vec::new();
        let pal_off = 14 + 40;
        let off = pal_off + 2 * 4;
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(off as u32).to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&2i32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8u16.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 12]);
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);
        b.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0]);
        let (w, h, px) = decode_bmp(&b).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(&px[0..4], &[0, 0, 255, 255]); // top row = red (index 0)
        assert_eq!(&px[8..12], &[0, 255, 0, 255]); // bottom row = green (index 1)
    }
}
