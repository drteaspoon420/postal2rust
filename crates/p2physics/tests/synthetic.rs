//! Behaviour tests on synthetic worlds (no game install needed).

use glam::Vec3;
use p2collide::geom::box_triangles;
use p2collide::{Aabb, World};
use p2physics::shape::BrushModel;
use p2physics::{Actor, Collider, EncroachType, Event, HitActor, LadderInfo, MoverInfo, PawnInfo, PhysWorld, Physics, VolumeInfo};
use std::sync::Arc;

fn quad(a: Vec3, b: Vec3, c: Vec3, d: Vec3) -> Vec<[Vec3; 3]> {
    vec![[a, b, c], [a, c, d]]
}

/// Floor at z = 0 (2000 x 2000), a wall at x = 800, a 20-unit step at x >= 400, and a ledge:
/// the floor ends at y = 600 with a drop to z = -500.
fn level() -> Arc<World> {
    let mut t = Vec::new();
    t.extend(quad(Vec3::new(-1000.0, -1000.0, 0.0), Vec3::new(400.0, -1000.0, 0.0), Vec3::new(400.0, 600.0, 0.0), Vec3::new(-1000.0, 600.0, 0.0)));
    t.extend(quad(Vec3::new(400.0, -1000.0, 0.0), Vec3::new(400.0, 600.0, 0.0), Vec3::new(400.0, 600.0, 20.0), Vec3::new(400.0, -1000.0, 20.0)));
    t.extend(quad(Vec3::new(400.0, -1000.0, 20.0), Vec3::new(400.0, 600.0, 20.0), Vec3::new(1000.0, 600.0, 20.0), Vec3::new(1000.0, -1000.0, 20.0)));
    t.extend(quad(Vec3::new(800.0, -1000.0, 0.0), Vec3::new(800.0, 600.0, 0.0), Vec3::new(800.0, 600.0, 500.0), Vec3::new(800.0, -1000.0, 500.0)));
    t.extend(quad(Vec3::new(-3000.0, -3000.0, -500.0), Vec3::new(3000.0, -3000.0, -500.0), Vec3::new(3000.0, 3000.0, -500.0), Vec3::new(-3000.0, 3000.0, -500.0)));
    Arc::new(World::from_triangles(t))
}

fn pawn(world: &mut PhysWorld, at: Vec3, player: bool) -> u32 {
    let info = PawnInfo { is_player: player, ..Default::default() };
    let id = world.add(Actor::pawn("Pawn", at, 28.0, 72.0, info));
    world.place_on_floor(id, at, 400.0);
    world.drain_events();
    id
}

fn run(world: &mut PhysWorld, secs: f32) -> Vec<Event> {
    let mut ev = Vec::new();
    for _ in 0..(secs * 60.0) as usize {
        world.tick(1.0 / 60.0);
        ev.extend(world.drain_events());
    }
    ev
}

fn box_volume(min: Vec3, max: Vec3, info: VolumeInfo) -> Actor {
    let mut v = Actor::new("PhysicsVolume", Vec3::ZERO);
    v.collider = Collider::brush(Arc::new(BrushModel::aabb(min, max)), Vec3::ONE, Vec3::ZERO);
    v.collide_actors = true;
    v.is_static = true;
    v.volume = Some(info);
    v
}

#[test]
fn walking_steps_stops_at_wall_and_hits_wall_event() {
    let mut w = PhysWorld::new(level());
    let p = pawn(&mut w, Vec3::new(0.0, 0.0, 100.0), true);
    assert_eq!(w.get(p).unwrap().physics, Physics::Walking);
    w.get_mut(p).unwrap().acceleration = Vec3::X * 2048.0;
    let ev = run(&mut w, 3.0);
    let a = w.get(p).unwrap();
    assert_eq!(a.physics, Physics::Walking);
    // Walking pawns float MIN..MAX_FLOOR_DIST (1.9..2.4) above the floor, like UE2.
    assert!((a.location.z - 72.0 - 20.0 - 2.15).abs() < 1.0, "on the step: z {}", a.location.z);
    assert!(a.location.x > 800.0 - 28.0 - 3.0 && a.location.x <= 800.0 - 28.0, "x {}", a.location.x);
    assert!(ev.iter().any(|e| matches!(e, Event::HitWall { actor, wall: None, .. } if *actor == p)), "HitWall");
}

#[test]
fn falling_lands_with_landed_event_and_gravity_from_volume() {
    let mut w = PhysWorld::new(level());
    w.default_volume_info.gravity = Vec3::new(0.0, 0.0, -1500.0);
    let p = w.add(Actor::pawn("Pawn", Vec3::new(0.0, 0.0, 400.0), 28.0, 72.0, PawnInfo::default()));
    let mut ev = Vec::new();
    let mut t: f32 = 0.0;
    while w.get(p).unwrap().physics == Physics::Falling && t < 3.0 {
        w.tick(1.0 / 120.0);
        ev.extend(w.drain_events());
        t += 1.0 / 120.0;
    }
    // Drop of 328 units at 1500 u/s^2: t = sqrt(2 * 328 / 1500) = 0.66 s.
    assert!((t - 0.66).abs() < 0.05, "fall time {t}");
    assert!(ev.iter().any(|e| matches!(e, Event::Landed { actor, .. } if *actor == p)));
    assert_eq!(w.get(p).unwrap().physics, Physics::Walking);
}

#[test]
fn ledges_npc_stops_player_falls() {
    let mut w = PhysWorld::new(level());
    let npc = pawn(&mut w, Vec3::new(-500.0, 500.0, 100.0), false);
    let pl = pawn(&mut w, Vec3::new(-300.0, 500.0, 100.0), true);
    for id in [npc, pl] {
        w.get_mut(id).unwrap().acceleration = Vec3::Y * 2048.0;
    }
    let ev = run(&mut w, 1.5);
    assert_eq!(w.get(npc).unwrap().physics, Physics::Walking, "npc must not walk off");
    // The cylinder may overhang the edge until its centre would leave the floor (as in UE2).
    assert!(w.get(npc).unwrap().location.y < 600.0 + 28.0 + 1.0);
    assert!(ev.iter().any(|e| matches!(e, Event::MayFall { actor } if *actor == npc)));
    assert!(ev.iter().any(|e| matches!(e, Event::Falling { actor } if *actor == pl)), "player walks off and falls");
    assert!(w.get(pl).unwrap().location.z < 0.0);
}

#[test]
fn pawns_bump_and_touch_pickup() {
    let mut w = PhysWorld::new(level());
    let a = pawn(&mut w, Vec3::new(0.0, 0.0, 100.0), true);
    let b = pawn(&mut w, Vec3::new(200.0, 0.0, 100.0), false);
    let mut pick = Actor::new("Pickup", Vec3::new(100.0, -200.0, 30.0));
    pick.collide_actors = true;
    pick.collision_radius = 20.0;
    pick.collision_height = 20.0;
    let pk = w.add(pick);
    w.get_mut(a).unwrap().acceleration = Vec3::X * 2048.0;
    let ev = run(&mut w, 1.0);
    let bumped = |x: p2physics::ActorId, y: p2physics::ActorId| ev.iter().any(|e| matches!(e, Event::Bump { actor, other, .. } if *actor == x && *other == y));
    assert!(bumped(b, a) && bumped(a, b));
    let hit_speed = ev.iter().find_map(|e| match e { Event::Bump { actor, other_velocity, .. } if *actor == b => Some(other_velocity.length()), _ => None }).unwrap();
    assert!(hit_speed > 100.0, "Bump carries the bumper's speed at the contact: {hit_speed}");
    let ax = w.get(a).unwrap().location.x;
    assert!(ax < 200.0 - 56.0 + 0.1, "stopped by the other pawn: {ax}");
    // Walk through the pickup.
    w.get_mut(a).unwrap().acceleration = Vec3::new(0.0, -1.0, 0.0) * 2048.0;
    let ev = run(&mut w, 1.5);
    assert!(ev.contains(&Event::Touch { actor: pk, other: a }));
    assert!(ev.contains(&Event::UnTouch { actor: pk, other: a }));
    assert!(w.get(pk).unwrap().touching.is_empty());
}

#[test]
fn water_volume_switches_to_swimming_and_back() {
    let mut w = PhysWorld::new(level());
    let water = w.add(box_volume(Vec3::new(-1000.0, -1000.0, -10.0), Vec3::new(-200.0, 600.0, 300.0), VolumeInfo { water: true, ..Default::default() }));
    let p = pawn(&mut w, Vec3::new(0.0, 0.0, 100.0), true);
    assert_ne!(w.get(p).unwrap().physics_volume, Some(water));
    w.get_mut(p).unwrap().acceleration = -Vec3::X * 2048.0;
    let ev = run(&mut w, 1.0);
    assert_eq!(w.get(p).unwrap().physics, Physics::Swimming);
    assert_eq!(w.get(p).unwrap().physics_volume, Some(water));
    assert!(ev.contains(&Event::PhysicsVolumeChange { actor: p, volume: Some(water) }));
    assert!(ev.contains(&Event::PawnEnteredVolume { actor: water, other: p }));
    assert!(ev.iter().any(|e| matches!(e, Event::HeadVolumeChange { actor, volume: Some(v) } if *actor == p && *v == water)));
    w.get_mut(p).unwrap().acceleration = Vec3::X * 2048.0;
    let ev = run(&mut w, 2.0);
    assert!(ev.contains(&Event::PawnLeavingVolume { actor: water, other: p }));
    assert_eq!(w.get(p).unwrap().physics, Physics::Walking);
}

#[test]
fn ladder_climbs() {
    let mut w = PhysWorld::new(level());
    let lad = w.add(box_volume(Vec3::new(-100.0, -100.0, 0.0), Vec3::new(100.0, 100.0, 600.0), VolumeInfo { ladder: Some(LadderInfo { climb_dir: Vec3::Z, look_dir: Vec3::X }), ..Default::default() }));
    let p = pawn(&mut w, Vec3::new(0.0, 0.0, 100.0), true);
    w.set_physics(p, Physics::Ladder);
    w.get_mut(p).unwrap().pawn.as_mut().unwrap().on_ladder = Some(lad);
    let z0 = w.get(p).unwrap().location.z;
    w.get_mut(p).unwrap().acceleration = Vec3::X * 2048.0;
    run(&mut w, 1.0);
    let dz = w.get(p).unwrap().location.z - z0;
    assert!((dz - 200.0).abs() < 10.0, "climbed {dz} at LadderSpeed 200");
}

#[test]
fn flying_projectile_rotating() {
    let mut w = PhysWorld::new(level());
    let mut proj = Actor::new("Projectile", Vec3::new(0.0, 0.0, 100.0));
    proj.collide_world = true;
    proj.collision_radius = 0.0;
    proj.collision_height = 0.0;
    proj.physics = Physics::Projectile;
    proj.velocity = Vec3::X * 1000.0;
    let pr = w.add(proj);
    let ev = run(&mut w, 1.5);
    assert!(ev.iter().any(|e| matches!(e, Event::HitWall { actor, normal, .. } if *actor == pr && normal.x < -0.9)));
    assert!((w.get(pr).unwrap().location.x - 800.0).abs() < 1.0);

    let mut r = Actor::new("Rotator", Vec3::new(0.0, 0.0, 300.0));
    r.physics = Physics::Rotating;
    r.rotation_rate = [0, 16384, 0];
    let rid = w.add(r);
    run(&mut w, 1.0);
    let yaw = w.get(rid).unwrap().rotation[1];
    assert!((yaw - 16384).abs() < 300, "yaw {yaw}");

    let mut d = Actor::new("Turner", Vec3::new(0.0, 0.0, 300.0));
    d.rotate_to_desired = true;
    d.rotation_rate = [0, 32768, 0];
    d.desired_rotation = [0, 8192, 0];
    let did = w.add(d);
    let ev = run(&mut w, 0.5);
    assert_eq!(w.get(did).unwrap().rotation[1], 8192);
    assert!(ev.contains(&Event::EndedRotation { actor: did }));
}

#[test]
fn trace_natives() {
    let mut w = PhysWorld::new(level());
    let p = pawn(&mut w, Vec3::new(300.0, 0.0, 100.0), false);
    let h = w.trace(Vec3::new(0.0, 0.0, 50.0), Vec3::new(1000.0, 0.0, 50.0), Vec3::ZERO, true, None).unwrap();
    assert_eq!(h.actor, HitActor::Actor(p));
    assert!((h.location.x - (300.0 - 28.0)).abs() < 0.5);
    let h = w.trace(Vec3::new(0.0, 0.0, 50.0), Vec3::new(1000.0, 0.0, 50.0), Vec3::ZERO, false, None).unwrap();
    assert_eq!(h.actor, HitActor::World);
    assert!(h.location.x > 399.0);
    assert!(w.fast_trace(Vec3::new(700.0, 0.0, 100.0), Vec3::new(0.0, 0.0, 100.0)), "pawns do not block FastTrace");
    assert!(!w.fast_trace(Vec3::new(900.0, 0.0, 100.0), Vec3::new(0.0, 0.0, 100.0)));
    let hits = w.trace_actors(Vec3::new(1000.0, 0.0, 50.0), Vec3::new(0.0, 0.0, 50.0), Vec3::ZERO, None);
    assert_eq!(hits.len(), 1);
    assert_eq!(w.radius_actors(Vec3::new(300.0, 0.0, 72.0), 10.0), vec![p]);
    assert_eq!(w.colliding_actors(Vec3::new(300.0, 0.0, 72.0), 10.0), vec![p]);
    assert_eq!(w.visible_actors(Vec3::new(0.0, 0.0, 72.0), 1000.0), vec![p]);
    assert!(w.visible_actors(Vec3::new(1000.0, 0.0, 72.0), 2000.0).is_empty(), "wall hides it");
    // Box trace: the extent widens the hit (28 + 10 > 35).
    let h = w.trace(Vec3::new(0.0, 35.0, 50.0), Vec3::new(1000.0, 35.0, 50.0), Vec3::new(10.0, 10.0, 10.0), true, None).unwrap();
    assert_eq!(h.actor, HitActor::Actor(p));
    assert!(w.trace(Vec3::new(0.0, 35.0, 50.0), Vec3::new(1000.0, 35.0, 50.0), Vec3::ZERO, true, None).unwrap().actor == HitActor::World);
}

#[test]
fn set_location_collision_size_and_base() {
    let mut w = PhysWorld::new(level());
    let p = pawn(&mut w, Vec3::new(0.0, 0.0, 100.0), true);
    let q = pawn(&mut w, Vec3::new(500.0, 0.0, 100.0), false);
    assert!(!w.set_location(p, Vec3::new(510.0, 0.0, 92.0)), "cannot teleport into another pawn");
    assert!(w.set_location(p, Vec3::new(0.0, 300.0, 72.6)));
    assert!(!w.set_location(p, Vec3::new(0.0, 0.0, 0.0)), "cannot teleport into the floor");
    w.set_collision_size(q, 10.0, 10.0);
    assert_eq!(w.get(q).unwrap().collision_radius, 10.0);
    w.set_base(p, Some(q));
    let ev = w.drain_events();
    assert!(ev.contains(&Event::Attach { actor: q, other: p }) && ev.contains(&Event::BaseChange { actor: p }));
    w.set_base(p, None);
    assert!(w.drain_events().contains(&Event::Detach { actor: q, other: p }));
}

/// A mesh "door" that slides along +X into a pawn standing in its path pushes the pawn.
#[test]
fn mover_pushes_pawn_and_carries_rider() {
    let mut w = PhysWorld::new(level());
    let slab = box_triangles(&Aabb { min: Vec3::new(-10.0, -60.0, 0.0), max: Vec3::new(10.0, 60.0, 120.0) });
    let mut door = Actor::new("Mover", Vec3::new(-400.0, 0.0, 0.5));
    door.collide_actors = true;
    door.block_actors = true;
    door.block_players = true;
    door.collider = Collider::mesh(Arc::new(slab), Vec3::ONE, Vec3::ZERO);
    let mut m = MoverInfo { base_pos: Vec3::new(-400.0, 0.0, 0.5), move_time: 1.0, glide: false, encroach: EncroachType::Ignore, ..Default::default() };
    m.key_pos[1] = Vec3::new(300.0, 0.0, 0.0);
    door.mover = Some(m);
    let d = w.add(door);
    let p = pawn(&mut w, Vec3::new(-200.0, 0.0, 100.0), true);
    w.mover_interpolate_to(d, 1, 1.0);
    let ev = run(&mut w, 1.2);
    assert!(ev.contains(&Event::KeyFrameReached { actor: d }));
    let dl = w.get(d).unwrap().location;
    assert!((dl.x - -100.0).abs() < 0.01, "door at key 1: {dl}");
    let px = w.get(p).unwrap().location.x;
    assert!(px >= -100.0 + 10.0 + 28.0 - 1.0, "pawn pushed ahead of the door: {px}");
    assert!(w.stats.encroach_pushes > 0);

    // A lift: a platform rising 200 units carries a pawn standing on it.
    let plat = box_triangles(&Aabb { min: Vec3::new(-80.0, -80.0, -10.0), max: Vec3::new(80.0, 80.0, 0.0) });
    let mut lift = Actor::new("Mover", Vec3::new(-600.0, -600.0, 10.0));
    lift.collide_actors = true;
    lift.block_actors = true;
    lift.block_players = true;
    lift.collider = Collider::mesh(Arc::new(plat), Vec3::ONE, Vec3::ZERO);
    let mut m = MoverInfo { base_pos: Vec3::new(-600.0, -600.0, 10.0), glide: false, ..Default::default() };
    m.key_pos[1] = Vec3::new(0.0, 0.0, 200.0);
    lift.mover = Some(m);
    let l = w.add(lift);
    let r = pawn(&mut w, Vec3::new(-600.0, -600.0, 200.0), true);
    assert_eq!(w.get(r).unwrap().base, Some(l), "standing on the lift");
    w.mover_interpolate_to(l, 1, 1.0);
    run(&mut w, 1.5);
    let rz = w.get(r).unwrap().location.z;
    assert!((rz - (210.0 + 72.0)).abs() < 3.0, "rider carried up: {rz}");
}

/// A stop-when-encroach mover pressing a pawn into a wall stops and reports EncroachingOn.
#[test]
fn mover_blocked_by_pinned_pawn() {
    let mut w = PhysWorld::new(level());
    let slab = box_triangles(&Aabb { min: Vec3::new(-10.0, -60.0, 0.0), max: Vec3::new(10.0, 60.0, 120.0) });
    let mut door = Actor::new("Mover", Vec3::new(500.0, 0.0, 20.5));
    door.collide_actors = true;
    door.block_actors = true;
    door.block_players = true;
    door.collider = Collider::mesh(Arc::new(slab), Vec3::ONE, Vec3::ZERO);
    let mut m = MoverInfo { base_pos: Vec3::new(500.0, 0.0, 20.5), glide: false, encroach: EncroachType::Stop, ..Default::default() };
    m.key_pos[1] = Vec3::new(300.0, 0.0, 0.0);
    door.mover = Some(m);
    let d = w.add(door);
    let p = pawn(&mut w, Vec3::new(760.0, 0.0, 200.0), true);
    w.mover_interpolate_to(d, 1, 1.0);
    let ev = run(&mut w, 1.5);
    assert!(ev.contains(&Event::EncroachingOn { actor: d, other: p }));
    let dx = w.get(d).unwrap().location.x;
    assert!(dx < 800.0 - 56.0 - 10.0 + 1.0, "door stopped at the pawn: {dx}");
}

#[test]
fn many_actors_hash_scales() {
    let mut w = PhysWorld::new(level());
    for i in 0..400 {
        let mut a = Actor::new("Decoration", Vec3::new(-900.0 + (i % 20) as f32 * 80.0, -900.0 + (i / 20) as f32 * 70.0, 30.0));
        a.collide_actors = true;
        a.block_actors = true;
        a.collision_radius = 10.0;
        a.collision_height = 30.0;
        w.add(a);
    }
    let t0 = std::time::Instant::now();
    for k in 0..2000 {
        let y = -900.0 + (k % 200) as f32 * 9.0;
        w.trace(Vec3::new(-950.0, y, 30.0), Vec3::new(950.0, y, 30.0), Vec3::ZERO, true, None);
    }
    let ms = t0.elapsed().as_millis();
    eprintln!("2000 traces among 400 actors: {ms} ms");
    assert!(ms < 3000);
}

#[test]
fn resting_bodies_and_idle_pawns_sleep_and_wake() {
    let mut w = PhysWorld::new(level());
    // A karma body dropped on the floor comes to rest and sleeps.
    let mut b = Actor::new("KActor", Vec3::new(-200.0, 0.0, 100.0));
    b.collide_world = true;
    b.collision_radius = 10.0;
    b.collision_height = 10.0;
    b.physics = Physics::Karma;
    let k = w.add(b);
    run(&mut w, 3.0);
    let a = w.get(k).unwrap();
    assert!(a.sleeping, "resting body sleeps (v {:?}, z {})", a.velocity, a.location.z);
    assert!((a.location.z - 10.0).abs() < 2.0);
    let ticks = w.stats.karma_ticks;
    run(&mut w, 1.0);
    assert_eq!(w.stats.karma_ticks, ticks, "sleeping bodies are not simulated");
    // An impulse (velocity) wakes it.
    w.get_mut(k).unwrap().velocity = Vec3::new(300.0, 0.0, 200.0);
    run(&mut w, 0.2);
    let a = w.get(k).unwrap();
    assert!(a.location.x > -190.0, "woken body moves: x {}", a.location.x);

    // An idle pawn sleeps; acceleration wakes it.
    let p = pawn(&mut w, Vec3::new(0.0, 0.0, 100.0), false);
    run(&mut w, 0.5);
    assert!(w.get(p).unwrap().sleeping, "idle pawn sleeps");
    let at = w.get(p).unwrap().location;
    run(&mut w, 0.5);
    assert!(w.get(p).unwrap().sleeping && w.get(p).unwrap().location == at, "idle pawn stays asleep in place");
    w.get_mut(p).unwrap().acceleration = Vec3::Y * 2048.0;
    run(&mut w, 0.5);
    let a = w.get(p).unwrap();
    assert!(!a.sleeping && a.location.y > 50.0, "accelerating pawn walks: y {}", a.location.y);
    // Teleport (relink) wakes a sleeping pawn; it then settles on the floor.
    w.get_mut(p).unwrap().acceleration = Vec3::ZERO;
    w.get_mut(p).unwrap().velocity = Vec3::ZERO;
    run(&mut w, 0.5);
    assert!(w.get(p).unwrap().sleeping);
    w.set_location(p, Vec3::new(0.0, 300.0, 74.0));
    assert!(!w.get(p).unwrap().sleeping, "relink wakes");
}
