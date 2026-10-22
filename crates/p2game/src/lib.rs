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
pub mod damage_bridge;
pub mod host;
pub mod ini;
pub mod level;
pub mod navfix;
pub mod physics;
pub mod pkgindex;
pub mod preload;
pub mod ragdoll;
pub mod save;
pub mod subtitles;
pub mod steam;
pub mod travel;
pub mod viewport;

use std::path::PathBuf;

/// Game root: `--game`, `POSTAL2_DIR`, a `postal2dir.txt` next to the exe (one line, the install
/// folder), the exe's own folder or its parent (dropped into / beside the install), then every
/// Steam library (registry SteamPath + libraryfolders.vdf) and the usual Steam/GOG folders.
/// The first valid install wins; a missing install is reported against `./postal2`.
pub fn game_root(arg: Option<&str>) -> PathBuf {
    if let Some(a) = arg {
        return PathBuf::from(a);
    }
    if let Ok(d) = std::env::var("POSTAL2_DIR") {
        return PathBuf::from(d);
    }
    find_install().unwrap_or_else(|| PathBuf::from("postal2"))
}

/// A POSTAL 2 install folder: one with `System\Postal2.ini` (or `System\Default.ini`).
pub fn is_install(p: &std::path::Path) -> bool {
    let s = upkg::ci::join(p, "System");
    (upkg::ci::join(&s, "Postal2.ini").is_file() || upkg::ci::join(&s, "Default.ini").is_file()) && upkg::ci::join(&s, "Engine.u").is_file()
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
    // Steam's manifests locate renamed installs and custom libraries.
    cands.extend(steam::installs(223470));
    for lib in steam::libraries() {
        let common = upkg::ci::join(lib, "steamapps/common");
        for name in ["POSTAL2Complete", "POSTAL 2", "Postal 2", "POSTAL2"] {
            cands.push(common.join(name));
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
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
    // Existing developer copies remain a fallback; they never override a
    // valid Steam install. No build-machine absolute path is embedded.
    if let Ok(cwd) = std::env::current_dir() {
        cands.push(cwd.join("postal2"));
    }
    if let Some(dir) = &exe_dir {
        for parent in dir.ancestors().take(4) { cands.push(parent.join("postal2")); }
    }
    cands.into_iter().find(|c| is_install(c))
}
