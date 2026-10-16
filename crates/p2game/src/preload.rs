//! Destination preload: while the player is near a level exit (a Teleporter / Telepad whose URL
//! names another map), worker threads build that map's collision world (see
//! [`crate::physics::preload`]) and decode the music files it names, so a travel through it does
//! not wait for them (collision ~0.3 s; a first PlayMusic decoded a 5 MB Ogg on the game thread,
//! ~0.5 s, a few ticks after arrival).
//!
//! Only engine-side derived data is prepared ahead; nothing in the VM changes, so script state,
//! inventory carry-over and day logic are the same as without it. `P2GAME_NO_PRELOAD=1` turns it
//! off (`P2GAME_NO_PREFETCH=1` also disables the at-travel collision prefetch).
//! `P2GAME_PRELOAD_TRACE=1` logs PRELOAD lines.

use crate::host::GameHost;
use uscript::vm::{ObjId, Value, Vm};

/// Distance (Unreal units) from a level exit at which its destination starts preloading.
const NEAR: f32 = 2500.0;
/// Frames between proximity checks.
const EVERY: u32 = 15;

fn enabled() -> bool {
    std::env::var_os("P2GAME_NO_PRELOAD").is_none()
}

/// Registers the per-level hook (call once at startup).
pub fn install() {
    crate::host::register_level_hook(level_hook);
}

/// Songs decoded by a preload worker, waiting to go into the audio cache (main thread).
static DECODED: std::sync::Mutex<Vec<(std::path::PathBuf, Option<std::sync::Arc<p2sound::SoundData>>)>> = std::sync::Mutex::new(Vec::new());
/// Music files a preload worker has taken on (decoded or decoding).
static SEEN: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Moves songs decoded ahead into the audio cache, so the next `PlayMusic` of one finds it
/// instead of decoding it on the game thread (~0.5 s for a 5 MB Ogg). Called every frame by the
/// preload hook and before the music natives resolve a song.
pub fn take_decoded_music() {
    let ready = std::mem::take(&mut *DECODED.lock().unwrap_or_else(|e| e.into_inner()));
    if ready.is_empty() {
        return;
    }
    if let Some(a) = crate::audio::audio() {
        let mut c = a.cache.borrow_mut();
        for (p, d) in ready {
            c.insert_music(p, d);
        }
    }
}

/// Music file names (`*.ogg`) a map package names (PlayMusic arguments are saved as strings in
/// the actors' properties and script, so a byte scan finds them).
fn map_songs(map: &std::path::Path) -> Vec<String> {
    let Ok(b) = std::fs::read(map) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(rel) = b[i..].windows(4).position(|w| w.eq_ignore_ascii_case(b".ogg")) {
        let end = i + rel + 4;
        let mut start = i + rel;
        while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_' || b[start - 1] == b'-') {
            start -= 1;
        }
        if start < i + rel {
            let s = String::from_utf8_lossy(&b[start..end]).to_string();
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
                out.push(s);
            }
        }
        i = end;
    }
    out
}

/// Map name of a teleporter URL (`Suburbs-1#pod1a?peer` -> `Suburbs-1`, `police.fuk#cell1` ->
/// `police`: the package name), if it names one.
pub fn url_map(url: &str) -> Option<&str> {
    let m = url.trim().split(['#', '?']).next()?.trim();
    if m.is_empty() || m.contains(':') {
        return None;
    }
    Some(std::path::Path::new(m).file_stem().and_then(|s| s.to_str()).unwrap_or(m))
}

fn level_hook(host: &GameHost, vm: &mut Vm) {
    if !enabled() {
        return;
    }
    let current = host.map_name.borrow().clone();
    // Level exits of this map: Teleporters whose URL names another map.
    let exits: Vec<(ObjId, String)> = vm
        .actors
        .iter()
        .copied()
        .filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Teleporter"))
        .filter_map(|a| {
            let url = match vm.get_prop(a, "URL") {
                Some(Value::Str(s)) => s.clone(),
                _ => return None,
            };
            let m = url_map(&url)?;
            (!m.eq_ignore_ascii_case(&current)).then(|| (a, m.to_string()))
        })
        .collect();
    if exits.is_empty() {
        return;
    }
    let lib = host.lib.clone();
    let trace = std::env::var_os("P2GAME_PRELOAD_TRACE").is_some();
    let mut frame = 0u32;
    let mut music_done: Vec<String> = Vec::new();
    host.add_frame_hook("preload", move |vm, _dt| {
        frame += 1;
        take_decoded_music();
        if frame % EVERY != 0 {
            return;
        }
        let Some(pawn) = crate::ai::local_player_controller(vm).and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj()) else { return };
        let p = vm.actor_loc(pawn);
        let nearest = exits
            .iter()
            .filter(|(a, _)| !vm.objects[*a].deleted)
            .map(|(a, m)| {
                let l = vm.actor_loc(*a);
                (((l[0] - p[0]).powi(2) + (l[1] - p[1]).powi(2) + (l[2] - p[2]).powi(2)).sqrt(), m)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((d, m)) = nearest else { return };
        if d > NEAR {
            return;
        }
        let Some(path) = lib.map_path(m) else { return };
        if !music_done.iter().any(|s| s.eq_ignore_ascii_case(m)) {
            music_done.push(m.clone());
            if let Some(a) = crate::audio::audio() {
                let game = a.game_dir.clone();
                let map_path = path.clone();
                let _ = std::thread::Builder::new().name("music-preload".into()).spawn(move || {
                    for song in map_songs(&map_path) {
                        let Some(p) = p2sound::load::music_path(&game, &song) else { continue };
                        {
                            // Each file is decoded ahead at most once per process (the audio cache
                            // keeps decoded songs for the session).
                            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
                            if seen.contains(&p) {
                                continue;
                            }
                            seen.push(p.clone());
                        }
                        let d = p2sound::load::decode_file(&p).map(std::sync::Arc::new);
                        if trace {
                            println!("PRELOAD music {} decoded", p.display());
                        }
                        DECODED.lock().unwrap_or_else(|e| e.into_inner()).push((p, d));
                    }
                });
            }
        }
        if crate::physics::preloading().is_some_and(|n| n.eq_ignore_ascii_case(m)) {
            return;
        }
        if trace {
            println!("PRELOAD {m} (exit {d:.0} uu away)");
        }
        crate::physics::preload(lib.clone(), m, path);
    });
}

#[cfg(test)]
mod tests {
    use super::url_map;

    #[test]
    fn songs_in_bytes() {
        let dir = std::env::temp_dir().join("p2game_preload_songs.bin");
        std::fs::write(&dir, b"\x00\x05xx\x10OnSilentWings.ogg\x00junk AFTB-StoleMyFaith.OGG\x00.ogg OnSilentWings.ogg").unwrap();
        assert_eq!(super::map_songs(&dir), vec!["OnSilentWings.ogg".to_string(), "AFTB-StoleMyFaith.OGG".to_string()]);
        let _ = std::fs::remove_file(dir);
    }

    #[test]
    fn teleporter_urls() {
        assert_eq!(url_map("Suburbs-1#pod1a?peer"), Some("Suburbs-1"));
        assert_eq!(url_map("police.fuk#cell1"), Some("police"));
        assert_eq!(url_map(""), None);
        assert_eq!(url_map("#pod1"), None);
        assert_eq!(url_map("http://x"), None);
    }
}
