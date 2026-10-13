//! Scripted demo actions for visual checks (`P2GAME_DEMO_*` environment variables).
//!
//! - `P2GAME_DEMO_KILL="N@T"`: at T seconds of game time, kill NPC number N (same ordering as the
//!   window's `--view-npc N`: AI-controlled pawns sorted by object id) the way lethal damage does
//!   (Health 0, TearOffMomentum, `Died`), so the original death script runs and ragdolls it.
//! - `P2GAME_DEMO_GOTO="Key@T[;Key@T...]"`: at T seconds, move the player pawn onto the first
//!   actor whose Tag, object name or class is Key (pickups get touched, as walking onto them does).
//! - `P2GAME_DEMO_LOOK="Key@T[;Key@T...]"`: at T seconds, turn the player's view (controller
//!   Rotation) to look at that actor, e.g. to frame a corpse that ran off before dying.
//!
//! Installed by `install()` as a level hook; does nothing unless a variable is set.

use crate::host::GameHost;
use std::cell::Cell;
use std::rc::Rc;
use uscript::vm::{ObjId, Value, Vm};

pub fn install() {
    if std::env::var_os("P2GAME_DEMO_KILL").is_some() {
        crate::host::register_level_hook(level_hook);
    }
    if std::env::var_os("P2GAME_DEMO_LIST").is_some() {
        crate::host::register_level_hook(list_hook);
    }
    if std::env::var_os("P2GAME_DEMO_GOTO").is_some() || std::env::var_os("P2GAME_BEG").is_some() {
        crate::host::register_level_hook(goto_hook);
    }
    if std::env::var_os("P2GAME_DEMO_LOOK").is_some() {
        crate::host::register_level_hook(look_hook);
    }
    if std::env::var_os("P2GAME_POSECHECK").is_some() {
        crate::host::register_level_hook(posecheck_hook);
    }
    if std::env::var_os("P2GAME_DEMO_HIT").is_some() {
        crate::host::register_level_hook(hit_hook);
    }
}

/// `P2GAME_DEMO_HIT="Key@T@Damage@Class@DZ[;...]"`: at T seconds the player pawn hits actor Key
/// with `TakeDamage(Damage, player, HitLocation, Momentum, Class)` (e.g. `ShotGunDamage`,
/// `MacheteDamage`), the way a weapon's trace hit does. HitLocation is the actor's MyHead (or its
/// Location) plus DZ in Z when the key ends in `.MyHead` style or DZ is given; momentum is 30000
/// along player -> hit. Gore checks (head explosions, severed limbs) without aiming by hand.
fn hit_hook(host: &GameHost, _vm: &mut Vm) {
    // Optional 6th field `-` (or `-1`): reverse the momentum (a hit from the opposite side).
    let mut steps: Vec<(f32, String, i32, String, f32, bool)> = std::env::var("P2GAME_DEMO_HIT")
        .unwrap_or_default()
        .split(';')
        .filter_map(|s| {
            let p: Vec<&str> = s.split('@').map(str::trim).collect();
            Some((when(p.get(1)?)?, p.first()?.to_string(), p.get(2).and_then(|d| d.parse().ok()).unwrap_or(50), p.get(3).unwrap_or(&"BulletDamage").to_string(), p.get(4).and_then(|z| z.parse().ok()).unwrap_or(0.0), p.get(5).is_some_and(|f| f.starts_with('-'))))
        })
        .collect();
    steps.sort_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
    let t = Rc::new(Cell::new(0.0f32));
    let frame = Rc::new(Cell::new(0u32));
    let next = Rc::new(Cell::new(0usize));
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        t.set(t.get() + dt);
        frame.set(frame.get() + 1);
        while let Some((at, key, dmg, class, dz, flip)) = steps.get(next.get()) {
            if !due(*at, t.get(), frame.get()) {
                break;
            }
            next.set(next.get() + 1);
            let pawn = crate::ai::local_player_controller(vm).and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
            let (Some(pawn), Some(x)) = (pawn, find_actor(vm, key)) else {
                println!("demo hit {key}: no player pawn or target");
                continue;
            };
            // A head key hits the body that owns it.
            let victim = if vm.class_is_a(vm.objects[x].class, "Head") { vm.get_prop(x, "MyBody").and_then(|v| v.as_obj()).unwrap_or(x) } else { x };
            let l = vm.actor_loc(x);
            let hit = [l[0], l[1], l[2] + dz];
            let p = vm.actor_loc(pawn);
            let d = glam::Vec3::new(hit[0] - p[0], hit[1] - p[1], hit[2] - p[2]).normalize_or(glam::Vec3::X) * if *flip { -30000.0 } else { 30000.0 };
            let cls = vm.find_class_by_name(class).ok();
            let h0 = vm.get_prop(victim, "Health").map(|v| v.as_int());
            let r = vm.call_event(victim, "TakeDamage", vec![Value::Int(*dmg), Value::Object(Some(pawn)), Value::Vector(hit), Value::Vector(d.to_array()), Value::Class(cls)]);
            println!("demo hit {key}: {} {dmg} {class} at {hit:?} -> {:?} health {h0:?} -> {:?} state {}", vm.object_name(victim), r.err(), vm.get_prop(victim, "Health").map(|v| v.as_int()), vm.state_name(victim));
        }
    }));
}

/// A step time: seconds of game time, or `fN` for the Nth script frame (negative internally).
fn when(s: &str) -> Option<f32> {
    match s.strip_prefix('f') {
        Some(n) => Some(-(n.parse::<u32>().ok()? as f32)),
        None => s.parse().ok(),
    }
}

fn due(at: f32, t: f32, frame: u32) -> bool {
    if at < 0.0 { frame as f32 >= -at } else { t >= at }
}

/// `P2GAME_POSECHECK=1`: every 2 s, prints human pawns whose skeleton pose looks broken
/// (spine bend, spine tilt from vertical, knee bend while standing), with channel anims/alphas
/// and the rig's bone-controller count, as rendered (after rig posing).
fn posecheck_hook(host: &GameHost, _vm: &mut Vm) {
    let t = Rc::new(Cell::new(0.0f32));
    let hp: *const GameHost = host;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        let before = t.get();
        t.set(before + dt);
        if (before / 2.0).floor() == (t.get() / 2.0).floor() {
            return;
        }
        // SAFETY: the hook is owned by this host's frame_hooks and only runs while it is alive.
        let host = unsafe { &*hp };
        let ps: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn")).collect();
        for p in ps {
            let world = crate::host::actor_world(vm, p);
            let bone = |b: &str| host.with_inst(vm, p, |i| i.get_bone_coords(b)).flatten().map(|c| world.transform_point3(c.origin));
            let (Some(pel), Some(sp2), Some(head), Some(th), Some(ca), Some(lf)) = (bone("MALE01 pelvis"), bone("MALE01 spine2"), bone("MALE01 head"), bone("MALE01 L Thigh"), bone("MALE01 L Calf"), bone("MALE01 L Foot")) else { continue };
            let ang = |a: glam::Vec3, b: glam::Vec3, c: glam::Vec3| (b - a).normalize_or_zero().angle_between((c - b).normalize_or_zero()).to_degrees();
            let bend = ang(pel, sp2, head);
            let knee = ang(th, ca, lf);
            let tilt = (sp2 - pel).normalize_or_zero().angle_between(glam::Vec3::Z).to_degrees();
            let speed = vm.get_prop(p, "Velocity").map(|v| v.as_vec()).map(|v| (v[0] * v[0] + v[1] * v[1]).sqrt()).unwrap_or(0.0);
            if bend > 40.0 || tilt > 35.0 || (speed < 10.0 && knee > 50.0) {
                let m = vm.get_prop(p, "Mesh").and_then(|v| v.as_resource().map(|s| s.to_string())).unwrap_or_default();
                let chans: Vec<String> = host.with_inst(vm, p, |i| i.channels.iter().enumerate().filter_map(|(k, c)| c.sequence_name().map(|s| format!("{k}:{s}@{:.2}a{:.2}", c.frame, c.alpha))).collect()).unwrap_or_default();
                let ctl = vm.get_prop(p, "Controller").and_then(|v| v.as_obj()).map(|c| vm.state_name(c));
                println!("POSE t={:.1} {} {m} at {:?} bend {bend:.0} tilt {tilt:.0} knee {knee:.0} speed {speed:.0} state {ctl:?} chans {chans:?}", t.get(), vm.object_name(p), vm.actor_loc(p));
            }
        }
    }));
}

/// Timed "Key@T;Key@T" steps, sorted by time.
fn timed_steps(var: &str) -> Vec<(String, f32)> {
    let spec = std::env::var(var).unwrap_or_default();
    let mut steps: Vec<(String, f32)> = spec.split(';').filter_map(|s| s.split_once('@')).filter_map(|(k, t)| Some((k.trim().to_string(), t.trim().parse().ok()?))).collect();
    steps.sort_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
    steps
}

fn look_hook(host: &GameHost, _vm: &mut Vm) {
    // "Key@T" or "Key@T@D"; the distance rides along in the key as "Key@D".
    let mut steps: Vec<(String, f32)> = std::env::var("P2GAME_DEMO_LOOK")
        .unwrap_or_default()
        .split(';')
        .filter_map(|s| {
            let mut it = s.split('@');
            let (k, t) = (it.next()?.trim(), when(it.next()?.trim())?);
            Some((it.next().map_or(k.to_string(), |d| format!("{k}@{}", d.trim())), t))
        })
        .collect();
    steps.sort_by(|a, b| a.1.abs().total_cmp(&b.1.abs()));
    let t = Rc::new(Cell::new(0.0f32));
    let frame = Rc::new(Cell::new(0u32));
    let next = Rc::new(Cell::new(0usize));
    // A key ending in `!` keeps the view on that actor every frame until the next step
    // (`+Bystanders.MyHead!@11`: track a walking pawn's head for a headshot).
    let mut hold: Option<String> = None;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        t.set(t.get() + dt);
        frame.set(frame.get() + 1);
        if let Some(k) = hold.as_deref() {
            let pc = crate::ai::local_player_controller(vm);
            let pawn = pc.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
            if let (Some(pc), Some(pawn), Some(x)) = (pc, pawn, find_actor(vm, k)) {
                let eye_h = vm.get_prop(pawn, "EyeHeight").map(|v| v.as_float()).unwrap_or(0.0);
                let (q, p) = (vm.actor_loc(x), vm.actor_loc(pawn));
                // Heads are placed at the neck base: aim a little above it.
                let up = if vm.class_is_a(vm.objects[x].class, "Head") { 6.0 } else { 0.0 };
                let d = glam::Vec3::new(q[0] - p[0], q[1] - p[1], q[2] + up - (p[2] + eye_h));
                let yaw = (d.y.atan2(d.x) * 32768.0 / std::f32::consts::PI) as i32;
                let pitch = (d.z.atan2(glam::Vec2::new(d.x, d.y).length()) * 32768.0 / std::f32::consts::PI) as i32;
                vm.set_prop(pc, "Rotation", Value::Rotator([pitch & 0xffff, yaw & 0xffff, 0]));
            }
        }
        while let Some((key, at)) = steps.get(next.get()) {
            if !due(*at, t.get(), frame.get()) {
                break;
            }
            next.set(next.get() + 1);
            let (key_only, dist) = match key.split_once('@') {
                Some((k, d)) => (k, d.parse::<f32>().ok()),
                None => (key.as_str(), None),
            };
            hold = key_only.strip_suffix('!').map(str::to_string);
            let key_only = key_only.trim_end_matches('!');
            let pc = crate::ai::local_player_controller(vm);
            let pawn = pc.and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
            let (Some(pc), Some(pawn), Some(x)) = (pc, pawn, find_actor(vm, key_only)) else {
                println!("demo look {key}: no player or target");
                continue;
            };
            let eye_h = vm.get_prop(pawn, "EyeHeight").map(|v| v.as_float()).unwrap_or(0.0);
            let q = vm.actor_loc(x);
            // "Key@T@D": first stand D units from the target (on the line towards it).
            if let Some(dd) = dist {
                let p = vm.actor_loc(pawn);
                let a0 = (p[1] - q[1]).atan2(p[0] - q[0]);
                if let Some(h) = crate::physics::current() {
                    // From the player's side first, then around the target until a spot is free.
                    let ok = (0..16).map(|k| a0 + (if k % 2 == 0 { 1.0 } else { -1.0 }) * (k / 2 + k % 2) as f32 * std::f32::consts::FRAC_PI_4 / 2.0).any(|a| uscript::host::Host::move_actor(&*h, vm, pawn, [q[0] + a.cos() * dd, q[1] + a.sin() * dd, q[2] + 40.0], false));
                    println!("demo look: stand {dd} from target -> {ok} at {:?}", vm.actor_loc(pawn));
                }
            }
            let p = vm.actor_loc(pawn);
            let d = glam::Vec3::new(q[0] - p[0], q[1] - p[1], q[2] - (p[2] + eye_h));
            let yaw = (d.y.atan2(d.x) * 32768.0 / std::f32::consts::PI) as i32;
            let pitch = (d.z.atan2(glam::Vec2::new(d.x, d.y).length()) * 32768.0 / std::f32::consts::PI) as i32;
            vm.set_prop(pc, "Rotation", Value::Rotator([pitch & 0xffff, yaw & 0xffff, 0]));
            println!("demo look {key}: {} at {q:?} -> pitch {pitch} yaw {yaw}", vm.object_name(x));
        }
    }));
}

/// First live actor whose Tag, object name or class name is `key` (case-insensitive). `+Class`
/// picks the newest live actor of that class (e.g. `+Bystanders` for the one just summoned).
pub fn find_actor(vm: &Vm, key: &str) -> Option<ObjId> {
    // `Key.Prop`: an object property of the found actor (e.g. `+Bystanders.MyHead`).
    if let Some((base, prop)) = key.split_once('.') {
        return find_actor(vm, base).and_then(|a| vm.get_prop(a, prop)).and_then(|v| v.as_obj());
    }
    if let Some(class) = key.strip_prefix('+') {
        return vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.classes[vm.objects[a].class].name.eq_ignore_ascii_case(class)).max();
    }
    let k = key.to_ascii_lowercase();
    let live = || vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted);
    live()
        .find(|&a| vm.get_prop(a, "Tag").is_some_and(|t| t.as_str().eq_ignore_ascii_case(&k)))
        .or_else(|| live().find(|&a| vm.object_name(a).eq_ignore_ascii_case(&k)))
        .or_else(|| live().find(|&a| vm.classes[vm.objects[a].class].name.eq_ignore_ascii_case(&k)))
}

fn goto_hook(host: &GameHost, _vm: &mut Vm) {
    // `P2GAME_BEG=Key@fN`: at frame N put that NPC's controller in PerformBegForLifeOnKnees with
    // the player as Attacker (begging-on-knees visual checks).
    if let Some((key, at)) = std::env::var("P2GAME_BEG").ok().and_then(|s| s.split_once('@').map(|(k, t)| (k.to_string(), when(t)))).and_then(|(k, t)| Some((k, t?))) {
        let (t, frame, done) = (Rc::new(Cell::new(0.0f32)), Rc::new(Cell::new(0u32)), Rc::new(Cell::new(false)));
        host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
            t.set(t.get() + dt);
            frame.set(frame.get() + 1);
            if done.get() || !due(at, t.get(), frame.get()) {
                return;
            }
            done.set(true);
            let Some(x) = find_actor(vm, &key) else { return };
            let Some(c) = vm.get_prop(x, "Controller").and_then(|v| v.as_obj()) else { return };
            if let Some(p) = crate::ai::local_player_controller(vm).and_then(|pc| vm.get_prop(pc, "Pawn")).and_then(|v| v.as_obj()) {
                vm.set_prop(c, "Attacker", Value::Object(Some(p)));
            }
            let r = vm.goto_state(c, "PerformBegForLifeOnKnees", "");
            println!("demo beg {key}: {} -> {} {:?}", vm.object_name(c), vm.state_name(c), r.err());
        }));
    }
    if std::env::var_os("P2GAME_DEMO_GOTO").is_none() {
        return;
    }
    let steps = timed_steps("P2GAME_DEMO_GOTO");
    let t = Rc::new(Cell::new(0.0f32));
    let next = Rc::new(Cell::new(0usize));
    let hp: *const GameHost = host;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        t.set(t.get() + dt);
        while let Some((key, at)) = steps.get(next.get()) {
            if t.get() < *at {
                break;
            }
            next.set(next.get() + 1);
            let pawn = crate::ai::local_player_controller(vm).and_then(|c| vm.get_prop(c, "Pawn")).and_then(|v| v.as_obj());
            // `X,Y,Z` targets a point instead of an actor.
            let point: Option<[f32; 3]> = key.split(',').map(|s| s.trim().parse::<f32>().ok()).collect::<Option<Vec<f32>>>().and_then(|v| v.try_into().ok());
            let (Some(pawn), Some(l)) = (pawn, point.or_else(|| find_actor(vm, key).map(|x| vm.actor_loc(x)))) else {
                println!("demo goto {key}: no player pawn or target");
                continue;
            };
            // SAFETY: the hook is owned by this host's frame_hooks and only runs while it is alive.
            let host = unsafe { &*hp };
            // On the target if there is room, else on a ring around it (pickups sit against shelves).
            let spots = std::iter::once((0.0, 0.0)).chain((1..=3).flat_map(|r| (0..8).map(move |k| (r as f32 * 25.0, k as f32 * std::f32::consts::FRAC_PI_4))));
            let ok = spots.flat_map(|(r, a)| (0..4).map(move |k| [l[0] + r * a.cos(), l[1] + r * a.sin(), l[2] + 15.0 * k as f32])).any(|p| uscript::host::Host::move_actor(host, vm, pawn, p, false));
            println!("demo goto {key}: at {l:?} -> {ok}");
        }
    }));
}

/// `P2GAME_DEMO_LIST=T`: at T seconds, prints every NPC in `--view-npc` order with its controller
/// state, physics and speed (to pick a walking one to frame).
fn list_hook(host: &GameHost, _vm: &mut Vm) {
    let at: f32 = std::env::var("P2GAME_DEMO_LIST").ok().and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let t = Rc::new(Cell::new(0.0f32));
    let done = Rc::new(Cell::new(false));
    let hp: *const GameHost = host;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        t.set(t.get() + dt);
        if done.get() || t.get() < at {
            return;
        }
        done.set(true);
        // SAFETY: the hook is owned by this host's frame_hooks and only runs while it is alive.
        let host = unsafe { &*hp };
        for (i, &p) in npc_order(host, vm).iter().enumerate() {
            let c = vm.get_prop(p, "Controller").and_then(|c| c.as_obj());
            let v = vm.get_prop(p, "Velocity").map(|v| v.as_vec()).unwrap_or_default();
            let speed = (v[0] * v[0] + v[1] * v[1]).sqrt();
            if speed < 20.0 {
                continue;
            }
            let ch: Vec<String> = host
                .anims
                .borrow()
                .get(&p)
                .map(|x| x.inst.channels.iter().enumerate().filter(|(_, c)| c.seq.is_some()).map(|(i, c)| format!("{i}:{}@{:.2}{}", c.sequence_name().unwrap_or("-"), c.rate, if c.looping { "L" } else { "" })).collect())
                .unwrap_or_default();
            println!("  anims {:?} bPhysicsAnimUpdate {:?} MovementAnims {:?}", ch, vm.get_prop(p, "bPhysicsAnimUpdate"), vm.get_prop(p, "MovementAnims").map(|m| vm.value_string(m)));
            println!(
                "demo npc {i}: {} state {} physics {:?} speed {:.0} at {:?}",
                vm.object_name(p),
                c.map(|c| vm.state_name(c)).unwrap_or_default(),
                vm.get_prop(p, "Physics").map(|x| x.as_int()),
                (v[0] * v[0] + v[1] * v[1]).sqrt(),
                vm.actor_loc(p)
            );
        }
    }));
}

fn parse_kill() -> Option<(usize, f32)> {
    let s = std::env::var("P2GAME_DEMO_KILL").ok()?;
    let (n, t) = s.split_once('@').unwrap_or((&s, "3"));
    // `fN` = frame N (negative encodes a frame time, as `when` does for DEMO_HIT).
    Some((n.trim().parse().ok()?, when(t.trim())?))
}

/// NPCs in `--view-npc` order (pawns with a non-player controller, by object id). The window only
/// counts pawns that have a render rig; skeletal pawns with an animation instance match that.
pub fn npc_order(host: &GameHost, vm: &Vm) -> Vec<ObjId> {
    let anims = host.anims.borrow();
    let mut v: Vec<ObjId> = anims
        .keys()
        .copied()
        .filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Pawn") && vm.get_prop(a, "Controller").and_then(|c| c.as_obj()).is_some_and(|c| !vm.class_is_a(vm.objects[c].class, "PlayerController")))
        .collect();
    v.sort();
    v
}

fn level_hook(host: &GameHost, _vm: &mut Vm) {
    let Some((n, at)) = parse_kill() else { return };
    let done = Rc::new(Cell::new(false));
    let t = Rc::new(Cell::new(0.0f32));
    let frame = Rc::new(Cell::new(0u32));
    let hp: *const GameHost = host;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        if done.get() {
            return;
        }
        t.set(t.get() + dt);
        frame.set(frame.get() + 1);
        if !due(at, t.get(), frame.get()) {
            return;
        }
        done.set(true);
        // SAFETY: the hook is owned by this host's frame_hooks and only runs while it is alive.
        let host = unsafe { &*hp };
        let order = npc_order(host, vm);
        let Some(&v) = order.get(n.min(order.len().saturating_sub(1))) else { return };
        let loc = vm.actor_loc(v);
        // The demo camera looks at this NPC, so it is on screen: bring it out of stasis first as
        // the engine would (its LastRenderTime follows the player's view, not the fly camera).
        if let Some(c) = vm.get_prop(v, "Controller").and_then(|c| c.as_obj()) {
            if vm.get_prop(c, "bStasis").is_some_and(|b| b.as_bool()) {
                vm.fire_event(c, "ComeOutOfStasis", vec![Value::Bool(false)]);
            }
        }
        let now = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(0.0);
        vm.set_prop(v, "LastRenderTime", Value::Float(now));
        println!("demo: {} state before death {} (controller {})", vm.object_name(v), vm.state_name(v), vm.get_prop(v, "Controller").and_then(|c| c.as_obj()).map(|c| vm.state_name(c)).unwrap_or_default());
        let dmg = vm.find_class_by_name("BulletDamage").ok();
        let killer = crate::ai::local_player_controller(vm);
        vm.set_prop(v, "Health", Value::Int(0));
        // As a pistol hit from the player: momentum along player -> victim (TakeDamage's
        // MomentumHitMag * X, divided by Mass as P2Pawn/AWPerson set TearOffMomentum), so the
        // body falls away from the shooter like the original.
        let from = killer.and_then(|pc| vm.get_prop(pc, "Pawn")).and_then(|p| p.as_obj()).map(|p| vm.actor_loc(p));
        let dir = from.map(|f| glam::Vec2::new(loc[0] - f[0], loc[1] - f[1]).normalize_or(glam::Vec2::Y)).unwrap_or(glam::Vec2::Y);
        let mass = vm.get_prop(v, "Mass").map(|m| m.as_float()).unwrap_or(100.0).max(1.0);
        vm.set_prop(v, "TearOffMomentum", Value::Vector([dir.x * 30000.0 / mass, dir.y * 30000.0 / mass, 0.0]));
        vm.fire_event(v, "Died", vec![Value::Object(killer), Value::Class(dmg), Value::Vector([loc[0], loc[1], loc[2] + 30.0])]);
        let kp = vm.get_prop(v, "KParams").and_then(|k| k.as_obj());
        let pc = crate::ai::local_player_controller(vm);
        println!(
            "demo: killed {} at {:?}: Physics {:?} KParams {:?} (player {:?} Player {:?})",
            vm.object_name(v),
            loc,
            vm.get_prop(v, "Physics"),
            kp.map(|k| vm.object_name(k)),
            pc.map(|p| vm.object_name(p)),
            pc.and_then(|p| vm.get_prop(p, "Player")).and_then(|x| x.as_obj()).map(|o| vm.classes[vm.objects[o].class].name.clone())
        );
    }));
}
