//! Save/load round trip and original-save loading.
//!
//! `save_roundtrip [Map] [--frames N] [--original G:\...\Save19.usa]`
//!
//! 1. Boots `Map` (default `suburbs-3`), runs N frames, takes a snapshot, writes `SaveRT.usa`
//!    (+ `.p2r`) into a temp dir with `p2game::save`, re-parses the package with `upkg`, loads it
//!    back into the same VM and diffs the snapshots (class, state, every non-default property of
//!    every actor). Then ticks 30 frames to show the loaded level runs.
//! 2. With `--original`, loads a save written by the original game and ticks it.
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use p2game::save;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let frames: u32 = get("--frames").and_then(|s| s.parse().ok()).unwrap_or(60);
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("");
    for _ in 0..frames {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
    }
    let before = save::snapshot(&game.world.vm);
    let dir = std::env::temp_dir().join("p2r_save_test");
    let t0 = std::time::Instant::now();
    let st = save::save_slot(&game, &dir, 99, "roundtrip").expect("save failed");
    let file = save::slot_path(&dir, 99);
    println!("saved {} in {:.2}s: {st:?}", file.display(), t0.elapsed().as_secs_f32());
    let pkg = upkg::Package::open(&file).expect("written save does not parse");
    println!("re-parsed: {} names, {} imports, {} exports", pkg.names.len(), pkg.imports.len(), pkg.exports.len());
    let t0 = std::time::Instant::now();
    // Raw restore by default (no PostLoadGame, which re-runs script setup and changes state on
    // purpose); `--post-load` compares after the events as the game would run them.
    let post = a.iter().any(|x| x == "--post-load");
    let ls = save::load_file_with(&mut game, &file, post).expect("load failed");
    println!("loaded in {:.2}s: {ls:?}", t0.elapsed().as_secs_f32());
    let after = save::snapshot(&game.world.vm);
    let d = save::diff(&before, &after);
    let props: usize = before.values().map(|m| m.len()).sum();
    println!("ROUNDTRIP actors {} -> {}, compared {props} values, {} differences", before.len(), after.len(), d.len());
    let mut by_prop: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, p, _, _) in &d {
        *by_prop.entry(p.clone()).or_default() += 1;
    }
    println!("differences by property: {by_prop:?}");
    for (actor, p, x, y) in d.iter().take(12) {
        println!("  {actor}.{p}: {x} -> {y}");
    }
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
    }
    println!("after load + 30 frames: {} live actors, player {:?}, script errors {}", game.live_actors(), game.player_controller, game.world.vm.script_errors.values().sum::<u64>());

    if let Some(orig) = get("--original") {
        let t0 = std::time::Instant::now();
        match save::load_file(&mut game, std::path::Path::new(&orig)) {
            Ok(ls) => {
                println!("ORIGINAL {orig} loaded in {:.2}s: {ls:?}", t0.elapsed().as_secs_f32());
                for _ in 0..30 {
                    game.tick(1.0 / 30.0);
                    game.drain_log(false);
                }
                println!("after 30 frames: {} live actors, script errors {}", game.live_actors(), game.world.vm.script_errors.values().sum::<u64>());
            }
            Err(e) => println!("ORIGINAL load failed: {e}"),
        }
    }
}
