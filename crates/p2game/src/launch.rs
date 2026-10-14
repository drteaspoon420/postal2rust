//! Player launch, built into the game exe (the release ships it as `Postal2Rust.exe`; there is no
//! separate launcher any more).
//!
//! * Install folder: `--game DIR`, else `POSTAL2_DIR`, else `postal2dir.txt` next to the exe, else
//!   auto-detection (Steam/GOG); if none is found, or with `--change-folder`, a folder picker asks
//!   and the pick is saved to `postal2dir.txt`.
//! * Window: the shipped exe (any name but `p2game`) starts fullscreen unless `--windowed`; dev runs
//!   of `p2game.exe` keep their old default.
//! * The console window stays open after a crash or a missing install so the player can read why.

use std::path::{Path, PathBuf};

const TITLE: &str = "Postal 2 Rust/Bevy";

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}

/// True for the shipped, player-facing exe (renamed from `p2game.exe`).
pub fn shipped() -> bool {
    std::env::current_exe().ok().and_then(|e| e.file_stem().map(|s| !s.eq_ignore_ascii_case("p2game"))).unwrap_or(false)
}

fn has(a: &[String], f: &str) -> bool {
    a.iter().any(|x| x.eq_ignore_ascii_case(f))
}

/// Window mode and crash handling for this run. Call first in `main` (before any threads).
pub fn prepare(a: &[String]) {
    if has(a, "--windowed") {
        // SAFETY: single-threaded at this point (start of main).
        unsafe { std::env::set_var("P2GAME_WINDOWED", "1") };
    } else if shipped() && std::env::var_os("P2GAME_FULLSCREEN").is_none() {
        // SAFETY: as above.
        unsafe { std::env::set_var("P2GAME_FULLSCREEN", "1") };
    }
    if shipped() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            prev(info);
            hold_console("\nThe game crashed. The messages above say where.");
        }));
    }
}

/// Keeps a double-clicked console open until Enter (shipped exe only).
pub fn hold_console(msg: &str) {
    if !shipped() || std::env::var_os("P2GAME_NO_PAUSE").is_some() {
        return;
    }
    eprintln!("{msg}\nPress Enter to close this window.");
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}

fn saved_file() -> PathBuf {
    exe_dir().join("postal2dir.txt")
}

/// A picked folder as the install root: the install itself, its `System` folder, or a folder
/// directly containing it (e.g. `steamapps\common`).
fn normalise(p: &Path) -> Option<PathBuf> {
    if p2game::is_install(p) {
        return Some(p.to_path_buf());
    }
    if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("System")) {
        if let Some(parent) = p.parent().filter(|q| p2game::is_install(q)) {
            return Some(parent.to_path_buf());
        }
    }
    std::fs::read_dir(p).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|c| p2game::is_install(c))
}

/// The install root for this run (see the module docs). None: the player cancelled.
pub fn resolve_root(a: &[String], game_flag: Option<&str>) -> Option<PathBuf> {
    if let Some(g) = game_flag {
        return Some(PathBuf::from(g));
    }
    if let Ok(d) = std::env::var("POSTAL2_DIR") {
        return Some(PathBuf::from(d));
    }
    let found = p2game::find_install();
    if !has(a, "--change-folder") {
        if let Some(f) = &found {
            return Some(f.clone());
        }
    }
    println!("Asking for the POSTAL 2 install folder...");
    ask(found)
}

/// Folder picker until a valid install is chosen (saved) or the player cancels.
fn ask(start: Option<PathBuf>) -> Option<PathBuf> {
    let intro = match &start {
        Some(p) => format!("Postal 2 Rust/Bevy runs on the assets of YOUR OWN copy of POSTAL 2 (no game files are included).\n\nA POSTAL 2 install was found here:\n{}\n\nClick OK and confirm (or pick another) POSTAL 2 folder.", p.display()),
        None => "Postal 2 Rust/Bevy runs on the assets of YOUR OWN copy of POSTAL 2 (no game files are included).\n\nClick OK and select your POSTAL 2 install folder: the one containing System, Maps and Textures (Steam: ...\\steamapps\\common\\POSTAL2Complete).".to_string(),
    };
    if !message(&intro, true) {
        return None;
    }
    let mut at = start.map(|p| p.display().to_string()).unwrap_or_default();
    loop {
        let picked = pick_folder(&at)?;
        if picked.is_empty() {
            return None;
        }
        if let Some(root) = normalise(Path::new(&picked)) {
            let _ = std::fs::write(saved_file(), format!("{}\r\n", root.display()));
            println!("POSTAL 2 folder saved to {}", saved_file().display());
            return Some(root);
        }
        if !message(&format!("This folder is not a POSTAL 2 install:\n\n{picked}\n\nIt must contain System\\Postal2.ini. Click OK to pick again."), true) {
            return None;
        }
        at = picked;
    }
}

pub fn message(text: &str, ok_cancel: bool) -> bool {
    #[cfg(windows)]
    {
        win::message(TITLE, text, ok_cancel)
    }
    #[cfg(not(windows))]
    {
        eprintln!("{text}");
        !ok_cancel
    }
}

fn pick_folder(at: &str) -> Option<String> {
    #[cfg(windows)]
    {
        win::pick_folder("Select your POSTAL 2 install folder (it contains System, Maps and Textures).", at)
    }
    #[cfg(not(windows))]
    {
        let _ = at;
        None
    }
}

#[cfg(windows)]
mod win {
    //! Native dialogs (user32 MessageBoxW, shell32 SHBrowseForFolderW), forced to the foreground.
    use std::ffi::c_void;
    type Hwnd = *mut c_void;

    #[repr(C)]
    struct BrowseInfoW {
        owner: Hwnd,
        root: *const c_void,
        display_name: *mut u16,
        title: *const u16,
        flags: u32,
        callback: Option<unsafe extern "system" fn(Hwnd, u32, isize, isize) -> i32>,
        lparam: isize,
        image: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(h: Hwnd, text: *const u16, caption: *const u16, kind: u32) -> i32;
        fn SendMessageW(h: Hwnd, msg: u32, w: usize, l: isize) -> isize;
        fn SetForegroundWindow(h: Hwnd) -> i32;
        fn SetWindowPos(h: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHBrowseForFolderW(bi: *mut BrowseInfoW) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, path: *mut u16) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn OleInitialize(r: *mut c_void) -> i32;
        fn CoTaskMemFree(p: *mut c_void);
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// OK / OK+Cancel message box on top of everything; true when OK was pressed.
    pub fn message(title: &str, text: &str, ok_cancel: bool) -> bool {
        const MB_OKCANCEL: u32 = 0x1;
        const MB_ICONINFORMATION: u32 = 0x40;
        const MB_SETFOREGROUND: u32 = 0x1_0000;
        const MB_TOPMOST: u32 = 0x4_0000;
        let (t, c) = (wide(text), wide(title));
        let kind = MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST | if ok_cancel { MB_OKCANCEL } else { 0 };
        unsafe {
            AllowSetForegroundWindow(u32::MAX);
            MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), kind) == 1
        }
    }

    unsafe extern "system" fn browse_cb(h: Hwnd, msg: u32, _l: isize, data: isize) -> i32 {
        const BFFM_INITIALIZED: u32 = 1;
        const BFFM_SETSELECTIONW: u32 = 0x400 + 103;
        const HWND_TOPMOST: isize = -1;
        if msg == BFFM_INITIALIZED {
            unsafe {
                SetWindowPos(h, HWND_TOPMOST as Hwnd, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0040);
                SetForegroundWindow(h);
                if data != 0 {
                    SendMessageW(h, BFFM_SETSELECTIONW, 1, data);
                }
            }
        }
        0
    }

    /// Folder browser starting at `start`; the chosen folder, or None when cancelled.
    pub fn pick_folder(prompt: &str, start: &str) -> Option<String> {
        const BIF_RETURNONLYFSDIRS: u32 = 0x1;
        const BIF_EDITBOX: u32 = 0x10;
        const BIF_NEWDIALOGSTYLE: u32 = 0x40;
        let title = wide(prompt);
        let start_w = wide(start);
        let mut name = vec![0u16; 260];
        let mut bi = BrowseInfoW {
            owner: std::ptr::null_mut(),
            root: std::ptr::null(),
            display_name: name.as_mut_ptr(),
            title: title.as_ptr(),
            flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            callback: Some(browse_cb),
            lparam: if start.is_empty() { 0 } else { start_w.as_ptr() as isize },
            image: 0,
        };
        unsafe {
            OleInitialize(std::ptr::null_mut());
            AllowSetForegroundWindow(u32::MAX);
            let pidl = SHBrowseForFolderW(&mut bi);
            if pidl.is_null() {
                return None;
            }
            let mut path = vec![0u16; 1024];
            let ok = SHGetPathFromIDListW(pidl, path.as_mut_ptr()) != 0;
            CoTaskMemFree(pidl);
            if !ok {
                return None;
            }
            let n = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            Some(String::from_utf16_lossy(&path[..n]))
        }
    }
}
