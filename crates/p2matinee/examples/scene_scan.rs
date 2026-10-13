//! Decodes every SceneManager in every map and plays each scene headless to its end.
//!
//! `cargo run -p p2matinee --example scene_scan [--game dir] [--map name] [-v]`

use p2matinee::{NoActors, SceneEvent, ScenePlayer, SubActionKind, scenes_in};
use std::collections::BTreeMap;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let verbose = a.iter().any(|x| x == "-v" || x == "-vv");
    // -vv: also each action's point and the camera once per second.
    let a_vv = a.iter().any(|x| x == "-vv");
    let game = get("--game").unwrap_or_else(|| r"G:\SteamLibrary\steamapps\common\POSTAL2Complete".into());
    let only = get("--map");
    let mut maps: Vec<_> = std::fs::read_dir(format!(r"{game}\Maps")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("fuk"))).collect();
    maps.sort();
    let (mut n_maps, mut n_scenes, mut n_actions, mut n_subs, mut no_point, mut played) = (0, 0, 0, 0, 0, 0);
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_time = 0.0f32;
    for m in maps {
        let stem = m.file_stem().unwrap().to_string_lossy().to_string();
        if only.as_ref().is_some_and(|o| !o.eq_ignore_ascii_case(&stem)) {
            continue;
        }
        let Ok(pkg) = upkg::Package::open(&m) else { continue };
        let pkg = Arc::new(pkg);
        let scenes = scenes_in(&pkg);
        if scenes.is_empty() {
            continue;
        }
        n_maps += 1;
        for s in scenes {
            n_scenes += 1;
            n_actions += s.actions.len();
            no_point += s.actions.iter().filter(|a| a.point_export.is_none()).count();
            for a in &s.actions {
                for sa in &a.sub_actions {
                    n_subs += 1;
                    let k = match &sa.kind {
                        SubActionKind::Other { class, .. } => class.clone(),
                        k => format!("{k:?}").split([' ', '(', '{']).next().unwrap().to_string(),
                    };
                    *kinds.entry(k).or_default() += 1;
                }
            }
            let mut p = ScenePlayer::new(s.clone());
            if a_vv {
                for (i, a) in s.actions.iter().enumerate() {
                    println!("  action {i} {:?} point {:?} rot {:?} dur {} subs {:?}", a.kind, a.point, a.point_rotation, a.duration, a.sub_actions.iter().map(|x| format!("{:?}", x.kind)).collect::<Vec<_>>());
                }
            }
            total_time += p.total_scene_time();
            p.trigger();
            let mut ev = Vec::new();
            let mut steps = 0;
            let mut path_len = 0.0;
            let mut last = None;
            while steps < 100_000 {
                match p.tick(1.0 / 30.0, &mut NoActors, &mut ev) {
                    Some(v) => {
                        if a_vv && steps % 30 == 0 {
                            println!("    t={:.1} cam {:?} rot {:?}", steps as f32 / 30.0, v.location, v.rotation);
                        }
                        if let Some(l) = last {
                            path_len += v.location.distance(l);
                        }
                        last = Some(v.location);
                    }
                    None => break,
                }
                steps += 1;
            }
            let ended = ev.contains(&SceneEvent::Ended);
            played += ended as usize;
            if verbose {
                let trig: Vec<_> = ev.iter().filter_map(|e| if let SceneEvent::Trigger(t) | SceneEvent::TriggerNext(t) = e { Some(t.as_str()) } else { None }).collect();
                println!(
                    "{stem}.{} tag={} actions={} time={:.1}s path={:.0}uu cinematic={} skip={} next={:?} events={:?}{}",
                    s.name,
                    s.tag,
                    s.actions.len(),
                    p.total_scene_time(),
                    path_len,
                    s.cinematic_view,
                    s.let_player_skip,
                    s.next_scene_tag,
                    trig,
                    if ended { "" } else { " NOT ENDED" }
                );
            }
        }
    }
    println!("SCENES maps={n_maps} scenes={n_scenes} actions={n_actions} (without IntPoint {no_point}) subactions={n_subs} total={:.0}s played_to_end={played}/{n_scenes}", total_time);
    println!("SUBACTIONS {kinds:?}");
}
