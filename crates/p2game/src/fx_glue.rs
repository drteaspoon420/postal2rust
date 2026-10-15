//! Particle effects, fluids and projector decals for the game host (p2fx).
//!
//! [`install`] registers a level hook. For every level it creates a [`p2fx::vm::FxWorld`], pushes
//! a [`p2fx::vm::FxHost`] into [`GameHost::ext`] (new Emitter actors and the effect natives:
//! Emitter.Kill, SpawnParticle, SpawnParticleLength, SetProjectionNormal, FireHurtRadius,
//! AttachProjector/AbandonProjector, ...) and a frame hook that runs [`p2fx::vm::FxWorld::sync`]
//! after script and physics: script edits to Emitters[i] are applied, systems tick at their
//! actor's Location/Rotation, AutoDestroy systems destroy their actor.
//!
//! Collision (UseCollision particles, decal geometry) comes from the physics subsystem's level
//! world ([`crate::physics::level_collision`]), so its level hook must be registered first.
//!
//! Rendering: when [`set_frame_queue`] was given a queue (windowed mode), each frame's snapshot
//! is put there for `p2fx::FxVmPlugin`. Headless runs simulate without snapshots.
//! `P2GAME_NO_FX=1` disables the subsystem.

use crate::host::GameHost;
use p2fx::vm::{FxFrame, FxHost, FxWorld, SharedFx};
use p2fx::FxLibrary;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use uscript::vm::Vm;

thread_local! {
    static LIB: RefCell<Option<Arc<FxLibrary>>> = const { RefCell::new(None) };
    static CURRENT: RefCell<Option<SharedFx>> = const { RefCell::new(None) };
    static QUEUE: RefCell<Option<Arc<Mutex<Option<FxFrame>>>>> = const { RefCell::new(None) };
}

/// Installs effects for every following level.
pub fn install(game_dir: &std::path::Path) {
    if std::env::var_os("P2GAME_NO_FX").is_some() {
        return;
    }
    match FxLibrary::new(game_dir) {
        Ok(l) => LIB.with(|s| *s.borrow_mut() = Some(Arc::new(l))),
        Err(e) => {
            eprintln!("fx: cannot index {}: {e}", game_dir.display());
            return;
        }
    }
    crate::host::register_level_hook(level_hook);
}

/// Hands per-frame render snapshots to a front end (`p2fx::FxFrameQueue`'s inner value).
pub fn set_frame_queue(q: Arc<Mutex<Option<FxFrame>>>) {
    QUEUE.with(|s| *s.borrow_mut() = Some(q));
}

/// The effects world of the current level (stats, tests).
pub fn current() -> Option<SharedFx> {
    CURRENT.with(|c| c.borrow().clone())
}

fn level_hook(host: &GameHost, _vm: &mut Vm) {
    let Some(lib) = LIB.with(|s| s.borrow().clone()) else { return };
    let map = host.map.borrow().clone();
    if let Some(m) = &map {
        lib.lib.insert(m.clone());
    }
    let mut w = FxWorld::new(lib);
    w.map = map;
    w.collision = crate::physics::level_collision();
    let world: SharedFx = Rc::new(RefCell::new(w));
    // Screenshot aid: `P2FX_CAM=ex,ey,ez;tx,ty,tz` holds the main camera at an Unreal-space eye
    // looking at a target (frames placed emitters without walking the pawn there).
    if let Some((e, t)) = std::env::var("P2FX_CAM").ok().and_then(|s| {
        let v: Vec<f32> = s.split([';', ',']).filter_map(|x| x.trim().parse().ok()).collect();
        (v.len() == 6).then(|| ([v[0], v[1], v[2]], [v[3], v[4], v[5]]))
    }) {
        world.borrow_mut().camera = Some((e, t));
    }
    CURRENT.with(|c| *c.borrow_mut() = Some(world.clone()));
    host.ext.borrow_mut().push(Rc::new(FxHost { world: world.clone() }));
    let mut frame = FxFrame::default();
    let mut demo = std::env::var("P2FX_DEMO").ok().map(|k| Demo { kind: k, t: 0.0, step: 0, actors: vec![], at: None });
    // Test hook: `P2FX_PITCH=-6000` holds the local player's view pitch (Unreal units) so scripted
    // runs can aim the gas can / zipper at the ground, `P2FX_YAW_ADD=N` turns the view by N.
    // `P2FX_PITCH=-6000` or a schedule `0:-3500;250:-9000` (from frame N on).
    let pitch_sched: Vec<(u32, i32)> = std::env::var("P2FX_PITCH")
        .ok()
        .map(|s| {
            s.split(';')
                .filter_map(|it| match it.split_once(':') {
                    Some((f, p)) => Some((f.trim().parse().ok()?, p.trim().parse().ok()?)),
                    None => Some((0, it.trim().parse().ok()?)),
                })
                .collect()
        })
        .unwrap_or_default();
    let pitch: Option<i32> = (!pitch_sched.is_empty()).then_some(0);
    let yaw_add: Option<i32> = std::env::var("P2FX_YAW_ADD").ok().and_then(|s| s.parse().ok());
    // `P2FX_YAW_SWEEP=from:to:rate`: turn the view by `rate` per frame over a frame range.
    let yaw_sweep: Option<(u32, u32, i32)> = std::env::var("P2FX_YAW_SWEEP").ok().and_then(|s| {
        let p: Vec<&str> = s.split(':').collect();
        Some((p.first()?.parse().ok()?, p.get(1)?.parse().ok()?, p.get(2)?.parse().ok()?))
    });
    let mut yaw_done = false;
    // Debug: `P2FX_FOLLOW=FireMatch` prints location / physics / state of actors of that class
    // every 10 frames.
    let follow = std::env::var("P2FX_FOLLOW").ok();
    let mut nframe = 0u32;
    // Test hook: `P2FX_CHEATS=1` keeps cheats enabled (Sissy is a toggle whose start state comes
    // from the shared user ini, so scripted runs could not rely on it).
    let force_cheats = std::env::var_os("P2FX_CHEATS").is_some();
    // Test hook: `P2FX_DROP_BOLTONS=N` calls DropBoltons on every pawn at frame N (what a
    // bystander does when it panics or dies: cigarettes and other boltons fall off).
    let drop_at: Option<u32> = std::env::var("P2FX_DROP_BOLTONS").ok().and_then(|s| s.parse().ok());
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        nframe += 1;
        if drop_at == Some(nframe) {
            let pawns: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "P2MoCapPawn")).collect();
            for p in pawns {
                if !vm.objects[p].deleted {
                    call(vm, p, "DropBoltons", vec![Value::Vector([0.0; 3])]);
                }
            }
            println!("fx: DropBoltons on every pawn at frame {nframe}");
        }
        if force_cheats {
            if let Some(g) = vm.level.and_then(|l| vm.get_prop(l, "Game").and_then(|v| v.as_obj())) {
                vm.set_prop(g, "bAllowCManager", Value::Bool(true));
                vm.set_prop(g, "bWarnedCheater", Value::Bool(true));
            }
        }
        if let (Some(c), true) = (&follow, nframe % 10 == 0) {
            for a in vm.actors.clone() {
                if !vm.objects[a].deleted && vm.classes[vm.objects[a].class].name.eq_ignore_ascii_case(c) {
                    let sub = match vm.get_prop(a, "Emitters") {
                        Some(Value::Array(v)) => v.first().and_then(|x| x.as_obj()),
                        _ => None,
                    };
                    let line = sub.map(|s| (vm.get_prop(s, "LineStart").map(|v| v.as_vec()), vm.get_prop(s, "LineEnd").map(|v| v.as_vec())));
                    println!(
                        "FOLLOW {} loc {:?} vel {:?} phys {:?} state {} life {:?} line {:?} colrad {:?} onfire {:?}",
                        vm.object_name(a),
                        vm.actor_loc(a),
                        vm.get_prop(a, "Velocity").map(|v| v.as_vec()),
                        vm.get_prop(a, "Physics"),
                        vm.state_name(a),
                        vm.get_prop(a, "LifeSpan"),
                        line,
                        vm.get_prop(a, "UseColRadius"),
                        vm.get_prop(a, "bOnFire")
                    );
                    if let Ok(extra) = std::env::var("P2FX_FOLLOW_PROPS") {
                        let vals: Vec<String> = extra.split(',').map(|p| format!("{p}={:?}", vm.get_prop(a, p))).collect();
                        println!("  props {}", vals.join(" "));
                    }
                }
            }
        }
        if pitch.is_some() || yaw_add.is_some() || yaw_sweep.is_some() {
            let pcs: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "PlayerController")).collect();
            for pc in pcs {
                let mut r = vm.actor_rot(pc);
                if pitch.is_some() {
                    if let Some(&(_, p)) = pitch_sched.iter().filter(|(f, _)| *f <= nframe).last() {
                        r[0] = p & 0xFFFF;
                    }
                }
                if let (Some(y), false) = (yaw_add, yaw_done) {
                    r[1] += y;
                }
                if let Some((a, b, rate)) = yaw_sweep {
                    if nframe >= a && nframe < b {
                        r[1] += rate;
                    }
                }
                vm.set_prop(pc, "Rotation", Value::Rotator(r));
            }
            yaw_done = true;
        }
        let Ok(mut w) = world.try_borrow_mut() else { return };
        if w.collision.is_none() {
            w.collision = crate::physics::level_collision();
        }
        if let Some(d) = demo.as_mut() {
            drop(w);
            d.run(vm, &world, dt);
            let Ok(w2) = world.try_borrow_mut() else { return };
            w = w2;
        }
        w.sync(vm, dt);
        // Debug: `P2FX_CENSUS=N` prints every live system (class, actor location, live particles,
        // mean particle speed / age / size and the spread of particle positions) every N frames.
        if let Some(every) = std::env::var("P2FX_CENSUS").ok().and_then(|s| s.parse::<u32>().ok()).filter(|n| *n > 0) {
            if nframe % every == 0 {
                census(vm, &w, nframe);
            }
        }
        if let Some(q) = QUEUE.with(|s| s.borrow().clone()) {
            w.snapshot(&mut frame);
            *q.lock().unwrap_or_else(|p| p.into_inner()) = Some(frame.clone());
        }
    }));
}

fn census(vm: &Vm, w: &FxWorld, nframe: u32) {
    let filter = std::env::var("P2FX_CENSUS_CLASS").ok().map(|s| s.to_ascii_lowercase());
    let mut list: Vec<_> = w.systems.values().filter(|fx| filter.as_ref().is_none_or(|f| fx.class.to_ascii_lowercase().contains(f.as_str()))).collect();
    list.sort_by_key(|fx| fx.id);
    for fx in list {
        for (i, (st, d)) in fx.state.emitters.iter().zip(&fx.defs).enumerate() {
            let alive: Vec<_> = st.particles.iter().take(st.active_particles).filter(|p| p.alive()).collect();
            let n = alive.len().max(1) as f32;
            let speed = alive.iter().map(|p| p.velocity.length()).sum::<f32>() / n;
            let age = alive.iter().map(|p| p.relative_time()).sum::<f32>() / n;
            let size = alive.iter().map(|p| p.size.x).sum::<f32>() / n;
            let alpha = alive.iter().map(|p| p.color[3] as f32).sum::<f32>() / n;
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for p in &alive {
                lo = lo.min(p.location);
                hi = hi.max(p.location);
            }
            let base = vm.get_prop(fx.actor, "Base").and_then(|v| v.as_obj());
            // Root of the attachment chain (smoke -> bolton -> pawn) and its distance.
            let mut root = base;
            for _ in 0..4 {
                match root.and_then(|r| vm.get_prop(r, "Base").and_then(|v| v.as_obj())) {
                    Some(n) => root = Some(n),
                    None => break,
                }
            }
            let root_info = root.map(|r| {
                let rl = Vec3::from(vm.actor_loc(r));
                format!("{}@{:?} d={:.0} hidden={:?} draw={:?}", vm.object_name(r), rl.round(), (rl - fx.owner.location).length(), vm.get_prop(r, "bHidden"), vm.get_prop(r, "DrawType"))
            });
            println!(
                "FXCENSUS@{nframe} {}#{} {} [{i}] hidden={} dis={} live={} active={} pps={} speed={speed:.1} age={age:.2} size={size:.1} a={alpha:.0} owner={:?} lo={:?} hi={:?} base={:?} root={:?}",
                fx.class,
                fx.id,
                vm.object_name(fx.actor),
                fx.hidden,
                st.disabled,
                alive.len(),
                st.active_particles,
                st.particles_per_second,
                fx.owner.location.round(),
                lo.round(),
                hi.round(),
                base.map(|b| vm.object_name(b)),
                root_info,
            );
            let _ = d;
        }
    }
}

/// `P2FX_DEMO=gas|blood`: drives the game's own effect classes through the VM, the way the
/// weapons do, so a screenshot can show them without player input.
/// - `gas`: spawns FX.GasPourFeeder like GasCanWeapon (SetDir each frame while sweeping the
///   pour across the floor), stops the flow (ToggleFlow), then lights the trail with
///   `SetAblaze` on the first gasoline actor, as a match would.
/// - `blood`: spawns Postal2Game.BloodMachineGunSplat projectors facing the nearest wall.
struct Demo {
    kind: String,
    t: f32,
    step: u32,
    actors: Vec<ObjId>,
    /// Ground point and pour direction picked at start.
    at: Option<(Vec3, Vec3)>,
}

use bevy::math::Vec3;
use uscript::host::SpawnParams;
use uscript::vm::{ObjId, Value};

fn spawn(vm: &mut Vm, class: &str, loc: Vec3, rot: [i32; 3]) -> Option<ObjId> {
    let c = vm.find_class_by_name(class).ok()?;
    vm.spawn_actor(c, SpawnParams { location: loc.to_array(), rotation: rot, ..Default::default() }, true)
}

fn call(vm: &mut Vm, a: ObjId, f: &str, args: Vec<Value>) {
    let c = vm.objects[a].class;
    if let Err(e) = vm.call(c, Some(a), f, args) {
        eprintln!("fx demo: {f}: {e}");
    }
}

fn yaw_rot(d: Vec3) -> [i32; 3] {
    let yaw = d.y.atan2(d.x) * 32768.0 / std::f32::consts::PI;
    let pitch = d.z.atan2((d.x * d.x + d.y * d.y).sqrt()) * 32768.0 / std::f32::consts::PI;
    [pitch as i32, yaw as i32, 0]
}

impl Demo {
    /// A standing spot: the player pawn (or its controller, or the first PlayerStart).
    fn origin(vm: &Vm) -> Option<(Vec3, Vec3)> {
        let pick = vm.actors.iter().copied().find(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "PlayerStart"));
        let a = pick?;
        let r = vm.actor_rot(a);
        let yaw = r[1] as f32 * std::f32::consts::TAU / 65536.0;
        Some((Vec3::from(vm.actor_loc(a)), Vec3::new(yaw.cos(), yaw.sin(), 0.0)))
    }

    fn run(&mut self, vm: &mut Vm, world: &SharedFx, dt: f32) {
        self.t += dt;
        let Some(coll) = world.borrow().collision.clone() else { return };
        if self.at.is_none() {
            let Some((p, fwd)) = Self::origin(vm) else { return };
            // Ground under the start.
            let ground = coll.line(p, p - Vec3::Z * 400.0).map(|h| h.location).unwrap_or(p - Vec3::Z * 40.0);
            self.at = Some((ground, fwd));
            println!("fx demo {}: origin {:?} facing {:?}", self.kind, ground, fwd);
        }
        let (ground, fwd) = self.at.unwrap();
        let side = Vec3::new(-fwd.y, fwd.x, 0.0);
        match self.kind.as_str() {
            "gas" => {
                // Pour site 260 uu ahead of the start (so the player is not standing in it).
                let site = {
                    let p = ground + fwd * 260.0 + Vec3::Z * 60.0;
                    coll.line(p, p - Vec3::Z * 400.0).map(|h| h.location).unwrap_or(ground + fwd * 260.0)
                };
                let pour_pos = site + Vec3::Z * 50.0;
                if self.step == 0 {
                    if let Some(f) = spawn(vm, "FX.GasPourFeeder", pour_pos, yaw_rot(fwd)) {
                        self.actors.push(f);
                    }
                    self.step = 1;
                    world.borrow_mut().camera = Some(((site - fwd * 170.0 + side * 200.0 + Vec3::Z * 150.0).to_array(), (site + fwd * 90.0).to_array()));
                }
                let Some(&feeder) = self.actors.first() else { return };
                if self.step == 1 {
                    // Sweep the pour from left to right in an arc ahead of the site.
                    let a = (self.t / 2.0).min(1.0) * 1.8 - 0.9;
                    let dir = (fwd * a.cos() + side * a.sin() - Vec3::Z * 0.45).normalize();
                    if !vm.objects[feeder].deleted {
                        vm.set_prop(feeder, "Location", Value::Vector(pour_pos.to_array()));
                        call(vm, feeder, "SetDir", vec![Value::Vector(pour_pos.to_array()), Value::Vector(dir.to_array()), Value::Float(0.0), Value::Bool(self.t < 0.05)]);
                    }
                    if self.t > 2.1 {
                        if !vm.objects[feeder].deleted {
                            call(vm, feeder, "ToggleFlow", vec![Value::Float(0.0), Value::Bool(false)]);
                        }
                        self.step = 2;
                    }
                } else if self.step == 2 && self.t > 2.5 {
                    // Light the gasoline like a match landing on it.
                    let gas: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && vm.class_is_a(vm.objects[a].class, "Fluid") && !vm.class_is_a(vm.objects[a].class, "FluidFeeder")).collect();
                    println!("fx demo gas: {} fluid actors ({})", gas.len(), gas.iter().map(|&a| vm.classes[vm.objects[a].class].name.clone()).collect::<Vec<_>>().join(", "));
                    if let Some(&g) = gas.iter().find(|&&a| vm.class_is_a(vm.objects[a].class, "FluidTrail")).or(gas.first()) {
                        vm.set_prop(g, "bCanBeDamaged", Value::Bool(true));
                        let at = vm.actor_loc(g);
                        call(vm, g, "SetAblaze", vec![Value::Vector(at), Value::Bool(true)]);
                    }
                    self.step = 3;
                }
            }
            "blood" => {
                if self.step == 0 {
                    // Nearest flat BSP wall around the start (not trees or props).
                    let eye = ground + Vec3::Z * 60.0;
                    let mut best: Option<(f32, Vec3, Vec3)> = None;
                    for k in 0..32 {
                        let a = k as f32 * std::f32::consts::TAU / 32.0;
                        let d = fwd * a.cos() + side * a.sin();
                        if let Some(h) = coll.line(eye, eye + d * 1500.0) {
                            let dist = (h.location - eye).length();
                            let flat = [Vec3::Z * 40.0, -Vec3::Z * 30.0, Vec3::new(-h.normal.y, h.normal.x, 0.0) * 50.0]
                                .iter()
                                .all(|o| coll.line(eye + *o, eye + *o + d * 1500.0).is_some_and(|h2| h2.normal.dot(h.normal) > 0.98));
                            if matches!(h.source, p2collide::world::Source::Bsp) && flat && h.normal.z.abs() < 0.3 && best.is_none_or(|b| dist < b.0) && dist > 150.0 {
                                best = Some((dist, h.location, h.normal));
                            }
                        }
                    }
                    let Some((_, hit, n)) = best else {
                        self.step = 9;
                        return;
                    };
                    for (i, off) in [(0, Vec3::ZERO), (1, Vec3::new(0.0, 0.0, 34.0)), (2, Vec3::new(0.0, 0.0, -26.0))] {
                        let s = Vec3::new(-n.y, n.x, 0.0);
                        let p = hit + n * 20.0 + off + s * ((i as f32 - 1.0) * 40.0);
                        let mut r = yaw_rot(-n);
                        r[2] = (i * 21000) as i32;
                        if let Some(a) = spawn(vm, "Postal2Game.BloodMachineGunSplat", p, r) {
                            self.actors.push(a);
                        }
                    }
                    println!("fx demo blood: {} splats at wall {:?} normal {:?}", self.actors.len(), hit, n);
                    world.borrow_mut().camera = Some(((hit + n * 160.0 + Vec3::Z * 20.0).to_array(), hit.to_array()));
                    self.step = 1;
                }
            }
            k if k.starts_with("spawn:") => {
                // `spawn:Pkg.Class[,Pkg.Class...]`: spawn effect classes 200 uu ahead, facing back
                // toward the camera, re-spawning every 1.5 s.
                if self.t >= self.step as f32 * 1.5 {
                    let at = ground + fwd * 200.0 + Vec3::Z * 50.0;
                    for (i, c) in k[6..].split(',').enumerate() {
                        let p = at + side * ((i as f32) * 60.0 - 30.0);
                        match spawn(vm, c, p, yaw_rot(-fwd)) {
                            Some(a) => println!("fx demo spawn: {c} -> {} at {:?}", vm.object_name(a), p),
                            None => println!("fx demo spawn: {c} failed"),
                        }
                    }
                    if self.step == 0 {
                        world.borrow_mut().camera = Some(((ground + side * 20.0 + Vec3::Z * 70.0).to_array(), at.to_array()));
                    }
                    self.step += 1;
                }
            }
            _ => {}
        }
    }
}
