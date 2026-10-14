//! The engine side of the VM: p2game's implementation of `uscript::host::Host`.
//!
//! - **Animation**: every actor whose script touches animation gets a `p2anim::runtime::AnimInstance`
//!   built from its `Mesh` (a SkeletalMesh resource). The Actor animation natives map onto it:
//!   LinkSkelAnim, LinkMesh, GetDefaultAnim, PlayAnim/LoopAnim/TweenAnim/PlayAnimAt/StopAnimating,
//!   FreezeAnimAt/SetAnimFrame, IsAnimating/IsTweening/HasAnim/GetAnimParams/AnimIsInGroup,
//!   AnimBlendParams/AnimBlendToAlpha, EnableChannelNotify/GetNotifyChannel, the bone controllers
//!   (SetBoneRotation/Direction/Location/Scale), bone and root queries (GetBoneCoords,
//!   GetBoneRotation, GetRoot*), LockRootMotion, and AttachToBone/DetachFromBone. `FinishAnim`
//!   completes through the VM's latent handling once the channel stops.
//! - **Per frame** ([`GameHost::tick_anims`]): every instance advances; `AnimEnd(Channel)` is sent
//!   back to the actor, `AnimNotify_Script`/old-style function notifies call the named function,
//!   `AnimNotify_Trigger` calls `TriggerEvent`, sounds go to `play_sound`. Bone attachments then
//!   get their world Location/Rotation from the bone.
//! - **Travel**: `open`/`start`/`travel`/`servertravel` console commands are recorded in
//!   [`GameHost::pending_travel`] for the front end to act on.
//! - **Extensions**: other subsystems register extra hosts in [`GameHost::ext`]; `native` and
//!   `resource_prop` fall through to them in order.
//!
//! Everything works headless; the windowed mode reads the same instances to pose rigs.

use glam::{Mat3, Mat4, Vec3};
use p2anim::notify::{NotifyKind, decode_anim_notifies};
use p2anim::runtime::{AnimEvent, AnimInstance, AnimSet, FrameUnit, RotSpace, Skeleton, mat3_to_rotator, rotator_mat3};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use upkg::ObjRef;
use upkg::library::{Library, Obj};
use upkg::skeletal::{read_mesh_animation, read_skeletal_mesh};
use uscript::host::{AnimChannel, AnimRequest, Host, NativeCall, SoundRequest};
use uscript::vm::{ObjId, Value, Vm};

/// Set by the `exit` / `quit` console command (menu Quit / Exit to desktop); the window front
/// end closes the app when it sees it.
pub static QUIT_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether script asked the engine to shut down.
pub fn quit_requested() -> bool {
    QUIT_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Decoded skeletal mesh data shared by every actor using the mesh.
pub struct MeshInfo {
    pub obj: Obj,
    pub path: String,
    pub skeleton: Arc<Skeleton>,
    /// The mesh's own default MeshAnimation (`GetDefaultAnim`), as a resource path.
    pub default_anim: Option<String>,
}

/// Animation state of one actor.
pub struct ActorAnim {
    pub mesh: Rc<MeshInfo>,
    pub inst: AnimInstance,
}

#[derive(Default)]
pub struct AnimStats {
    pub instances: usize,
    pub anim_ends: u64,
    pub notifies: BTreeMap<String, u64>,
    pub unresolved_meshes: BTreeMap<String, u64>,
}

pub struct GameHost {
    pub lib: Arc<Library>,
    pub map_name: RefCell<String>,
    pub anims: RefCell<HashMap<ObjId, ActorAnim>>,
    meshes: RefCell<HashMap<String, Option<Rc<MeshInfo>>>>,
    sets: RefCell<HashMap<String, Option<Arc<AnimSet>>>>,
    /// Channel of the notify being delivered (`GetNotifyChannel`).
    notify_channel: Cell<i32>,
    pub pending_travel: RefCell<Option<String>>,
    pub sounds: RefCell<Vec<(Option<ObjId>, String)>>,
    pub stats: RefCell<AnimStats>,
    /// Extra hosts consulted (in order) for natives and resource properties this host lacks.
    pub ext: RefCell<Vec<Rc<dyn Host>>>,
    /// The collision/physics host: receives move_actor, set_rotation, trace, fast_trace,
    /// line_of_sight and latent_tick (MoveTo etc.). None: VM defaults.
    pub world: RefCell<Option<Rc<dyn Host>>>,
    /// Called every frame after the script tick and animation (subsystem sync: fx, sound, ...).
    pub frame_hooks: RefCell<Vec<Box<dyn FnMut(&mut Vm, f32)>>>,
    /// Optional names of `frame_hooks` by index (profiling labels; see [`GameHost::add_frame_hook`]).
    pub frame_hook_names: RefCell<Vec<Option<String>>>,
    /// The current level's map package (set before level hooks run).
    pub map: RefCell<Option<Arc<upkg::Package>>>,
    /// The current level's export index -> VM object table (set before level hooks run).
    pub level_objects: RefCell<HashMap<usize, ObjId>>,
    /// Accumulated wall time per frame phase (profiling: `--profile`, `P2GAME_PROFILE`).
    pub times: RefCell<FrameTimes>,
}

/// Wall-clock seconds spent per frame phase since the last reset.
#[derive(Default, Clone, Debug)]
pub struct FrameTimes {
    pub frames: u64,
    /// Script: `tick_world` (timers, state code, latent actions, Tick events).
    pub script: f64,
    /// Animation advance + AnimEnd/notify delivery + bone attachments.
    pub anim: f64,
    /// Per frame hook, by registration index (physics, audio, cinema, fx, ...).
    pub hooks: Vec<f64>,
}

/// Registered per-level setup functions: called for every new [`GameHost`] (at boot and after
/// each travel) so subsystems can push their `ext` host and `frame_hooks`.
pub type LevelHook = fn(&GameHost, &mut Vm);

thread_local! {
    static LEVEL_HOOKS: RefCell<Vec<(Rc<dyn Fn(&GameHost, &mut Vm)>, String)>> = const { RefCell::new(Vec::new()) };
}

/// Registers a subsystem's per-level setup (call once at startup, before the first level). The
/// function's module names its frame hooks in profiles.
pub fn register_level_hook<F: Fn(&GameHost, &mut Vm) + 'static>(f: F) {
    let full = std::any::type_name::<F>();
    let module = full.rsplit("::").nth(1).unwrap_or(full).to_string();
    LEVEL_HOOKS.with(|h| h.borrow_mut().push((Rc::new(f), module)));
}

pub(crate) fn run_level_hooks(host: &GameHost, vm: &mut Vm) {
    let hooks = LEVEL_HOOKS.with(|h| h.borrow().clone());
    for (f, module) in hooks {
        let before = host.frame_hooks.borrow().len();
        f(host, vm);
        // Label the frame hooks this subsystem added (`p2game::physics::level_hook` -> physics).
        let n = host.frame_hooks.borrow().len();
        let mut names = host.frame_hook_names.borrow_mut();
        names.resize(n, None);
        for slot in names.iter_mut().take(n).skip(before) {
            slot.get_or_insert_with(|| module.clone());
        }
    }
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Finds an object by its dotted path ("Package.Group.Name"), matching the outer group chain
/// when possible and falling back to the leaf name in the package.
pub fn resolve_path(lib: &Library, path: &str, class: Option<&str>) -> Option<Obj> {
    let parts: Vec<&str> = path.split('.').collect();
    let (pkg_name, rest) = parts.split_first()?;
    let leaf = rest.last()?;
    let groups: Vec<&str> = rest[..rest.len() - 1].iter().rev().copied().collect();
    let pkg = lib.package(pkg_name)?;
    let mut fallback = None;
    for (i, e) in pkg.exports.iter().enumerate() {
        if !pkg.name(e.name).eq_ignore_ascii_case(leaf) || class.is_some_and(|c| !pkg.export_class(i).eq_ignore_ascii_case(c)) {
            continue;
        }
        let mut outer = e.outer;
        let mut ok = true;
        for g in &groups {
            match outer {
                ObjRef::Export(o) if pkg.name(pkg.exports[o].name).eq_ignore_ascii_case(g) => outer = pkg.exports[o].outer,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && outer == ObjRef::Null {
            return Some(Obj { pkg, export: i });
        }
        fallback.get_or_insert(i);
    }
    fallback.map(|export| Obj { pkg, export })
}

fn obj_path(o: &Obj) -> String {
    o.pkg.full_path(ObjRef::Export(o.export))
}

fn vecv(v: Vec3) -> Value {
    Value::Vector(v.to_array())
}

fn arg_f(a: &[Value], k: usize, d: f32) -> f32 {
    match a.get(k) {
        Some(Value::Void) | None => d,
        Some(v) => v.as_float(),
    }
}

fn arg_i(a: &[Value], k: usize) -> i32 {
    a.get(k).map(|v| v.as_int()).unwrap_or(0)
}

fn arg_name(a: &[Value], k: usize) -> Option<String> {
    a.get(k).map(|v| v.as_name()).filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case("None"))
}

fn arg_rot(a: &[Value], k: usize) -> [i32; 3] {
    a.get(k).map(|v| v.as_rot()).unwrap_or_default()
}

fn arg_vec(a: &[Value], k: usize) -> Vec3 {
    Vec3::from(a.get(k).map(|v| v.as_vec()).unwrap_or_default())
}

/// Actor-to-world transform in Unreal space from the actor's Location/Rotation/DrawScale.
pub fn actor_world(vm: &Vm, a: ObjId) -> Mat4 {
    let s = vm.get_prop(a, "DrawScale").map(|v| v.as_float()).unwrap_or(1.0);
    let s3 = match vm.get_prop(a, "DrawScale3D") {
        Some(Value::Vector(v)) => Vec3::from(*v),
        _ => Vec3::ONE,
    };
    let mut loc = Vec3::from(vm.actor_loc(a));
    // A crouched pawn's cylinder shrinks and its Location drops by the height difference (feet
    // stay on the floor), but its mesh is authored around the standing origin: UE2 draws it
    // HeightAdjust higher. Without this every crouched pawn (begging NPCs on their knees) sank
    // ~20 uu into the floor.
    if vm.get_prop(a, "bIsCrouched").is_some_and(|v| v.as_bool()) {
        let h = vm.get_prop(a, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
        let h0 = vm.default_value(vm.objects[a].class, "CollisionHeight").map(|v| v.as_float()).unwrap_or(h);
        if h0 > h {
            loc.z += h0 - h;
        }
    }
    Mat4::from_translation(loc) * Mat4::from_mat3(rotator_mat3(vm.actor_rot(a))) * Mat4::from_scale(s3 * s)
}

fn orthonormal(m: Mat3) -> Mat3 {
    let x = m.x_axis.normalize_or_zero();
    let y = (m.y_axis - x * x.dot(m.y_axis)).normalize_or_zero();
    Mat3::from_cols(x, y, x.cross(y))
}

impl GameHost {
    pub fn new(lib: Arc<Library>) -> Self {
        Self {
            lib,
            map_name: RefCell::new(String::new()),
            anims: RefCell::new(HashMap::new()),
            meshes: RefCell::new(HashMap::new()),
            sets: RefCell::new(HashMap::new()),
            notify_channel: Cell::new(0),
            pending_travel: RefCell::new(None),
            sounds: RefCell::new(Vec::new()),
            stats: RefCell::new(AnimStats::default()),
            ext: RefCell::new(Vec::new()),
            frame_hooks: RefCell::new(Vec::new()),
            frame_hook_names: RefCell::new(Vec::new()),
            world: RefCell::new(None),
            map: RefCell::new(None),
            level_objects: RefCell::new(HashMap::new()),
            times: RefCell::new(FrameTimes::default()),
        }
    }

    /// Registers the loaded map so references back into it resolve.
    pub fn map_lib_insert(&self, map: &Arc<upkg::Package>) {
        self.lib.insert(map.clone());
    }

    /// Adds a named per-frame hook (the name labels it in frame-time profiles).
    pub fn add_frame_hook(&self, name: &str, f: impl FnMut(&mut Vm, f32) + 'static) {
        let mut hooks = self.frame_hooks.borrow_mut();
        let mut names = self.frame_hook_names.borrow_mut();
        names.resize(hooks.len(), None);
        names.push(Some(name.to_string()));
        hooks.push(Box::new(f));
    }

    /// Loads (cached) a skeletal mesh by resource path.
    pub fn mesh(&self, path: &str) -> Option<Rc<MeshInfo>> {
        let key = lower(path);
        if let Some(m) = self.meshes.borrow().get(&key) {
            return m.clone();
        }
        let m = resolve_path(&self.lib, path, Some("SkeletalMesh")).and_then(|obj| {
            let mesh = read_skeletal_mesh(&obj.pkg, obj.export).ok()?;
            if mesh.bones.is_empty() {
                return None;
            }
            let default_anim = self.lib.resolve(&obj.pkg, mesh.animation).map(|o| obj_path(&o));
            Some(Rc::new(MeshInfo { path: obj_path(&obj), skeleton: Arc::new(Skeleton::from_mesh(&mesh)), default_anim, obj }))
        });
        if m.is_none() {
            *self.stats.borrow_mut().unresolved_meshes.entry(path.to_string()).or_default() += 1;
        }
        self.meshes.borrow_mut().insert(key, m.clone());
        m
    }

    /// Loads (cached) a MeshAnimation with its notifies by resource path.
    pub fn anim_set(&self, path: &str) -> Option<Arc<AnimSet>> {
        let key = lower(path);
        if let Some(s) = self.sets.borrow().get(&key) {
            return s.clone();
        }
        let s = resolve_path(&self.lib, path, Some("MeshAnimation")).and_then(|obj| {
            let anim = read_mesh_animation(&obj.pkg, obj.export).ok()?;
            let notifies = decode_anim_notifies(Some(&self.lib), &obj.pkg, &anim);
            Some(Arc::new(AnimSet::new(Arc::new(anim), notifies)))
        });
        self.sets.borrow_mut().insert(key, s.clone());
        s
    }

    /// Makes sure the actor has an animation instance for its current `Mesh`. Returns false if the
    /// actor has no usable skeletal mesh.
    pub fn ensure(&self, vm: &Vm, a: ObjId) -> bool {
        let Some(path) = vm.get_prop(a, "Mesh").and_then(|v| v.as_resource()).map(str::to_string) else { return false };
        if let Some(cur) = self.anims.borrow().get(&a) {
            if cur.mesh.path.eq_ignore_ascii_case(&path) || lower(&cur.mesh.path).ends_with(&lower(&path)) {
                return true;
            }
        }
        let Some(mesh) = self.mesh(&path) else { return false };
        let mut inst = AnimInstance::new(mesh.skeleton.clone());
        if let Some(set) = mesh.default_anim.as_deref().and_then(|p| self.anim_set(p)) {
            inst.link_skel_anim(set);
        }
        self.stats.borrow_mut().instances += 1;
        self.anims.borrow_mut().insert(a, ActorAnim { mesh, inst });
        true
    }

    /// Runs `f` on the actor's animation instance (creating it from `Mesh` if needed).
    pub fn with_inst<R>(&self, vm: &Vm, a: ObjId, f: impl FnOnce(&mut AnimInstance) -> R) -> Option<R> {
        if !self.ensure(vm, a) {
            return None;
        }
        let mut m = self.anims.borrow_mut();
        m.get_mut(&a).map(|x| f(&mut x.inst))
    }

    /// Advances every actor's animation, delivers AnimEnd and notifies to script, and moves bone
    /// attachments. Call once per frame after the script tick.
    pub fn tick_anims(&self, vm: &mut Vm, dt: f32) {
        let t0 = std::time::Instant::now();
        let ids: Vec<ObjId> = self.anims.borrow().keys().copied().collect();
        let mut events: Vec<(ObjId, AnimEvent)> = Vec::new();
        {
            let mut m = self.anims.borrow_mut();
            for a in ids {
                if vm.objects.get(a).is_none_or(|o| o.deleted) {
                    m.remove(&a);
                    continue;
                }
                let Some(x) = m.get_mut(&a) else { continue };
                let mut ev: Vec<AnimEvent> = Vec::new();
                x.inst.tick(dt, &mut ev);
                events.extend(ev.into_iter().map(|e| (a, e)));
            }
        }
        for (a, e) in events {
            if vm.objects[a].deleted {
                continue;
            }
            self.deliver(vm, a, e);
        }
        // `P2GAME_ATTACH_EARLY=1`: the old order (attachments before the frame hooks), for A/B.
        let early = std::env::var_os("P2GAME_ATTACH_EARLY").is_some();
        if early {
            self.update_attachments(vm);
        }
        self.times.borrow_mut().anim += t0.elapsed().as_secs_f64();
        // Subsystem hooks; taken out while running so a hook can register more.
        let mut hooks = std::mem::take(&mut *self.frame_hooks.borrow_mut());
        for (i, h) in hooks.iter_mut().enumerate() {
            let t = std::time::Instant::now();
            h(vm, dt);
            let mut tm = self.times.borrow_mut();
            if tm.hooks.len() <= i {
                tm.hooks.resize(i + 1, 0.0);
            }
            tm.hooks[i] += t.elapsed().as_secs_f64();
        }
        let mut cur = self.frame_hooks.borrow_mut();
        hooks.append(&mut cur);
        *cur = hooks;
        drop(cur);
        // Bone attachments last: physics, AI steering, ragdolls and cinema are frame hooks that
        // move and turn pawns after the animation tick. Placed before them, heads (and hats,
        // held weapons) were drawn where the body had been a frame earlier: speed x dt behind
        // (13.5 uu running at 405 uu/s on 30 fps) and a frame of turning off (up to 42 deg).
        if early {
            return;
        }
        let t1 = std::time::Instant::now();
        self.update_attachments(vm);
        self.times.borrow_mut().anim += t1.elapsed().as_secs_f64();
    }

    fn deliver(&self, vm: &mut Vm, a: ObjId, e: AnimEvent) {
        match e {
            AnimEvent::AnimEnd { channel } => {
                self.stats.borrow_mut().anim_ends += 1;
                // UE2 sends a pawn's AnimEnd to its Controller when that controller drives the
                // animations (bControlAnimations: ScriptedController sequences, which loop
                // Action_PLAYANIM's BaseAnim from there). Sent to the pawn, PlayWaiting replaced
                // MilitaryBase's cellmate c_base_pose (sitting on the bed) with standing (FIDELITY #53).
                // POSTAL 2's AI controllers also take it whenever their current state (or class)
                // defines AnimEnd; those handlers forward to MyPawn.AnimEnd themselves. Without this
                // a fed dog froze in DogController.WaitForNewThrow after its pounce. Only channel 0:
                // blend channels (WEAPONCHANNEL 13 s_load_sh, head look) end in the pawn's own
                // AnimEnd, which fades them out; sent to a controller state that ignores them they
                // stayed at alpha 1 and froze the upper body in the weapon-load pose.
                let ctl = vm.get_prop(a, "Controller").and_then(|v| v.as_obj()).filter(|&c| !vm.objects[c].deleted);
                let ctl = ctl.filter(|&c| {
                    vm.get_prop(c, "bControlAnimations").is_some_and(|v| v.as_bool()) || (channel == 0 && {
                        let cls = vm.objects[c].class;
                        vm.find_virtual(Some(c), cls, "AnimEnd").is_some()
                    })
                });
                vm.fire_event(ctl.unwrap_or(a), "AnimEnd", vec![Value::Int(channel as i32)]);
            }
            AnimEvent::Notify { channel, notify, .. } => {
                self.notify_channel.set(channel as i32);
                if std::env::var_os("P2GAME_NOTIFY_TRACE").is_some() {
                    println!("NOTIFY {} ch {} {:?}", vm.object_name(a), channel, notify.kind);
                }
                let kind = match &notify.kind {
                    NotifyKind::Script { notify_name } | NotifyKind::Function { name: notify_name } => {
                        if !notify_name.is_empty() {
                            vm.fire_event(a, notify_name, vec![]);
                        }
                        "Script"
                    }
                    NotifyKind::Trigger { event_name } => {
                        let inst = vm.get_prop(a, "Instigator").cloned().unwrap_or(Value::Object(None));
                        vm.fire_event(a, "TriggerEvent", vec![Value::Name(event_name.clone()), Value::Object(Some(a)), inst]);
                        "Trigger"
                    }
                    NotifyKind::Sound { sound, volume, radius } => {
                        if let Some(s) = sound {
                            let req = SoundRequest { sound: s.path(), slot: 0, volume: *volume, no_override: false, radius: *radius, pitch: 1.0, attenuate: true };
                            self.play_sound(vm, Some(a), &req);
                        }
                        "Sound"
                    }
                    NotifyKind::Effect { .. } => "Effect",
                    NotifyKind::DestroyEffect { .. } => "DestroyEffect",
                    NotifyKind::Scripted { .. } => "Scripted",
                    NotifyKind::Other { .. } => "Other",
                };
                *self.stats.borrow_mut().notifies.entry(kind.into()).or_default() += 1;
                if matches!(kind, "Effect" | "DestroyEffect" | "Scripted" | "Other") {
                    // Not delivered yet: show up in the native priority list.
                    *vm.missing_natives.entry(format!("AnimNotify.{kind}")).or_default() += 1;
                }
                self.notify_channel.set(0);
            }
        }
    }

    /// Places every bone-attached actor at its bone (world Location/Rotation).
    pub fn update_attachments(&self, vm: &mut Vm) {
        // Attachments chain (shades/hat bolton -> head -> body). The anim map has no order, so a
        // child placed before its parent had moved used the parent's previous transform: head
        // boltons trailed the head by a frame and jittered or sank in while walking. Place
        // parents first (attachment depth order).
        let owners: Vec<ObjId> = self.anims.borrow().iter().filter(|(a, x)| x.inst.attachments().next().is_some() && vm.objects.get(**a).is_some_and(|o| !o.deleted)).map(|(a, _)| *a).collect();
        let depth = |vm: &Vm, mut a: ObjId| {
            let mut d = 0;
            while d < 8 {
                let bone = vm.get_prop(a, "AttachmentBone").map(|v| v.as_name()).unwrap_or_default();
                let Some(b) = vm.get_prop(a, "Base").and_then(|v| v.as_obj()).filter(|_| !bone.is_empty() && !bone.eq_ignore_ascii_case("None")) else { break };
                a = b;
                d += 1;
            }
            d
        };
        let mut order: Vec<(usize, ObjId)> = owners.into_iter().map(|a| (depth(vm, a), a)).collect();
        order.sort_unstable();
        for (_, a) in order {
            self.place_children(vm, a);
        }
    }

    /// Moves every actor attached to `a`'s bones to its bone transform.
    fn place_children(&self, vm: &mut Vm, a: ObjId) {
        let mut moves = Vec::new();
        {
            let mut m = self.anims.borrow_mut();
            let Some(x) = m.get_mut(&a) else { return };
            let world = actor_world(vm, a);
            let ids: Vec<(u64, usize)> = x.inst.attachments().map(|(id, at)| (*id, at.bone)).collect();
            for (id, bone) in ids {
                // Script may change RelativeLocation/Rotation after attaching (heads do).
                if let Some(child) = vm.objects.get(id as usize).filter(|o| !o.deleted).map(|_| id as ObjId) {
                    let rel_l = Vec3::from(vm.get_prop(child, "RelativeLocation").map(|v| v.as_vec()).unwrap_or_default());
                    let rel_r = vm.get_prop(child, "RelativeRotation").map(|v| v.as_rot()).unwrap_or_default();
                    let name = x.inst.skeleton.names[bone].clone();
                    x.inst.attach_to_bone(id, &name, rel_l, rel_r);
                }
                let Some(t) = x.inst.attachment_transform(id) else { continue };
                moves.push((id as ObjId, world * t));
            }
        }
        for (child, w) in moves {
            if vm.objects.get(child).is_none_or(|o| o.deleted) {
                continue;
            }
            vm.set_prop(child, "Location", Value::Vector(w.w_axis.truncate().to_array()));
            vm.set_prop(child, "Rotation", Value::Rotator(mat3_to_rotator(orthonormal(Mat3::from_mat4(w)))));
        }
    }

    #[allow(dead_code)]
    fn update_attachments_unordered(&self, vm: &mut Vm) {
        let mut moves = Vec::new();
        {
            let mut m = self.anims.borrow_mut();
            for (&a, x) in m.iter_mut() {
                if x.inst.attachments().next().is_none() || vm.objects[a].deleted {
                    continue;
                }
                let world = actor_world(vm, a);
                let ids: Vec<(u64, usize)> = x.inst.attachments().map(|(id, at)| (*id, at.bone)).collect();
                for (id, bone) in ids {
                    // Script may change RelativeLocation/Rotation after attaching (heads do).
                    if let Some(child) = vm.objects.get(id as usize).filter(|o| !o.deleted).map(|_| id as ObjId) {
                        let rel_l = Vec3::from(vm.get_prop(child, "RelativeLocation").map(|v| v.as_vec()).unwrap_or_default());
                        let rel_r = vm.get_prop(child, "RelativeRotation").map(|v| v.as_rot()).unwrap_or_default();
                        let name = x.inst.skeleton.names[bone].clone();
                        x.inst.attach_to_bone(id, &name, rel_l, rel_r);
                    }
                    let Some(t) = x.inst.attachment_transform(id) else { continue };
                    let w = world * t;
                    moves.push((id as ObjId, w));
                }
            }
        }
        for (child, w) in moves {
            if vm.objects.get(child).is_none_or(|o| o.deleted) {
                continue;
            }
            vm.set_prop(child, "Location", Value::Vector(w.w_axis.truncate().to_array()));
            vm.set_prop(child, "Rotation", Value::Rotator(mat3_to_rotator(orthonormal(Mat3::from_mat4(w)))));
        }
    }

    /// `FPSGameInfo.BuildPathNodeLists` / `BuildHomeNodeLists`: link the level's PathNodes into
    /// closed loops through `NextNode` and record each loop's start and length on the GameInfo.
    /// Home nodes are grouped by `Tag` (one loop per tag in `HomeNodeLists`); every other path node
    /// goes into the single `PathNodeList` loop. Script walks these loops for random destinations.
    fn build_node_lists(&self, vm: &mut Vm, game: ObjId, homes: bool) -> Value {
        let mut groups: Vec<(String, Vec<ObjId>)> = Vec::new();
        for &a in &vm.actors {
            if vm.objects[a].deleted {
                continue;
            }
            let c = vm.objects[a].class;
            if !vm.class_is_a(c, "PathNode") {
                continue;
            }
            let is_home = vm.class_is_a(c, "HomeNode");
            if is_home != homes {
                continue;
            }
            let key = if homes { vm.get_prop(a, "Tag").map(|v| v.as_name()).unwrap_or_default().to_ascii_lowercase() } else { String::new() };
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => g.1.push(a),
                None => groups.push((key, vec![a])),
            }
        }
        let mut infos = Vec::new();
        for (_, nodes) in &groups {
            for (i, &n) in nodes.iter().enumerate() {
                vm.set_prop(n, "NextNode", Value::Object(Some(nodes[(i + 1) % nodes.len()])));
            }
            infos.push(Value::Struct(vec![("node".into(), Value::Object(Some(nodes[0]))), ("Length".into(), Value::Int(nodes.len() as i32))]));
        }
        if homes {
            vm.set_prop(game, "HomeNodeLists", Value::Array(infos));
        } else {
            let v = infos.into_iter().next().unwrap_or(Value::Struct(vec![("node".into(), Value::Object(None)), ("Length".into(), Value::Int(0))]));
            vm.set_prop(game, "PathNodeList", v);
        }
        Value::Void
    }

    /// `Actor.HurtRadius(Damage, Radius, DamageType, Momentum, HitLocation)`: every other
    /// collidable actor within the radius gets `TakeDamage` scaled by distance (1 at the centre,
    /// 0 at the edge, measured to the actor's collision cylinder) and a momentum push away from
    /// the centre. Guarded against re-entry with `bHurtEntry`, like the script-visible flag.
    fn hurt_radius(&self, vm: &mut Vm, me: ObjId, a: &[Value]) -> Value {
        if vm.get_prop(me, "bHurtEntry").is_some_and(|v| v.as_bool()) {
            return Value::Void;
        }
        let (dmg, radius, dtype, momentum) = (arg_f(a, 0, 0.0), arg_f(a, 1, 0.0), a.get(2).cloned().unwrap_or(Value::Class(None)), arg_f(a, 3, 0.0));
        let center = Vec3::from(a.get(4).map(|v| v.as_vec()).unwrap_or_else(|| vm.actor_loc(me)));
        if radius <= 0.0 {
            return Value::Void;
        }
        vm.set_prop(me, "bHurtEntry", Value::Bool(true));
        let inst = vm.get_prop(me, "Instigator").and_then(|v| v.as_obj());
        // Candidates: the collision world's spatial query when physics runs (cheap), else a scan.
        let near: Option<Vec<ObjId>> = self.world.borrow().clone().and_then(|w| {
            w.iterate(vm, Some(me), "CollidingActors", &[Value::Class(None), Value::Void, Value::Float(radius + 200.0), Value::Vector(center.to_array())])
                .ok()
                .flatten()
                .map(|rows| rows.into_iter().filter_map(|r| r.first().and_then(|v| v.as_obj())).collect())
        });
        let candidates = near.unwrap_or_else(|| vm.live_actors());
        let victims: Vec<ObjId> = candidates
            .into_iter()
            .filter(|&v| v != me && vm.get_prop(v, "bCollideActors").is_some_and(|b| b.as_bool()))
            .filter(|&v| {
                let r = vm.get_prop(v, "CollisionRadius").map(|x| x.as_float()).unwrap_or(0.0);
                (Vec3::from(vm.actor_loc(v)) - center).length() <= radius + r
            })
            .collect();
        if std::env::var_os("P2_HURT_TRACE").is_some() {
            let names: Vec<String> = victims.iter().map(|&v| vm.object_name(v).to_string()).collect();
            let t = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(0.0);
            println!("HURT t={t:.2} {} dmg {dmg:.2} radius {radius:.0} at {center:.0} -> {names:?}", vm.object_name(me));
        }
        for v in victims {
            if vm.objects[v].deleted {
                continue;
            }
            let loc = Vec3::from(vm.actor_loc(v));
            let r = vm.get_prop(v, "CollisionRadius").map(|x| x.as_float()).unwrap_or(0.0);
            let dir = (loc - center).normalize_or_zero();
            let dist = (loc - center).length();
            let scale = (1.0 - ((dist - r).max(0.0) / radius)).clamp(0.0, 1.0);
            let hit = loc - dir * r;
            let args = vec![
                Value::Int((scale * dmg) as i32),
                Value::Object(inst),
                Value::Vector(hit.to_array()),
                Value::Vector((dir * scale * momentum).to_array()),
                dtype.clone(),
            ];
            vm.fire_event(v, "TakeDamage", args);
        }
        if !vm.objects[me].deleted {
            vm.set_prop(me, "bHurtEntry", Value::Bool(false));
        }
        Value::Void
    }

    fn anim_native(&self, vm: &mut Vm, me: ObjId, name: &str, a: &mut Vec<Value>) -> Option<Value> {
        Some(match name {
            "hurtradius" => self.hurt_radius(vm, me, a),
            // Collision filter for Karma/projectiles: kept on the actor for the physics host.
            "onlyaffectpawns" => {
                let on = a.first().is_some_and(|v| v.as_bool());
                vm.set_prop(me, "bOnlyAffectPawns", Value::Bool(on));
                Value::Void
            }
            // There is no separate persistent entry level in this engine: the current level
            // stands in for it.
            "getentrylevel" => Value::Object(vm.level),
            // The shipped Steam exe (same rule as p2canvas::shell::steam_build; p2canvas is a
            // window-only dependency).
            "issteambuild" => Value::Bool(std::env::var("P2_STEAM_BUILD").map(|v| v != "0").unwrap_or(true)),
            // `Pawn.SetTwistLook(Twist, Look)`: torso twist (yaw) spread over the spine bones with
            // the root counter-rotated so the legs stay put, and look (pitch) on the head. (0, 0)
            // clears the controllers (what script uses before scripted sequences).
            "settwistlook" => {
                let (twist, look) = (arg_i(a, 0), arg_i(a, 1));
                let bone = |n: &str| vm.get_prop(me, n).map(|v| v.as_name()).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("None"));
                let (root, s1, s2, head) = (bone("RootBone"), bone("SpineBone1"), bone("SpineBone2"), bone("HeadBone"));
                let alpha = if twist == 0 && look == 0 { 0.0 } else { 1.0 };
                self.with_inst(vm, me, |i| {
                    if let Some(b) = &root {
                        i.set_bone_rotation(b, [0, -twist, 0], RotSpace::Local, alpha);
                    }
                    for b in [&s1, &s2].into_iter().flatten() {
                        i.set_bone_rotation(b, [0, twist / 2, 0], RotSpace::Local, alpha);
                    }
                    if let Some(b) = &head {
                        i.set_bone_rotation(b, [look, 0, 0], RotSpace::Local, alpha);
                    }
                });
                Value::Void
            }
            "buildpathnodelists" => self.build_node_lists(vm, me, false),
            "buildhomenodelists" => self.build_node_lists(vm, me, true),
            "linkskelanim" => {
                let set = a.first().and_then(|v| v.as_resource()).and_then(|p| self.anim_set(p));
                if let Some(Value::Resource(m)) = a.get(1) {
                    vm.set_prop(me, "Mesh", Value::Resource(m.clone()));
                }
                if let Some(set) = set {
                    self.with_inst(vm, me, |i| i.link_skel_anim(set));
                }
                Value::Void
            }
            "linkmesh" => {
                let keep = a.get(1).is_some_and(|v| v.as_bool());
                let old_sets: Vec<Arc<AnimSet>> = if keep { self.anims.borrow().get(&me).map(|x| x.inst.anim_sets().cloned().collect()).unwrap_or_default() } else { Vec::new() };
                vm.set_prop(me, "Mesh", a.first().cloned().unwrap_or(Value::Object(None)));
                self.anims.borrow_mut().remove(&me);
                if self.ensure(vm, me) && keep {
                    self.with_inst(vm, me, |i| {
                        for s in old_sets {
                            i.add_anim_set(s);
                        }
                    });
                }
                Value::Void
            }
            "getdefaultanim" => {
                let p = a.first().and_then(|v| v.as_resource()).and_then(|p| self.mesh(p)).and_then(|m| m.default_anim.clone());
                p.map(Value::Resource).unwrap_or(Value::Object(None))
            }
            "playanimat" => {
                let (seq, rate, tween, ch, at) = (arg_name(a, 0).unwrap_or_default(), arg_f(a, 1, 1.0), arg_f(a, 2, 0.0), arg_i(a, 3).max(0) as usize, arg_f(a, 4, 0.0));
                Value::Bool(self.with_inst(vm, me, |i| i.play_anim_at(&seq, rate, tween, ch, at)).unwrap_or(false))
            }
            "freezeanimat" => {
                let (t, ch) = (arg_f(a, 0, 0.0), arg_i(a, 1).max(0) as usize);
                self.with_inst(vm, me, |i| i.freeze_anim_at(t, ch));
                Value::Void
            }
            "setanimframe" => {
                let (t, ch, unit) = (arg_f(a, 0, 0.0), arg_i(a, 1).max(0) as usize, arg_i(a, 2));
                self.with_inst(vm, me, |i| i.set_anim_frame(t, ch, if unit == 1 { FrameUnit::Frames } else { FrameUnit::Normalized }));
                Value::Void
            }
            "istweening" => Value::Bool(self.with_inst(vm, me, |i| i.is_tweening(arg_i(a, 0).max(0) as usize)).unwrap_or(false)),
            "enablechannelnotify" => {
                let (ch, on) = (arg_i(a, 0).max(0) as usize, arg_i(a, 1) != 0);
                self.with_inst(vm, me, |i| i.enable_channel_notify(ch, on));
                Value::Void
            }
            "getnotifychannel" => Value::Int(self.notify_channel.get()),
            "animblendparams" => {
                let (st, alpha, tin, tout, bone, global) = (arg_i(a, 0).max(0) as usize, arg_f(a, 1, 0.0), arg_f(a, 2, 0.0), arg_f(a, 3, 0.0), arg_name(a, 4), a.get(5).is_some_and(|v| v.as_bool()));
                self.with_inst(vm, me, |i| i.anim_blend_params(st, alpha, tin, tout, bone.as_deref(), global));
                Value::Void
            }
            "animblendtoalpha" => {
                let (st, alpha, t) = (arg_i(a, 0).max(0) as usize, arg_f(a, 1, 0.0), arg_f(a, 2, 0.0));
                self.with_inst(vm, me, |i| i.anim_blend_to_alpha(st, alpha, t));
                Value::Void
            }
            "setbonerotation" => {
                let (bone, turn, space, alpha) = (arg_name(a, 0).unwrap_or_default(), arg_rot(a, 1), arg_i(a, 2), arg_f(a, 3, 1.0));
                let space = match space {
                    1 => RotSpace::Replace,
                    2 => RotSpace::Component,
                    _ => RotSpace::Local,
                };
                self.with_inst(vm, me, |i| i.set_bone_rotation(&bone, turn, space, alpha));
                Value::Void
            }
            "setbonedirection" => {
                let (bone, turn, trans, alpha) = (arg_name(a, 0).unwrap_or_default(), arg_rot(a, 1), arg_vec(a, 2), arg_f(a, 3, 1.0));
                self.with_inst(vm, me, |i| i.set_bone_direction(&bone, turn, trans, alpha));
                Value::Void
            }
            "setbonelocation" => {
                let (bone, trans, alpha) = (arg_name(a, 0).unwrap_or_default(), arg_vec(a, 1), arg_f(a, 2, 1.0));
                self.with_inst(vm, me, |i| i.set_bone_location(&bone, trans, alpha));
                Value::Void
            }
            "setbonescale" => {
                let (slot, scale, bone) = (arg_i(a, 0), arg_f(a, 1, 1.0), arg_name(a, 2));
                self.with_inst(vm, me, |i| i.set_bone_scale(slot, scale, bone.as_deref()));
                Value::Void
            }
            "getbonecoords" => {
                let bone = arg_name(a, 0).unwrap_or_default();
                let world = actor_world(vm, me);
                let c = self.with_inst(vm, me, |i| i.skeleton.bone(&bone).map(|b| world * i.bone_actor_matrix(b))).flatten().unwrap_or(world);
                Value::Struct(vec![
                    ("Origin".into(), vecv(c.w_axis.truncate())),
                    ("XAxis".into(), vecv(c.x_axis.truncate().normalize_or_zero())),
                    ("YAxis".into(), vecv(c.y_axis.truncate().normalize_or_zero())),
                    ("ZAxis".into(), vecv(c.z_axis.truncate().normalize_or_zero())),
                ])
            }
            "getbonerotation" => {
                let (bone, space) = (arg_name(a, 0).unwrap_or_default(), arg_i(a, 1));
                let world = actor_world(vm, me);
                let r = self.with_inst(vm, me, |i| {
                    let b = i.skeleton.bone(&bone)?;
                    Some(if space == 0 { mat3_to_rotator(orthonormal(Mat3::from_mat4(world * i.bone_actor_matrix(b)))) } else { i.get_bone_rotation(&bone, 1)? })
                });
                Value::Rotator(r.flatten().unwrap_or_default())
            }
            "getrootlocation" => {
                let world = actor_world(vm, me);
                vecv(self.with_inst(vm, me, |i| world.transform_point3(i.get_root_location())).unwrap_or(world.w_axis.truncate()))
            }
            "getrootrotation" => Value::Rotator(self.with_inst(vm, me, |i| i.get_root_rotation()).unwrap_or_default()),
            "getrootlocationdelta" => vecv(self.with_inst(vm, me, |i| i.get_root_location_delta()).unwrap_or(Vec3::ZERO)),
            "getrootrotationdelta" => Value::Rotator(self.with_inst(vm, me, |i| i.get_root_rotation_delta()).unwrap_or_default()),
            "lockrootmotion" => {
                let l = arg_i(a, 0);
                self.with_inst(vm, me, |i| i.lock_root_motion(l));
                Value::Void
            }
            "attachtobone" => {
                let Some(child) = a.first().and_then(|v| v.as_obj()) else { return Some(Value::Bool(false)) };
                let bone = arg_name(a, 1).unwrap_or_default();
                let rel_l = Vec3::from(vm.get_prop(child, "RelativeLocation").map(|v| v.as_vec()).unwrap_or_default());
                let rel_r = vm.get_prop(child, "RelativeRotation").map(|v| v.as_rot()).unwrap_or_default();
                let ok = self.with_inst(vm, me, |i| i.attach_to_bone(child as u64, &bone, rel_l, rel_r)).unwrap_or(false);
                if ok {
                    vm.set_prop(child, "AttachmentBone", Value::Name(bone));
                    vm.set_prop(child, "Base", Value::Object(Some(me)));
                    if let Some(Value::Array(mut att)) = vm.get_prop(me, "Attached").cloned() {
                        if !att.iter().any(|v| v.as_obj() == Some(child)) {
                            att.push(Value::Object(Some(child)));
                            vm.set_prop(me, "Attached", Value::Array(att));
                        }
                    }
                }
                Value::Bool(ok)
            }
            "detachfrombone" => {
                let Some(child) = a.first().and_then(|v| v.as_obj()) else { return Some(Value::Bool(false)) };
                let ok = self.anims.borrow_mut().get_mut(&me).is_some_and(|x| x.inst.detach_from_bone(child as u64));
                if ok {
                    vm.set_prop(child, "AttachmentBone", Value::Name("None".into()));
                    if let Some(Value::Array(mut att)) = vm.get_prop(me, "Attached").cloned() {
                        att.retain(|v| v.as_obj() != Some(child));
                        vm.set_prop(me, "Attached", Value::Array(att));
                    }
                }
                Value::Bool(ok)
            }
            _ => return None,
        })
    }
}

impl Host for GameHost {
    fn actor_spawned(&self, vm: &mut Vm, actor: ObjId) {
        let ext = self.ext.borrow().clone();
        for h in ext.iter() {
            h.actor_spawned(vm, actor);
        }
    }

    fn actor_destroyed(&self, vm: &mut Vm, actor: ObjId) {
        self.anims.borrow_mut().remove(&actor);
        let ext = self.ext.borrow().clone();
        for h in ext.iter() {
            h.actor_destroyed(vm, actor);
        }
    }

    fn play_sound(&self, vm: &mut Vm, actor: Option<ObjId>, req: &SoundRequest) {
        {
            let mut s = self.sounds.borrow_mut();
            if s.len() < 4096 {
                s.push((actor, req.sound.clone()));
            }
        }
        let ext = self.ext.borrow().clone();
        for h in ext.iter() {
            h.play_sound(vm, actor, req);
        }
    }

    fn sound_duration(&self, vm: &mut Vm, sound: &str) -> f32 {
        let ext = self.ext.borrow().clone();
        ext.iter().map(|h| h.sound_duration(vm, sound)).find(|d| *d > 0.0).unwrap_or(0.0)
    }

    fn trace(&self, vm: &mut Vm, from: Option<ObjId>, req: &uscript::host::TraceRequest) -> Option<uscript::host::TraceHit> {
        let w = self.world.borrow().clone();
        w.and_then(|h| h.trace(vm, from, req))
    }

    /// Movement, traces and sight go to the world (collision/physics) host when one is installed
    /// in [`GameHost::world`]; otherwise the VM defaults apply (teleport, empty world).
    fn move_actor(&self, vm: &mut Vm, actor: ObjId, to: [f32; 3], sweep: bool) -> bool {
        match self.world.borrow().clone() {
            Some(h) => h.move_actor(vm, actor, to, sweep),
            None => {
                vm.set_prop(actor, "Location", Value::Vector(to));
                true
            }
        }
    }

    fn set_rotation(&self, vm: &mut Vm, actor: ObjId, rot: [i32; 3]) -> bool {
        match self.world.borrow().clone() {
            Some(h) => h.set_rotation(vm, actor, rot),
            None => {
                vm.set_prop(actor, "Rotation", Value::Rotator(rot));
                true
            }
        }
    }

    fn fast_trace(&self, vm: &mut Vm, start: [f32; 3], end: [f32; 3]) -> bool {
        match self.world.borrow().clone() {
            Some(h) => h.fast_trace(vm, start, end),
            None => true,
        }
    }

    fn line_of_sight(&self, vm: &mut Vm, viewer: ObjId, other: ObjId) -> bool {
        match self.world.borrow().clone() {
            Some(h) => h.line_of_sight(vm, viewer, other),
            None => false,
        }
    }

    fn latent_tick(&self, vm: &mut Vm, actor: ObjId, latent: &mut uscript::vm::Latent, dt: f32) -> uscript::Result<bool> {
        if latent.name.eq_ignore_ascii_case("FinishAnim") {
            let ch = latent.args.first().map(|v| v.as_int()).unwrap_or(0);
            return Ok(!self.anim_channel(vm, actor, ch).is_some_and(|c| c.playing));
        }
        match self.world.borrow().clone() {
            Some(h) => h.latent_tick(vm, actor, latent, dt),
            None => Ok(uscript::host::default_latent_tick(self, vm, actor, latent)),
        }
    }

    /// Collision iterators (TouchingActors, TraceActors, RadiusActors, ...) go to the world host.
    fn iterate(&self, vm: &mut Vm, this: Option<ObjId>, name: &str, args: &[Value]) -> uscript::Result<Option<Vec<Vec<Value>>>> {
        match self.world.borrow().clone() {
            Some(h) => h.iterate(vm, this, name, args),
            None => Ok(uscript::host::default_iterate(vm, this, name, args)),
        }
    }

    fn play_anim(&self, vm: &mut Vm, actor: ObjId, req: &AnimRequest) -> bool {
        let ch = req.channel.max(0) as usize;
        self.with_inst(vm, actor, |i| {
            if req.rate == 0.0 && !req.looping {
                i.tween_anim(&req.sequence, req.tween, ch)
            } else if req.looping {
                i.loop_anim(&req.sequence, req.rate, req.tween, ch)
            } else {
                i.play_anim(&req.sequence, req.rate, req.tween, ch)
            }
        })
        .unwrap_or(false)
    }

    fn stop_anims(&self, vm: &mut Vm, actor: ObjId) {
        self.with_inst(vm, actor, |i| i.stop_animating());
    }

    fn has_anim(&self, vm: &mut Vm, actor: ObjId, sequence: &str) -> bool {
        self.with_inst(vm, actor, |i| i.has_anim(sequence)).unwrap_or(false)
    }

    fn anim_channel(&self, vm: &mut Vm, actor: ObjId, channel: i32) -> Option<AnimChannel> {
        let ch = channel.max(0) as usize;
        self.with_inst(vm, actor, |i| {
            let c = i.channels.get(ch)?;
            Some(AnimChannel { sequence: c.sequence_name().unwrap_or_default().to_string(), frame: c.frame, rate: c.rate, playing: c.playing })
        })
        .flatten()
    }

    fn anim_in_group(&self, vm: &mut Vm, actor: ObjId, channel: i32, group: &str) -> bool {
        self.with_inst(vm, actor, |i| i.anim_is_in_group(channel.max(0) as usize, group)).unwrap_or(false)
    }

    fn map_name(&self, _vm: &mut Vm) -> String {
        self.map_name.borrow().clone()
    }

    fn console_command(&self, vm: &mut Vm, from: Option<ObjId>, command: &str) -> String {
        let c = command.trim();
        let (verb, rest) = c.split_once(char::is_whitespace).unwrap_or((c, ""));
        // `exit` / `quit` (UWindowRootWindow.DoQuitGame: menu Exit / Quit to desktop): the engine
        // shuts down; the window front end polls [`quit_requested`] and closes the app.
        if matches!(verb.to_ascii_lowercase().as_str(), "exit" | "quit") && rest.trim().is_empty() {
            QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            return "1".into();
        }
        if matches!(verb.to_ascii_lowercase().as_str(), "open" | "start" | "travel" | "servertravel") && !rest.trim().is_empty() {
            *self.pending_travel.borrow_mut() = Some(rest.trim().to_string());
        }
        // Extensions answer the commands they own (SaveGame, CHECKSAVEGAME, ...).
        let ext = self.ext.borrow().clone();
        let r = ext.iter().map(|h| h.console_command(vm, from, command)).find(|r| !r.is_empty()).unwrap_or_default();
        if !r.is_empty() {
            return r;
        }
        // The engine's GET/SET (class defaults, live objects and the user ini).
        vm.console_get_set(c).unwrap_or_default()
    }

    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        if let Some(me) = call.this {
            if let Some(v) = self.anim_native(vm, me, &call.name.to_ascii_lowercase(), args) {
                return Some(Ok(v));
            }
        }
        let ext = self.ext.borrow().clone();
        if let Some(r) = ext.iter().find_map(|h| h.native(vm, call, args)) {
            return Some(r);
        }
        // ClientTravel with no front end to answer it (headless probes): queue the travel.
        if call.name.eq_ignore_ascii_case("ClientTravel") {
            let cmd = crate::travel::client_travel(args);
            uscript::host::Host::console_command(self, vm, call.this, &cmd);
            return Some(Ok(Value::Void));
        }
        None
    }

    fn resource_prop(&self, vm: &mut Vm, path: &str, prop: &str) -> Option<Value> {
        let ext = self.ext.borrow().clone();
        if let Some(v) = ext.iter().find_map(|h| h.resource_prop(vm, path, prop)) {
            return Some(v);
        }
        // Material.SurfaceType (footsteps, impact effects) with no front end to answer it: the
        // saved property, else the class default (EST_Default).
        if prop.eq_ignore_ascii_case("SurfaceType") {
            let o = resolve_path(&self.lib, path, None)?;
            let (_, props) = upkg::begin_object(&o.pkg, o.export).ok()?;
            return Some(Value::Byte(props.byte("SurfaceType").unwrap_or(0)));
        }
        None
    }

    fn load_object(&self, vm: &mut Vm, path: &str, class: uscript::vm::ClassId) -> Option<Value> {
        let ext = self.ext.borrow().clone();
        ext.iter().find_map(|h| h.load_object(vm, path, class))
    }
}
