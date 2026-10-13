//! Parses the user's real User.ini (skipped when the game is not installed).

use p2input::{Bindings, Key};

#[test]
fn real_user_ini_bindings() {
    let root = std::env::var("POSTAL2_DIR").unwrap_or_else(|_| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let path = std::path::Path::new(&root).join("System").join("User.ini");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skipping: {} not found", path.display());
        return;
    };
    let b = Bindings::from_ini(&p2input::bindings::decode_ini(&bytes));
    assert_eq!(b.alias("MoveForward").map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")), Some("Axis aBaseY Speed=+1200.0".into()));
    assert!(b.alias("Fire").unwrap().contains("Button bFire"));
    assert!(b.binding(Key::MOUSE_X).contains("aMouseX"));
    let bound = (1..255u8).filter(|&k| !b.binding(Key(k)).is_empty()).count();
    let aliases = b.aliases.iter().filter(|a| !a.alias.eq_ignore_ascii_case("None")).count();
    println!("User.ini: {bound} bound keys, {aliases} aliases");
    assert!(bound > 30 && aliases > 20);
}
