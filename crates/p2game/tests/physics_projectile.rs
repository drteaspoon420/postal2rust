//! Projectiles and explosions through the physics host (skipped without the game install).
//!
//! A script GrenadeProjectile is spawned above the floor next to a live NPC: PHYS_Projectile
//! moves it, HitWall with bBounce runs the script's BounceRecoil (the reflection is pulled back
//! the next frame), and on its timer / touch it spawns a GrenadeExplosion whose
//! CheckHurtRadius -> HurtRadiusEX (answered by the physics host) damages the NPC.
//!
//! Also prints the per-phase physics frame cost on a large map (suburbs-3) as a perf probe.

use glam::Vec3;
use p2game::boot::Game;
use p2game::config::BootConfig;
use p2game::level::World;
use std::sync::Arc;
use uscript::host::SpawnParams;
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

fn pb(vm: &uscript::vm::Vm, a: usize, n: &str) -> bool {
    vm.get_prop(a, n).is_some_and(|x| x.as_bool())
}

#[test]
fn grenade_bounces_explodes_and_hurts() {
    let Some(mut game) = boot("police") else { return };
    let ph = p2game::physics::current().expect("physics installed");
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    let player = game.player_controller.and_then(|c| game.world.vm.get_prop(c, "Pawn").and_then(|v| v.as_obj()));
    // A live walking NPC with open floor and headroom beside it.
    let level = ph.world.borrow().level.clone();
    let victim = {
        let vm = &game.world.vm;
        vm.actors.iter().copied().find(|&a| {
            if vm.objects[a].deleted || Some(a) == player || !vm.class_is_a(vm.objects[a].class, "Pawn") {
                return false;
            }
            let alive = vm.get_prop(a, "Health").is_some_and(|h| h.as_int() > 0);
            let walking = vm.get_prop(a, "Physics").is_some_and(|p| p.as_int() == 1);
            let l = Vec3::from(vm.actor_loc(a));
            let side = l + Vec3::new(100.0, 0.0, 60.0);
            alive && walking && level.line(l, side).is_none() && level.line(side, side + Vec3::Z * 100.0).is_none()
        })
    };
    let Some(victim) = victim else { panic!("no NPC with room for a grenade") };
    let vm = &mut game.world.vm;
    let h0 = vm.get_prop(victim, "Health").map(|h| h.as_int()).unwrap_or(0);
    let vl = Vec3::from(vm.actor_loc(victim));
    let class = vm.find_class_by_name("Inventory.GrenadeProjectile").expect("GrenadeProjectile class");
    let start = vl + Vec3::new(100.0, 0.0, 60.0);
    let g = vm.spawn_actor(class, SpawnParams { location: start.to_array(), ..Default::default() }, true).expect("grenade spawned");
    let phys0 = vm.get_prop(g, "Physics").map(|p| p.as_int());
    vm.set_prop(g, "Velocity", Value::Vector([-60.0, 0.0, -500.0]));
    eprintln!(
        "victim {} health {h0} at {vl:?}; grenade {g} phys {phys0:?} bBounce {} DetonateTime {:?} registered {}",
        vm.object_name(victim),
        pb(vm, g, "bBounce"),
        vm.get_prop(g, "DetonateTime"),
        ph.bridge.borrow().phys(g).is_some()
    );
    let exp_class = vm.find_class_by_name("FX.GrenadeExplosion").ok();
    let actors0 = vm.objects.len();
    let mut bounced = false;
    let mut last_vz = -500.0f32;
    let mut exploded_at = None;
    for f in 0..240 {
        game.tick(1.0 / 30.0);
        let vm = &game.world.vm;
        if vm.objects[g].deleted {
            exploded_at.get_or_insert(f);
            continue;
        }
        let vz = vm.get_prop(g, "Velocity").map(|v| v.as_vec()[2]).unwrap_or(0.0);
        if last_vz < -50.0 && vz > 0.0 {
            bounced = true;
            eprintln!("frame {f}: bounce at {:?}, vz {last_vz} -> {vz}", vm.actor_loc(g));
        }
        last_vz = vz;
        if exploded_at.is_some() && f > exploded_at.unwrap() + 30 {
            break;
        }
    }
    game.drain_log(false);
    let vm = &game.world.vm;
    // Spawned explosions (alive or already finished and destroyed).
    let explosions = exp_class.map(|c| vm.objects[actors0.min(vm.objects.len())..].iter().filter(|o| vm.is_child_of(o.class, c)).count()).unwrap_or(0);
    let h1 = if vm.objects[victim].deleted { 0 } else { vm.get_prop(victim, "Health").map(|h| h.as_int()).unwrap_or(0) };
    eprintln!("grenade: bounced={bounced} exploded at frame {exploded_at:?}, explosions spawned {explosions}, victim health {h0} -> {h1}, deleted {}", vm.objects[victim].deleted);
    assert!(bounced, "HitWall with bBounce reflected the grenade off the floor");
    assert!(exploded_at.is_some(), "grenade exploded (destroyed) within 8 s");
    assert!(explosions >= 1, "a GrenadeExplosion was spawned");
    assert!(h1 < h0, "the explosion's HurtRadiusEX damaged the NPC next to it");

    // Karma rigid body: a KActor dropped near the victim lands and sleeps; damage with momentum
    // (KActor.TakeDamage -> KAddImpulse) wakes it and pushes it along the momentum.
    let vm = &mut game.world.vm;
    let kclass = vm.find_class_by_name("Engine.KActor").expect("KActor class");
    let kat = vl + Vec3::new(0.0, 0.0, 40.0) + Vec3::new(100.0, 0.0, 0.0);
    let k = vm.spawn_actor(kclass, SpawnParams { location: kat.to_array(), ..Default::default() }, true).expect("KActor spawned");
    vm.set_prop(k, "Physics", Value::Byte(13));
    vm.set_prop(k, "CollisionRadius", Value::Float(16.0));
    vm.set_prop(k, "CollisionHeight", Value::Float(16.0));
    vm.set_prop(k, "bCollideActors", Value::Bool(true));
    for _ in 0..60 {
        game.tick(1.0 / 30.0);
    }
    let vm = &game.world.vm;
    let pid = ph.bridge.borrow().phys(k).expect("KActor has a physics body");
    let rest = Vec3::from(vm.actor_loc(k));
    let asleep = ph.world.borrow().get(pid).is_some_and(|a| a.sleeping);
    eprintln!("KActor rest at {rest:?} (spawned {kat:?}), sleeping {asleep}");
    assert!(asleep, "the body comes to rest");
    let vm = &mut game.world.vm;
    vm.fire_event(k, "TakeDamage", vec![Value::Int(10), Value::Object(None), Value::Vector(rest.to_array()), Value::Vector([0.0, 30000.0, 20000.0]), Value::Class(None)]);
    let kv = Vec3::from(game.world.vm.get_prop(k, "Velocity").map(|v| v.as_vec()).unwrap_or_default());
    eprintln!("KActor velocity after KAddImpulse: {kv:?}");
    assert!(kv.y > 10.0 && kv.z > 10.0, "KAddImpulse added velocity along the momentum: {kv:?}");
    for _ in 0..10 {
        game.tick(1.0 / 30.0);
    }
    let moved = Vec3::from(game.world.vm.actor_loc(k));
    eprintln!("KActor after damage impulse: {moved:?}");
    // (It may bounce off nearby geometry, so only the distance is checked.)
    assert!(moved.distance(rest) > 10.0, "the damage impulse moved the body ({rest:?} -> {moved:?})");
}

/// Perf probe: the physics frame cost on suburbs-3 with all its pawns.
#[test]
fn suburbs3_physics_frame_cost() {
    let Some(mut game) = boot("suburbs-3") else { return };
    let ph = p2game::physics::current().expect("physics installed");
    for _ in 0..30 {
        game.tick(1.0 / 30.0);
    }
    ph.timings.set([0.0; 4]);
    ph.pull_relinks.set(0);
    let n = 150;
    let t = std::time::Instant::now();
    for _ in 0..n {
        game.tick(1.0 / 30.0);
    }
    let total = t.elapsed().as_secs_f64();
    game.drain_log(false);
    let ms = ph.timings.get().map(|s| s * 1000.0 / n as f64);
    let (dynamic, all) = {
        let w = ph.world.borrow();
        let ids = w.ids();
        (ids.iter().filter(|&&i| w.get(i).is_some_and(|a| !a.is_static || a.is_mover())).count(), ids.len())
    };
    eprintln!("pull relinks per frame {:.1}", ph.pull_relinks.get() as f64 / n as f64);
    // Isolated costs (repeated, so machine load matters less): pull of all dynamic actors and
    // one physics tick.
    {
        let vm = &game.world.vm;
        let mut w = ph.world.borrow_mut();
        let mut b = ph.bridge.borrow_mut();
        let awake = |w: &p2physics::PhysWorld| w.ids().iter().filter(|&&i| w.get(i).is_some_and(|a| a.physics == p2physics::Physics::Karma && !a.sleeping)).count();
        eprintln!("awake karma bodies: {}", awake(&w));
        let t = std::time::Instant::now();
        for _ in 0..20 {
            b.pull_dynamic(vm, &mut w);
        }
        let pull = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
        let t = std::time::Instant::now();
        for _ in 0..20 {
            w.tick(1.0 / 30.0);
        }
        let tick = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
        let _ = w.drain_events();
        eprintln!("isolated: pull_dynamic {pull:.2} ms, tick {tick:.2} ms, stats {:?}", w.stats);
        // Loose budget (the target is < 2 ms on an idle machine; CI boxes are loaded).
        assert!(pull + tick < 20.0, "physics pull + tick on suburbs-3 took {:.1} ms", pull + tick);
        assert_eq!(awake(&w), 0, "map-placed karma bodies come to rest");
        // Per physics mode: count and ms per tick.
        let mut by: std::collections::BTreeMap<String, (usize, f64)> = Default::default();
        for _ in 0..10 {
            for id in w.ids() {
                let Some(a) = w.get(id) else { continue };
                let key = format!("{:?}{}", a.physics, if a.is_pawn() { " pawn" } else { "" });
                let t = std::time::Instant::now();
                w.tick_actor(id, 1.0 / 30.0);
                let e = by.entry(key).or_default();
                e.0 += 1;
                e.1 += t.elapsed().as_secs_f64() * 1000.0;
            }
        }
        let _ = w.drain_events();
        for (k, (n, ms)) in by {
            eprintln!("  mode {k}: {} actors, {:.3} ms/tick ({:.1} us each)", n / 10, ms / 10.0, ms * 1000.0 / n as f64);
        }
        let mut shown = 0;
        for id in w.ids() {
            let a = w.get(id).unwrap();
            if (a.physics == p2physics::Physics::Karma && shown < 5) || a.physics == p2physics::Physics::Rotating {
                shown += (a.physics == p2physics::Physics::Karma) as usize;
                let tris = match &a.collider {
                    p2physics::Collider::Mesh { local, .. } => local.len(),
                    _ => 0,
                };
                let floor = w.level.line(a.location, a.location - Vec3::Z * 500.0).map(|h| a.location.z - h.location.z);
                eprintln!(
                    "  {} {:?}: loc {:?} vel {:?} r {} h {} sleeping {} collide_world {} mesh tris {tris} floor below {floor:?} rate {:?}",
                    a.class, a.physics, a.location, a.velocity, a.collision_radius, a.collision_height, a.sleeping, a.collide_world, a.rotation_rate
                );
            }
        }
    }
    eprintln!(
        "suburbs-3: {} vm actors, {all} physics actors ({dynamic} dynamic); physics per frame: register {:.2} ms, pull {:.2} ms, tick {:.2} ms, push+events {:.2} ms = {:.2} ms (game tick {:.1} ms)",
        game.world.vm.actors.len(),
        ms[0],
        ms[1],
        ms[2],
        ms[3],
        ms.iter().sum::<f64>(),
        total * 1000.0 / n as f64
    );
}
