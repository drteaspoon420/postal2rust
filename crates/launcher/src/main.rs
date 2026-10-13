//! `postal2rust.exe`: the player-facing launcher for Postal 2 Rust/Bevy.
//!
//! 1. Finds the player's POSTAL 2 install: the folder saved by a previous launch in
//!    `postal2dir.txt` (next to this exe), else asks with a folder picker that starts at a
//!    detected Steam/GOG install. The pick is validated (`System\Postal2.ini`) and saved.
//! 2. Starts `p2game.exe` (same folder) with `--game <install>`, fullscreen unless `--windowed`,
//!    passing any other arguments through (e.g. `--map suburbs-1`).
//!
//! `--change-folder` asks again. No game files are bundled: assets are read from that install.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::Command;

const TITLE: &str = "Postal 2 Rust/Bevy";

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}

fn saved_file() -> PathBuf {
    exe_dir().join("postal2dir.txt")
}

fn is_install(p: &Path) -> bool {
    let s = p.join("System");
    s.join("Postal2.ini").is_file() || (s.join("Default.ini").is_file() && s.join("Engine.u").is_file())
}

/// A picked folder as the install root: the install itself, its `System` folder, or a folder
/// directly containing it (e.g. `steamapps\common`).
fn normalise(p: &Path) -> Option<PathBuf> {
    if is_install(p) {
        return Some(p.to_path_buf());
    }
    if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("System")) {
        if let Some(parent) = p.parent().filter(|q| is_install(q)) {
            return Some(parent.to_path_buf());
        }
    }
    std::fs::read_dir(p).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).find(|c| is_install(c))
}

fn saved() -> Option<PathBuf> {
    let s = std::fs::read_to_string(saved_file()).ok()?;
    let p = PathBuf::from(s.lines().map(str::trim).find(|l| !l.is_empty())?.trim_matches('"'));
    is_install(&p).then_some(p)
}

/// Best guess to start the picker at: Steam libraries (registry + libraryfolders.vdf) and the
/// usual GOG / retail folders.
fn detect() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(out) = hidden(Command::new("reg").args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])).output() {
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some(v) = text.lines().find(|l| l.contains("SteamPath")).and_then(|l| l.split("REG_SZ").nth(1)) {
            roots.push(PathBuf::from(v.trim().replace('/', "\\")));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    roots.push(PathBuf::from(r"C:\Program Files\Steam"));
    let mut libs = Vec::new();
    for r in &roots {
        libs.push(r.clone());
        if let Ok(vdf) = std::fs::read_to_string(r.join("steamapps").join("libraryfolders.vdf")) {
            for l in vdf.lines() {
                if let Some(rest) = l.trim().strip_prefix("\"path\"") {
                    libs.push(PathBuf::from(rest.trim().trim_matches('"').replace("\\\\", "\\")));
                }
            }
        }
    }
    for d in 'C'..='Z' {
        libs.push(PathBuf::from(format!(r"{d}:\SteamLibrary")));
    }
    let mut cands: Vec<PathBuf> = vec![exe_dir(), exe_dir().parent().map(Path::to_path_buf).unwrap_or_default()];
    for lib in libs {
        for name in ["POSTAL2Complete", "POSTAL 2", "Postal 2", "POSTAL2"] {
            cands.push(lib.join("steamapps").join("common").join(name));
        }
    }
    for p in [r"C:\GOG Games\POSTAL 2", r"C:\Program Files (x86)\GOG Galaxy\Games\POSTAL 2", r"C:\Program Files (x86)\Running With Scissors\POSTAL 2"] {
        cands.push(PathBuf::from(p));
    }
    cands.into_iter().find(|c| is_install(c))
}

/// Runs a child without flashing a console window.
fn hidden(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

#[cfg(windows)]
mod win {
    //! Native dialogs (user32 MessageBoxW, shell32 SHBrowseForFolderW), forced to the foreground:
    //! PowerShell/WinForms dialogs could open behind other windows and look like a hang.
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

fn message(text: &str, ok_cancel: bool) -> bool {
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
        let Some(picked) = pick_folder(&at) else { return None };
        if picked.is_empty() {
            return None;
        }
        if let Some(root) = normalise(Path::new(&picked)) {
            let _ = std::fs::write(saved_file(), format!("{}\r\n", root.display()));
            return Some(root);
        }
        if !message(&format!("This folder is not a POSTAL 2 install:\n\n{picked}\n\nIt must contain System\\Postal2.ini. Click OK to pick again."), true) {
            return None;
        }
        at = picked;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let change = args.iter().any(|a| a == "--change-folder");
    let windowed = args.iter().any(|a| a == "--windowed");
    let pass: Vec<&String> = args.iter().filter(|a| *a != "--change-folder" && *a != "--windowed").collect();

    let game = exe_dir().join("p2game.exe");
    if !game.is_file() {
        message(&format!("p2game.exe is missing next to postal2rust.exe:\n{}\n\nRe-extract the release zip.", game.display()), false);
        std::process::exit(1);
    }
    let root = match (change, saved()) {
        (false, Some(p)) => p,
        _ => match ask(detect()) {
            Some(p) => p,
            None => std::process::exit(0),
        },
    };
    let mut cmd = Command::new(&game);
    cmd.current_dir(exe_dir()).arg("--game").arg(&root).args(pass);
    if !windowed {
        cmd.env("P2GAME_FULLSCREEN", "1");
    }
    // Game output goes to a log beside the exe (the launcher has no console).
    if let Ok(log) = std::fs::File::create(exe_dir().join("p2game.log")) {
        if let Ok(log2) = log.try_clone() {
            cmd.stdout(log).stderr(log2);
        }
    }
    if let Err(e) = hidden(&mut cmd).spawn() {
        message(&format!("Could not start p2game.exe: {e}"), false);
        std::process::exit(1);
    }
}
