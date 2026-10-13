//! POSTAL 2 save games: slot listing for the Load Game menu and inspection of `.usa` files.
//!
//! # Format
//!
//! * `Save\SaveN.usa` (N = slot 0..21) is an ordinary UE2 package (same header, name, import
//!   and export tables as a map; version 118, licensee 2). It is the running level serialized
//!   with every actor's current properties: `LevelInfo0` (with `Title`, `TimeSeconds`,
//!   `VisibleGroups`, `Game`, `ControllerList`), the `MyLevel` object, all actors (pawns,
//!   controllers, doors, pickups, emitters...) and the game-wide `P2GameState` / `AWGameState`
//!   object that travels between levels (`CurrentDay`, `GameDifficulty`, `TimeElapsed`, kill
//!   statistics, errands, inventory bookkeeping, `MostRecentGameSlot`). Imports point at the
//!   shipped packages for classes and assets. Actors carry script state frames (flag 0x02000000)
//!   exactly like map actors.
//! * `System\SavedGameInfo.ini`, section `[FPSGame.SlotInfoMgr]`: one
//!   `Slots=(Name="...",Day=d,Time="YYYY-MM-DD : HH:MM",lTime=unix,LoadScreen="pkg.tex")` line
//!   per slot, in slot order (22 slots). This is what the Load menu shows; it is kept in its own
//!   ini so it survives deleting the other inis. When a line is blank but the file exists the
//!   menu shows "<Unknown Day> - <Unknown Level> - <Difficulty?>"; this crate can instead
//!   reconstruct the information from the `.usa` itself ([`read_save_summary`]).
//! * Special slots: 19 checkpoint, 20 quicksave, 21 autosave. The `Name` is
//!   `"<Day> - <Level title> - <Difficulty> - <Game>"`.
//!
//! # Saving / loading design for p2game
//!
//! `SaveGame N` (console command, called by `P2GameInfoSingle.SaveGame`) runs `PreSaveGame` on
//! all actors, writes the level and the game state into a package with the same layout (live
//! objects become exports, classes and assets become imports), writes the slot line, then runs
//! `PostSaveGame`. `LoadGame N` / the `?load=N` travel option opens the package as the level
//! instead of the map, recreates actors from the exports without running `BeginPlay`, restores
//! the script state frames and calls `PostLoadGame` on every actor. `CHECKSAVEGAME N` returns
//! whether the file exists. Writing is not implemented here yet; [`SaveSlot`] and
//! [`SaveSummary`] are what the Load / Save menus need today.

use std::path::{Path, PathBuf};
use upkg::{Package, Value, begin_object};

pub const SLOT_COUNT: usize = 22;
pub const CHECKPOINT_SLOT: usize = 19;
pub const QUICKSAVE_SLOT: usize = 20;
pub const AUTOSAVE_SLOT: usize = 21;

pub const DAY_NAMES: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// One `SlotInfo` entry of `SavedGameInfo.ini`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SlotInfo {
    pub name: String,
    pub day: i32,
    pub time: String,
    /// Unix time of the save (sort key).
    pub ltime: i64,
    pub load_screen: String,
}

/// A slot as the Load Game menu sees it.
#[derive(Debug, Clone)]
pub struct SaveSlot {
    pub slot: usize,
    pub path: PathBuf,
    /// The save file exists (`CHECKSAVEGAME`).
    pub exists: bool,
    pub info: SlotInfo,
    pub file_size: u64,
    /// File modification time (unix seconds), used when the ini time is blank.
    pub file_mtime: Option<i64>,
}

impl SaveSlot {
    /// Label for special slots ("Checkpoint", "Quicksave", "Autosave") or the slot number.
    pub fn label(&self) -> String {
        match self.slot {
            CHECKPOINT_SLOT => "Checkpoint".into(),
            QUICKSAVE_SLOT => "Quicksave".into(),
            AUTOSAVE_SLOT => "Autosave".into(),
            n => format!("Slot {}", n),
        }
    }

    /// Display name with the game's fallback for lost slot info.
    pub fn display_name(&self) -> String {
        if self.info.name.is_empty() { "<Unknown Day> - <Unknown Level> - <Difficulty?>".into() } else { self.info.name.clone() }
    }

    /// Sort key: the ini lTime, else the file time.
    pub fn sort_time(&self) -> i64 {
        if self.info.ltime > 0 { self.info.ltime } else { self.file_mtime.unwrap_or(0) }
    }
}

/// Parses `SavedGameInfo.ini` text. Returns one entry per `Slots=` line in order.
pub fn parse_slot_ini(text: &str) -> Vec<SlotInfo> {
    let mut in_section = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l.eq_ignore_ascii_case("[FPSGame.SlotInfoMgr]");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else { continue };
        if !k.trim().eq_ignore_ascii_case("Slots") {
            continue;
        }
        out.push(parse_struct(v.trim()));
    }
    out
}

/// Parses `(Name="..",Day=0,Time="..",lTime=0,LoadScreen="..")`.
fn parse_struct(v: &str) -> SlotInfo {
    let inner = v.trim().trim_start_matches('(').trim_end_matches(')');
    let mut info = SlotInfo::default();
    let mut rest = inner;
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else { break };
        let key = key.trim().trim_start_matches(',').trim();
        let (value, next) = if let Some(s) = after.strip_prefix('"') {
            match s.find('"') {
                Some(e) => (&s[..e], s[e + 1..].trim_start_matches(',')),
                None => (s, ""),
            }
        } else {
            match after.find(',') {
                Some(e) => (&after[..e], &after[e + 1..]),
                None => (after, ""),
            }
        };
        match key.to_ascii_lowercase().as_str() {
            "name" => info.name = value.to_string(),
            "day" => info.day = value.trim().parse().unwrap_or(0),
            "time" => info.time = value.to_string(),
            "ltime" => info.ltime = value.trim().parse().unwrap_or(0),
            "loadscreen" => info.load_screen = value.to_string(),
            _ => {}
        }
        rest = next;
    }
    info
}

/// Formats a slot line in the same syntax (for writing the ini after a save).
pub fn format_slot(info: &SlotInfo) -> String {
    format!("Slots=(Name=\"{}\",Day={},Time=\"{}\",lTime={},LoadScreen=\"{}\")", info.name, info.day, info.time, info.ltime, info.load_screen)
}

/// Save folder and slot ini for a game directory (the base game; add-ons such as
/// "Paradise Lost" have their own `PLSave` / `System` folders under their directory).
pub fn save_dir(game: &Path) -> PathBuf {
    game.join("Save")
}

pub fn slot_ini_path(game: &Path) -> PathBuf {
    game.join("System").join("SavedGameInfo.ini")
}

pub fn slot_file(dir: &Path, slot: usize) -> PathBuf {
    dir.join(format!("Save{slot}.usa"))
}

/// Lists all slots of a save folder with their ini info.
pub fn list_slots(save_dir: &Path, ini: &Path) -> Vec<SaveSlot> {
    let infos = std::fs::read(ini).map(|b| parse_slot_ini(&String::from_utf8_lossy(&b))).unwrap_or_default();
    (0..SLOT_COUNT)
        .map(|slot| {
            let path = slot_file(save_dir, slot);
            let meta = std::fs::metadata(&path).ok();
            SaveSlot {
                slot,
                exists: meta.is_some(),
                info: infos.get(slot).cloned().unwrap_or_default(),
                file_size: meta.as_ref().map_or(0, |m| m.len()),
                file_mtime: meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64),
                path,
            }
        })
        .collect()
}

/// Existing slots, most recent first (the Load menu order).
pub fn list_existing_by_time(game: &Path) -> Vec<SaveSlot> {
    let mut v: Vec<SaveSlot> = list_slots(&save_dir(game), &slot_ini_path(game)).into_iter().filter(|s| s.exists).collect();
    v.sort_by(|a, b| b.sort_time().cmp(&a.sort_time()));
    v
}

/// What can be read from the `.usa` itself.
#[derive(Debug, Clone, Default)]
pub struct SaveSummary {
    /// `LevelInfo.Title` (e.g. "Brewery").
    pub level_title: String,
    /// Class of `LevelInfo.Game` (e.g. "AWPGameInfo").
    pub game_info_class: String,
    pub game_state_class: String,
    /// `CurrentDay` (0 = Monday).
    pub day: Option<i32>,
    pub difficulty: Option<i32>,
    /// Seconds played in the whole game (`TimeElapsed`).
    pub time_elapsed: Option<f32>,
    /// Level time (`LevelInfo.TimeSeconds`).
    pub level_time: Option<f32>,
    pub most_recent_slot: Option<i32>,
    pub people_killed: Option<i32>,
    pub player_class: String,
    pub exports: usize,
    pub version: (u16, u16),
}

impl SaveSummary {
    pub fn day_name(&self) -> &'static str {
        self.day.and_then(|d| DAY_NAMES.get(d as usize).copied()).unwrap_or("<Unknown Day>")
    }
}

/// Reads level title, day, difficulty and statistics from a save package.
pub fn read_save_summary(path: &Path) -> upkg::Result<SaveSummary> {
    let pkg = match Package::open(path) {
        Ok(p) => p,
        Err(e) => {
            // Interrupted writes leave zero-filled files; say so instead of "bad tag".
            let head = std::fs::read(path).ok().map(|b| b.iter().take(4096).all(|&x| x == 0));
            if head == Some(true) {
                return Err(upkg::Error::Corrupt("save file is zero-filled (interrupted write)"));
            }
            return Err(e);
        }
    };
    let mut s = SaveSummary { exports: pkg.exports.len(), version: (pkg.version, pkg.licensee), ..Default::default() };
    for i in 0..pkg.exports.len() {
        let class = pkg.export_class(i);
        let lc = class.to_ascii_lowercase();
        if lc == "levelinfo" && s.level_title.is_empty() {
            if let Ok((_, p)) = begin_object(&pkg, i) {
                if let Some(Value::Str(t)) = p.get("Title") {
                    s.level_title = t.clone();
                }
                s.level_time = p.float("TimeSeconds");
                if let Some(g) = p.object("Game") {
                    s.game_info_class = pkg.class_of(g).to_string();
                }
                if let Some(c) = p.object("ControllerList") {
                    s.player_class = pkg.class_of(c).to_string();
                }
            }
        } else if lc.ends_with("gamestate") && s.game_state_class.is_empty() {
            s.game_state_class = class.to_string();
            if let Ok((_, p)) = begin_object(&pkg, i) {
                s.day = Some(p.int("CurrentDay").unwrap_or(0));
                s.difficulty = p.int("GameDifficulty");
                s.time_elapsed = p.float("TimeElapsed");
                s.most_recent_slot = p.int("MostRecentGameSlot");
                s.people_killed = Some(p.int("PeopleKilled").unwrap_or(0));
            }
        }
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_roundtrip() {
        let text = "[FPSGame.SlotInfoMgr]\r\nSlots=(Name=\"\",Day=0,Time=\"\",lTime=0,LoadScreen=\"\")\r\nSlots=(Name=\"Friday - Suburbs - Hard - A Week In Paradise (Classic)\",Day=4,Time=\"2025-08-13 : 15:09\",lTime=1755094169,LoadScreen=\"p2misc_full.loading5\")\r\n[Other]\r\nSlots=(Name=\"x\")\r\n";
        let v = parse_slot_ini(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], SlotInfo::default());
        assert_eq!(v[1].name, "Friday - Suburbs - Hard - A Week In Paradise (Classic)");
        assert_eq!(v[1].day, 4);
        assert_eq!(v[1].time, "2025-08-13 : 15:09");
        assert_eq!(v[1].ltime, 1755094169);
        assert_eq!(v[1].load_screen, "p2misc_full.loading5");
        let line = format_slot(&v[1]);
        assert_eq!(parse_slot_ini(&format!("[FPSGame.SlotInfoMgr]\n{line}"))[0], v[1]);
    }

    #[test]
    fn labels() {
        let s = SaveSlot { slot: QUICKSAVE_SLOT, path: PathBuf::new(), exists: true, info: SlotInfo::default(), file_size: 0, file_mtime: Some(5) };
        assert_eq!(s.label(), "Quicksave");
        assert_eq!(s.display_name(), "<Unknown Day> - <Unknown Level> - <Difficulty?>");
        assert_eq!(s.sort_time(), 5);
    }
}
