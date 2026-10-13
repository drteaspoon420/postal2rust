//! Config persistence: SaveConfig / StaticSaveConfig write to the user directory, values are read
//! back on the next VM, and the game install is never written. Skipped without the game.

use std::path::PathBuf;
use uscript::config::ConfigSource;
use uscript::vm::{Value, Vm};

fn system_dir() -> Option<PathBuf> {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let p = PathBuf::from(root).join("System");
    p.join("Core.u").is_file().then_some(p)
}

struct Fixed;
impl ConfigSource for Fixed {
    fn values(&self, file: &str, section: &str, key: &str) -> Vec<(usize, String)> {
        if !file.eq_ignore_ascii_case("Postal2.ini") {
            return Vec::new();
        }
        match (section.to_ascii_lowercase().as_str(), key.to_ascii_lowercase().as_str()) {
            ("engine.gameinfo", "gorelevel") => vec![(0, "2".into())],
            ("engine.engine", "viewportmanager") => vec![(0, "WinDrv.WindowsClient".into())],
            ("windrv.windowsclient", "brightness") => vec![(0, "0.500000".into())],
            _ => Vec::new(),
        }
    }
}

#[test]
fn save_config_round_trip_in_user_dir() {
    let Some(sys) = system_dir() else { return };
    let dir = std::env::temp_dir().join(format!("p2cfg_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut vm = Vm::new(&sys).unwrap();
    vm.set_user_config_dir(&dir).unwrap();
    // Refuses the install.
    assert!(vm.set_user_config_dir(sys.join("..").join("userstuff")).is_err());
    vm.set_user_config_dir(&dir).unwrap();
    vm.set_config_source(Box::new(Fixed));
    vm.init_world(None).unwrap();
    let gi = vm.find_class("Engine", "GameInfo").unwrap();
    // Base layer applied at spawn.
    assert_eq!(vm.default_value(gi, "GoreLevel"), Some(&Value::Int(2)));
    let game = vm.level.and_then(|l| vm.get_prop(l, "Game").and_then(|v| v.as_obj())).unwrap();
    vm.set_prop(game, "GoreLevel", Value::Int(1));
    vm.call(gi, Some(game), "SaveConfig", vec![]).unwrap();
    let text = std::fs::read_to_string(dir.join("Postal2.ini")).unwrap();
    assert!(text.contains("[Engine.GameInfo]") && text.contains("GoreLevel=1"), "{text}");
    // A fresh VM reads the user layer over the base layer.
    let mut vm2 = Vm::new(&sys).unwrap();
    vm2.set_user_config_dir(&dir).unwrap();
    vm2.set_config_source(Box::new(Fixed));
    let gi2 = vm2.find_class("Engine", "GameInfo").unwrap();
    vm2.apply_class_config(gi2);
    assert_eq!(vm2.default_value(gi2, "GoreLevel"), Some(&Value::Int(1)));
    // ResetConfig drops the user value.
    let o = vm2.new_object(gi2);
    vm2.call(gi2, Some(o), "ResetConfig", vec![]).unwrap();
    assert_eq!(vm2.default_value(gi2, "GoreLevel"), Some(&Value::Int(2)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn console_get_set_reach_live_objects_and_user_ini() {
    let Some(sys) = system_dir() else { return };
    let dir = std::env::temp_dir().join(format!("p2cfg_getset_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut vm = Vm::new(&sys).unwrap();
    vm.set_user_config_dir(&dir).unwrap();
    vm.set_config_source(Box::new(Fixed));
    vm.init_world(None).unwrap();
    let game = vm.level.and_then(|l| vm.get_prop(l, "Game").and_then(|v| v.as_obj())).unwrap();
    // A menu sets a globalconfig value as the shell does for the difficulty.
    assert_eq!(vm.console_get_set("set Engine.GameInfo GameDifficulty 7").as_deref(), Some("1"));
    assert_eq!(vm.get_prop(game, "GameDifficulty"), Some(&Value::Float(7.0)), "live object updated");
    assert_eq!(vm.console_get_set("get Engine.GameInfo GameDifficulty").as_deref(), Some("7.000000"));
    let text = std::fs::read_to_string(dir.join("Postal2.ini")).unwrap();
    assert!(text.contains("GameDifficulty=7.000000"), "{text}");
    // Unknown property: empty answer.
    assert_eq!(vm.console_get_set("set Engine.GameInfo NoSuchProp 1").as_deref(), Some(""));
    // `ini:` paths go through the class named by the system ini, user layer over base.
    assert_eq!(vm.console_get_set("get ini:Engine.Engine.ViewportManager Brightness").as_deref(), Some("0.500000"));
    assert_eq!(vm.console_get_set("set ini:Engine.Engine.ViewportManager Brightness 0.7").as_deref(), Some("1"));
    assert_eq!(vm.console_get_set("get ini:Engine.Engine.ViewportManager Brightness").as_deref(), Some("0.7"));
    assert_eq!(vm.console_get_set("get ini:Engine.Engine.ViewportManager NoSuchKey").as_deref(), Some(""));
    let _ = std::fs::remove_dir_all(&dir);
}
