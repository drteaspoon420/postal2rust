//! Fat pawn animation check (headless): boots a map, finds pawns whose mesh starts with "Fat",
//! prints their linked animation sets and current channel-0 sequence, and compares each bone's
//! bind (ref pose) translation length with the playing animation's translation length. Large
//! ratios mean an animation authored for another skeleton (e.g. Avg) is stretching the fat mesh.
//! `fat_probe [Map] [--secs N] [--day N] [--all]`
use glam::Vec3;
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let secs: f32 = get("--secs").and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let all = a.iter().any(|x| x == "--all");
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ai::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    let day = get("--day").unwrap_or_else(|| "1".into());
    game.begin_play(&format!("?SetDay={day}"));
    if let Some(pc) = game.player_controller {
        game.world.vm.fire_event(pc, "TravelPostAccept", vec![]);
    }
    for _ in 0..(secs * 30.0) as usize {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
    }
    let host = game.host.clone();
    let vm = &game.world.vm;
    let mut shown = std::collections::HashSet::new();
    for &p in &vm.actors {
        if vm.objects[p].deleted || !vm.class_is_a(vm.objects[p].class, "Pawn") {
            continue;
        }
        let mesh = vm.get_prop(p, "Mesh").and_then(|v| v.as_resource().map(str::to_string)).unwrap_or_default();
        let short = mesh.rsplit('.').next().unwrap_or("").to_string();
        if !short.to_ascii_lowercase().starts_with("fat") {
            continue;
        }
        let cls = vm.classes[vm.objects[p].class].name.clone();
        if !all && !shown.insert(format!("{cls}|{mesh}")) {
            continue;
        }
        let anims = host.anims.borrow();
        let Some(x) = anims.get(&p) else {
            println!("FAT {} ({cls}) mesh {mesh}: no anim instance", vm.object_name(p));
            continue;
        };
        let inst = &x.inst;
        let sets: Vec<String> = inst.anim_sets().map(|s| s.name.clone()).collect();
        let seq = inst.channels.first().and_then(|c| c.seq.clone());
        println!("FAT {} ({cls}) mesh {mesh} bIsFat {:?} sets {:?} ch0 {:?} from {:?}", vm.object_name(p), vm.get_prop(p, "bIsFat"), sets, inst.channels.first().and_then(|c| c.sequence_name()), seq.as_ref().map(|(l, _)| l.set.name.clone()));
        if let Some((l, s)) = seq {
            let sk = &inst.skeleton;
            let mut out = vec![None; sk.len()];
            l.sample(s, inst.channels[0].frame, false, &mut out);
            for b in 0..sk.len() {
                let bind = sk.ref_pose[b].pos.length();
                let Some(t) = out[b] else { continue };
                let anim = t.pos.length();
                if bind > 0.5 && ((anim / bind) > 1.15 || (anim / bind) < 0.87) {
                    println!("   bone {:2} {:22} bind {:7.2} anim {:7.2} ratio {:.2}  bindpos {:?} animpos {:?}", b, sk.names[b], bind, anim, anim / bind, sk.ref_pose[b].pos, Vec3::from(t.pos));
                }
            }
        }
    }
}
