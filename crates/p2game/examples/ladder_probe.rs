//! Ladder check (headless): boots a map as the AW game on a day, runs N seconds and prints, once a
//! second, every pawn whose physics is Ladder or whose Z rose more than 150 uu above its start:
//! physics, location, velocity, OnLadder, controller state and MoveTarget.
//! `ladder_probe [Map] [--day N] [--secs S] [--game Class]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::collections::HashMap;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "VincesHouse".into());
    let secs: f32 = get("--secs").and_then(|s| s.parse().ok()).unwrap_or(60.0);
    let day = get("--day").unwrap_or_else(|| "7".into());
    let gc = get("--game").unwrap_or_else(|| "GameTypes.AWPGameInfo".into());
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ai::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some(&gc));
    game.begin_play(&format!("?SetDay={day}"));
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    let mut start_z: HashMap<usize, f32> = HashMap::new();
    let dt = 1.0 / 30.0;
    let mut t = 0.0;
    let mut next = 1.0;
    let mut reported = std::collections::HashSet::new();
    let mut climbed = std::collections::HashSet::new();
    while t < secs {
        game.tick(dt);
        game.drain_log(false);
        t += dt;
        if t < next {
            continue;
        }
        next += 1.0;
        let vm = &game.world.vm;
        for &p in &vm.actors {
            if vm.objects[p].deleted || !vm.class_is_a(vm.objects[p].class, "Pawn") {
                continue;
            }
            let loc = vm.actor_loc(p);
            let z0 = *start_z.entry(p).or_insert(loc[2]);
            let phys = vm.get_prop(p, "Physics").map(|v| v.as_int()).unwrap_or(0);
            if phys != 11 && loc[2] - z0 < 150.0 && !climbed.contains(&p) {
                continue;
            }
            let c = vm.get_prop(p, "Controller").and_then(|v| v.as_obj());
            let mt = c.and_then(|c| vm.get_prop(c, "MoveTarget")).and_then(|v| v.as_obj());
            let s = |o: Option<usize>| o.map(|o| vm.object_name(o)).unwrap_or_default();
            if phys == 11 {
                climbed.insert(p);
            }
            if reported.insert(p) || climbed.contains(&p) {
                println!(
                    "t={t:5.1} {} ({}) phys {phys} loc {:?} dz {:.0} vel {} OnLadder {} ctrl {} state {} MoveTarget {} {:?}",
                    vm.object_name(p),
                    vm.classes[vm.objects[p].class].name,
                    loc.map(|v| v as i32),
                    loc[2] - z0,
                    vm.get_prop(p, "Velocity").map(|v| vm.value_string(v)).unwrap_or_default(),
                    s(vm.get_prop(p, "OnLadder").and_then(|v| v.as_obj())),
                    s(c),
                    c.map(|c| vm.state_name(c)).unwrap_or_default(),
                    s(mt),
                    mt.map(|m| vm.actor_loc(m).map(|v| v as i32))
                );
            }
        }
    }
}
