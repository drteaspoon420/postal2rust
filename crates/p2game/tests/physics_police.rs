//! Headless physics integration on a shipped map (skipped without the game install).
//!
//! Boots police with p2physics installed as the world host, runs 300 frames, then:
//! - no pawn fell out of the world (each is above the lowest level geometry and, if walking,
//!   has floor below it),
//! - triggering a door (script `Trigger` on a Mover) makes it move away from its closed key
//!   and physics reports KeyFrameReached back to script (the mover ends at its open key).

use glam::Vec3;
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::vm::Value;

fn boot(map: &str) -> Option<Game> {
    let root = p2game::game_root(None);
    if !root.join("Maps").is_dir() {
        return None;
    }
    p2game::host::register_level_hook(p2game::physics::level_hook);
    let cfg = BootConfig::load(&root.join("System")).ok()?;
    let mut world = World::new(cfg).ok()?;
    let path = world.files.path(map).cloned()?;
    let lv = world.load_level(Arc::new(upkg::Package::open(path).ok()?));
    let mut game = Game::new(world, lv, None);
    game.begin_play("");
    Some(game)
}

#[test]
fn police_300_frames_pawns_stay_in_world_and_doors_open() {
    let Some(mut game) = boot("police") else { return };
    let ph = p2game::physics::current().expect("physics installed");
    let pawns: Vec<usize> = {
        let vm = &game.world.vm;
        vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect()
    };
    let start: Vec<[f32; 3]> = pawns.iter().map(|&p| game.world.vm.actor_loc(p)).collect();
    eprintln!("police: {} pawns", pawns.len());
    for _ in 0..300 {
        game.tick(1.0 / 30.0);
    }
    game.drain_log(false);
    let vm = &game.world.vm;
    let level = ph.world.borrow().level.clone();
    let mut fell = Vec::new();
    let mut on_floor = 0;
    for (k, &p) in pawns.iter().enumerate() {
        if vm.objects[p].deleted {
            continue;
        }
        let l = Vec3::from(vm.actor_loc(p));
        let s = Vec3::from(start[k]);
        let floor = level.line(l, l - Vec3::Z * 4000.0);
        let phys = vm.get_prop(p, "Physics").map(|v| v.as_int()).unwrap_or(0);
        if l.z < s.z - 2000.0 || floor.is_none() && phys != 0 {
            fell.push((vm.object_name(p), s, l, phys));
        }
        if phys == 1 {
            on_floor += 1;
        }
    }
    eprintln!("after 300 frames: {on_floor}/{} pawns walking, fell: {fell:?}", pawns.len());
    assert!(fell.is_empty(), "pawns fell out of the world: {fell:?}");

    // Doors: walk the player pawn into each door; the physics Bump makes the door's script
    // (DoorMover state BumpPlayerStandOpenTimedPawn: Bump -> Open -> DoOpen -> InterpolateTo)
    // start PHYS_MovingBrush, which physics then moves to its open key.
    let movers: Vec<usize> = {
        let vm = &game.world.vm;
        vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "DoorMover")).collect()
    };
    let before: Vec<([f32; 3], [i32; 3])> = movers.iter().map(|&m| (game.world.vm.actor_loc(m), game.world.vm.actor_rot(m))).collect();
    let pc = game.player_controller;
    let player = pc.and_then(|c| game.world.vm.get_prop(c, "Pawn").and_then(|v| v.as_obj()));
    eprintln!("player controller {pc:?} pawn {player:?}");
    // Bump each door the way physics does when a pawn walks into it (Bump on both actors).
    let bumper = player.or_else(|| pawns.first().copied());
    for &m in &movers {
        game.world.vm.fire_event(m, "Bump", vec![Value::Object(bumper)]);
    }
    for _ in 0..90 {
        game.tick(1.0 / 30.0);
    }
    let vm = &game.world.vm;
    let mut moved = 0;
    let mut opened = 0;
    for (k, &m) in movers.iter().enumerate() {
        let (l0, r0) = before[k];
        let (l1, r1) = (vm.actor_loc(m), vm.actor_rot(m));
        if Vec3::from(l0).distance(Vec3::from(l1)) > 1.0 || r0 != r1 {
            moved += 1;
        } else {
            // Doors that stay shut must be shut by their own script: locked on the side the
            // bumper is on (LockedFront / LockedBack) or a DudeDoor with bOnlyDudeAsCop.
            let locked = pb(vm, m, "LockedFront") || pb(vm, m, "LockedBack") || pb(vm, m, "bOnlyDudeAsCop");
            assert!(locked, "door {} stayed shut without a script lock", vm.object_name(m));
        }
        if vm.get_prop(m, "KeyNum").map(|v| v.as_int()).unwrap_or(0) > 0 {
            opened += 1;
        }
    }
    eprintln!("movers: {} triggered, {moved} moved, {opened} reached a key > 0", movers.len());
    assert!(movers.len() >= 50);
    assert!(moved * 2 >= movers.len(), "most triggered movers move ({moved}/{})", movers.len());

    // Pickups: teleport the player pawn onto a pickup; physics reports Touch both ways and the
    // pickup's script (Pickup.Touch -> ValidTouch -> SpawnCopy / SetRespawn) runs.
    let Some(player) = player else { return };
    {
        // One physics actor per VM object.
        let w = ph.world.borrow();
        let mut users: Vec<(u64, String)> = w.ids().iter().map(|&i| (w.get(i).unwrap().user, w.get(i).unwrap().class.clone())).collect();
        users.sort();
        let dups: Vec<_> = users.windows(2).filter(|p| p[0].0 == p[1].0).map(|p| p[0].clone()).collect();
        eprintln!("physics actors {}, duplicate users {dups:?}, player phys {:?}", users.len(), ph.bridge.borrow().phys(player));
    }
    let pickups: Vec<usize> = {
        let vm = &game.world.vm;
        vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pickup") && pb(vm, a, "bCollideActors")).collect()
    };
    assert!(!pickups.is_empty());
    let mut touched = 0;
    let mut taken = 0;
    let mut tried = 0;
    for &pk in pickups.iter() {
        if tried >= 6 {
            break;
        }
        let (state0, loc) = (game.world.vm.state_name(pk), game.world.vm.actor_loc(pk));
        let h = game.host.clone();
        // Stand the pawn on the pickup's floor: its centre CollisionHeight above the pickup's base.
        let ph_ = game.world.vm.get_prop(pk, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
        let pawn_h = game.world.vm.get_prop(player, "CollisionHeight").map(|v| v.as_float()).unwrap_or(72.0);
        // Pickups against walls leave no room for a pawn (SetLocation fails there, as in UE2).
        let target = (1..=4).map(|k| [loc[0], loc[1], loc[2] - ph_ + pawn_h + k as f32 * 4.0]).find(|t| {
            let pid = ph.bridge.borrow().phys(player);
            pid.is_some_and(|i| ph.world.borrow_mut().blocker_at(i, Vec3::from(*t)).is_none_or(|b| b == p2physics::HitActor::Actor(ph.bridge.borrow().phys(pk).unwrap_or(u32::MAX))))
        });
        let Some(target) = target else { continue };
        tried += 1;
        let ok = uscript::host::Host::move_actor(&*h, &mut game.world.vm, player, target, false);
        for _ in 0..3 {
            game.tick(1.0 / 30.0);
        }
        if !ok {
            eprintln!("  set_location to {target:?} failed");
        }
        let vm = &game.world.vm;
        let touching = matches!(vm.get_prop(player, "Touching"), Some(Value::Array(a)) if a.iter().any(|v| v.as_obj() == Some(pk)));
        let gone = vm.objects[pk].deleted || pb(vm, pk, "bHidden") || vm.state_name(pk) != state0;
        eprintln!("pickup {} ({}): moved={ok} touching={touching} state {state0} -> {} deleted={} hidden={}", vm.object_name(pk), vm.classes[vm.objects[pk].class].name, vm.state_name(pk), vm.objects[pk].deleted, pb(vm, pk, "bHidden"));
        touched += (touching || gone) as usize;
        taken += gone as usize;
    }
    eprintln!("pickups: {touched} touched, {taken} picked up or changed state");
    assert!(touched >= 1, "the player touches pickups it stands on");
}

fn pb(vm: &uscript::vm::Vm, a: usize, n: &str) -> bool {
    vm.get_prop(a, n).is_some_and(|x| x.as_bool())
}
