//! Actor physics for the game host: `p2physics` (UE2 physics modes, movers, volumes, touch /
//! bump / encroach events over `p2collide`) installed as the [`GameHost`] world host.
//!
//! Per level ([`level_hook`]): the map's collision world is built with `LoadedMap::load`, the
//! loader's movers and volumes are bound to their VM objects by export index
//! (`GameHost::level_objects`), and every other colliding non-static actor (pawns, pickups,
//! projectiles, decorations) is registered from its VM state. Actors spawned later are added in
//! `actor_spawned`, destroyed ones removed.
//!
//! Per frame (a frame hook, after script and animation): script-side state is pulled in (the VM
//! builtins SetCollision / SetCollisionSize / SetPhysics / SetBase and Mover.InterpolateTo only
//! write properties, so they take effect here), actors that started colliding are registered,
//! the physics world ticks, changed actors are pushed back to the VM and queued events (Touch,
//! Bump, HitWall, Landed, volume and zone changes, KeyFrameReached, ...) are delivered to script.
//!
//! Movement natives (SetLocation, Move, MoveSmooth), SetRotation, traces, sight and the
//! collision iterators are answered by the physics world immediately.
//!
//! `P2GAME_NO_PHYSICS=1` disables the subsystem.

use crate::host::GameHost;
use glam::Vec3;
use p2physics::{ActorId, LoadedMap, PhysWorld, VmBridge};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use uscript::host::{Host, TraceHit, TraceRequest};
use uscript::vm::{ObjId, Value, Vm};

pub struct PhysHost {
    pub world: RefCell<PhysWorld>,
    pub bridge: RefCell<VmBridge>,
    /// Actors already checked for registration (non-colliding ones are re-checked when their
    /// collision flags change; see `frame`).
    seen: RefCell<HashSet<ObjId>>,
    /// Number of `vm.actors` entries already scanned for late registration.
    scanned: Cell<usize>,
    pub frames: Cell<u64>,
    /// Accumulated seconds in `frame`: [registration, pull, tick, push+events].
    pub timings: Cell<[f64; 4]>,
    /// Relinks caused by `pull` (script-side moves) since install (perf diagnostics).
    pub pull_relinks: Cell<u64>,
    /// Non-static actors without collision yet (re-checked every 15 frames in case script
    /// switches their collision or physics on).
    pending: Cell<Vec<ObjId>>,
    /// Package library (static mesh trace geometry for KActors).
    lib: std::sync::Arc<upkg::library::Library>,
    /// Mesh-space collision triangles per static mesh path.
    trace_meshes: RefCell<std::collections::HashMap<String, Option<std::sync::Arc<Vec<[Vec3; 3]>>>>>,
    /// Actors colliding / traced on their static mesh, with the mesh path in use: a script mesh
    /// swap (PropBreakable's BrokenStaticMesh) rebuilds the collider.
    mesh_actors: RefCell<std::collections::HashMap<ObjId, String>>,
    /// Per-controller steering state for MoveTo/MoveToward (see `steer`).
    nav: RefCell<std::collections::HashMap<ObjId, SteerState>>,
    /// Map export of each level static mesh actor baked into the level collision, so its
    /// triangles can stop colliding when script destroys it (PrepActorsByGroup).
    baked_exports: RefCell<std::collections::HashMap<ObjId, u32>>,
}

/// Obstacle steering memory of one moving controller.
#[derive(Clone, Copy)]
struct SteerState {
    /// Heading in use (unit, horizontal) and the level time it is re-probed.
    dir: Vec3,
    next_probe: f32,
    /// Side chosen when the way ahead was first blocked (+1 left, -1 right), 0 = none yet.
    side: f32,
    /// Closest horizontal distance to the destination so far and when it last improved.
    best: f32,
    since: f32,
    /// Destination this state belongs to.
    dest: Vec3,
    /// Detour waypoint from the path network after a stall (walked to before `dest`).
    via: Option<Vec3>,
    detours: u8,
}

thread_local! {
    /// The physics host of the current level (for front ends and tests).
    static CURRENT: RefCell<Option<Rc<PhysHost>>> = const { RefCell::new(None) };
    /// AI moves ended for lack of progress / headings deflected around obstacles (probes).
    pub static NAV_STUCK: Cell<u64> = const { Cell::new(0) };
    pub static NAV_DEFLECT: Cell<u64> = const { Cell::new(0) };
    pub static NAV_DETOUR: Cell<u64> = const { Cell::new(0) };
}

/// The installed physics host of the current level, if any.
pub fn current() -> Option<Rc<PhysHost>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// The current level's static collision (BSP, level static meshes, terrain), shared with other
/// subsystems (particle collision, decals) so they need not build their own.
pub fn level_collision() -> Option<std::sync::Arc<p2collide::World>> {
    current().map(|h| h.world.borrow().level.clone())
}

fn v(a: [f32; 3]) -> Vec3 {
    Vec3::from(a)
}

fn pb(vm: &Vm, a: ObjId, n: &str) -> bool {
    vm.get_prop(a, n).is_some_and(|x| x.as_bool())
}

impl PhysHost {
    /// Pulls the actor's script state in, runs `f` on the world, pushes results and events back.
    fn with_actor<R>(&self, vm: &mut Vm, a: ObjId, f: impl FnOnce(&mut PhysWorld, ActorId) -> R) -> Option<R> {
        let id = self.bridge.borrow().phys(a)?;
        let r = {
            let mut w = self.world.borrow_mut();
            self.bridge.borrow_mut().pull(vm, &mut w, a);
            f(&mut w, id)
        };
        // The actor script called on is always written back: SetRotation changes no location, so
        // it is not in the moved list, and without this the player's turn never reached script
        // (he kept walking the old heading whatever the mouse did).
        {
            let mut w = self.world.borrow_mut();
            self.bridge.borrow().push(vm, &mut w, id);
        }
        self.flush_from(vm, false);
        Some(r)
    }

    /// Pushes changed actors and delivers queued events. Events may run script that calls back
    /// into the host, so no borrow is held while delivering.
    fn flush(&self, vm: &mut Vm) {
        self.flush_from(vm, true);
    }

    /// `all`: push every active actor (after the frame tick). Otherwise only the actors physics
    /// moved: after one script call into physics (SetLocation), pushing every active actor
    /// overwrote what script had set on other actors earlier in the frame. A bleeding corpse's
    /// blood spout (PersonPawn.Tick -> SnapSpout -> SetLocation every tick) reset the player's
    /// DoJump Velocity and Physics before physics saw them: no jumps, no door bumps.
    fn flush_from(&self, vm: &mut Vm, all: bool) {
        for _ in 0..8 {
            let events = {
                let mut w = self.world.borrow_mut();
                if all {
                    self.bridge.borrow().push_changed(vm, &mut w);
                } else {
                    self.bridge.borrow().push_moved(vm, &mut w);
                }
                w.drain_events()
            };
            if events.is_empty() {
                return;
            }
            // Script run by an event may call back into this host (move, spawn, trace), so
            // deliver from a snapshot of the id map with no borrow held.
            let snapshot = self.bridge.borrow().clone();
            let targets: Vec<ObjId> = events.iter().filter_map(|e| snapshot.obj(e.target())).collect();
            snapshot.deliver(vm, events);
            // What the handlers wrote (BounceRecoil's reflected Velocity, SetPhysics, ...) must
            // reach physics before the next push, or the push would overwrite it.
            let mut w = self.world.borrow_mut();
            let mut b = self.bridge.borrow_mut();
            for t in targets {
                if !vm.objects[t].deleted {
                    b.pull(vm, &mut w, t);
                }
            }
        }
    }

    /// Should the VM actor `a` have a physics actor?
    fn wants_physics(vm: &Vm, a: ObjId) -> bool {
        if vm.objects[a].deleted {
            return false;
        }
        // Controllers never collide (UE2 keeps them out of the collision hash). A scene viewer
        // controller can carry collision flags; registered, its SetRotation went through the
        // encroacher path and silently failed, freezing mouse look after a skipped cutscene.
        if vm.class_is_a(vm.objects[a].class, "Controller") {
            return false;
        }
        if pb(vm, a, "bStatic") {
            // UE2 keeps static actors with bCollideActors in the collision hash, so they still
            // get Touch/UnTouch (BuyerStandPoint: the cashier/priest serves whoever touches it).
            // Static blocking geometry is already in the level collision; only touch-only static
            // actors (no blocking, not world geometry, not drawn as a static mesh) are added.
            const DT_STATIC_MESH: u8 = 8;
            return pb(vm, a, "bCollideActors")
                && !pb(vm, a, "bBlockActors")
                && !pb(vm, a, "bBlockPlayers")
                && !pb(vm, a, "bWorldGeometry")
                && !matches!(vm.get_prop(a, "DrawType"), Some(Value::Byte(DT_STATIC_MESH)))
                && !vm.class_is_a(vm.objects[a].class, "Volume")
                && !vm.class_is_a(vm.objects[a].class, "Mover");
        }
        pb(vm, a, "bCollideActors") || pb(vm, a, "bCollideWorld") || vm.get_prop(a, "Physics").is_some_and(|p| p.as_int() != 0)
    }

    pub(crate) fn register(&self, vm: &mut Vm, a: ObjId) {
        if self.bridge.borrow().phys(a).is_some() {
            return;
        }
        if !Self::wants_physics(vm, a) {
            // May switch collision on later (rechecked by `frame`); static actors never do.
            if !vm.objects[a].deleted && !pb(vm, a, "bStatic") {
                let mut p = self.pending.take();
                p.push(a);
                self.pending.set(p);
            }
            return;
        }
        let mut w = self.world.borrow_mut();
        let id = self.bridge.borrow_mut().add_actor(vm, &mut w, a);
        if let Some(path) = attach_trace_mesh(&self.lib, &mut self.trace_meshes.borrow_mut(), vm, &mut w, id, a) {
            self.mesh_actors.borrow_mut().insert(a, path);
        }
    }

    /// `Actor.HurtRadiusEX(Damage, Radius, DamageType, Momentum, HitLocation, bNoScale,
    /// RoundUpIf, bStopAtWall, bIgnoreInstigator, IgnoreActor, TargetClass, bExplosion,
    /// ClassCheck, out bClassCheckResult, InstigatorRadius)`: UE2 HurtRadius with POSTAL 2's
    /// options. Victims are the colliding actors whose cylinder is within the radius; damage
    /// scales `1 - max(0, dist - CollisionRadius) / Radius` (unless bNoScale), momentum points
    /// away from the centre (thrown upward for explosions). Returns the number of live pawns hit.
    fn hurt_radius_ex(&self, vm: &mut Vm, me: ObjId, a: &mut [Value]) -> Value {
        if pb(vm, me, "bHurtEntry") {
            return Value::Int(0);
        }
        let f = |k: usize, d: f32| match a.get(k) {
            Some(Value::Void) | None => d,
            Some(v) => v.as_float(),
        };
        let b = |k: usize| a.get(k).is_some_and(|v| v.as_bool());
        let (dmg, radius, momentum) = (f(0, 0.0), f(1, 0.0), f(3, 0.0));
        let dtype = a.get(2).cloned().unwrap_or(Value::Class(None));
        let center = Vec3::from(match a.get(4) {
            Some(Value::Vector(v)) => *v,
            _ => vm.actor_loc(me),
        });
        let (no_scale, round_up, stop_at_wall, ignore_inst) = (b(5), f(6, 0.0), b(7), b(8));
        let ignore = a.get(9).and_then(|v| v.as_obj());
        let target = a.get(10).and_then(|v| v.as_class());
        let explosion = b(11);
        let class_check: Vec<String> = a.get(12).map(|v| v.as_str()).unwrap_or_default().split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
        let inst_radius = f(14, 0.0);
        if radius <= 0.0 {
            return Value::Int(0);
        }
        vm.set_prop(me, "bHurtEntry", Value::Bool(true));
        let inst = vm.get_prop(me, "Instigator").and_then(|v| v.as_obj());
        let reach = radius.max(inst_radius) + 200.0;
        let ids = self.world.borrow_mut().colliding_actors(center, reach);
        let cands: Vec<ObjId> = ids.into_iter().filter_map(|i| self.bridge.borrow().obj(i)).collect();
        let mut hit_pawns = 0;
        let mut class_hit = false;
        for v in cands {
            if v == me || Some(v) == ignore || vm.objects[v].deleted || pb(vm, v, "bHidden") {
                continue;
            }
            if ignore_inst && Some(v) == inst {
                continue;
            }
            if target.is_some_and(|t| !vm.is_child_of(vm.objects[v].class, t)) {
                continue;
            }
            let loc = Vec3::from(vm.actor_loc(v));
            let r = vm.get_prop(v, "CollisionRadius").map(|x| x.as_float()).unwrap_or(0.0);
            let h = vm.get_prop(v, "CollisionHeight").map(|x| x.as_float()).unwrap_or(0.0);
            let rad = if Some(v) == inst && inst_radius > 0.0 { inst_radius } else { radius };
            let dist = (loc - center).length();
            if dist > rad + r {
                continue;
            }
            // Wall check. Map-placed props (cars, pickups) often have their origin a few units
            // under a terrain or floor surface; a strict line to the origin then hits the ground
            // and a grenade next to a car never damaged it (the original blows such cars up,
            // FIDELITY #26). Also accept a clear line to the top of the victim's cylinder.
            if stop_at_wall && !self.world.borrow_mut().fast_trace(loc, center) && !(h > 0.0 && self.world.borrow_mut().fast_trace(loc + Vec3::Z * h, center)) {
                if std::env::var_os("P2_HURT_TRACE").is_some() {
                    eprintln!("HURT  blocked: {} center {center:.0} victim {loc:.0} h {h:.0}", vm.object_name(v));
                }
                continue;
            }
            let dir = (loc - center).normalize_or(Vec3::Z);
            let scale = if no_scale { 1.0 } else { (1.0 - (dist - r).max(0.0) / rad).clamp(0.0, 1.0) };
            let mut amount = scale * dmg;
            if round_up > 0.0 && amount >= round_up && amount < 1.0 {
                amount = 1.0;
            }
            let mut mom = dir * scale * momentum;
            if explosion {
                mom.z = mom.z.max(0.0) + scale * momentum * 0.5;
            }
            let hit_loc = loc - dir * 0.5 * (h + r);
            let is_pawn = vm.class_is_a(vm.objects[v].class, "Pawn");
            let alive = is_pawn && vm.get_prop(v, "Health").is_some_and(|x| x.as_int() > 0);
            if !class_check.is_empty() {
                let cn = vm.classes[vm.objects[v].class].name.to_ascii_lowercase();
                class_hit |= class_check.contains(&cn);
            }
            if std::env::var_os("P2_HURT_TRACE").is_some() {
                eprintln!("HURT {} -> {} dist {dist:.0} amount {amount:.1} mom {mom:.0}", vm.object_name(me), vm.object_name(v));
            }
            vm.fire_event(v, "TakeDamage", vec![Value::Int(amount as i32), Value::Object(inst), Value::Vector(hit_loc.to_array()), Value::Vector(mom.to_array()), dtype.clone()]);
            if alive {
                hit_pawns += 1;
            }
        }
        if let Some(slot) = a.get_mut(13) {
            *slot = Value::Int(class_hit as i32);
        }
        if !vm.objects[me].deleted {
            vm.set_prop(me, "bHurtEntry", Value::Bool(false));
        }
        Value::Int(hit_pawns)
    }

    /// Karma natives for PHYS_Karma rigid bodies (KActors, explodable barrels and cars). The body
    /// is the physics actor's cylinder; impulses become velocity changes (`Impulse / Mass`,
    /// like the ragdoll natives), and anything that adds motion wakes it. Ragdolls
    /// (PHYS_KarmaRagDoll) are left to the ragdoll host. Returns None for other natives.
    fn karma_native(&self, vm: &mut Vm, me: ObjId, name: &str, a: &[Value]) -> Option<Value> {
        const PHYS_KARMA: i32 = 13;
        if vm.get_prop(me, "Physics").map(|p| p.as_int()) != Some(PHYS_KARMA) {
            return None;
        }
        let id = self.bridge.borrow().phys(me)?;
        let vec = |k: usize| Vec3::from(a.get(k).map(|v| v.as_vec()).unwrap_or_default());
        let mass = Self::k_mass(vm, me);
        if name == "ksetmass" {
            if let Some(p) = vm.get_prop(me, "KParams").and_then(|v| v.as_obj()) {
                vm.set_prop(p, "KMass", Value::Float(a.first().map(|v| v.as_float()).unwrap_or(1.0)));
            }
            return Some(Value::Void);
        }
        let mut w = self.world.borrow_mut();
        let body = w.get_mut(id)?;
        match name {
            "kaddimpulse" => {
                if std::env::var_os("P2_KARMA_TRACE").is_some() {
                    let kmass = vm.get_prop(me, "KParams").and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "KMass")).map(|v| v.as_float());
                    eprintln!("KIMPULSE {} imp={:.0} mass={mass} kmass={kmass:?} v_before={:.0}", vm.object_name(me), vec(0), body.velocity);
                }
                // UE2 execKAddImpulse converts the impulse to Karma units (x0.02 for
                // UU->metres, then x0.01) and Karma divides by KParams.KMass; the resulting
                // velocity in metres/s is 50 UU/s each. Net: dv = impulse * 0.01 / KMass UU/s.
                // (Dividing by the actor's Mass instead made a front kick on a KMass-2 chair
                // leave at ~19000 UU/s: playtest "objects fly very far".)
                body.velocity += vec(0) * 0.01 / Self::k_karma_mass(vm, me);
                body.sleeping = false;
            }
            // A force for one frame (Karma applies KAddForce over the next step).
            "kaddforce" => {
                body.velocity += vec(0) / mass * (1.0 / 30.0);
                body.sleeping = false;
            }
            "kwake" => body.sleeping = false,
            "kisawake" => return Some(Value::Bool(!body.sleeping)),
            "kgetmass" => return Some(Value::Float(mass)),
            "kdisablekarma" => {
                body.velocity = Vec3::ZERO;
                body.sleeping = true;
            }
            // Accepted without simulation effect (no rotational dynamics or per-pair filtering).
            "kaddtorque" | "ksetinertiatensor" | "ksetdampingprops" | "ksetfriction" | "ksetrestitution" | "ksetcomoffset" | "ksetimpactthreshold" | "ksetstayupright" | "ksetblockkarma" | "kdisablecollision" | "kenablecollision" => {}
            _ => return None,
        }
        let v = body.velocity;
        drop(w);
        // Keep script's view in sync (the next pull would otherwise overwrite the impulse).
        vm.set_prop(me, "Velocity", Value::Vector(v.to_array()));
        Some(Value::Void)
    }

    /// Karma body mass (`KParams.KMass`, KarmaParams default 1), as the native impulse uses it.
    fn k_karma_mass(vm: &Vm, me: ObjId) -> f32 {
        vm.get_prop(me, "KParams").and_then(|v| v.as_obj()).and_then(|p| vm.get_prop(p, "KMass")).map(|v| v.as_float()).filter(|m| *m > 0.0).unwrap_or(1.0).max(0.05)
    }

    /// The mass impulses are divided by: the actor's `Mass` (as script's own `Momentum / Mass`
    /// and the ragdoll natives do; Karma's `KMass` is in different units), at least 1.
    fn k_mass(vm: &Vm, me: ObjId) -> f32 {
        vm.get_prop(me, "Mass").map(|v| v.as_float()).filter(|m| *m > 0.0).unwrap_or(100.0).max(1.0)
    }

    /// Obstacle avoidance for a walking AI move (UE2 does this with AdjustFromWall /
    /// PickWallAdjust on HitWall; we look ahead instead, which also covers props and corners the
    /// pawn would only graze). Returns the heading to accelerate along, or None when the pawn has
    /// made no progress toward `dest` for STUCK_SECS (the move then ends so script re-plans).
    #[allow(clippy::too_many_arguments)]
    fn avoid(&self, vm: &mut Vm, controller: ObjId, pawn: ObjId, here: Vec3, dest: Vec3, dir: Vec3, dist: f32, arrive: f32, radius: f32, height: f32, elapsed: f32) -> Option<Vec3> {
        const STUCK_SECS: f32 = 2.5;
        const PROBE_EVERY: f32 = 0.15;
        let mut map = self.nav.borrow_mut();
        let fresh = SteerState { dir, next_probe: 0.0, side: 0.0, best: dist, since: elapsed, dest, via: None, detours: 0 };
        let st = map.entry(controller).or_insert(fresh);
        if st.dest.distance(dest) > 16.0 || elapsed < st.since {
            *st = fresh;
        }
        // On a detour: head for the waypoint until close, then resume the real destination.
        let (dir, dist) = match st.via {
            Some(v) => {
                let fv = Vec3::new(v.x - here.x, v.y - here.y, 0.0);
                if fv.length() < radius * 2.0 + 16.0 {
                    st.via = None;
                    st.best = dist;
                    st.since = elapsed;
                    (dir, dist)
                } else {
                    (fv.normalize_or_zero(), fv.length())
                }
            }
            None => (dir, dist),
        };
        // Progress watchdog (toward the waypoint while detouring). Horizontally on top of the
        // destination (a target on another floor, EastMall atrium) is not an obstacle stall:
        // leave that to the move's own timeout, as before.
        if dist <= arrive + radius {
            st.since = elapsed;
        }
        if dist < st.best - 8.0 {
            st.best = dist;
            st.since = elapsed;
        } else if elapsed - st.since > STUCK_SECS {
            // Stalled: the destination is probably behind a wall. Ask the path network for a
            // route (as UE2 script would after PointReachable failed) and walk to its first
            // node; give up (end the move so script re-plans) after a few detours.
            let detour = if st.detours < 3 {
                drop(map);
                // The route search writes RouteCache/RouteGoal/RouteDist; script owns those (its
                // own path), so keep them as they were.
                let saved: Vec<(&str, Option<Value>)> = ["RouteCache", "RouteGoal", "RouteDist"].iter().map(|k| (*k, vm.get_prop(controller, k).cloned())).collect();
                let hop = vm.find_route(controller, uscript::nav::Goal::Point(dest.to_array()), false).ok().flatten();
                // First route node not already under the pawn (the first hop is often the node
                // it stands at, which made the detour a no-op).
                let near = |p: Vec3| Vec3::new(p.x - here.x, p.y - here.y, 0.0).length() <= radius * 2.0 + 16.0;
                let mut route: Vec<Vec3> = hop.map(|h| Vec3::from(vm.actor_loc(h))).into_iter().collect();
                if let Some(Value::Array(rc)) = vm.get_prop(controller, "RouteCache") {
                    route.extend(rc.iter().filter_map(|v| v.as_obj()).filter(|&o| !vm.objects[o].deleted).map(|o| Vec3::from(vm.actor_loc(o))));
                }
                let pick = route.into_iter().find(|p| !near(*p));
                for (k, v) in saved {
                    if let Some(v) = v {
                        vm.set_prop(controller, k, v);
                    }
                }
                map = self.nav.borrow_mut();
                pick
            } else {
                None
            };
            let st = map.get_mut(&controller).expect("steer state");
            if let Some(h) = detour {
                if std::env::var_os("P2GAME_NAV_TRACE").is_some() {
                    eprintln!("NAV detour {} at {:.0},{:.0} via {:.0},{:.0} to {:.0},{:.0}", vm.object_name(pawn), here.x, here.y, h.x, h.y, dest.x, dest.y);
                }
                NAV_DETOUR.with(|c| c.set(c.get() + 1));
                st.via = Some(h);
                st.detours += 1;
                st.best = Vec3::new(h.x - here.x, h.y - here.y, 0.0).length();
                st.since = elapsed;
                st.side = 0.0;
                st.next_probe = 0.0;
                st.dir = Vec3::new(h.x - here.x, h.y - here.y, 0.0).normalize_or_zero();
                return Some(st.dir);
            }
            if std::env::var_os("P2GAME_NAV_TRACE").is_some() {
                eprintln!("NAV stuck {} at {:.0},{:.0},{:.0} dest {:.0},{:.0},{:.0} dist {dist:.0}", vm.object_name(pawn), here.x, here.y, here.z, dest.x, dest.y, dest.z);
            }
            NAV_STUCK.with(|c| c.set(c.get() + 1));
            return None;
        }
        let st = map.get_mut(&controller).expect("steer state");
        // Close to the goal: head straight in (the last bit is usually around a doorway or the
        // goal actor itself, which the probe would flag as an obstacle).
        if st.via.is_none() && dist < arrive + radius * 2.0 {
            st.dir = dir;
            return Some(dir);
        }
        if elapsed >= st.next_probe {
            st.next_probe = elapsed + PROBE_EVERY;
            let mut world = self.world.borrow_mut();
            let look = (radius * 3.0 + 40.0).min(dist);
            let step = 35.0;
            if st.side == 0.0 {
                // Pick the side that turns least away from where we are heading next.
                st.side = if (pawn as u32).wrapping_mul(2654435761) & 1 == 0 { 1.0 } else { -1.0 };
            }
            let want = match crate::navfix::steer_dir(&mut *world, here, dir, look, radius, height, step, st.side) {
                Some(d) => {
                    if (d - dir).length() > 1e-3 {
                        NAV_DEFLECT.with(|c| c.set(c.get() + 1));
                    } else {
                        st.side = 0.0;
                    }
                    d
                }
                // Boxed in: keep the old heading; the watchdog ends the move if this persists.
                None => st.dir,
            };
            st.dir = want;
        }
        // Blend toward the goal again once the deflection is no longer needed.
        Some(st.dir)
    }

    /// MoveTo / MoveToward for a controller: sets the pawn's Acceleration toward the destination
    /// (physics walks it there) and completes on arrival, when stuck, or after a timeout.
    fn steer(&self, vm: &mut Vm, controller: ObjId, l: &mut uscript::vm::Latent, dt: f32) -> bool {
        let Some(pawn) = vm.get_prop(controller, "Pawn").and_then(|v| v.as_obj()).filter(|p| !vm.objects[*p].deleted) else { return true };
        let toward = l.name.to_ascii_lowercase().starts_with("movetoward");
        let target = vm.get_prop(controller, "MoveTarget").and_then(|v| v.as_obj()).filter(|t| !vm.objects[*t].deleted);
        let dest = match target {
            Some(t) if toward => Vec3::from(vm.actor_loc(t)),
            _ => Vec3::from(vm.get_prop(controller, "Destination").map(|v| v.as_vec()).unwrap_or_default()),
        };
        let here = Vec3::from(vm.actor_loc(pawn));
        let radius = vm.get_prop(pawn, "CollisionRadius").map(|v| v.as_float()).unwrap_or(20.0);
        let d = dest - here;
        let flat = Vec3::new(d.x, d.y, 0.0);
        let phys = vm.get_prop(pawn, "Physics").map(|p| p.as_int()).unwrap_or(0);
        // PHYS_Ladder (11) is steered too, with the full 3D direction: the ladder physics turns
        // acceleration along the wall/up into climbing (UE2 physLadder). Without it an AI on a
        // ladder (AW zombies in ClimbingLadder) got no acceleration and hung there forever.
        let on_ladder = phys == 11;
        let falling_or_walking = matches!(phys, 1 | 2) || on_ladder;
        // Reaching an actor (MoveToward) counts once the collision cylinders meet, as UE2's
        // ReachedDestination does for pawn targets; walking further only bumps into it.
        let target_r = match target {
            Some(t) if toward && vm.class_is_a(vm.objects[t].class, "Pawn") && vm.get_prop(t, "bCollideActors").is_some_and(|v| v.as_bool()) => vm.get_prop(t, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0),
            _ => 0.0,
        };
        // MoveToWithRadius / MoveTowardWithRadius carry their own arrival radius (third arg). Stop
        // a little inside it: callers re-check with `<= radius` (CashierController.CloseEnoughTo).
        let with_radius = l.name.to_ascii_lowercase().ends_with("withradius");
        let with_r = if with_radius { l.args.get(2).map(|v| v.as_float()).unwrap_or(0.0) * 0.85 } else { 0.0 };
        let arrive = if with_r > 4.0 { with_r } else { radius.max(16.0) } + target_r;
        let height = vm.get_prop(pawn, "CollisionHeight").map(|v| v.as_float()).unwrap_or(40.0);
        let reached = flat.length() <= arrive && d.z.abs() < if on_ladder { height } else { 120.0 };
        // FPSController's WithRadius moves set MoveTimer from the pawn's PathCheckTimeActor /
        // PathCheckTimePoint and end once it runs out, so leg-motion states reassess (their
        // InterimChecks) every few seconds instead of only on arrival.
        let timed_out = if with_radius {
            let t = if l.elapsed <= dt {
                let k = if toward { "PathCheckTimeActor" } else { "PathCheckTimePoint" };
                vm.get_prop(pawn, k).map(|v| v.as_float()).unwrap_or(4.0) - dt
            } else {
                vm.get_prop(controller, "MoveTimer").map(|v| v.as_float()).unwrap_or(0.0) - dt
            };
            vm.set_prop(controller, "MoveTimer", Value::Float(t));
            t < 0.0
        } else if !on_ladder && !crate::navfix::disabled() {
            // UE2 APawn::setMoveTimer at the start of MoveTo/MoveToward: 0.5 + Extra * distance /
            // (DesiredSpeed * MaxSpeed), Extra = 2 or 1/WalkingPct (1/CrouchedPct) when slowed.
            // The move ends once the controller's MoveTimer runs out (execPollMoveTo), so a pawn
            // whose goal spot is taken (queues, crowds) gives up and lets script re-plan.
            let t = if l.elapsed <= dt {
                let pf = |k: &str, d: f32| vm.get_prop(pawn, k).map(|v| v.as_float()).unwrap_or(d);
                let pb = |k: &str| vm.get_prop(pawn, k).is_some_and(|v| v.as_bool());
                let mut extra: f32 = 2.0;
                if pb("bIsCrouched") {
                    extra = extra.max(1.0 / pf("CrouchedPct", 0.5).max(0.05));
                } else if pb("bIsWalking") {
                    extra = extra.max(1.0 / pf("WalkingPct", 0.5).max(0.05));
                }
                let speed = pf("DesiredSpeed", 1.0).max(0.05) * pf("GroundSpeed", 440.0).max(1.0);
                0.5 + extra * d.length() / speed - dt
            } else {
                vm.get_prop(controller, "MoveTimer").map(|v| v.as_float()).unwrap_or(0.0) - dt
            };
            vm.set_prop(controller, "MoveTimer", Value::Float(t));
            // Never below the old fixed floor for short hops (stairs, doors).
            t < 0.0 && l.elapsed > 2.0
        } else {
            false
        };
        let done = reached || timed_out || l.elapsed > 20.0;
        if done {
            self.nav.borrow_mut().remove(&controller);
        }
        if done || !falling_or_walking {
            vm.set_prop(pawn, "Acceleration", Value::Vector([0.0; 3]));
            if !falling_or_walking && !done {
                // Not a walking pawn (e.g. no physics yet): fall back to the VM default teleport-step.
                return uscript::host::default_latent_tick(self, vm, controller, l);
            }
            if reached {
                // The engine tells the controller it hit what it was walking to; Postal's leg
                // motion states advance their path (or finish) from this event.
                let goal = Value::Object(if toward { target } else { None });
                if let Err(e) = vm.call_event(controller, "HitPathGoal", vec![goal, Value::Vector(dest.to_array())]) {
                    eprintln!("HitPathGoal on {controller}: {e}");
                }
            }
            return true;
        }
        let accel = vm.get_prop(pawn, "AccelRate").map(|v| v.as_float()).unwrap_or(2048.0);
        let mut dir = flat.normalize_or_zero();
        if !on_ladder && !crate::navfix::disabled() {
            match self.avoid(vm, controller, pawn, here, dest, dir, flat.length(), arrive, radius, height, l.elapsed) {
                Some(d) => dir = d,
                None => {
                    // No progress for a while: end the move like UE2's MoveTimer running out,
                    // so the controller's state picks a new route (LambController re-paths).
                    vm.set_prop(pawn, "Acceleration", Value::Vector([0.0; 3]));
                    vm.set_prop(controller, "MoveTimer", Value::Float(-1.0));
                    self.nav.borrow_mut().remove(&controller);
                    return true;
                }
            }
        }
        vm.set_prop(pawn, "Acceleration", Value::Vector((if on_ladder { d.normalize_or_zero() } else { dir } * accel).to_array()));
        if on_ladder {
            // Keep facing the wall (ClimbLadder set Rotation to WallDir).
            return false;
        }
        // Face the movement direction.
        let yaw = (dir.y.atan2(dir.x) * 32768.0 / std::f32::consts::PI) as i32;
        let mut r = vm.actor_rot(pawn);
        r[1] = yaw;
        vm.set_prop(pawn, "Rotation", Value::Rotator(r));
        false
    }

    /// Per-frame physics step.
    pub fn frame(&self, vm: &mut Vm, dt: f32) {
        let t0 = std::time::Instant::now();
        self.frames.set(self.frames.get() + 1);
        // Late registration: new actors in the list, and known actors whose collision/physics
        // flags were switched on by script.
        let n = vm.actors.len();
        let start = self.scanned.get().min(n);
        let fresh: Vec<ObjId> = vm.actors[start..].to_vec();
        self.scanned.set(n);
        for a in fresh {
            self.seen.borrow_mut().insert(a);
            self.register(vm, a);
        }
        if self.frames.get() % 15 == 0 {
            // Only non-static actors can switch collision on (bStatic never changes), so the
            // rescan walks the unregistered non-static list instead of every actor.
            let mut pend = self.pending.take();
            pend.sort_unstable();
            pend.dedup();
            // `register` re-queues the ones still without collision.
            for a in pend {
                self.register(vm, a);
            }
        }
        if self.frames.get() % 5 == 0 {
            // Static mesh swaps (SetStaticMesh: broken props) rebuild the mesh collider.
            let swapped: Vec<ObjId> = self
                .mesh_actors
                .borrow()
                .iter()
                .filter(|(o, p)| !vm.objects[**o].deleted && vm.get_prop(**o, "StaticMesh").and_then(|v| v.as_resource()).is_some_and(|s| s != p.as_str()))
                .map(|(o, _)| *o)
                .collect();
            for o in swapped {
                let Some(id) = self.bridge.borrow().phys(o) else { continue };
                let mut w = self.world.borrow_mut();
                match attach_trace_mesh(&self.lib, &mut self.trace_meshes.borrow_mut(), vm, &mut w, id, o) {
                    Some(p) => {
                        self.mesh_actors.borrow_mut().insert(o, p);
                    }
                    None => {
                        self.mesh_actors.borrow_mut().remove(&o);
                    }
                }
            }
        }
        let t1 = std::time::Instant::now();
        let t2;
        {
            let mut w = self.world.borrow_mut();
            let mut b = self.bridge.borrow_mut();
            let m0 = w.moved_count();
            b.pull_dynamic(vm, &mut w);
            self.pull_relinks.set(self.pull_relinks.get() + (w.moved_count() - m0) as u64);
            t2 = std::time::Instant::now();
            w.tick(dt);
        }
        let t3 = std::time::Instant::now();
        self.flush(vm);
        let t4 = std::time::Instant::now();
        let mut t = self.timings.get();
        for (k, (a, b)) in [(t0, t1), (t1, t2), (t2, t3), (t3, t4)].into_iter().enumerate() {
            t[k] += (b - a).as_secs_f64();
        }
        self.timings.set(t);
        // `P2GAME_SPIKES`: physics phase split (register, pull, tick, flush) every 300 frames.
        if self.frames.get() % 300 == 0 && std::env::var_os("P2GAME_SPIKES").is_some() {
            let f = 300.0 / 1000.0;
            println!("physics split ms/frame: register {:.2} pull {:.2} tick {:.2} flush {:.2}", t[0] / f, t[1] / f, t[2] / f, t[3] / f);
            self.timings.set([0.0; 4]);
        }
        // `P2GAME_DIRTY_VERIFY`: running totals of the unchanged-actor pull skips and their checks.
        if self.frames.get() % 300 == 0 && std::env::var_os("P2GAME_DIRTY_VERIFY").is_some() {
            let (skipped, checked, bad) = self.bridge.borrow().dirty_stats();
            println!("DIRTY_VERIFY frame {} skipped {skipped} checked {checked} mismatches {bad}", self.frames.get());
        }
    }
}

/// Static mesh collision for dynamic (non-static) static mesh actors, as UE2 does it: an actor
/// drawn as DT_StaticMesh without `bUseCylinderCollision` collides on its StaticMesh, for line
/// checks and for pawn movement. Before this, every such actor (Window, Bottle, DayBlocker,
/// breakable props) blocked with its CollisionRadius x CollisionHeight cylinder: the suburbs
/// windows (meshes 8 units thick, 224 or 432 wide) stood as 112-224 radius pillars, so the Dude
/// could barely get near a window opening, let alone through it once broken.
///
/// - Actors physics does not simulate (PHYS_None / PHYS_Rotating) get the mesh as their collider
///   (encroacher style, like movers): pawns slide along the real glass plane and Bump it
///   (Window.Bump breaks it above BreakPct of GroundSpeed), and SetCollision(false) in the
///   Broken state really opens the hole.
/// - KActors keep the cylinder our Karma stand-in moves and get the mesh as trace geometry, which
///   shots and kicks hit (FIDELITY #27; also with bUseCylinderCollision, as the barrels set it).
///
/// Returns the mesh path used, or None.
fn attach_trace_mesh(lib: &upkg::library::Library, cache: &mut std::collections::HashMap<String, Option<std::sync::Arc<Vec<[Vec3; 3]>>>>, vm: &Vm, w: &mut PhysWorld, id: ActorId, a: ObjId) -> Option<String> {
    const DT_STATIC_MESH: u8 = 8;
    if !matches!(vm.get_prop(a, "DrawType"), Some(Value::Byte(DT_STATIC_MESH))) || pb(vm, a, "bStatic") {
        return None;
    }
    let class = vm.objects[a].class;
    if vm.class_is_a(class, "Pawn") || vm.class_is_a(class, "Mover") || vm.class_is_a(class, "Volume") || vm.class_is_a(class, "Controller") {
        return None;
    }
    let karma = vm.class_is_a(class, "KActor");
    // PHYS_None (0) and PHYS_Rotating (5) actors stand still for physics.
    let still = vm.get_prop(a, "Physics").map(|p| p.as_int()).is_none_or(|p| p == 0 || p == 5);
    let as_collider = !karma && still && !pb(vm, a, "bUseCylinderCollision");
    if !karma && !as_collider {
        return None;
    }
    let path = vm.get_prop(a, "StaticMesh").and_then(|v| v.as_resource()).map(|s| s.to_string())?;
    let tris = cache
        .entry(path.clone())
        .or_insert_with(|| crate::host::resolve_path(lib, &path, Some("StaticMesh")).map(|o| p2collide::world::mesh_collision_tris(&o).0).filter(|t| !t.is_empty()).map(std::sync::Arc::new))
        .clone()?;
    if std::env::var_os("P2GAME_TRACE_LOG").is_some() {
        println!("{} mesh {} {path} ({} tris)", if as_collider { "collision" } else { "trace" }, vm.object_name(a), tris.len());
    }
    let s = vm.get_prop(a, "DrawScale").map(|v| v.as_float()).unwrap_or(1.0);
    let s3 = match vm.get_prop(a, "DrawScale3D") {
        Some(Value::Vector(v)) => Vec3::from(*v),
        _ => Vec3::ONE,
    };
    let pivot = match vm.get_prop(a, "PrePivot") {
        Some(Value::Vector(v)) => Vec3::from(*v),
        _ => Vec3::ZERO,
    };
    let p = w.get_mut(id)?;
    let asleep = p.sleeping;
    if as_collider {
        p.collider = p2physics::Collider::mesh(tris, s3 * s, pivot);
        p.trace_collider = None;
    } else {
        p.trace_collider = Some(p2physics::Collider::mesh(tris, s3 * s, pivot));
    }
    w.relink(id);
    // relink wakes the body; keep KStartEnabled=false bodies asleep.
    if let Some(p) = w.get_mut(id) {
        p.sleeping = asleep;
    }
    Some(path)
}

thread_local! {
    /// Collision world being built on a worker thread for the named map (see [`prefetch`]).
    static PREFETCH: RefCell<Option<(String, std::thread::JoinHandle<upkg::Result<LoadedMap>>)>> = const { RefCell::new(None) };
}

/// Starts building `map`'s collision world on a worker thread, so it overlaps level
/// instantiation and the begin-play scripts; [`install`] picks it up. A build already under way
/// for the same map (a [`preload`] near a telepad) is kept.
pub fn prefetch(lib: std::sync::Arc<upkg::library::Library>, map: std::sync::Arc<upkg::Package>) {
    if std::env::var_os("P2GAME_NO_PHYSICS").is_some() || std::env::var_os("P2GAME_NO_PREFETCH").is_some() {
        return;
    }
    if PREFETCH.with(|p| p.borrow().as_ref().is_some_and(|(n, _)| n.eq_ignore_ascii_case(&map.name))) {
        return;
    }
    let name = map.name.clone();
    let h = std::thread::Builder::new().name("collision-build".into()).spawn(move || LoadedMap::load(&lib, &map));
    if let Ok(h) = h {
        PREFETCH.with(|p| *p.borrow_mut() = Some((name, h)));
    }
}

/// Name of the map whose collision world is being built ahead (see [`preload`]).
pub fn preloading() -> Option<String> {
    PREFETCH.with(|p| p.borrow().as_ref().map(|(n, _)| n.clone()))
}

/// Opens the map package at `path` and builds its collision world on a worker thread while the
/// current level keeps playing (the player is near a level exit). A later travel to that map
/// takes the result instead of building it (the collision world depends only on the package).
pub fn preload(lib: std::sync::Arc<upkg::library::Library>, name: &str, path: std::path::PathBuf) {
    if std::env::var_os("P2GAME_NO_PHYSICS").is_some() || std::env::var_os("P2GAME_NO_PREFETCH").is_some() {
        return;
    }
    if preloading().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
        return;
    }
    let h = std::thread::Builder::new().name("collision-preload".into()).spawn(move || {
        let map = std::sync::Arc::new(upkg::Package::open(&path)?);
        LoadedMap::load(&lib, &map)
    });
    if let Ok(h) = h {
        PREFETCH.with(|p| *p.borrow_mut() = Some((name.to_string(), h)));
    }
}

fn take_prefetched(lib: &upkg::library::Library, map: &std::sync::Arc<upkg::Package>) -> upkg::Result<LoadedMap> {
    let pre = PREFETCH.with(|p| p.borrow_mut().take());
    if let Some((name, h)) = pre {
        if name.eq_ignore_ascii_case(&map.name) {
            if let Ok(r) = h.join() {
                // The worker registered the map with its own handle; make sure the shared library
                // knows it too (`load` does this as its first step on the calling thread).
                lib.insert(map.clone());
                return r;
            }
        }
    }
    LoadedMap::load(lib, map)
}

/// Builds the physics world for the host's level and installs it as the world host.
pub fn install(host: &GameHost, vm: &mut Vm) -> Option<Rc<PhysHost>> {
    let map = host.map.borrow().clone()?;
    let objects = host.level_objects.borrow().clone();
    let t0 = std::time::Instant::now();
    let mut m = match take_prefetched(&host.lib, &map) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("physics: cannot load collision for {}: {e}", map.name);
            return None;
        }
    };
    let mut bridge = VmBridge::new();
    let mut baked_exports = std::collections::HashMap::new();
    for (export, id) in &m.by_export {
        if let Some(&o) = objects.get(&(*export as usize)) {
            bridge.bind(&mut m.world, o, *id);
        }
    }
    for (&export, &o) in &objects {
        if vm.class_is_a(vm.objects[o].class, "ZoneInfo") {
            bridge.zone_objects.insert(export as u32, o);
        }
        // Non-static static-mesh actors baked into the level collision (UrineBucket grave
        // mounds, scripted props) must be what a trace returns, so their TakeDamage runs.
        // Static ones stay "world" (script treats them like the level anyway).
        if !pb(vm, o, "bStatic") && vm.get_prop(o, "StaticMesh").is_some_and(|v| v.as_resource().is_some()) {
            bridge.level_actors.insert(export as u32, o);
        }
        if vm.get_prop(o, "StaticMesh").is_some_and(|v| v.as_resource().is_some()) {
            baked_exports.insert(o, export as u32);
        }
    }
    // A loaded save has no actor for map static meshes destroyed before it was written
    // (PrepActorsByGroup: other days, holidays): their baked triangles must not collide.
    // (On a fresh level load level_objects has every map export, so nothing is removed here.)
    {
        let mut gone = 0;
        for e in m.world.level.actor_exports() {
            if !objects.contains_key(&(e as usize)) {
                m.world.level.remove_actor(e);
                gone += 1;
            }
        }
        if gone > 0 {
            eprintln!("physics: {gone} map static mesh actors absent from the level (destroyed before save) stop colliding");
        }
    }
    let mut registered = 0;
    let mut pending = Vec::new();
    let mut trace_meshes = std::collections::HashMap::new();
    let mut mesh_actors = std::collections::HashMap::new();
    for a in vm.actors.clone() {
        if bridge.phys(a).is_none() {
            if PhysHost::wants_physics(vm, a) {
                if std::env::var_os("P2GAME_STATIC_TOUCH").is_some() && pb(vm, a, "bStatic") {
                    println!("static touch actor {} ({})", vm.object_name(a), vm.classes[vm.objects[a].class].name);
                }
                let id = bridge.add_actor(vm, &mut m.world, a);
                if let Some(path) = attach_trace_mesh(&host.lib, &mut trace_meshes, vm, &mut m.world, id, a) {
                    mesh_actors.insert(a, path);
                }
                registered += 1;
            } else if !vm.objects[a].deleted && !pb(vm, a, "bStatic") {
                pending.push(a);
            }
        }
    }
    m.world.drain_events();
    let _ = m.world.take_moved();
    eprintln!(
        "physics: {} movers, {} volumes, {} dynamic actors, level {} tris, {} ms",
        m.stats.movers,
        m.stats.volumes,
        registered,
        m.world.level.triangle_count(),
        t0.elapsed().as_millis()
    );
    let ph = Rc::new(PhysHost { world: RefCell::new(m.world), bridge: RefCell::new(bridge), seen: RefCell::new(HashSet::new()), scanned: Cell::new(vm.actors.len()), frames: Cell::new(0), timings: Cell::new([0.0; 4]), pull_relinks: Cell::new(0), pending: Cell::new(pending), lib: host.lib.clone(), trace_meshes: RefCell::new(trace_meshes), mesh_actors: RefCell::new(mesh_actors), nav: RefCell::new(Default::default()), baked_exports: RefCell::new(baked_exports) });
    crate::navfix::install(vm);
    let old = host.world.borrow_mut().replace(ph.clone());
    // Also an extension host for natives (HurtRadiusEX, Karma). Drop the previous level's.
    {
        let mut ext = host.ext.borrow_mut();
        if let Some(old) = old {
            ext.retain(|h| !Rc::ptr_eq(h, &old));
        }
        ext.push(ph.clone());
    }
    let h2 = ph.clone();
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| h2.frame(vm, dt)));
    CURRENT.with(|c| *c.borrow_mut() = Some(ph.clone()));
    crate::gibs::install(vm);
    Some(ph)
}

impl Host for PhysHost {
    fn actor_spawned(&self, vm: &mut Vm, actor: ObjId) {
        self.seen.borrow_mut().insert(actor);
        self.register(vm, actor);
    }

    fn actor_destroyed(&self, _vm: &mut Vm, actor: ObjId) {
        self.mesh_actors.borrow_mut().remove(&actor);
        if let Some(e) = self.baked_exports.borrow_mut().remove(&actor) {
            self.world.borrow().level.remove_actor(e);
        }
        let mut w = self.world.borrow_mut();
        self.bridge.borrow_mut().remove_actor(&mut w, actor);
    }

    fn move_actor(&self, vm: &mut Vm, actor: ObjId, to: [f32; 3], sweep: bool) -> bool {
        if self.bridge.borrow().phys(actor).is_none() {
            self.register(vm, actor);
        }
        if self.bridge.borrow().phys(actor).is_none() {
            vm.set_prop(actor, "Location", Value::Vector(to));
            return true;
        }
        let from = v(vm.actor_loc(actor));
        self.with_actor(vm, actor, |w, id| if sweep { w.move_by(id, v(to) - from).ok } else { w.set_location(id, v(to)) }).unwrap_or(true)
    }

    fn move_smooth(&self, vm: &mut Vm, actor: ObjId, delta: [f32; 3]) -> bool {
        if self.bridge.borrow().phys(actor).is_none() {
            let l = vm.actor_loc(actor);
            vm.set_prop(actor, "Location", Value::Vector([l[0] + delta[0], l[1] + delta[1], l[2] + delta[2]]));
            return true;
        }
        self.with_actor(vm, actor, |w, id| w.move_smooth(id, v(delta)).ok).unwrap_or(true)
    }

    fn set_rotation(&self, vm: &mut Vm, actor: ObjId, rot: [i32; 3]) -> bool {
        if self.bridge.borrow().phys(actor).is_none() {
            vm.set_prop(actor, "Rotation", Value::Rotator(rot));
            return true;
        }
        self.with_actor(vm, actor, |w, id| w.set_rotation(id, rot)).unwrap_or(true)
    }

    fn trace(&self, vm: &mut Vm, from: Option<ObjId>, req: &TraceRequest) -> Option<TraceHit> {
        // The tracing actor is skipped together with its owners (UE2 ignores actors that own the
        // trace source). Weapons and other inventory are not in the physics world, so walk the
        // Owner chain to the first registered actor (the pawn holding the weapon); otherwise
        // instant-hit traces starting inside the shooter's cylinder hit the shooter.
        let mut src = None;
        let mut cur = from;
        for _ in 0..4 {
            let Some(c) = cur else { break };
            if let Some(id) = self.bridge.borrow().phys(c) {
                src = Some(id);
                break;
            }
            cur = vm.get_prop(c, "Owner").and_then(|o| o.as_obj()).filter(|o| !vm.objects[*o].deleted);
        }
        let h = self.world.borrow_mut().trace(v(req.start), v(req.end), v(req.extent), req.trace_actors, src);
        if std::env::var_os("P2GAME_TRACE_LOG").is_some() {
            let who = from.map(|f| vm.object_name(f)).unwrap_or_default();
            let what = h.as_ref().map(|h| (self.bridge.borrow().hit_object(vm, h.actor).map(|o| vm.object_name(o)), h.location, self.world.borrow().level.material_path(h.material).map(|s| s.to_string())));
            println!("TRACE {who} {:?} -> {:?} actors={} hit {:?}", req.start, req.end, req.trace_actors, what);
        }
        let h = h?;
        let actor = self.bridge.borrow().trace_hit_object(vm, &h);
        // The level surface's material (BSP surface, static mesh section, terrain layer): script
        // reads HitMaterial.SurfaceType for footsteps and impact effects.
        let material = self.world.borrow().level.material_path(h.material).map(|s| s.to_string());
        Some(TraceHit { actor, location: h.location.to_array(), normal: h.normal.to_array(), material })
    }

    fn fast_trace(&self, _vm: &mut Vm, start: [f32; 3], end: [f32; 3]) -> bool {
        self.world.borrow_mut().fast_trace(v(end), v(start))
    }

    fn line_of_sight(&self, vm: &mut Vm, viewer: ObjId, other: ObjId) -> bool {
        let eye = vm.get_prop(viewer, "Pawn").and_then(|p| p.as_obj()).unwrap_or(viewer);
        let eh = vm.get_prop(eye, "BaseEyeHeight").map(|x| x.as_float()).unwrap_or(0.0);
        let mut s = vm.actor_loc(eye);
        s[2] += eh;
        self.world.borrow_mut().fast_trace(v(vm.actor_loc(other)), v(s))
    }

    fn iterate(&self, vm: &mut Vm, this: Option<ObjId>, name: &str, args: &[Value]) -> uscript::Result<Option<Vec<Vec<Value>>>> {
        let rows = {
            let mut w = self.world.borrow_mut();
            self.bridge.borrow().iterate(vm, &mut w, this, name, args)
        };
        Ok(rows.or_else(|| uscript::host::default_iterate(vm, this, name, args)))
    }

    fn latent_begin(&self, vm: &mut Vm, actor: ObjId, latent: &mut uscript::vm::Latent) -> uscript::Result<()> {
        uscript::host::default_latent_begin(vm, actor, latent);
        Ok(())
    }

    fn native(&self, vm: &mut Vm, call: &uscript::host::NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        let me = call.this?;
        let name = call.name.to_ascii_lowercase();
        match name.as_str() {
            "hurtradiusex" => Some(Ok(self.hurt_radius_ex(vm, me, args))),
            n if n.starts_with('k') => self.karma_native(vm, me, n, args).map(Ok),
            _ => None,
        }
    }

    /// FinishInterpolation waits until the mover's interpolation ends (physics clears
    /// `bInterpolating` at the keyframe); WaitForLanding until the pawn stops falling. Movement
    /// latents (MoveTo/MoveToward) steer the pawn by Acceleration and let physics move it.
    fn latent_tick(&self, vm: &mut Vm, actor: ObjId, latent: &mut uscript::vm::Latent, dt: f32) -> uscript::Result<bool> {
        let name = latent.name.to_ascii_lowercase();
        match name.as_str() {
            "finishinterpolation" => Ok(!pb(vm, actor, "bInterpolating") || latent.elapsed > 60.0),
            "waitforlanding" => {
                let pawn = vm.get_prop(actor, "Pawn").and_then(|v| v.as_obj()).unwrap_or(actor);
                let falling = vm.get_prop(pawn, "Physics").is_some_and(|p| p.as_int() == 2);
                Ok(!falling || latent.elapsed > 4.0)
            }
            "moveto" | "movetoward" | "movetowithradius" | "movetowardwithradius" => Ok(self.steer(vm, actor, latent, dt)),
            "finishrotation" => {
                let pawn = vm.get_prop(actor, "Pawn").and_then(|v| v.as_obj()).unwrap_or(actor);
                let cur = vm.actor_rot(pawn);
                let want = vm.get_prop(pawn, "DesiredRotation").map(|v| v.as_rot()).unwrap_or(cur);
                Ok(((cur[1] - want[1]) as i16).abs() < 300 || latent.elapsed > 3.0)
            }
            _ => Ok(true),
        }
    }
}

/// Level hook: installs physics for every level the game boots or travels to.
pub fn level_hook(host: &GameHost, vm: &mut Vm) {
    CURRENT.with(|c| *c.borrow_mut() = None);
    if std::env::var_os("P2GAME_NO_PHYSICS").is_some() {
        return;
    }
    install(host, vm);
}
