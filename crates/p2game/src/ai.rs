//! NPC perception and stasis for the game host: the engine-side parts of POSTAL 2's script AI.
//!
//! The AI itself is script (AIPack / BasePeople / Postal2Game controllers). The engine supplies:
//!
//! - **Rendering time.** `Actor.LastRenderTime` is set when an actor is drawn. Headless runs (and
//!   the windowed renderer, which does not report per-actor visibility) use the player's view
//!   instead: a pawn counts as rendered when it is within view distance, inside the view cone
//!   (`FOVAngle` plus a margin) and not hidden behind level geometry from the eye.
//! - **Stasis** (native FPSPawn tick). A controller in stasis (`bStasis`) is woken with
//!   `ComeOutOfStasis(false)` as soon as its pawn is rendered again (slider-stasis pawns stay
//!   asleep). A pawn not rendered for `TimeTillStasis` seconds with `bAllowStasis` gets
//!   `bPendingStasis` set on its controller, which script turns into `GoIntoStasis` when it is
//!   idle. `SafeToGoIntoStasis(MinPlayerDist)` is false while any player pawn is closer.
//! - **Sight.** Every controller counts down `SightCounter` (0.15 to 0.25 s per check). On each
//!   check an AI controller whose pawn is awake sees the local player's pawn (`SeePlayer`) when it
//!   is within `SightRadius`, inside `PeripheralVision` (cosine of the half-angle) and in line of
//!   sight. `CanSee` uses the same test.
//! - **Hearing.** `MakeNoise(Loudness)` from a player-instigated actor reaches every awake AI
//!   controller within `HearingThreshold * Loudness`: with line of sight (`bLOSHearing`), in the
//!   same zone (`bSameZoneHearing`), or through walls at half range (`bMuffledHearing`). Each one
//!   gets `HearNoise(Loudness, NoiseMaker)`.
//!
//! Movement (MoveTo / MoveToward steering) and traces live in the physics host
//! (`physics.rs`); route finding in the VM's nav natives (`p2nav`).
//!
//! `P2GAME_NO_AI=1` disables the subsystem.

use crate::host::GameHost;
use glam::Vec3;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use uscript::vm::{ObjId, Value, Vm};

/// Maximum distance at which an actor is considered drawn (UU), roughly the far plane of the
/// game's outdoor maps.
const VIEW_DISTANCE: f32 = 6000.0;
/// Extra half-angle (degrees) beyond FOVAngle/2 for "rendered" (actors partly on screen).
const VIEW_MARGIN_DEG: f32 = 15.0;

#[derive(Default, Debug, Clone)]
pub struct AiStats {
    pub in_range: u64,
    pub in_cone: u64,
    pub occluded: u64,
    pub rendered: u64,
    pub woke: u64,
    pub pending: u64,
    pub seen: u64,
    pub heard: u64,
    pub walk_anims: u64,
    pub walk_anim_missing: u64,
}

pub struct Ai {
    pub stats: RefCell<AiStats>,
    enabled: Cell<bool>,
    /// Pawns currently playing a physics-driven movement animation (name of the sequence).
    moving: RefCell<std::collections::HashMap<ObjId, String>>,
    /// Last occlusion result per pawn for the "rendered" test: (level time of the next re-check,
    /// visible). The view-cone test runs every frame; the two traces only every ~0.1 s per pawn
    /// (staggered), and a visible pawn keeps getting `LastRenderTime = now` in between.
    occl: RefCell<std::collections::HashMap<ObjId, (f32, bool)>>,
    /// Turn-in-place: last yaw per standing pawn, and whether its turn anim is playing.
    turning: RefCell<std::collections::HashMap<ObjId, (i32, bool)>>,
}

thread_local! {
    static AI: RefCell<Option<Rc<Ai>>> = const { RefCell::new(None) };
}

pub fn ai() -> Option<Rc<Ai>> {
    AI.with(|a| a.borrow().clone())
}

pub fn install() -> Option<Rc<Ai>> {
    if std::env::var_os("P2GAME_NO_AI").is_some() {
        return None;
    }
    let a = Rc::new(Ai { stats: RefCell::new(AiStats::default()), enabled: Cell::new(true), moving: RefCell::new(Default::default()), occl: RefCell::new(Default::default()), turning: RefCell::new(Default::default()) });
    AI.with(|s| *s.borrow_mut() = Some(a.clone()));
    crate::host::register_level_hook(level_hook);
    Some(a)
}

fn level_hook(host: &GameHost, vm: &mut Vm) {
    let Some(a) = ai() else { return };
    vm.register_native("MakeNoise", make_noise);
    vm.register_native("SafeToGoIntoStasis", safe_to_go_into_stasis);
    vm.register_native("CanSee", can_see_native);
    a.moving.borrow_mut().clear();
    a.occl.borrow_mut().clear();
    ISA.with(|m| m.borrow_mut().clear());
    let a2 = a.clone();
    let hp: *const GameHost = host;
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, dt| {
        if a2.enabled.get() {
            a2.frame(vm, dt);
            // SAFETY: this hook is owned by the host's frame_hooks and only runs while it lives.
            a2.movement_anims(vm, unsafe { &*hp }, dt);
        }
    }));
}

fn pb(vm: &Vm, a: ObjId, n: &str) -> bool {
    vm.get_prop(a, n).is_some_and(|v| v.as_bool())
}

fn pf(vm: &Vm, a: ObjId, n: &str, d: f32) -> f32 {
    vm.get_prop(a, n).map(|v| v.as_float()).filter(|f| f.is_finite()).unwrap_or(d)
}

fn obj(vm: &Vm, a: ObjId, n: &str) -> Option<ObjId> {
    vm.get_prop(a, n).and_then(|v| v.as_obj()).filter(|&o| !vm.objects[o].deleted)
}

fn level_time(vm: &Vm) -> f32 {
    vm.level.map(|l| pf(vm, l, "TimeSeconds", 0.0)).unwrap_or(0.0)
}

thread_local! {
    /// `P2GAME_AI_TIMING=1`: per-stage perception timings on stderr every frame.
    static AI_TIMING: bool = std::env::var_os("P2GAME_AI_TIMING").is_some();
    /// `class_is_a` results per (class, name): the perception passes ask the same few questions of
    /// every actor every frame, and walking the super chain with string compares adds up.
    static ISA: RefCell<std::collections::HashMap<(usize, &'static str), bool>> = RefCell::new(Default::default());
}

fn is_a(vm: &Vm, a: ObjId, class: &'static str) -> bool {
    let c = vm.objects[a].class;
    ISA.with(|m| *m.borrow_mut().entry((c, class)).or_insert_with(|| vm.class_is_a(c, class)))
}

/// Unreal rotator to forward vector.
fn rot_dir(r: [i32; 3]) -> Vec3 {
    let u = std::f32::consts::TAU / 65536.0;
    let (p, y) = (r[0] as f32 * u, r[1] as f32 * u);
    Vec3::new(p.cos() * y.cos(), p.cos() * y.sin(), p.sin())
}

/// Line of sight through level geometry (static collision; actors do not block sight). Contacts
/// the line starts inside (the eye in a wall's skin) or reaches only at its very end (the
/// target standing on the floor) do not block.
fn clear_line(a: Vec3, b: Vec3) -> bool {
    match crate::physics::level_collision() {
        Some(w) => match w.line(a, b) {
            None => true,
            Some(h) => h.start_solid || h.t >= 0.97,
        },
        None => true,
    }
}

fn eye(vm: &Vm, p: ObjId) -> Vec3 {
    let mut l = Vec3::from(vm.actor_loc(p));
    l.z += pf(vm, p, "BaseEyeHeight", pf(vm, p, "EyeHeight", 0.0));
    l
}

/// True if `a` should not draw: its own `bHidden`, or it rides a bone of a hidden actor. Bone
/// attachments (heads, boltons, weapon attachments) render as part of their base mesh, so a
/// body hidden by script (slider stasis) takes its head with it.
pub fn render_hidden(vm: &Vm, a: ObjId) -> bool {
    let mut cur = a;
    for _ in 0..8 {
        if vm.objects.get(cur).is_none_or(|o| o.deleted) || pb(vm, cur, "bHidden") {
            return true;
        }
        let on_bone = matches!(vm.get_prop(cur, "AttachmentBone"), Some(Value::Name(n)) if !n.is_empty() && !n.eq_ignore_ascii_case("None"));
        match obj(vm, cur, "Base") {
            Some(b) if on_bone && b != cur => cur = b,
            _ => return false,
        }
    }
    false
}

/// The local human player's controller: a PlayerController that is not a `Camera` (Camera
/// subclasses PlayerController in UE2), preferring one that possesses a pawn.
pub fn local_player_controller(vm: &Vm) -> Option<ObjId> {
    let pcs: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && is_a(vm, a, "PlayerController") && !is_a(vm, a, "Camera")).collect();
    pcs.iter().copied().find(|&c| obj(vm, c, "Pawn").is_some()).or_else(|| pcs.first().copied())
}

/// The local player: (controller, pawn, eye, view direction, cosine of the rendered half-angle).
fn local_view(vm: &Vm) -> Option<(ObjId, Option<ObjId>, Vec3, Vec3, f32)> {
    let pc = local_player_controller(vm)?;
    let pawn = obj(vm, pc, "Pawn");
    let target = obj(vm, pc, "ViewTarget").or(pawn).unwrap_or(pc);
    let mut loc = Vec3::from(vm.actor_loc(target));
    loc.z += pf(vm, target, "EyeHeight", 0.0);
    let rot = vm.actor_rot(pc);
    let fov = pf(vm, pc, "FOVAngle", 90.0);
    let fov = if fov.is_finite() { fov.clamp(10.0, 170.0) } else { 90.0 };
    let half = ((fov * 0.5 + VIEW_MARGIN_DEG).min(89.0)).to_radians();
    Some((pc, pawn, loc, rot_dir(rot), half.cos()))
}

/// `CanSee`: within SightRadius, inside PeripheralVision and in line of sight.
pub fn can_see(vm: &Vm, viewer: ObjId, other: ObjId) -> bool {
    let vp = if is_a(vm, viewer, "Controller") { obj(vm, viewer, "Pawn") } else { Some(viewer) };
    let Some(vp) = vp else { return false };
    let from = eye(vm, vp);
    let to = Vec3::from(vm.actor_loc(other));
    let d = to - from;
    let dist = d.length();
    if dist > pf(vm, vp, "SightRadius", 5000.0) {
        return false;
    }
    let fwd = rot_dir(vm.actor_rot(vp));
    let periph = pf(vm, vp, "PeripheralVision", 0.0);
    if dist > 1.0 && fwd.dot(d / dist) < periph {
        return false;
    }
    clear_line(from, to) || clear_line(from, to + Vec3::Z * pf(vm, other, "BaseEyeHeight", 0.0))
}

fn can_see_native(vm: &mut Vm, a: &mut Vec<Value>) -> uscript::Result<Value> {
    let Some(me) = vm.native_self() else { return Ok(Value::Bool(false)) };
    let Some(o) = a.first().and_then(|v| v.as_obj()).filter(|&o| !vm.objects[o].deleted) else { return Ok(Value::Bool(false)) };
    Ok(Value::Bool(can_see(vm, me, o)))
}

fn safe_to_go_into_stasis(vm: &mut Vm, a: &mut Vec<Value>) -> uscript::Result<Value> {
    let Some(me) = vm.native_self() else { return Ok(Value::Bool(true)) };
    let min = a.first().map(|v| v.as_float()).unwrap_or(1000.0);
    let Some(p) = obj(vm, me, "Pawn") else { return Ok(Value::Bool(true)) };
    let here = Vec3::from(vm.actor_loc(p));
    let near = vm.actors.iter().copied().any(|c| {
        !vm.objects[c].deleted && is_a(vm, c, "PlayerController") && obj(vm, c, "Pawn").is_some_and(|pp| Vec3::from(vm.actor_loc(pp)).distance(here) < min)
    });
    Ok(Value::Bool(!near))
}

/// `Actor.MakeNoise(Loudness)`: delivers `HearNoise` to AI controllers that can hear it.
fn make_noise(vm: &mut Vm, a: &mut Vec<Value>) -> uscript::Result<Value> {
    let Some(me) = vm.native_self() else { return Ok(Value::Void) };
    let loud = a.first().map(|v| v.as_float()).unwrap_or(1.0).max(0.0);
    if loud <= 0.0 || vm.objects[me].deleted {
        return Ok(Value::Void);
    }
    // Only noises made by (or for) players are propagated, as the engine does for AI.
    let inst = if is_a(vm, me, "Pawn") { Some(me) } else { obj(vm, me, "Instigator") };
    let player_made = inst.and_then(|p| obj(vm, p, "Controller")).is_some_and(|c| is_a(vm, c, "PlayerController") || pb(vm, c, "bIsPlayer"));
    if !player_made {
        return Ok(Value::Void);
    }
    let src = Vec3::from(vm.actor_loc(me));
    let src_zone = vm.get_prop(me, "Region").cloned();
    let listeners: Vec<(ObjId, ObjId)> = vm
        .actors
        .iter()
        .copied()
        .filter(|&c| !vm.objects[c].deleted && is_a(vm, c, "AIController") && !pb(vm, c, "bStasis"))
        .filter_map(|c| obj(vm, c, "Pawn").filter(|&p| Some(p) != inst && p != me).map(|p| (c, p)))
        .collect();
    let mut heard = 0;
    for (c, p) in listeners {
        let range = pf(vm, p, "HearingThreshold", 2800.0) * loud;
        let at = eye(vm, p);
        let dist = at.distance(src);
        if dist > range {
            continue;
        }
        let los = || clear_line(src + Vec3::Z * 8.0, at);
        let same_zone = || src_zone.is_some() && vm.get_prop(p, "Region").cloned() == src_zone;
        let hears = (pb(vm, p, "bLOSHearing") && los()) || (pb(vm, p, "bSameZoneHearing") && same_zone()) || (pb(vm, p, "bMuffledHearing") && dist * 2.0 <= range) || (pb(vm, p, "bAdjacentZoneHearing") && dist * 1.5 <= range);
        if hears {
            vm.fire_event(c, "HearNoise", vec![Value::Float(loud), Value::Object(Some(me))]);
            heard += 1;
        }
    }
    if let Some(ai) = ai() {
        ai.stats.borrow_mut().heard += heard;
    }
    Ok(Value::Void)
}

impl Ai {
    /// Native movement animation (UE2 `UpdateMovementAnimation`) for pawns with
    /// `bPhysicsAnimUpdate`: while walking faster than a crawl, channel 0 loops
    /// `MovementAnims[dir]` (0 forward, 1 back, 2 left, 3 right of the pawn's facing) at a rate
    /// that follows the speed. When the pawn stops, script gets `PlayWaiting` to pick its idle.
    /// Controllers with `bControlAnimations` (scripted sequences) keep control.
    fn movement_anims(&self, vm: &mut Vm, host: &GameHost, dt: f32) {
        let pawns: Vec<ObjId> = host.anims.borrow().keys().copied().filter(|&p| !vm.objects[p].deleted && is_a(vm, p, "Pawn") && pb(vm, p, "bPhysicsAnimUpdate")).collect();
        let mut stopped = Vec::new();
        let dt = dt.max(1e-3);
        {
            let mut moving = self.moving.borrow_mut();
            moving.retain(|p, _| vm.objects.get(*p).is_some_and(|o| !o.deleted));
            let mut turning = self.turning.borrow_mut();
            turning.retain(|p, _| vm.objects.get(*p).is_some_and(|o| !o.deleted));
            let mut anims = host.anims.borrow_mut();
            for p in pawns {
                let phys = vm.get_prop(p, "Physics").map(|v| v.as_int()).unwrap_or(0);
                let v = Vec3::from(vm.get_prop(p, "Velocity").map(|v| v.as_vec()).unwrap_or_default());
                let speed = Vec3::new(v.x, v.y, 0.0).length();
                let scripted = obj(vm, p, "Controller").is_some_and(|c| pb(vm, c, "bControlAnimations"));
                let yaw = vm.actor_rot(p)[1];
                let (old_yaw, was_turning) = turning.get(&p).copied().unwrap_or((yaw, false));
                if phys != 1 || speed < 10.0 || scripted || pb(vm, p, "bIsCrouched") {
                    // Stopped: hand channel 0 to script (PlayWaiting) only while it still plays the
                    // movement loop this code started. Script that stopped the pawn to play its own
                    // anim (AWZombie attacks: StopAcc then PlayAnim z_swiperight/z_smash) kept it;
                    // PlayWaiting replaced it with the looping idle, so AnimEnd never came and the
                    // zombie stood in SwipeRight/BigSmash forever without hitting (#55).
                    let still = |n: &str| anims.get(&p).and_then(|x| x.inst.channels.first()).is_some_and(|c| c.looping && c.sequence_name().is_some_and(|s| s.eq_ignore_ascii_case(n)));
                    if let Some(n) = moving.remove(&p) {
                        if phys == 1 && !scripted && still(&n) {
                            stopped.push(p);
                        }
                    }
                    // Turn in place (UE2 UpdateMovementAnimation): standing on the ground while
                    // the yaw changes faster than ~1000 units/s loops TurnLeft/RightAnim at a rate
                    // that follows the turn speed; when it stops turning the pawn gets PlayWaiting.
                    let d = ((yaw - old_yaw) as i16) as f32;
                    let turn_rate = d.abs() / dt;
                    let mut now_turning = false;
                    if phys == 1 && !scripted && speed < 10.0 && !moving.contains_key(&p) {
                        if turn_rate > 1000.0 {
                            let key = if d > 0.0 { "TurnRightAnim" } else { "TurnLeftAnim" };
                            let name = vm.get_prop(p, key).map(|v| v.as_name()).unwrap_or_default();
                            if !name.is_empty() && !name.eq_ignore_ascii_case("None") {
                                if let Some(x) = anims.get_mut(&p) {
                                    let rate = (turn_rate / 16384.0).clamp(0.5, 2.0);
                                    if was_turning { let _ = x.inst.loop_anim(&name, rate, 0.0, 0); } else { let _ = x.inst.loop_anim(&name, rate, 0.1, 0); }
                                    now_turning = true;
                                }
                            }
                        } else if was_turning && anims.get(&p).and_then(|x| x.inst.channels.first()).is_some_and(|c| c.looping && c.sequence_name().is_some_and(|s| ["TurnLeftAnim", "TurnRightAnim"].iter().any(|k| vm.get_prop(p, k).is_some_and(|v| v.as_name().eq_ignore_ascii_case(s))))) {
                            stopped.push(p);
                        }
                    }
                    turning.insert(p, (yaw, now_turning));
                    continue;
                }
                turning.insert(p, (yaw, false));
                let fwd = rot_dir([0, vm.actor_rot(p)[1], 0]);
                let dir = v.normalize_or_zero();
                let (f, r) = (fwd.dot(dir), fwd.cross(Vec3::Z).dot(dir) * -1.0);
                let idx = if f >= 0.7 { 0 } else if f <= -0.7 { 1 } else if r > 0.0 { 3 } else { 2 };
                let name = match vm.get_prop(p, "MovementAnims") {
                    Some(Value::Array(a)) => a.get(idx).or_else(|| a.first()).map(|n| n.as_name()).unwrap_or_default(),
                    _ => String::new(),
                };
                if name.is_empty() || name.eq_ignore_ascii_case("None") {
                    continue;
                }
                let walking = pb(vm, p, "bIsWalking");
                let ground = pf(vm, p, "GroundSpeed", 300.0).max(1.0) * if walking { pf(vm, p, "WalkingPct", 0.4).max(0.05) } else { 1.0 };
                let rate = (speed / ground).clamp(0.3, 2.5);
                let Some(x) = anims.get_mut(&p) else { continue };
                // A one-shot that script started on channel 0 (attack, gesture) plays out; its
                // AnimEnd hands back to ChangeAnimation/PlayWaiting. Replacing it with the walk
                // loop on the frame after (the pawn still had some velocity) swallowed every
                // AWZombie swipe/smash/spit, so their AnimEnd-driven attacks never landed (#55).
                if x.inst.channels.first().is_some_and(|c| c.playing && !c.looping) {
                    moving.remove(&p);
                    continue;
                }
                let same = moving.get(&p).is_some_and(|n| n.eq_ignore_ascii_case(&name)) && x.inst.channels.first().is_some_and(|c| c.playing && c.looping);
                if same {
                    // UE2 rescales the movement anim every tick (speed / movement speed); keeping the
                    // rate from the first frame left pawns that started walking while accelerating
                    // at ~0.67x for the whole walk (planted foot sliding forward).
                    let _ = x.inst.loop_anim(&name, rate, 0.0, 0);
                } else if x.inst.loop_anim(&name, rate, 0.2, 0) {
                    moving.insert(p, name);
                    self.stats.borrow_mut().walk_anims += 1;
                } else if !same {
                    self.stats.borrow_mut().walk_anim_missing += 1;
                }
            }
        }
        for p in stopped {
            vm.fire_event(p, "PlayWaiting", vec![]);
        }
    }

    fn frame(&self, vm: &mut Vm, dt: f32) {
        let timing = AI_TIMING.with(|t| *t);
        let t0 = std::time::Instant::now();
        let now = level_time(vm);
        let view = local_view(vm);
        let pawns: Vec<ObjId> = vm.actors.iter().copied().filter(|&a| !vm.objects[a].deleted && is_a(vm, a, "Pawn")).collect();
        let t_scan = t0.elapsed();
        let mut st = self.stats.borrow_mut();
        // 1. Rendered this frame: pawns the player's camera can see.
        if let Some((_, ppawn, eye_at, fwd, cos_half)) = view {
            for &p in &pawns {
                if Some(p) == ppawn || render_hidden(vm, p) {
                    continue;
                }
                let at = Vec3::from(vm.actor_loc(p));
                let d = at - eye_at;
                let dist = d.length();
                if dist > VIEW_DISTANCE {
                    continue;
                }
                st.in_range += 1;
                let inside = dist < 60.0 || fwd.dot(d / dist) >= cos_half;
                if !inside {
                    continue;
                }
                st.in_cone += 1;
                let mut occl = self.occl.borrow_mut();
                let visible = match occl.get(&p) {
                    Some(&(next, vis)) if now < next && next - now < 0.5 => vis,
                    _ => {
                        let h = pf(vm, p, "CollisionHeight", 40.0);
                        let vis = clear_line(eye_at, at) || clear_line(eye_at, at + Vec3::Z * h * 0.8);
                        // Spread re-checks over frames: 0.08 to 0.14 s per pawn.
                        let next = now + 0.08 + 0.06 * ((p as u32).wrapping_mul(2654435761) >> 24) as f32 / 255.0;
                        occl.insert(p, (next, vis));
                        vis
                    }
                };
                if !visible {
                    st.occluded += 1;
                } else {
                    vm.set_prop(p, "LastRenderTime", Value::Float(now));
                    st.rendered += 1;
                }
            }
        }
        // 2. Native stasis tick.
        let mut wake = Vec::new();
        for &p in &pawns {
            let Some(c) = obj(vm, p, "Controller") else { continue };
            if !is_a(vm, p, "FPSPawn") || !is_a(vm, c, "FPSController") {
                continue;
            }
            let lrt = pf(vm, p, "LastRenderTime", -1e9);
            let stasis = pb(vm, c, "bStasis");
            if stasis && !pb(vm, p, "bSliderStasis") && lrt + dt >= now {
                wake.push(c);
            } else if !stasis && pb(vm, p, "bAllowStasis") {
                let ttl = pf(vm, p, "TimeTillStasis", 0.0);
                if ttl > 0.0 && lrt + ttl < now && !pb(vm, c, "bPendingStasis") {
                    vm.set_prop(c, "bPendingStasis", Value::Bool(true));
                    st.pending += 1;
                }
            }
        }
        st.woke += wake.len() as u64;
        drop(st);
        let t_render = t0.elapsed();
        for c in wake {
            vm.fire_event(c, "ComeOutOfStasis", vec![Value::Bool(false)]);
        }
        let t_wake = t0.elapsed();
        // 2b. Controller focus (native Controller rotation update): an AI controller with a Focus
        // turns its pawn toward it. The pawn's DesiredRotation yaw is set; physics turns the pawn at
        // RotationRate (bRotateToDesired). While a movement latent steers, the movement direction
        // wins (physics::steer faces the destination).
        for &p in &pawns {
            let Some(c) = obj(vm, p, "Controller") else { continue };
            if !is_a(vm, c, "AIController") || pb(vm, c, "bStasis") {
                continue;
            }
            let Some(f) = obj(vm, c, "Focus").filter(|&f| f != p && !vm.objects[f].deleted) else { continue };
            if vm.objects[c].exec.latent.as_ref().is_some_and(|l| l.name.to_ascii_lowercase().starts_with("move")) {
                continue;
            }
            let d = Vec3::from(vm.actor_loc(f)) - Vec3::from(vm.actor_loc(p));
            if d.x.abs() + d.y.abs() < 1.0 {
                continue;
            }
            let yaw = (d.y.atan2(d.x) * 32768.0 / std::f32::consts::PI) as i32 & 0xffff;
            let mut want = vm.get_prop(p, "DesiredRotation").map(|v| v.as_rot()).unwrap_or_default();
            if want[1] & 0xffff != yaw {
                want[1] = yaw;
                vm.set_prop(p, "DesiredRotation", Value::Rotator(want));
            }
            // Controller rotation follows the view (script reads Controller.Rotation for aim).
            let mut cr = vm.actor_rot(c);
            cr[1] = yaw;
            vm.set_prop(c, "Rotation", Value::Rotator(cr));
        }
        // 3. Sight checks against the local player's pawn.
        let Some((_, Some(ppawn), _, _, _)) = view else { return };
        if vm.objects[ppawn].deleted {
            return;
        }
        let ctrls: Vec<(ObjId, ObjId)> = pawns.iter().copied().filter(|&p| p != ppawn).filter_map(|p| obj(vm, p, "Controller").map(|c| (c, p))).filter(|&(c, _)| is_a(vm, c, "AIController")).collect();
        let mut seen = Vec::new();
        let mut checks = 0;
        for (c, _) in ctrls {
            let sc = pf(vm, c, "SightCounter", 0.0) - dt;
            if sc >= 0.0 {
                vm.set_prop(c, "SightCounter", Value::Float(sc));
                continue;
            }
            let next = sc + 0.15 + 0.1 * vm.frand();
            vm.set_prop(c, "SightCounter", Value::Float(next.max(0.0)));
            if pb(vm, c, "bStasis") || vm.is_ignored(c, "SeePlayer") {
                continue;
            }
            checks += 1;
            if can_see(vm, c, ppawn) {
                seen.push(c);
            }
        }
        let t_sight = t0.elapsed();
        self.stats.borrow_mut().seen += seen.len() as u64;
        let n_seen = seen.len();
        for c in seen {
            vm.fire_event(c, "SeePlayer", vec![Value::Object(Some(ppawn))]);
        }
        if timing {
            let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
            eprintln!(
                "ai: scan {:.2} render+stasis {:.2} wake {:.2} sight {:.2} ({checks} checks) SeePlayer {:.2} ({n_seen}) pawns {}",
                ms(t_scan),
                ms(t_render - t_scan),
                ms(t_wake - t_render),
                ms(t_sight - t_wake),
                ms(t0.elapsed() - t_sight),
                pawns.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotator_forward() {
        assert!((rot_dir([0, 0, 0]) - Vec3::X).length() < 1e-5);
        assert!((rot_dir([0, 16384, 0]) - Vec3::Y).length() < 1e-4);
        assert!((rot_dir([16384, 0, 0]) - Vec3::Z).length() < 1e-4);
    }
}
