//! Offline platform natives used by the shell and HUD: Steam (ownership, achievements, stats,
//! workshop), travel (`PlayerController.ClientTravel`, `UpdateURL`, `GetEntryLevel`), and a few
//! engine queries the menus make. They are meant to be answered from a `Host::native`
//! implementation (`ShellNatives::native`), so they never shadow a host's own natives.
//!
//! Steam behaviour is "offline, owns what is installed": `SteamOwnsDLC` is true only when the
//! DLC's content exists in the install (Paradise Lost = `<game>/Paradise Lost/System/PLGame.u`),
//! `IsSteamBuild` is true like the shipped exe (see [`steam_build`]), Steam does not own POSTAL 4
//! (so the main menu shows the original's banner), achievements and stats are kept in memory only.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use uscript::Result;
use uscript::host::NativeCall;
use uscript::vm::{Value, Vm};

/// Steam app id of the Paradise Lost DLC (`MenuMain.PL_DLC_APPID`).
pub const PARADISE_LOST_APPID: i32 = 360960;

/// `LevelInfo.IsSteamBuild`: true like the shipped Steam executable (main menu shows the
/// POSTAL 4 banner, Options has Purge Workshop, Start lists Workshop). `P2_STEAM_BUILD=0`
/// switches to the DRM-free behaviour.
pub fn steam_build() -> bool {
    std::env::var("P2_STEAM_BUILD").map(|v| v != "0").unwrap_or(true)
}

/// State behind the offline platform natives.
#[derive(Default)]
pub struct ShellNatives {
    /// Install root (`.../POSTAL2Complete`), for DLC content checks.
    pub game_dir: PathBuf,
    /// Achievements unlocked this session (name -> unlocked).
    pub achievements: RefCell<HashMap<String, bool>>,
    /// Stats set this session.
    pub stats: RefCell<HashMap<String, f32>>,
    /// Travel URLs requested by `ClientTravel` (oldest first). The host drains this.
    pub travel: RefCell<Vec<String>>,
    /// Options set through `UpdateURL` (key -> value).
    pub url_options: RefCell<HashMap<String, String>>,
}

impl ShellNatives {
    pub fn new(game_dir: impl Into<PathBuf>) -> Self {
        Self { game_dir: game_dir.into(), ..Default::default() }
    }

    /// True if the DLC's content is installed.
    pub fn dlc_installed(&self, app_id: i32) -> bool {
        match app_id {
            PARADISE_LOST_APPID => self.game_dir.join("Paradise Lost").join("System").join("PLGame.u").exists(),
            _ => false,
        }
    }

    /// Takes the oldest pending travel URL.
    pub fn take_travel(&self) -> Option<String> {
        let mut t = self.travel.borrow_mut();
        (!t.is_empty()).then(|| t.remove(0))
    }

    /// Answers the natives this module implements; `None` for anything else.
    pub fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut [Value]) -> Option<Result<Value>> {
        let s = |k: usize| args.get(k).map(|v| v.as_str()).unwrap_or_default();
        let i = |k: usize| args.get(k).map(|v| v.as_int()).unwrap_or(0);
        let v = match call.name.to_ascii_lowercase().as_str() {
            // Steam / platform.
            "issteambuild" => Value::Bool(steam_build()),
            "issteamdeck" | "isworkshopbuild" => Value::Bool(false),
            "steamownsdlc" => Value::Bool(self.dlc_installed(i(0))),
            "steamownsgame" => Value::Bool(false),
            "steamgetworkshopstatus" => Value::Str(String::new()),
            // Offline Steam: the "download" completes at once (LevelInfo.SteamStatsReady).
            "steamrequeststats" => {
                if let Some(l) = vm.level {
                    vm.fire_event(l, "SteamStatsReady", vec![]);
                }
                Value::Void
            }
            "steampurgeworkshop" | "steamviewstorepage" | "steamindicateachievementprogress" => Value::Void,
            "steamunlockachievement" => {
                self.achievements.borrow_mut().insert(s(0).to_ascii_lowercase(), true);
                Value::Void
            }
            "steamlockachievement" => {
                self.achievements.borrow_mut().remove(&s(0).to_ascii_lowercase());
                Value::Void
            }
            "steamgetachievement" => Value::Bool(self.achievements.borrow().contains_key(&s(0).to_ascii_lowercase())),
            "steamgetstatint" => Value::Int(self.stats.borrow().get(&s(0).to_ascii_lowercase()).copied().unwrap_or(0.0) as i32),
            "steamgetstatfloat" => Value::Float(self.stats.borrow().get(&s(0).to_ascii_lowercase()).copied().unwrap_or(0.0)),
            "steamsetstatint" => {
                self.stats.borrow_mut().insert(s(0).to_ascii_lowercase(), i(1) as f32);
                Value::Void
            }
            "steamsetstatfloat" => {
                let f = args.get(1).map(|v| v.as_float()).unwrap_or(0.0);
                self.stats.borrow_mut().insert(s(0).to_ascii_lowercase(), f);
                Value::Void
            }
            "steamresetstats" => {
                self.stats.borrow_mut().clear();
                if args.first().is_some_and(|v| v.as_bool()) {
                    self.achievements.borrow_mut().clear();
                }
                Value::Void
            }
            // Travel.
            "clienttravel" => {
                self.travel.borrow_mut().push(s(0));
                Value::Void
            }
            "updateurl" => {
                self.url_options.borrow_mut().insert(s(0).to_ascii_lowercase(), s(1));
                Value::Void
            }
            // The entry level is the persistent LevelInfo of Entry.fuk; this host has one level.
            "getentrylevel" => Value::Object(vm.level),
            // No navigation network is built by the shell host.
            "buildpathnodelists" | "buildhomenodelists" => Value::Void,
            _ => return None,
        };
        Some(Ok(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dlc_check_uses_disk() {
        let dir = std::env::temp_dir().join(format!("p2shell_{}", std::process::id()));
        let sn = ShellNatives::new(&dir);
        assert!(!sn.dlc_installed(PARADISE_LOST_APPID));
        std::fs::create_dir_all(dir.join("Paradise Lost/System")).unwrap();
        std::fs::write(dir.join("Paradise Lost/System/PLGame.u"), b"x").unwrap();
        assert!(sn.dlc_installed(PARADISE_LOST_APPID));
        assert!(!sn.dlc_installed(12345));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn travel_queue() {
        let sn = ShellNatives::new(".");
        sn.travel.borrow_mut().push("intro.fuk#".into());
        assert_eq!(sn.take_travel().as_deref(), Some("intro.fuk#"));
        assert_eq!(sn.take_travel(), None);
    }
}
