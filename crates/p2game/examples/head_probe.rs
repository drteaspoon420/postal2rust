//! Head/body consistency pass: boots a map, runs a few seconds, and for every pawn with a head
//! (`MyHead`) checks that the head actor sits at the pawn's head bone, that both have render
//! state (mesh, visible) and that the body has an animation instance. Prints anomalies.
//! `head_probe [Map] [--secs N]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let secs: f32 = a.iter().position(|x| x == "--secs").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(5.0);
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    p2game::ragdoll::install(root.clone());
    p2game::ai::install();
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("");
    let mut dudes = 0;
    for f in 0..(secs * 30.0) as usize {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
        let vm = &game.world.vm;
        let n = vm.actors.iter().filter(|&&a| !vm.objects[a].deleted && vm.classes[vm.objects[a].class].name == "AWPostalDude").count();
        if n != dudes {
            let pcs: Vec<String> = vm
                .actors
                .iter()
                .filter(|&&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "PlayerController"))
                .map(|&a| format!("{}:{}:{:?}", vm.object_name(a), vm.state_name(a), vm.get_prop(a, "Pawn").and_then(|v| v.as_obj()).map(|p| vm.object_name(p))))
                .collect();
            println!("frame {f}: {n} AWPostalDude, PCs {pcs:?}");
            dudes = n;
        }
    }
    let vm = &game.world.vm;
    let host = game.host.clone();
    let (mut pawns, mut heads, mut bad) = (0, 0, 0);
    let mut orphans = 0;
    for &p in &vm.actors {
        if vm.objects[p].deleted || !vm.class_is_a(vm.objects[p].class, "Pawn") {
            continue;
        }
        pawns += 1;
        let Some(h) = vm.get_prop(p, "MyHead").and_then(|v| v.as_obj()).filter(|&h| !vm.objects[h].deleted) else { continue };
        heads += 1;
        let pl = vm.actor_loc(p);
        let hl = vm.actor_loc(h);
        let bone = host.with_inst(vm, p, |i| i.get_bone_coords("MALE01 head").or_else(|| i.get_bone_coords("MALE01 neck")).map(|c| c.origin));
        let bw = bone.flatten().map(|o| p2game::host::actor_world(vm, p) * glam::Vec4::from((o, 1.0)));
        let d_pawn = ((pl[0] - hl[0]).powi(2) + (pl[1] - hl[1]).powi(2) + (pl[2] - hl[2]).powi(2)).sqrt();
        let d_bone = bw.map(|b| ((b.x - hl[0]).powi(2) + (b.y - hl[1]).powi(2) + (b.z - hl[2]).powi(2)).sqrt());
        let body_drawn = matches!(vm.get_prop(p, "DrawType"), Some(Value::Byte(2))) && !vm.get_prop(p, "bHidden").is_some_and(|v| v.as_bool());
        let head_drawn = !p2game::ai::render_hidden(vm, h);
        let has_anim = host.anims.borrow().contains_key(&p);
        let base = vm.get_prop(h, "Base").and_then(|v| v.as_obj());
        let anomaly = d_pawn > 120.0 || d_bone.is_some_and(|d| d > 25.0) || (head_drawn && !body_drawn) || !has_anim;
        if head_drawn && !body_drawn {
            orphans += 1;
        }
        if anomaly {
            bad += 1;
            println!(
                "BAD {} ({}) head {}: d_pawn {:.0} d_bone {:?} body_drawn {} (DrawType {:?} hidden {:?}) head_drawn {} anim {} base {:?} phys {:?} state {} ctrl {}",
                vm.object_name(p),
                vm.classes[vm.objects[p].class].name,
                vm.object_name(h),
                d_pawn,
                d_bone.map(|d| d.round()),
                body_drawn,
                vm.get_prop(p, "DrawType"),
                vm.get_prop(p, "bHidden"),
                head_drawn,
                has_anim,
                base.map(|b| vm.object_name(b)),
                vm.get_prop(p, "Physics"),
                vm.state_name(p),
                vm.get_prop(p, "Controller").and_then(|c| c.as_obj()).map(|c| vm.state_name(c)).unwrap_or_default()
            );
        }
    }
    // Heads whose pawn is gone or which nobody owns.
    for &h in &vm.actors {
        if vm.objects[h].deleted || !vm.class_is_a(vm.objects[h].class, "Head") {
            continue;
        }
        let owner = vm.get_prop(h, "Owner").and_then(|v| v.as_obj());
        let owned = owner.is_some_and(|o| !vm.objects[o].deleted && vm.get_prop(o, "MyHead").and_then(|v| v.as_obj()) == Some(h));
        if !owned && !p2game::ai::render_hidden(vm, h) {
            orphans += 1;
            println!("ORPHAN head {} at {:?} owner {:?}", vm.object_name(h), vm.actor_loc(h), owner.map(|o| (vm.object_name(o), vm.objects[o].deleted)));
        }
    }
    println!("HEADS {map}: {pawns} pawns, {heads} with heads, {bad} anomalies, {orphans} visible heads without a visible body");
}
