//! p2game: the POSTAL 2 game host.
//!
//! - [`ini`]: clean-room UE2 ini parser.
//! - [`config`]: boot configuration from `System\Postal2.ini` and `User.ini`.
//! - [`pkgindex`]: table-only package index for resolving cross-package references cheaply.
//! - [`level`]: map exports to VM objects (class defaults, config, saved properties, references).
//! - [`boot`]: GameInfo, UE2 startup order, frame tick, native/error histograms.

pub mod ai;
pub mod audio;
pub mod boot;
pub mod cinema;
pub mod clock;
pub mod config;
pub mod demo;
#[cfg(feature = "window")]
pub mod fx_glue;
pub mod gibs;
pub mod host;
pub mod ini;
pub mod level;
pub mod navfix;
pub mod physics;
pub mod pkgindex;
pub mod ragdoll;
pub mod save;
pub mod subtitles;
pub mod travel;
pub mod viewport;

use std::path::PathBuf;

/// Game root: `--game`, `POSTAL2_DIR`, a `postal2dir.txt` next to the exe (one line, the install
/// folder), the exe's own folder or its parent (dropped into / beside the install), then every
/// Steam library (registry SteamPath + libraryfolders.vdf) and the usual Steam/GOG folders.
/// The first folder holding `System\Postal2.ini` wins; else the old default (dev machine).
pub fn game_root(arg: Option<&str>) -> PathBuf {
    if let Some(a) = arg {
        return PathBuf::from(a);
    }
    if let Ok(d) = std::env::var("POSTAL2_DIR") {
        return PathBuf::from(d);
    }
    find_install().unwrap_or_else(|| PathBuf::from(r"G:\SteamLibrary\steamapps\common\POSTAL2Complete"))
}

/// A POSTAL 2 install folder: one with `System\Postal2.ini` (or `System\Default.ini`).
pub fn is_install(p: &std::path::Path) -> bool {
    let s = upkg::ci::join(p, "System");
    upkg::ci::join(&s, "Postal2.ini").is_file() || upkg::ci::join(&s, "Default.ini").is_file() && upkg::ci::join(&s, "Engine.u").is_file()
}

pub fn find_install() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf()));
    if let Some(dir) = &exe_dir {
        if let Ok(s) = std::fs::read_to_string(dir.join("postal2dir.txt")) {
            if let Some(line) = s.lines().map(str::trim).find(|l| !l.is_empty()) {
                cands.push(PathBuf::from(line.trim_matches('"')));
            }
        }
        cands.push(dir.clone());
        if let Some(p) = dir.parent() {
            cands.push(p.to_path_buf());
            cands.push(p.join("POSTAL2Complete"));
        }
    }
    // Steam libraries.
    let mut steam_roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        if let Ok(out) = std::process::Command::new("reg").args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"]).output() {
            let text = String::from_utf8_lossy(&out.stdout);
            if let Some(v) = text.lines().find(|l| l.contains("SteamPath")).and_then(|l| l.split("REG_SZ").nth(1)) {
                steam_roots.push(PathBuf::from(v.trim().replace('/', "\\")));
            }
        }
        for p in [r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam"] {
            steam_roots.push(PathBuf::from(p));
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    if let Some(h) = &home {
        steam_roots.push(h.join("Library/Application Support/Steam"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(h) = &home {
        // Native Steam, the ~/.steam symlinks, and the Flatpak / Snap packages.
        for rel in [".local/share/Steam", ".steam/steam", ".steam/root", ".var/app/com.valvesoftware.Steam/.local/share/Steam", "snap/steam/common/.local/share/Steam"] {
            steam_roots.push(h.join(rel));
        }
    }
    let mut libs: Vec<PathBuf> = Vec::new();
    for root in &steam_roots {
        libs.push(root.clone());
        if let Ok(vdf) = std::fs::read_to_string(upkg::ci::join(root, "steamapps/libraryfolders.vdf")) {
            for line in vdf.lines() {
                let l = line.trim();
                if let Some(rest) = l.strip_prefix("\"path\"") {
                    let v = rest.trim().trim_matches('"').replace("\\\\", "\\");
                    libs.push(PathBuf::from(v));
                }
            }
        }
    }
    for drive in 'C'..='Z' {
        if cfg!(windows) {
            libs.push(PathBuf::from(format!(r"{drive}:\SteamLibrary")));
        }
    }
    for lib in libs {
        let common = upkg::ci::join(lib, "steamapps/common");
        for name in ["POSTAL2Complete", "POSTAL 2", "Postal 2", "POSTAL2"] {
            cands.push(common.join(name));
        }
    }
    #[cfg(windows)]
    for p in [r"C:\GOG Games\POSTAL 2", r"C:\Program Files (x86)\GOG Galaxy\Games\POSTAL 2", r"C:\Program Files (x86)\Running With Scissors\POSTAL 2"] {
        cands.push(PathBuf::from(p));
    }
    // GOG installs via Heroic / Lutris / a manual copy.
    #[cfg(unix)]
    if let Some(h) = &home {
        for rel in ["Games/Heroic/POSTAL 2", "Games/POSTAL 2", "Games/postal-2", "GOG Games/POSTAL 2", "Applications/POSTAL 2"] {
            cands.push(h.join(rel));
        }
    }
    #[cfg(not(unix))]
    let _ = home;
    cands.into_iter().find(|c| is_install(c))
}
