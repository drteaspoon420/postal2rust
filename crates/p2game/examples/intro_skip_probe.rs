//! Intro skip check (headless): runs `intro?Game=GameTypes.GameSinglePlayer`, optionally pressing
//! skip (`bWantsToSkip`) at the given seconds, and once a second prints the running scene and
//! every live Dude-like pawn (class name containing "Dude", or the player's pawn): bHidden,
//! DrawType, Location, Mesh, whether an animation instance exists and its channel-0 sequence.
//! `intro_skip_probe [--secs N] [--skip T1,T2,...] [--every S]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let secs: f32 = get("--secs").and_then(|s| s.parse().ok()).unwrap_or(120.0);
    let every: f32 = get("--every").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let mut skips: Vec<f32> = get("--skip").map(|s| s.split(',').filter_map(|t| t.parse().ok()).collect()).unwrap_or_default();
    skips.sort_by(|x, y| x.total_cmp(y));
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ai::install();
    p2game::audio::install(root.clone(), None);
    p2game::cinema::install(root.clone());
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path("intro").cloned().expect("intro map");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("?Game=GameTypes.GameSinglePlayer");
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    let dt = 1.0 / 30.0;
    let mut t = 0.0f32;
    let mut next_report = 0.0f32;
    let mut si = 0;
    while t < secs {
        if si < skips.len() && t >= skips[si] {
            si += 1;
            if let Some(pc) = game.player_controller {
                game.world.vm.set_prop(pc, "bWantsToSkip", Value::Byte(1));
                println!("t={t:6.2} SKIP pressed");
            }
        }
        game.tick(dt);
        game.drain_log(false);
        t += dt;
        if let Some((m, o)) = game.take_travel() {
            println!("t={t:6.2} travel requested {m}{o}");
            break;
        }
        if t >= next_report {
            next_report += every;
            report(&game, t);
        }
    }
    report(&game, t);
}

fn report(game: &Game, t: f32) {
    let vm = &game.world.vm;
    let running: Vec<String> = vm
        .actors
        .iter()
        .copied()
        .filter(|&x| !vm.objects[x].deleted && p2game::cinema::is_a(vm, x, "SceneManager") && vm.get_prop(x, "bIsRunning").is_some_and(|v| v.as_bool()))
        .map(|x| format!("{}({:.2})", vm.object_name(x), vm.get_prop(x, "PctSceneComplete").map(|v| v.as_float()).unwrap_or(0.0)))
        .collect();
    let pc = game.player_controller;
    let ppawn = pc.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
    let vt = pc.and_then(|c| vm.get_prop(c, "ViewTarget")).and_then(|v| v.as_obj());
    println!("t={t:6.2} scenes {running:?} pc.Pawn {:?} ViewTarget {:?} pc state {:?}", ppawn.map(|p| vm.object_name(p)), vt.map(|p| vm.object_name(p)), pc.map(|c| vm.state_name(c)));
    let host = game.host.clone();
    let anims = host.anims.borrow();
    for &x in &vm.actors {
        if vm.objects[x].deleted || !vm.class_is_a(vm.objects[x].class, "Pawn") {
            continue;
        }
        let cls = vm.classes[vm.objects[x].class].name.clone();
        if !cls.to_ascii_lowercase().contains("dude") && Some(x) != ppawn {
            continue;
        }
        let p = |k: &str| vm.get_prop(x, k).map(|v| vm.value_string(v)).unwrap_or_default();
        let inst = anims.get(&x);
        let head = vm.get_prop(x, "MyHead").and_then(|v| v.as_obj());
        println!(
            "      {} ({cls}) bHidden {} DrawType {} Loc {} Mesh {} Tag {} anim {} seq {:?} head {:?} headHidden {:?} state {}",
            vm.object_name(x),
            p("bHidden"),
            p("DrawType"),
            p("Location"),
            p("Mesh"),
            p("Tag"),
            inst.is_some(),
            inst.and_then(|i| i.inst.channels.first().and_then(|c| c.sequence_name().map(str::to_string))),
            head.map(|h| vm.object_name(h)),
            head.and_then(|h| vm.get_prop(h, "bHidden")).map(|v| vm.value_string(v)),
            vm.state_name(x)
        );
    }
}
