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

fn ps(script: &str) -> Option<String> {
    let out = hidden(Command::new("powershell").args(["-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-Command", script])).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn q(s: &str) -> String {
    s.replace('\'', "''")
}

fn message(text: &str, ok_cancel: bool) -> bool {
    let buttons = if ok_cancel { "OKCancel" } else { "OK" };
    let r = ps(&format!(
        "Add-Type -AssemblyName System.Windows.Forms; $f=New-Object System.Windows.Forms.Form -Property @{{TopMost=$true}}; [System.Windows.Forms.MessageBox]::Show($f, '{}', '{TITLE}', '{buttons}')",
        q(text)
    ));
    r.as_deref() == Some("OK")
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
        let picked = ps(&format!(
            "Add-Type -AssemblyName System.Windows.Forms; $f=New-Object System.Windows.Forms.Form -Property @{{TopMost=$true}}; $d=New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description='Select your POSTAL 2 install folder (contains System, Maps, Textures)'; $d.ShowNewFolderButton=$false; $d.SelectedPath='{}'; if($d.ShowDialog($f) -eq 'OK'){{ $d.SelectedPath }}",
            q(&at)
        ))?;
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
