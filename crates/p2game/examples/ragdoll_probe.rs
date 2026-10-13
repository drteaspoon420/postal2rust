//! Ragdoll end-to-end: boots a map, kills NPCs through script `TakeDamage` and checks that the
//! original death code puts them in PHYS_KarmaRagDoll and the ragdoll runs and settles.
//! `ragdoll_probe [Map] [--kills N]`
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::Value;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let map = a.first().filter(|x| !x.starts_with("--")).cloned().unwrap_or_else(|| "suburbs-3".into());
    let kills: usize = a.iter().position(|x| x == "--kills").and_then(|i| a.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(3);
    let root = p2game::game_root(None);
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let rd = p2game::ragdoll::install(root.clone()).expect("ragdolls disabled");
    // With AI perception on, as in the game (pawns out of view go into stasis).
    if a.iter().any(|x| x == "--ai") {
        p2game::ai::install();
    }
    println!("karma assets: {}", rd.assets.len());
    let cfg = BootConfig::load(&root.join("System")).unwrap();
    let mut world = World::new(cfg).unwrap();
    let path = world.files.path(&map).cloned().expect("map not found");
    let lv = world.load_level(Arc::new(upkg::Package::open(path).unwrap()));
    let mut game = Game::new(world, lv, Some("GameTypes.GameSinglePlayer"));
    game.begin_play("");
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
        for l in game.world.vm.log.iter().filter(|l| l.starts_with("ragdoll")) {
            println!("  {l}");
        }
        game.drain_log(false);
    }
    let vm = &mut game.world.vm;
    let dmg = vm.find_class_by_name("BulletDamage").ok();
    let player_pawn = game.player_controller.and_then(|pc| vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()));
    let victims: Vec<usize> = vm
        .actors
        .iter()
        .copied()
        .filter(|&x| !vm.objects[x].deleted && Some(x) != player_pawn && vm.class_is_a(vm.objects[x].class, "P2MoCapPawn") && vm.get_prop(x, "Health").is_some_and(|h| h.as_int() > 0) && game.host.anims.borrow().contains_key(&x))
        .take(kills)
        .collect();
    let mut start_z = Vec::new();
    // Shot direction per victim: bullets travel from the shooter (the player) to the victim, so
    // the momentum is along shooter->victim and the body should end up farther along it.
    let mut shot_dir = Vec::new();
    let mut start_xy = Vec::new();
    let pl = player_pawn.map(|p| vm.actor_loc(p)).unwrap_or_default();
    // The pistol's real hit momentum (PistolBulletAmmoInv: TakeDamage(.., MomentumHitMag * X, ..)).
    let ammo = vm.find_class_by_name("PistolBulletAmmoInv").ok();
    let hit_mag = ammo.and_then(|c| vm.default_value(c, "MomentumHitMag")).map(|v| v.as_float()).unwrap_or(20000.0);
    println!("pistol MomentumHitMag {hit_mag}");
    // CapKarmaMomentum check (P2MoCapPawn): (-15000, 2550, 0) BulletDamage must become
    // 0.5x then per-axis clamp to +-5000: (-5000, 1275, 0).
    if let Some(&v) = victims.first() {
        let args = vec![Value::Vector([-30000.0, 5100.0, 0.0]), Value::Class(dmg), Value::Float(1.0), Value::Float(1.0), Value::Vector([0.0; 3])];
        let r = vm.call_event_out(v, "CapKarmaMomentum", args);
        println!("CapKarmaMomentum((-30000,5100,0)) -> {:?} (expected (-5000, 2550, 0))", r.map(|o| o.map(|(_, out)| out.first().cloned())));
    }
    for &v in &victims {
        let loc = vm.actor_loc(v);
        start_z.push(loc[2]);
        start_xy.push([loc[0], loc[1]]);
        let d = glam::Vec2::new(loc[0] - pl[0], loc[1] - pl[1]).normalize_or(glam::Vec2::X);
        shot_dir.push(d);
        // Pistol-ish momentum (P2 BulletDamage: a few thousand along the shot).
        let mom = [d.x * hit_mag, d.y * hit_mag, 0.0];
        println!("victim {} Mass {:?} DeathVelMag {:?}", vm.object_name(v), vm.get_prop(v, "Mass"), vm.get_prop(v, "DeathVelMag"));
        let class = vm.objects[v].class;
        let r: Result<(), ()> = Ok(());
        vm.fire_event(v, "TakeDamage", vec![Value::Int(1000), Value::Object(player_pawn), Value::Vector([loc[0], loc[1], loc[2] + 30.0]), Value::Vector(mom), Value::Class(dmg)]);
        if vm.get_prop(v, "Health").is_some_and(|h| h.as_int() > 0) {
            // KParams comes from the script itself (P2MoCapPawn.GetKarmaSkeleton through the local
            // player's Viewport, which p2game::ragdoll provides headless).
            // Lethal damage path of Pawn.TakeDamage: Health <= 0, TearOffMomentum, Died().
            vm.set_prop(v, "Health", Value::Int(0));
            vm.set_prop(v, "TearOffMomentum", Value::Vector(mom));
            let killer = game.player_controller;
            vm.fire_event(v, "Died", vec![Value::Object(killer), Value::Class(dmg), Value::Vector([loc[0], loc[1], loc[2] + 30.0])]);
        }
        let kp = vm.get_prop(v, "KParams").and_then(|x| x.as_obj());
        println!("  sent momentum {mom:?}, TearOffMomentum now {:?}", vm.get_prop(v, "TearOffMomentum"));
        println!("killed {} ({}) at {:?}: {:?} health now {:?} state {} physics {:?} KParams {:?} KSkeleton {:?}", vm.object_name(v), vm.classes[class].name, loc, r.err(), vm.get_prop(v, "Health"), vm.state_name(v), vm.get_prop(v, "Physics"), kp.map(|k| vm.object_name(k)), kp.and_then(|k| vm.get_prop(k, "KSkeleton")));
    }
    if let Some(pc) = game.player_controller {
        let vm = &game.world.vm;
        println!("player RagdollSkels {:?} RagdollMax {:?} Player {:?}", vm.get_prop(pc, "RagdollSkels").map(|v| match v { Value::Array(a) => a.len(), _ => 0 }), vm.get_prop(pc, "RagdollMax"), vm.get_prop(pc, "Player").and_then(|p| p.as_obj()).map(|p| vm.classes[vm.objects[p].class].name.clone()));
    }
    for l in game.world.vm.log.iter().filter(|l| l.contains("ragdoll") || l.contains("Ragdoll") || l.contains("Karma")).take(20) {
        println!("  log: {l}");
    }
    for f in 0..120 {
        game.tick(1.0 / 30.0);
        game.drain_log(false);
        if f % 30 == 29 {
            let vm = &game.world.vm;
            let line: Vec<String> = victims.iter().map(|&v| format!("{} phys={} z={:.0}", vm.object_name(v), vm.get_prop(v, "Physics").map(|p| p.as_int()).unwrap_or(-1), vm.actor_loc(v)[2])).collect();
            println!("t={:.1}s {:?} live={} stats={:?}", (f + 1) as f32 / 30.0, line, rd.live.borrow().len(), rd.stats.borrow());
        }
    }
    let vm = &game.world.vm;
    for (i, &v) in victims.iter().enumerate() {
        let pose = game.host.anims.borrow().get(&v).map(|x| x.inst.has_physics_pose());
        let l = vm.actor_loc(v);
        let root = rd.live.borrow().get(&v).map(|x| x.doll.root_position());
        let root_along = root.map(|r| glam::Vec2::new(r.x - start_xy[i][0], r.y - start_xy[i][1]).dot(shot_dir[i]));
        // Where the head came to rest relative to the standing position: positive = the body
        // fell away from the shooter (the original), negative = toward the shooter.
        let head_bone = game.host.anims.borrow().get(&v).and_then(|x| {
            let n = &x.inst.skeleton.names;
            n.iter().position(|s| s.to_ascii_lowercase().contains("head")).or_else(|| n.iter().position(|s| s.to_ascii_lowercase().contains("neck")))
        });
        let head = head_bone.and_then(|b| rd.live.borrow().get(&v).and_then(|x| x.doll.bone_origin(b)));
        let head_along = head.map(|h| glam::Vec2::new(h.x - start_xy[i][0], h.y - start_xy[i][1]).dot(shot_dir[i]));
        println!("{} ragdoll root moved {root_along:?}uu, head lies {head_along:?}uu along the shot", vm.object_name(v));
        // Settled pose quality: worst joint anchor gap (UU, stretched limbs) and worst limit
        // violation (rad, joints bent past their .ka limits).
        if let Some((ae, lv)) = rd.live.borrow().get(&v).map(|x| x.doll.joint_errors().iter().fold((0.0f32, 0.0f32), |(a, b), &(_, e, l)| (a.max(e), b.max(l)))) {
            println!("JOINTS {} max anchor gap {ae:.2}uu max limit violation {lv:.3}rad", vm.object_name(v));
        }
        let along = glam::Vec2::new(l[0] - start_xy[i][0], l[1] - start_xy[i][1]).dot(shot_dir[i]);
        println!("{}: start z {:.0}, now z {:.0}, moved {along:.0}uu along the shot (positive = away from the shooter), physics pose {:?}, state {}", vm.object_name(v), start_z[i], l[2], pose, vm.state_name(v));
        // The head actor (MyHead) must stay on the ragdoll's head bone until it is knocked off.
        if let Some(h) = vm.get_prop(v, "MyHead").and_then(|x| x.as_obj()).filter(|&h| !vm.objects[h].deleted) {
            let hl = glam::Vec3::from(vm.actor_loc(h));
            let d = head.map(|b| (b - hl).length());
            println!("HEAD {} {} d_to_head_bone {d:?} phys {:?} base {:?} bone {:?} at {hl:?}", vm.object_name(v), vm.object_name(h), vm.get_prop(h, "Physics").map(|p| p.as_int()), vm.get_prop(h, "Base").and_then(|b| b.as_obj()).map(|b| vm.object_name(b)), vm.get_prop(h, "AttachmentBone"));
        }
    }
    println!("RAGDOLL started={} asleep={} missing_asset={} script errors {}", rd.stats.borrow().started, rd.stats.borrow().asleep, rd.stats.borrow().missing_asset, vm.script_errors.values().sum::<u64>());
}
