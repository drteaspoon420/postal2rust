//! Intro diagnostics: boots `intro?Game=GameTypes.GameSinglePlayer` headless and reports the
//! GameInfo's `Days` objects, the scene managers, and the script errors with context.
//! Usage: intro_probe [URL] [frames] [via-startup]
//! With a third argument the URL is reached by traveling from the startup map (as New Game does),
//! and the player gets `TravelPostAccept` like the front end sends.
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let url = a.first().cloned().unwrap_or_else(|| "intro?Game=GameTypes.GameSinglePlayer".into());
    let frames: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let root = p2game::game_root(None);
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    p2game::cinema::install(root.clone());
    let mut world = World::new(cfg).unwrap();
    let (map, opts) = p2game::boot::split_url(&url);
    let via = a.get(2).is_some();
    let first = if via { "Startup".to_string() } else { map.clone() };
    let path = world.files.path(&first).cloned().unwrap();
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let gc = if via { None } else { p2game::boot::url_option(&opts, "Game") };
    let mut game = Game::new(world, lv, gc.as_deref());
    game.begin_play(if via { "" } else { &opts });
    if via {
        for _ in 0..5 {
            game.tick(1.0 / 30.0);
        }
        let days = |g: &Game| {
            let vm = &g.world.vm;
            g.game_info.and_then(|gi| vm.get_prop(gi, "Days").cloned()).map(|d| format!("{d:?}")).unwrap_or_default()
        };
        println!("startup: base_objects {} objects {} gameinfo {} days {}", game.level.base_objects, game.world.vm.objects.len(), game.game_class, days(&game));
        let d = game.world.vm.class_default("GameTypes", "GameSinglePlayer", "Days");
        println!("GameSinglePlayer default Days before travel: {d:?}");
        if let Ok(Some(uscript::vm::Value::Array(a))) = &d {
            for v in a {
                if let Some(o) = v.as_obj() {
                    let vm = &game.world.vm;
                    println!("  before travel obj {o}: {} class {}", vm.object_name(o), vm.classes[vm.objects[o].class].name);
                }
            }
        }
        assert!(game.travel(&map, &opts), "travel failed");
        println!("traveled to {} ({}) base_objects {} days {}", game.level.map.name, game.game_class, game.level.base_objects, days(&game));
        let d = game.world.vm.class_default("GameTypes", "GameSinglePlayer", "Days");
        println!("GameSinglePlayer default Days after travel: {d:?}");
        if let Some(pc) = game.player_controller {
            game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
        }
        for l in std::mem::take(&mut game.world.vm.log) {
            if l.contains("rror") || l.contains("not found") || l.contains("not in object") {
                println!("travel: {l}");
            }
        }
    }
    let vm = &game.world.vm;
    let g = game.game_info.unwrap();
    println!("GameInfo {} ({})", vm.object_name(g), game.game_class);
    if let Some(uscript::vm::Value::Array(days)) = vm.get_prop(g, "Days") {
        for (i, d) in days.iter().enumerate() {
            match d.as_obj() {
                Some(o) => println!("  Days[{i}] = obj {o} {} class {}", vm.object_name(o), vm.classes[vm.objects[o].class].name),
                None => println!("  Days[{i}] = {d:?}"),
            }
        }
    }
    for &a in &vm.actors {
        if p2game::cinema::is_a(vm, a, "SceneManager") {
            println!("SceneManager {} running {:?} trigger tag {:?}", vm.object_name(a), vm.get_prop(a, "bIsRunning"), vm.get_prop(a, "Tag"));
        }
    }
    for i in 0..frames {
        game.tick(1.0 / 30.0);
        for l in std::mem::take(&mut game.world.vm.log) {
            if l.contains("rror") || l.contains("not found") || l.contains("not in object") {
                println!("frame {i}: {l}");
            }
        }
        if let Some((m, o)) = game.take_travel() {
            println!("frame {i}: travel requested {m}{o}");
            break;
        }
    }
    let vm = &game.world.vm;
    println!("script errors:");
    for (k, v) in &vm.script_errors {
        println!("  {v:>5} {k}");
    }
    for &a in &vm.actors {
        if p2game::cinema::is_a(vm, a, "SceneManager") {
            println!("SceneManager {} running {:?} pct {:?}", vm.object_name(a), vm.get_prop(a, "bIsRunning"), vm.get_prop(a, "PctSceneComplete"));
        }
    }
}
