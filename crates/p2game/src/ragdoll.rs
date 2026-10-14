//! Karma ragdolls for the game host (`p2anim::ragdoll`).
//!
//! Script starts a ragdoll the way the engine expects (P2MoCapPawn.PlayDying): it gets a
//! `KarmaParamsSkel` into `KParams` (with `KSkeleton` = `Avg_Dude_Skel` etc.), calls
//! `SetPhysics(PHYS_KarmaRagDoll)`, then `KSetSkelVel` / `KAddImpulse`. Each frame (a frame hook,
//! after animation and actor physics) this module:
//! - notices actors whose `Physics` is `PHYS_KarmaRagDoll` with a skeleton asset and an animation
//!   instance, and builds a [`Ragdoll`] from their current pose (the asset comes from
//!   `KarmaData\<KSkeleton>.ka`);
//! - steps every live ragdoll against the level's static collision (`p2collide`, shared with the
//!   physics subsystem), drives the pose through `AnimInstance::set_physics_pose`, and keeps the
//!   actor's Location on the pelvis so attachments, culling and traces follow the body;
//! - puts ragdolls to sleep when they come to rest (`KFreezeRagdoll` freezes immediately;
//!   `KDisableKarma` or leaving PHYS_KarmaRagDoll drops the ragdoll and returns to animation).
//!
//! Natives: `KSetSkelVel`, `KAddImpulse`, `KWake`, `KFreezeRagdoll`, `KDisableKarma`,
//! `KIsRagdollAvailable`, `KMakeRagdollAvailable` (only for ragdoll actors; other Karma actors
//! fall through to the physics host).
//!
//! `P2GAME_NO_RAGDOLL=1` disables the subsystem.

use crate::host::{GameHost, actor_world};
use glam::{Mat4, Vec3};
use p2anim::ragdoll::{KaAsset, Ragdoll, RagdollWorld, load_karma_dir};
use p2collide::{Shape, TraceFlags};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use uscript::host::{Host, NativeCall};
use uscript::vm::{ObjId, Value, Vm};

/// `EPhysics::PHYS_KarmaRagDoll`.
pub const PHYS_KARMA_RAGDOLL: u8 = 14;

pub struct Live {
    pub doll: Ragdoll,
    pub frozen: bool,
    /// Frozen dolls: actor transform the pose was last built for (skip rebuilding it).
    pub posed_at: Option<Mat4>,
}

pub struct Ragdolls {
    pub assets: HashMap<String, Arc<KaAsset>>,
    pub live: RefCell<HashMap<ObjId, Live>>,
    /// Velocity / impulses requested by script before the ragdoll exists (same frame).
    pending: RefCell<HashMap<ObjId, (Option<Vec3>, Vec<(Vec3, Vec3, Option<String>)>)>>,
    pub stats: RefCell<RagdollStats>,
    /// The local player's Viewport was checked for this level.
    viewport_checked: std::cell::Cell<bool>,
}

#[derive(Default, Debug, Clone)]
pub struct RagdollStats {
    pub started: u64,
    pub missing_asset: u64,
    pub steps: u64,
    pub asleep: u64,
}

thread_local! {
    static RAGDOLLS: RefCell<Option<Rc<Ragdolls>>> = const { RefCell::new(None) };
}

pub fn ragdolls() -> Option<Rc<Ragdolls>> {
    RAGDOLLS.with(|r| r.borrow().clone())
}

/// Loads `KarmaData\*.ka` from the install and registers the per-level hook.
pub fn install(game_dir: PathBuf) -> Option<Rc<Ragdolls>> {
    if std::env::var_os("P2GAME_NO_RAGDOLL").is_some() {
        return None;
    }
    let assets = load_karma_dir(&upkg::ci::join(&game_dir, "KarmaData")).into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
    let r = Rc::new(Ragdolls { assets, live: RefCell::new(HashMap::new()), pending: RefCell::new(HashMap::new()), stats: RefCell::new(RagdollStats::default()), viewport_checked: std::cell::Cell::new(false) });
    RAGDOLLS.with(|s| *s.borrow_mut() = Some(r.clone()));
    crate::host::register_level_hook(level_hook);
    Some(r)
}

fn level_hook(host: &GameHost, _vm: &mut Vm) {
    let Some(r) = ragdolls() else { return };
    r.live.borrow_mut().clear();
    r.pending.borrow_mut().clear();
    r.viewport_checked.set(false);
    host.ext.borrow_mut().push(Rc::new(RagdollHost { r: r.clone() }));
    let r2 = r.clone();
    // The frame hook needs the host's animation table. The hook is owned by `host.frame_hooks`
    // and only runs from `host.tick_anims(&self)` (after the animation borrow is released), so
    // the host is alive and pinned (it lives in an Rc) whenever the hook runs.
    let hp: *const GameHost = host;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        // SAFETY: see above; the pointer cannot outlive the host that owns this closure.
        let host = unsafe { &*hp };
        r2.frame(vm, host, dt)
    }));
}

/// Collision against the level's static geometry.
struct LevelWorld(Option<Arc<p2collide::World>>);

impl RagdollWorld for LevelWorld {
    fn resolve(&mut self, p: Vec3, radius: f32) -> Option<(Vec3, Vec3)> {
        let w = self.0.as_ref()?;
        let (n, depth) = w.penetration(p, &Shape::Box(Vec3::splat(radius)))?;
        Some((p + n * (depth + 0.05), n))
    }
    fn sweep(&mut self, from: Vec3, to: Vec3, radius: f32) -> Option<(Vec3, Vec3)> {
        let w = self.0.as_ref()?;
        if (to - from).length_squared() < 1e-4 {
            return None;
        }
        let h = w.trace(from, to, &Shape::Box(Vec3::splat(radius)), TraceFlags::ALL)?;
        (!h.start_solid).then_some((h.location + h.normal * 0.05, h.normal))
    }
}

fn physics(vm: &Vm, a: ObjId) -> u8 {
    vm.get_prop(a, "Physics").map(|v| v.as_int() as u8).unwrap_or(0)
}

/// `KParams.KSkeleton` when `KParams` is a KarmaParamsSkel.
fn skeleton_name(vm: &Vm, a: ObjId) -> Option<String> {
    let kp = vm.get_prop(a, "KParams").and_then(|v| v.as_obj())?;
    let n = vm.get_prop(kp, "KSkeleton")?.as_name();
    (!n.is_empty() && !n.eq_ignore_ascii_case("None")).then_some(n)
}

impl Ragdolls {
    /// The engine always gives the local PlayerController a `Viewport` as `Player`; script relies
    /// on it (P2MoCapPawn.GetKarmaSkeleton only hands out ragdoll skeletons through the player
    /// whose `Player` is a Viewport). Windowed front ends link their own (p2canvas MenuHost);
    /// headless runs get a bare one here.
    fn ensure_local_viewport(&self, vm: &mut Vm) {
        if self.viewport_checked.get() {
            return;
        }
        let Some(pc) = crate::ai::local_player_controller(vm) else { return };
        if vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj()).is_none() {
            // Not logged in yet; check again next frame.
            return;
        }
        self.viewport_checked.set(true);
        if vm.get_prop(pc, "Player").and_then(|v| v.as_obj()).is_some() {
            return;
        }
        // Viewport is native-only; a Player object stands for it (the VM's Viewport casts accept
        // Players).
        let cls = match vm.find_class("Engine", "Viewport").or_else(|_| vm.find_class("Engine", "Player")) {
            Ok(c) => c,
            Err(e) => {
                vm.push_log(format!("ragdoll: no Engine.Viewport class ({e})"));
                return;
            }
        };
        let vp = vm.new_object(cls);
        vm.set_prop(vp, "Actor", Value::Object(Some(pc)));
        let ok = vm.set_prop(pc, "Player", Value::Object(Some(vp)));
        vm.push_log(format!("ragdoll: local viewport {} for {} ({})", vm.object_name(vp), vm.object_name(pc), if ok { "linked" } else { "no Player slot" }));
    }

    fn frame(&self, vm: &mut Vm, host: &GameHost, dt: f32) {
        self.ensure_local_viewport(vm);
        // Start ragdolls for actors that just switched to PHYS_KarmaRagDoll.
        let candidates: Vec<ObjId> = host.anims.borrow().keys().copied().filter(|&a| !vm.objects[a].deleted && physics(vm, a) == PHYS_KARMA_RAGDOLL && !self.live.borrow().contains_key(&a)).collect();
        if std::env::var_os("P2GAME_RAGDOLL_TRACE").is_some() {
            // Ragdoll-physics actors with no animation instance never start; dead pawns that
            // never reached ragdoll physics are reported once.
            let anims = host.anims.borrow();
            for &a in vm.actors.iter() {
                if vm.objects[a].deleted || self.live.borrow().contains_key(&a) {
                    continue;
                }
                if physics(vm, a) == PHYS_KARMA_RAGDOLL && !anims.contains_key(&a) {
                    eprintln!("ragdoll: {} is PHYS_KarmaRagDoll but has no animation instance", vm.object_name(a));
                }
                if vm.state_name(a) == "Dying" && vm.get_prop(a, "KParams").and_then(|v| v.as_obj()).is_some() && physics(vm, a) != PHYS_KARMA_RAGDOLL {
                    eprintln!("ragdoll: {} Dying with KParams but Physics {}", vm.object_name(a), physics(vm, a));
                }
            }
        }
        for a in candidates {
            self.start(vm, host, a);
        }
        // Drop ragdolls whose actor went away or left ragdoll physics.
        let ids: Vec<ObjId> = self.live.borrow().keys().copied().collect();
        let mut dropped = Vec::new();
        for a in &ids {
            if vm.objects.get(*a).is_none_or(|o| o.deleted) || physics(vm, *a) != PHYS_KARMA_RAGDOLL {
                dropped.push(*a);
            }
        }
        for a in dropped {
            self.live.borrow_mut().remove(&a);
            if let Some(x) = host.anims.borrow_mut().get_mut(&a) {
                x.inst.set_physics_pose(None);
            }
        }
        if self.live.borrow().is_empty() {
            return;
        }
        let mut world = LevelWorld(crate::physics::level_collision());
        let mut live = self.live.borrow_mut();
        let mut anims = host.anims.borrow_mut();
        for (&a, l) in live.iter_mut() {
            let Some(x) = anims.get_mut(&a) else { continue };
            if !l.frozen {
                l.doll.step(dt, &mut world);
                self.stats.borrow_mut().steps += 1;
                if l.doll.is_asleep() {
                    l.frozen = true;
                    self.stats.borrow_mut().asleep += 1;
                }
                if std::env::var_os("P2GAME_RAGDOLL_TRACE").is_some() && (l.doll.time / 1.0).floor() != ((l.doll.time - dt) / 1.0).floor() {
                    eprintln!("RAGDOLL {} t={:.1} speed {:.0} asleep {} root {:?}", vm.object_name(a), l.doll.time, l.doll.speed, l.frozen, l.doll.root_position().round());
                }
                // The actor follows the pelvis (keeps its rotation; the pose is re-expressed).
                let root = l.doll.root_position();
                vm.set_prop(a, "Location", Value::Vector(root.to_array()));
                vm.set_prop(a, "Velocity", Value::Vector([0.0; 3]));
            }
            let m2w = actor_world(vm, a) * x.inst.skeleton.mesh_xform;
            // A settled doll keeps its pose: rebuilding the bone matrices each frame cost
            // ~2 ms on MilitaryBase with nothing moving.
            if l.frozen && l.posed_at == Some(m2w) {
                continue;
            }
            l.posed_at = l.frozen.then_some(m2w);
            let mats = l.doll.bone_matrices(&x.inst.skeleton, m2w);
            x.inst.set_physics_pose(Some(mats));
        }
    }

    fn start(&self, vm: &mut Vm, host: &GameHost, a: ObjId) {
        let trace = std::env::var_os("P2GAME_RAGDOLL_TRACE").is_some();
        let Some(skel) = skeleton_name(vm, a) else {
            if trace {
                eprintln!("ragdoll: {} has no KParams skeleton", vm.object_name(a));
            }
            return;
        };
        let Some(asset) = self.assets.get(&skel.to_ascii_lowercase()).cloned() else {
            self.stats.borrow_mut().missing_asset += 1;
            if trace {
                eprintln!("ragdoll: {} skeleton {skel} not in KarmaData", vm.object_name(a));
            }
            return;
        };
        if trace {
            eprintln!("ragdoll: start {} skeleton {skel}", vm.object_name(a));
        }
        let aw = actor_world(vm, a);
        let vel = Vec3::from(vm.get_prop(a, "Velocity").map(|v| v.as_vec()).unwrap_or_default());
        let mut anims = host.anims.borrow_mut();
        let Some(x) = anims.get_mut(&a) else { return };
        let mut doll = Ragdoll::new(&asset, &mut x.inst, aw, vel);
        if doll.body_count() == 0 {
            self.stats.borrow_mut().missing_asset += 1;
            return;
        }
        let (pv, pi) = self.pending.borrow_mut().remove(&a).unwrap_or_default();
        if trace {
            eprintln!("ragdoll: {} start vel {pv:?} impulses {pi:?} actor vel {vel:?}", vm.object_name(a));
        }
        if let Some(v) = pv {
            doll.set_velocity(v);
        }
        for (dv, at, bone) in pi {
            doll.add_impulse(dv, at, bone.as_deref());
        }
        self.stats.borrow_mut().started += 1;
        self.live.borrow_mut().insert(a, Live { doll, frozen: false, posed_at: None });
    }

    fn native(&self, vm: &mut Vm, me: ObjId, name: &str, args: &[Value]) -> Option<Value> {
        let ragdoll_actor = physics(vm, me) == PHYS_KARMA_RAGDOLL || self.live.borrow().contains_key(&me);
        if !ragdoll_actor && !matches!(name, "kisragdollavailable" | "kmakeragdollavailable") {
            return None;
        }
        let vec = |k: usize| Vec3::from(args.get(k).map(|v| v.as_vec()).unwrap_or_default());
        let mut live = self.live.borrow_mut();
        match name {
            "ksetskelvel" => {
                let v = vec(0);
                match live.get_mut(&me) {
                    Some(l) => {
                        l.doll.set_velocity(v);
                        l.frozen = false;
                    }
                    None => self.pending.borrow_mut().entry(me).or_default().0 = Some(v),
                }
            }
            "kaddimpulse" => {
                // Impulse (mass * velocity, UU) at a point: as a velocity change for a ~200 kg
                // scaled body (P2 pawns' Mass is in that range; CapKarmaMomentum caps it).
                let mass = vm.get_prop(me, "Mass").map(|v| v.as_float()).unwrap_or(100.0).max(1.0);
                let dv = vec(0) / mass;
                let at = vec(1);
                // Optional bone (P2MoCapPawn passes the pelvis for shots on corpses).
                let bone = args.get(2).map(|v| v.as_name()).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("None"));
                match live.get_mut(&me) {
                    Some(l) => {
                        l.doll.add_impulse(dv, at, bone.as_deref());
                        l.frozen = false;
                    }
                    None => self.pending.borrow_mut().entry(me).or_default().1.push((dv, at, bone)),
                }
            }
            "kwake" => {
                if let Some(l) = live.get_mut(&me) {
                    l.frozen = false;
                    l.doll.wake();
                }
            }
            "kfreezeragdoll" => {
                if let Some(l) = live.get_mut(&me) {
                    l.frozen = true;
                }
            }
            "kdisablekarma" => {
                live.remove(&me);
            }
            "kisragdollavailable" | "kmakeragdollavailable" => return Some(Value::Bool(true)),
            "kisawake" => return Some(Value::Bool(live.get(&me).is_some_and(|l| !l.frozen))),
            _ => return None,
        }
        Some(Value::Void)
    }
}

struct RagdollHost {
    r: Rc<Ragdolls>,
}

impl Host for RagdollHost {
    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        let me = call.this?;
        let name = call.name.to_ascii_lowercase();
        if !name.starts_with('k') {
            return None;
        }
        self.r.native(vm, me, &name, args).map(Ok)
    }

    fn actor_destroyed(&self, _vm: &mut Vm, actor: ObjId) {
        self.r.live.borrow_mut().remove(&actor);
        self.r.pending.borrow_mut().remove(&actor);
    }
}
