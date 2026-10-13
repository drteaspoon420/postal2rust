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

/// Game root from `--game`, `POSTAL2_DIR`, or the default Steam location.
pub fn game_root(arg: Option<&str>) -> PathBuf {
    if let Some(a) = arg {
        return PathBuf::from(a);
    }
    if let Ok(d) = std::env::var("POSTAL2_DIR") {
        return PathBuf::from(d);
    }
    PathBuf::from(r"G:\SteamLibrary\steamapps\common\POSTAL2Complete")
}
