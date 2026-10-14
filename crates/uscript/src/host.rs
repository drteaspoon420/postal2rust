//! Host interface: the boundary between the script VM and a game engine.
//!
//! The VM implements the language, Core natives and the actor life cycle (spawn, events, states,
//! timers, `LifeSpan`, destruction). Anything that needs a world (collision, rendering, audio,
//! animation, navigation) goes through the [`Host`] trait, which an engine implements and installs
//! with [`Vm::set_host`]. Every method has a default suitable for a headless world, so hosts only
//! override what they support.
//!
//! - Typed hooks for the common world natives: [`Host::move_actor`] (SetLocation / Move),
//!   [`Host::set_rotation`], [`Host::trace`], [`Host::fast_trace`], [`Host::play_sound`],
//!   [`Host::play_anim`] / [`Host::anim_channel`], [`Host::line_of_sight`], plus
//!   [`Host::actor_spawned`] / [`Host::actor_destroyed`] notifications.
//! - [`Host::iterate`] produces the rows of `foreach` iterators (AllActors, RadiusActors, ...).
//!   The default is [`default_iterate`], a brute-force scan of [`Vm::actors`].
//! - [`Host::latent_begin`] / [`Host::latent_tick`] drive latent functions other than `Sleep`
//!   (FinishAnim, MoveTo, MoveToward, FinishRotation, WaitForLanding, ...).
//! - [`Host::native`] is the fallback for every other native (Canvas drawing, Karma, Steam, ...).
//!   Returning `None` means "not implemented": the VM counts it in [`Vm::missing_natives`].
//!
//! [`HeadlessHost`] is a recording host for tests and tools: it keeps the default world behaviour
//! and logs sounds and animations.

use crate::Result;
use crate::natives::urad;
use crate::vm::{ClassId, Latent, ObjId, Value, Vm};
use std::cell::RefCell;

/// A native call the VM did not handle itself.
pub struct NativeCall<'a> {
    /// The object the function was called on (`self`), if any.
    pub this: Option<ObjId>,
    /// Name of the class declaring the native (e.g. `Canvas`).
    pub class: &'a str,
    pub name: &'a str,
}

/// Parameters of `Trace`.
#[derive(Debug, Clone)]
pub struct TraceRequest {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub extent: [f32; 3],
    pub trace_actors: bool,
}

/// Result of a trace that hit something.
#[derive(Debug, Clone)]
pub struct TraceHit {
    /// Hit actor (the LevelInfo for world geometry).
    pub actor: Option<ObjId>,
    pub location: [f32; 3],
    pub normal: [f32; 3],
    /// Material path, if known.
    pub material: Option<String>,
}

/// Parameters of `PlaySound` / `PlayOwnedSound`.
#[derive(Debug, Clone)]
pub struct SoundRequest {
    /// Sound object path (e.g. `"WeaponSounds.pistol_fire"`).
    pub sound: String,
    pub slot: u8,
    pub volume: f32,
    pub no_override: bool,
    pub radius: f32,
    pub pitch: f32,
    pub attenuate: bool,
}

/// Parameters of `PlayAnim` / `LoopAnim` / `TweenAnim`.
#[derive(Debug, Clone)]
pub struct AnimRequest {
    pub sequence: String,
    pub rate: f32,
    pub tween: f32,
    pub channel: i32,
    pub looping: bool,
}

/// Animation state of one channel.
#[derive(Debug, Clone, Default)]
pub struct AnimChannel {
    pub sequence: String,
    /// Normalized frame (0..1).
    pub frame: f32,
    pub rate: f32,
    pub playing: bool,
}

/// Engine services used by the VM. All methods have headless defaults.
#[allow(unused_variables)]
pub trait Host {
    /// Called after an actor is created and placed, before its `PreBeginPlay`.
    fn actor_spawned(&self, vm: &mut Vm, actor: ObjId) {}
    /// Called when an actor is destroyed (after its `Destroyed` event).
    fn actor_destroyed(&self, vm: &mut Vm, actor: ObjId) {}

    /// Moves an actor (`SetLocation` when `sweep` is false, `Move`/`MoveSmooth` when true).
    /// Returns false if the move is blocked. The default teleports.
    fn move_actor(&self, vm: &mut Vm, actor: ObjId, to: [f32; 3], sweep: bool) -> bool {
        vm.set_prop(actor, "Location", Value::Vector(to));
        true
    }
    /// `MoveSmooth(Delta)`: like `Move` but slides along what it hits (and steps up while
    /// walking). The default delegates to [`Host::move_actor`] with `sweep = true`.
    fn move_smooth(&self, vm: &mut Vm, actor: ObjId, delta: [f32; 3]) -> bool {
        let l = vm.actor_loc(actor);
        self.move_actor(vm, actor, [l[0] + delta[0], l[1] + delta[1], l[2] + delta[2]], true)
    }
    fn set_rotation(&self, vm: &mut Vm, actor: ObjId, rot: [i32; 3]) -> bool {
        vm.set_prop(actor, "Rotation", Value::Rotator(rot));
        true
    }
    /// Line / box trace. The default world is empty (no hits).
    fn trace(&self, vm: &mut Vm, from: Option<ObjId>, req: &TraceRequest) -> Option<TraceHit> {
        None
    }
    /// True if nothing in the world geometry blocks the segment.
    fn fast_trace(&self, vm: &mut Vm, start: [f32; 3], end: [f32; 3]) -> bool {
        true
    }
    /// Controller sight checks (`LineOfSightTo`, `CanSee`). Default: nothing is visible.
    fn line_of_sight(&self, vm: &mut Vm, viewer: ObjId, other: ObjId) -> bool {
        false
    }

    fn play_sound(&self, vm: &mut Vm, actor: Option<ObjId>, req: &SoundRequest) {}
    fn sound_duration(&self, vm: &mut Vm, sound: &str) -> f32 {
        0.0
    }
    /// Starts an animation. Returns false if the sequence does not exist.
    fn play_anim(&self, vm: &mut Vm, actor: ObjId, req: &AnimRequest) -> bool {
        false
    }
    fn stop_anims(&self, vm: &mut Vm, actor: ObjId) {}
    fn has_anim(&self, vm: &mut Vm, actor: ObjId, sequence: &str) -> bool {
        false
    }
    fn anim_channel(&self, vm: &mut Vm, actor: ObjId, channel: i32) -> Option<AnimChannel> {
        None
    }
    fn anim_in_group(&self, vm: &mut Vm, actor: ObjId, channel: i32, group: &str) -> bool {
        false
    }

    /// Current map name (`GetURLMap`).
    fn map_name(&self, vm: &mut Vm) -> String {
        String::new()
    }
    fn console_command(&self, vm: &mut Vm, from: Option<ObjId>, command: &str) -> String {
        String::new()
    }

    /// Rows of a `foreach` iterator: one row per iteration holding the values of the iterator's
    /// out parameters in declaration order (e.g. `[Actor]`, or `[Actor, HitLoc, HitNorm]` for
    /// TraceActors). `args` holds every parameter value (Void for omitted optionals).
    /// `Ok(None)` means the iterator is not implemented.
    fn iterate(&self, vm: &mut Vm, this: Option<ObjId>, name: &str, args: &[Value]) -> Result<Option<Vec<Vec<Value>>>> {
        Ok(default_iterate(vm, this, name, args))
    }

    /// A latent function was reached in state code (arguments evaluated).
    fn latent_begin(&self, vm: &mut Vm, actor: ObjId, latent: &mut Latent) -> Result<()> {
        default_latent_begin(vm, actor, latent);
        Ok(())
    }
    /// Advances a latent action; returns true when it is complete. `latent.elapsed` already
    /// includes `dt`. Not called for `Sleep`, which the VM times itself.
    fn latent_tick(&self, vm: &mut Vm, actor: ObjId, latent: &mut Latent, dt: f32) -> Result<bool> {
        Ok(default_latent_tick(self, vm, actor, latent))
    }

    /// Any other native. `None` = not implemented (counted in `Vm::missing_natives`).
    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<Result<Value>> {
        None
    }

    /// A property of a non-script object (texture, font, sound, ...) read from script, e.g.
    /// `Tex.USize` where `Tex` evaluates to `Value::Resource(path)`. `path` is the object path
    /// as stored in the value (e.g. `"P2LookAndFeel.b_MenuTL"`). `None` = unknown (reads as the
    /// zero value with an "Accessed None" warning, as before).
    fn resource_prop(&self, vm: &mut Vm, path: &str, prop: &str) -> Option<Value> {
        None
    }

    /// `DynamicLoadObject` of a non-class object whose class is a script class (e.g. a
    /// `MovieTexture`): the host may return a VM object standing for it. `None` keeps the default
    /// (`Value::Resource(path)`).
    fn load_object(&self, vm: &mut Vm, path: &str, class: ClassId) -> Option<Value> {
        None
    }
}

/// The host installed by `Vm::new`: every default, nothing recorded.
pub struct DefaultHost;
impl Host for DefaultHost {}

/// A recording host for headless runs: default world behaviour, plus a log of sounds and
/// animations and instantly finishing animations.
#[derive(Default)]
pub struct HeadlessHost {
    pub sounds: RefCell<Vec<(Option<ObjId>, String)>>,
    pub anims: RefCell<Vec<(ObjId, String)>>,
}

impl Host for HeadlessHost {
    fn play_sound(&self, _vm: &mut Vm, actor: Option<ObjId>, req: &SoundRequest) {
        self.sounds.borrow_mut().push((actor, req.sound.clone()));
    }
    fn play_anim(&self, _vm: &mut Vm, actor: ObjId, req: &AnimRequest) -> bool {
        self.anims.borrow_mut().push((actor, req.sequence.clone()));
        true
    }
}

/// Iterators the VM knows (implemented through [`Host::iterate`]).
pub const BUILTIN_ITERATORS: &[&str] = &[
    "AllActors",
    "DynamicActors",
    "ChildActors",
    "BasedActors",
    "TouchingActors",
    "TraceActors",
    "RadiusActors",
    "VisibleActors",
    "VisibleCollidingActors",
    "CollidingActors",
    "ZoneActors",
];

fn vdist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

impl Vm {
    pub fn actor_loc(&self, a: ObjId) -> [f32; 3] {
        self.get_prop(a, "Location").map(|v| v.as_vec()).unwrap_or_default()
    }
    pub fn actor_rot(&self, a: ObjId) -> [i32; 3] {
        self.get_prop(a, "Rotation").map(|v| v.as_rot()).unwrap_or_default()
    }
    fn prop_bool(&self, a: ObjId, n: &str) -> bool {
        self.get_prop(a, n).is_some_and(|v| v.as_bool())
    }
    fn prop_obj(&self, a: ObjId, n: &str) -> Option<ObjId> {
        self.get_prop(a, n).and_then(|v| v.as_obj())
    }

    /// Live actors (not destroyed) in spawn order.
    pub fn live_actors(&self) -> Vec<ObjId> {
        self.actors.iter().copied().filter(|&a| !self.objects[a].deleted).collect()
    }
}

/// The default `foreach` iterators: brute-force scans of [`Vm::actors`] using Location, Owner,
/// Base, Touching, bHidden and bCollideActors. TraceActors needs a world and yields nothing.
pub fn default_iterate(vm: &mut Vm, this: Option<ObjId>, name: &str, args: &[Value]) -> Option<Vec<Vec<Value>>> {
    let base = args.first().and_then(|v| v.as_class());
    let n = name.to_ascii_lowercase();
    if !BUILTIN_ITERATORS.iter().any(|b| b.eq_ignore_ascii_case(name)) {
        return None;
    }
    let me = this;
    let center = |vm: &Vm, k: usize| match args.get(k) {
        Some(Value::Vector(v)) if *v != [0.0; 3] => *v,
        _ => me.map(|m| vm.actor_loc(m)).unwrap_or_default(),
    };
    let candidates: Vec<ObjId> = match n.as_str() {
        "touchingactors" => match me.and_then(|m| vm.get_prop(m, "Touching").cloned()) {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_obj()).collect(),
            _ => Vec::new(),
        },
        _ => vm.live_actors(),
    };
    let mut rows = Vec::new();
    for a in candidates {
        if vm.objects[a].deleted {
            continue;
        }
        if let Some(b) = base {
            if !vm.is_child_of(vm.objects[a].class, b) {
                continue;
            }
        }
        let keep = match n.as_str() {
            "allactors" | "dynamicactors" => {
                let tag_ok = match args.get(2) {
                    Some(t @ Value::Name(_)) if !t.is_none_name() => vm.get_prop(a, "Tag").is_some_and(|x| x.script_eq(t)),
                    _ => true,
                };
                tag_ok && (n == "allactors" || !vm.prop_bool(a, "bStatic"))
            }
            "childactors" => me.is_some() && vm.prop_obj(a, "Owner") == me,
            "basedactors" => me.is_some() && vm.prop_obj(a, "Base") == me,
            "touchingactors" | "zoneactors" => true,
            "radiusactors" => vdist(vm.actor_loc(a), center(vm, 3)) <= args.get(2).map(|v| v.as_float()).unwrap_or(0.0),
            "visibleactors" => {
                let r = args.get(2).map(|v| v.as_float()).filter(|r| *r > 0.0).unwrap_or(f32::MAX);
                !vm.prop_bool(a, "bHidden") && vdist(vm.actor_loc(a), center(vm, 3)) <= r
            }
            "visiblecollidingactors" => {
                let hidden_ok = !args.get(4).is_some_and(|v| v.as_bool()) || !vm.prop_bool(a, "bHidden");
                hidden_ok && vm.prop_bool(a, "bCollideActors") && vdist(vm.actor_loc(a), center(vm, 3)) <= args.get(2).map(|v| v.as_float()).unwrap_or(0.0)
            }
            "collidingactors" => vm.prop_bool(a, "bCollideActors") && vdist(vm.actor_loc(a), center(vm, 3)) <= args.get(2).map(|v| v.as_float()).unwrap_or(0.0),
            _ => false,
        };
        if keep {
            rows.push(vec![Value::Object(Some(a))]);
        }
    }
    Some(rows)
}

/// Records movement targets on the controller (`Destination`, `MoveTarget`, `Focus`).
pub fn default_latent_begin(vm: &mut Vm, actor: ObjId, l: &mut Latent) {
    match l.name.to_ascii_lowercase().as_str() {
        "moveto" | "movetowithradius" => {
            vm.set_prop(actor, "Destination", l.args.first().cloned().unwrap_or(Value::Vector([0.0; 3])));
            if let Some(f @ Value::Object(_)) = l.args.get(1) {
                vm.set_prop(actor, "Focus", f.clone());
            }
        }
        "movetoward" | "movetowardwithradius" => {
            let t = l.args.first().cloned().unwrap_or(Value::Object(None));
            vm.set_prop(actor, "MoveTarget", t.clone());
            if let Some(o) = t.as_obj() {
                let loc = vm.actor_loc(o);
                vm.set_prop(actor, "Destination", Value::Vector(loc));
            }
        }
        _ => {}
    }
}

/// Default latent completion: `FinishAnim` waits until the host reports the channel stopped;
/// movement moves the controller's pawn straight to the destination at its ground speed; the
/// rest complete immediately.
pub fn default_latent_tick<H: Host + ?Sized>(host: &H, vm: &mut Vm, actor: ObjId, l: &mut Latent) -> bool {
    match l.name.to_ascii_lowercase().as_str() {
        "finishanim" => {
            let ch = l.args.first().map(|v| v.as_int()).unwrap_or(0);
            !host.anim_channel(vm, actor, ch).is_some_and(|c| c.playing)
        }
        "moveto" | "movetoward" | "movetowithradius" | "movetowardwithradius" => {
            let Some(pawn) = vm.get_prop(actor, "Pawn").and_then(|v| v.as_obj()).filter(|p| !vm.objects[*p].deleted) else { return true };
            let dest = match vm.get_prop(actor, "MoveTarget").and_then(|v| v.as_obj()) {
                Some(t) if l.name.to_ascii_lowercase().starts_with("movetoward") && !vm.objects[t].deleted => vm.actor_loc(t),
                _ => vm.get_prop(actor, "Destination").map(|v| v.as_vec()).unwrap_or_default(),
            };
            let from = vm.actor_loc(pawn);
            let speed = vm.get_prop(pawn, "GroundSpeed").map(|v| v.as_float()).filter(|s| *s > 0.0).unwrap_or(400.0);
            let d = vdist(from, dest);
            let step = speed * l.elapsed.min(0.25);
            l.elapsed = 0.0;
            let to = if d <= step.max(1.0) {
                dest
            } else {
                let k = step / d;
                [from[0] + (dest[0] - from[0]) * k, from[1] + (dest[1] - from[1]) * k, from[2] + (dest[2] - from[2]) * k]
            };
            host.move_actor(vm, pawn, to, true);
            vdist(to, dest) <= 1.0 || l.elapsed > 30.0
        }
        _ => true,
    }
}

/// Spawn parameters (`Spawn(Class, Owner, Tag, Location, Rotation)`).
#[derive(Debug, Clone, Default)]
pub struct SpawnParams {
    pub owner: Option<ObjId>,
    pub instigator: Option<ObjId>,
    pub tag: Option<String>,
    pub location: [f32; 3],
    pub rotation: [i32; 3],
    /// First skin override (`SpawnSkin`).
    pub skin: Option<String>,
}

impl Vm {
    /// Creates a world: a LevelInfo (assigned to [`Vm::level`]) and a GameInfo of the given class
    /// (`"Package.Class"`, default `Engine.GameInfo`) on which `InitGame` and the begin-play events
    /// run. Returns the GameInfo actor.
    pub fn init_world(&mut self, game_class: Option<&str>) -> Result<ObjId> {
        let li_class = self.find_class("Engine", "LevelInfo")?;
        let level = self.new_object(li_class);
        self.level = Some(level);
        self.set_prop(level, "Level", Value::Object(Some(level)));
        self.set_prop(level, "NetMode", Value::Byte(0));
        self.set_prop(level, "Role", Value::Byte(4));
        self.actors.push(level);
        let h = self.host();
        h.actor_spawned(self, level);
        let gc = self.find_class_by_name(game_class.unwrap_or("Engine.GameInfo"))?;
        let game = self
            .spawn_actor(gc, SpawnParams::default(), false)
            .ok_or_else(|| crate::ScriptError::Bad("could not spawn the game info".into()))?;
        self.set_prop(level, "Game", Value::Object(Some(game)));
        if let Err(e) = self.call_event(game, "InitGame", vec![Value::Str(String::new()), Value::Str(String::new())]) {
            self.record_error("InitGame", &e);
        }
        self.begin_play(level);
        self.begin_play(game);
        Ok(game)
    }

    /// Spawns an actor of `class`. When `events` is true the begin-play sequence runs
    /// (`PreBeginPlay`, `BeginPlay`, `PostBeginPlay`, `SetInitialState`, `PostNetBeginPlay`).
    /// Returns None for abstract classes, non-actors, or actors destroyed during their events.
    pub fn spawn_actor(&mut self, class: ClassId, p: SpawnParams, events: bool) -> Option<ObjId> {
        let actor = self.actor_class()?;
        if !self.is_child_of(class, actor) || self.classes[class].is_abstract() {
            return None;
        }
        let tp = self.profile.then(std::time::Instant::now);
        self.apply_class_config(class);
        let a = self.new_object(class);
        self.apply_object_config(a);
        if let Some(t) = tp {
            let e = self.profile_natives.entry("(spawn: config+new_object)".into()).or_default();
            e.0 += 1;
            e.1 += t.elapsed().as_secs_f64();
        }
        self.set_prop(a, "Level", Value::Object(self.level));
        self.set_prop(a, "Location", Value::Vector(p.location));
        self.set_prop(a, "Rotation", Value::Rotator(p.rotation));
        self.set_prop(a, "Role", Value::Byte(4));
        self.set_prop(a, "Owner", Value::Object(p.owner));
        if p.instigator.is_some() {
            self.set_prop(a, "Instigator", Value::Object(p.instigator));
        }
        match &p.tag {
            Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("None") => {
                self.set_prop(a, "Tag", Value::Name(t.clone()));
            }
            _ => {
                if self.get_prop(a, "Tag").is_none_or(|t| t.is_none_name()) {
                    let n = self.classes[class].name.clone();
                    self.set_prop(a, "Tag", Value::Name(n));
                }
            }
        }
        if let Some(skin) = &p.skin {
            if let Some(Value::Array(mut s)) = self.get_prop(a, "Skins").cloned() {
                if s.is_empty() {
                    s.push(Value::Resource(skin.clone()));
                } else {
                    s[0] = Value::Resource(skin.clone());
                }
                self.set_prop(a, "Skins", Value::Array(s));
            }
        }
        self.actors.push(a);
        let tp = self.profile.then(std::time::Instant::now);
        let h = self.host();
        h.actor_spawned(self, a);
        if let Some(t) = tp {
            let e = self.profile_natives.entry("(spawn: host actor_spawned)".into()).or_default();
            e.0 += 1;
            e.1 += t.elapsed().as_secs_f64();
        }
        if let Some(o) = p.owner {
            self.fire_event(o, "GainedChild", vec![Value::Object(Some(a))]);
        }
        if events {
            self.begin_play(a);
        }
        (!self.objects[a].deleted).then_some(a)
    }

    /// Runs the begin-play event sequence on a spawned actor (errors go to `script_errors`).
    pub fn begin_play(&mut self, a: ObjId) {
        for ev in ["Spawned", "PreBeginPlay", "BeginPlay", "PostBeginPlay", "SetInitialState", "PostNetBeginPlay"] {
            if self.objects[a].deleted {
                return;
            }
            self.fire_event(a, ev, vec![]);
        }
    }

    /// Destroys an actor: `Destroyed` event, owner `LostChild`, host notification. References
    /// to it are cleared by [`Vm::cleanup_destroyed`]. Returns false for `bNoDelete`/`bStatic`.
    pub fn destroy_actor(&mut self, a: ObjId) -> bool {
        if self.objects[a].deleted {
            return true;
        }
        if self.prop_bool(a, "bNoDelete") || (self.prop_bool(a, "bStatic") && !self.allow_static_destroy) {
            return false;
        }
        self.fire_event(a, "Destroyed", vec![]);
        if let Some(o) = self.prop_obj(a, "Owner") {
            self.fire_event(o, "LostChild", vec![Value::Object(Some(a))]);
        }
        self.objects[a].deleted = true;
        self.objects[a].exec.latent = None;
        self.objects[a].exec.code = None;
        self.set_prop(a, "bDeleteMe", Value::Bool(true));
        self.destroyed_pending = true;
        let h = self.host();
        h.actor_destroyed(self, a);
        true
    }

    /// Removes destroyed actors from [`Vm::actors`] and sets object references to them to None
    /// (as the engine does at the end of a tick). Top-level object properties of live actors are
    /// cleared here; references nested in arrays/structs, locals and other objects read as None
    /// through the interpreter (destroyed objects always evaluate to None).
    pub fn cleanup_destroyed(&mut self) {
        if !self.destroyed_pending {
            return;
        }
        self.destroyed_pending = false;
        let objects = &self.objects;
        self.actors.retain(|&a| !objects[a].deleted);
        if self.level.is_some_and(|l| self.objects[l].deleted) {
            self.level = None;
        }
        let refs = self.object_slots();
        for &a in &self.actors {
            let c = self.objects[a].class;
            let Some(slots) = refs.get(c) else { continue };
            for &s in slots {
                if let Value::Object(Some(o)) = self.objects[a].slots[s] {
                    if self.objects[o].deleted {
                        self.objects[a].slots[s] = Value::Object(None);
                    }
                }
            }
        }
    }

    /// Per class: indices of object-typed slots (cached; extended as classes load).
    fn object_slots(&mut self) -> std::rc::Rc<Vec<Vec<usize>>> {
        if self.object_slot_cache.len() != self.classes.len() {
            let v = self
                .classes
                .iter()
                .map(|c| c.slots.iter().enumerate().filter(|(_, s)| matches!(s.ty, crate::vm::Ty::Object)).map(|(i, _)| i).collect())
                .collect();
            self.object_slot_cache = std::rc::Rc::new(v);
        }
        self.object_slot_cache.clone()
    }

    /// Number of live (not destroyed) actors.
    pub fn live_actor_count(&self) -> usize {
        self.actors.iter().filter(|&&a| !self.objects[a].deleted).count()
    }
}

type A = Vec<Value>;

fn this(vm: &Vm) -> Result<ObjId> {
    vm.native_self().ok_or_else(|| crate::ScriptError::Bad("actor native called without an actor".into()))
}

fn arg_f(a: &A, k: usize) -> f32 {
    a.get(k).map(|v| v.as_float()).unwrap_or(0.0)
}

fn arg_b(a: &A, k: usize) -> bool {
    a.get(k).map(|v| v.as_bool()).unwrap_or(false)
}

fn arg_i(a: &A, k: usize) -> i32 {
    a.get(k).map(|v| v.as_int()).unwrap_or(0)
}

fn opt_vec(a: &A, k: usize) -> Option<[f32; 3]> {
    match a.get(k) {
        Some(Value::Vector(v)) => Some(*v),
        _ => None,
    }
}

fn sound_req(a: &A) -> SoundRequest {
    let def = |k: usize, d: f32| match a.get(k) {
        Some(Value::Void) | None => d,
        Some(v) => v.as_float(),
    };
    SoundRequest {
        sound: match a.first() {
            Some(Value::Resource(p)) => p.clone(),
            Some(v) => v.as_str(),
            None => String::new(),
        },
        slot: arg_i(a, 1) as u8,
        volume: def(2, 1.0),
        no_override: arg_b(a, 3),
        radius: def(4, 0.0),
        pitch: def(5, 1.0),
        attenuate: !matches!(a.get(6), Some(Value::Bool(false))),
    }
}

fn anim_req(a: &A, looping: bool) -> AnimRequest {
    AnimRequest {
        sequence: a.first().map(|v| v.as_name()).unwrap_or_default(),
        rate: match a.get(1) {
            Some(Value::Void) | None => 1.0,
            Some(v) => v.as_float(),
        },
        tween: arg_f(a, 2),
        channel: arg_i(a, 3),
        looping,
    }
}

macro_rules! reg {
    ($vm:expr, $($name:literal => $f:expr),* $(,)?) => {
        $( $vm.register_native($name, $f); )*
    };
}

/// Registers the world/actor natives. They implement the script-visible semantics and call the
/// installed [`Host`] for anything that needs a world.
pub fn register_engine(vm: &mut Vm) {
    // Life cycle.
    reg!(vm,
        "Spawn" => |vm, a| {
            let me = vm.native_self();
            let Some(class) = a.first().and_then(|v| v.as_class()) else { return Ok(Value::Object(None)) };
            let p = SpawnParams {
                owner: a.get(1).and_then(|v| v.as_obj()),
                instigator: me.and_then(|m| vm.get_prop(m, "Instigator").and_then(|v| v.as_obj())),
                tag: a.get(2).filter(|v| !v.is_none_name()).map(|v| v.as_name()),
                location: opt_vec(a, 3).unwrap_or_else(|| me.map(|m| vm.actor_loc(m)).unwrap_or_default()),
                rotation: match a.get(4) { Some(Value::Rotator(r)) => *r, _ => me.map(|m| vm.actor_rot(m)).unwrap_or_default() },
                skin: a.get(5).and_then(|v| v.as_resource()).map(str::to_string),
            };
            Ok(Value::Object(vm.spawn_actor(class, p, true)))
        },
        "Destroy" => |vm, _| { let me = this(vm)?; Ok(Value::Bool(vm.destroy_actor(me))) },
        "Error" => |vm, a| {
            let me = this(vm)?;
            let msg = format!("ScriptError: {} {}", vm.object_name(me), a.first().map(|v| v.as_str()).unwrap_or_default());
            vm.push_log(msg);
            vm.destroy_actor(me);
            Ok(Value::Void)
        },
        "SetTimer" => |vm, a| {
            let me = this(vm)?;
            vm.set_prop(me, "TimerRate", Value::Float(arg_f(a, 0).max(0.0)));
            vm.set_prop(me, "TimerCounter", Value::Float(0.0));
            vm.set_prop(me, "bTimerLoop", Value::Bool(arg_b(a, 1)));
            Ok(Value::Void)
        },
        "SetOwner" => |vm, a| {
            let me = this(vm)?;
            let new = a.first().and_then(|v| v.as_obj());
            let old = vm.get_prop(me, "Owner").and_then(|v| v.as_obj());
            if old != new {
                if let Some(o) = old { vm.fire_event(o, "LostChild", vec![Value::Object(Some(me))]); }
                vm.set_prop(me, "Owner", Value::Object(new));
                if let Some(n) = new { vm.fire_event(n, "GainedChild", vec![Value::Object(Some(me))]); }
            }
            Ok(Value::Void)
        },
        "SetBase" => |vm, a| {
            let me = this(vm)?;
            let new = a.first().and_then(|v| v.as_obj());
            let old = vm.get_prop(me, "Base").and_then(|v| v.as_obj());
            if old != new {
                if let Some(o) = old { vm.fire_event(o, "Detach", vec![Value::Object(Some(me))]); }
                vm.set_prop(me, "Base", Value::Object(new));
                if let Some(n) = new { vm.fire_event(n, "Attach", vec![Value::Object(Some(me))]); }
                vm.fire_event(me, "BaseChange", vec![]);
            }
            Ok(Value::Void)
        },
        "AddController" => |vm, _| {
            let me = this(vm)?;
            if let Some(l) = vm.level {
                let head = vm.get_prop(l, "ControllerList").cloned().unwrap_or(Value::Object(None));
                vm.set_prop(me, "nextController", head);
                vm.set_prop(l, "ControllerList", Value::Object(Some(me)));
            }
            Ok(Value::Void)
        },
        "RemoveController" => |vm, _| {
            let me = this(vm)?;
            let Some(l) = vm.level else { return Ok(Value::Void) };
            let next = vm.get_prop(me, "nextController").cloned().unwrap_or(Value::Object(None));
            let mut cur = vm.get_prop(l, "ControllerList").and_then(|v| v.as_obj());
            if cur == Some(me) {
                vm.set_prop(l, "ControllerList", next);
                return Ok(Value::Void);
            }
            let mut guard = 0;
            while let Some(c) = cur {
                let n = vm.get_prop(c, "nextController").and_then(|v| v.as_obj());
                if n == Some(me) {
                    vm.set_prop(c, "nextController", next);
                    break;
                }
                cur = n;
                guard += 1;
                if guard > 100_000 { break; }
            }
            Ok(Value::Void)
        },
    );
    // Movement and display state.
    reg!(vm,
        "SetLocation" => |vm, a| { let me = this(vm)?; let h = vm.host(); Ok(Value::Bool(h.move_actor(vm, me, opt_vec(a, 0).unwrap_or_default(), false))) },
        "Move" => |vm, a| {
            let me = this(vm)?;
            let (l, d) = (vm.actor_loc(me), opt_vec(a, 0).unwrap_or_default());
            let h = vm.host();
            Ok(Value::Bool(h.move_actor(vm, me, [l[0] + d[0], l[1] + d[1], l[2] + d[2]], true)))
        },
        "MoveSmooth" => |vm, a| {
            let me = this(vm)?;
            let d = opt_vec(a, 0).unwrap_or_default();
            let h = vm.host();
            Ok(Value::Bool(h.move_smooth(vm, me, d)))
        },
        "SetRotation" => |vm, a| { let me = this(vm)?; let h = vm.host(); Ok(Value::Bool(h.set_rotation(vm, me, a.first().map(|v| v.as_rot()).unwrap_or_default()))) },
        "SetRelativeLocation" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "RelativeLocation", a[0].clone()); Ok(Value::Bool(true)) },
        "SetRelativeRotation" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "RelativeRotation", a[0].clone()); Ok(Value::Bool(true)) },
        "SetCollision" => |vm, a| {
            let me = this(vm)?;
            for (k, n) in ["bCollideActors", "bBlockActors", "bBlockPlayers"].iter().enumerate() {
                if let Some(v @ Value::Bool(_)) = a.get(k) { vm.set_prop(me, n, v.clone()); }
            }
            Ok(Value::Void)
        },
        "SetCollisionSize" => |vm, a| {
            let me = this(vm)?;
            vm.set_prop(me, "CollisionRadius", Value::Float(arg_f(a, 0)));
            vm.set_prop(me, "CollisionHeight", Value::Float(arg_f(a, 1)));
            Ok(Value::Bool(true))
        },
        "SetDrawScale" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "DrawScale", Value::Float(arg_f(a, 0))); Ok(Value::Void) },
        "SetDrawScale3D" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "DrawScale3D", a[0].clone()); Ok(Value::Void) },
        "SetDrawType" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "DrawType", Value::Byte(arg_i(a, 0) as u8)); Ok(Value::Void) },
        "SetStaticMesh" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "StaticMesh", a[0].clone()); Ok(Value::Void) },
        "SetPhysics" => |vm, a| {
            let me = this(vm)?;
            // `P2GAME_JUMP_TRACE`: name the script function that switches a pawn to walking.
            if std::env::var_os("P2GAME_JUMP_TRACE").is_some() && arg_i(a, 0) == 1 && vm.get_prop(me, "Physics").map(|v| v.as_int()) == Some(2) {
                println!("SETPHYS walking {} from {}", vm.object_name(me), vm.call_stack_summary());
            }
            if std::env::var_os("P2GAME_JUMP_TRACE").is_some() && arg_i(a, 0) == 2 && vm.get_prop(me, "bPlayer").is_some_and(|v| v.as_bool()) {
                let base = vm.get_prop(me, "Base").and_then(|v| v.as_obj());
                println!("SETPHYS falling {} vel {:?} base {:?} from {}", vm.object_name(me), vm.get_prop(me, "Velocity").map(|v| v.as_vec()), base.map(|b| (vm.object_name(b), vm.get_prop(b, "bWorldGeometry").map(|v| v.as_bool()), vm.get_prop(b, "Velocity").map(|v| v.as_vec()), vm.get_prop(b, "Physics").map(|v| v.as_int()))), vm.call_stack_summary());
            }
            vm.set_prop(me, "Physics", Value::Byte(arg_i(a, 0) as u8));
            Ok(Value::Void)
        },
        "SetViewTarget" => |vm, a| { let me = this(vm)?; vm.set_prop(me, "ViewTarget", a[0].clone()); Ok(Value::Void) },
    );
    // Queries.
    reg!(vm,
        "Trace" => |vm, a| {
            let me = vm.native_self();
            let start = opt_vec(a, 3).filter(|v| *v != [0.0; 3]).unwrap_or_else(|| me.map(|m| vm.actor_loc(m)).unwrap_or_default());
            let end = opt_vec(a, 2).unwrap_or_default();
            let req = TraceRequest { start, end, extent: opt_vec(a, 5).unwrap_or_default(), trace_actors: arg_b(a, 4) };
            let h = vm.host();
            match h.trace(vm, me, &req) {
                Some(hit) => {
                    a[0] = Value::Vector(hit.location);
                    a[1] = Value::Vector(hit.normal);
                    if let (Some(m), Some(slot)) = (hit.material, a.get_mut(6)) { *slot = Value::Resource(m); }
                    Ok(Value::Object(hit.actor))
                }
                None => {
                    a[0] = Value::Vector(end);
                    a[1] = Value::Vector([0.0; 3]);
                    Ok(Value::Object(None))
                }
            }
        },
        "FastTrace" => |vm, a| {
            let me = vm.native_self();
            let end = opt_vec(a, 0).unwrap_or_default();
            let start = opt_vec(a, 1).filter(|v| *v != [0.0; 3]).unwrap_or_else(|| me.map(|m| vm.actor_loc(m)).unwrap_or_default());
            let h = vm.host();
            Ok(Value::Bool(h.fast_trace(vm, start, end)))
        },
        "LineOfSightTo" => |vm, a| {
            let me = this(vm)?;
            let Some(o) = a.first().and_then(|v| v.as_obj()) else { return Ok(Value::Bool(false)) };
            let h = vm.host();
            Ok(Value::Bool(h.line_of_sight(vm, me, o)))
        },
        "CanSee" => |vm, a| {
            let me = this(vm)?;
            let Some(o) = a.first().and_then(|v| v.as_obj()) else { return Ok(Value::Bool(false)) };
            let h = vm.host();
            Ok(Value::Bool(h.line_of_sight(vm, me, o)))
        },
        "PlayerCanSeeMe" => |vm, _| {
            let me = this(vm)?;
            let pcs: Vec<ObjId> = vm.live_actors().into_iter().filter(|&p| vm.class_is_a(vm.objects[p].class, "PlayerController")).collect();
            let h = vm.host();
            Ok(Value::Bool(pcs.into_iter().any(|p| h.line_of_sight(vm, p, me))))
        },
        "ReachedDestination" => |vm, a| {
            let me = this(vm)?;
            let Some(g) = a.first().and_then(|v| v.as_obj()) else { return Ok(Value::Bool(false)) };
            let (p, q) = (vm.actor_loc(me), vm.actor_loc(g));
            let r = vm.get_prop(me, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0)
                + vm.get_prop(g, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0);
            let h = vm.get_prop(me, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0)
                + vm.get_prop(g, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
            let horiz = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
            Ok(Value::Bool(horiz <= r.max(16.0) && (p[2] - q[2]).abs() <= h.max(16.0)))
        },
        "InLatentExecution" => |vm, a| {
            let me = this(vm)?;
            let n = arg_i(a, 0);
            Ok(Value::Bool(vm.objects[me].exec.latent.as_ref().is_some_and(|l| l.native as i32 == n)))
        },
        "StopWaiting" => |vm, _| {
            let me = this(vm)?;
            vm.objects[me].exec.latent = None;
            Ok(Value::Void)
        },
        "MakeNoise" => |_, _| Ok(Value::Void),
        "GetURLMap" => |vm, _| { let h = vm.host(); Ok(Value::Str(h.map_name(vm))) },
        "GetMapName" => |vm, a| { let h = vm.host(); let m = h.map_name(vm); Ok(Value::Str(if m.is_empty() { a.get(1).map(|v| v.as_str()).unwrap_or_default() } else { m })) },
        "ConsoleCommand" => |vm, a| { let me = vm.native_self(); let h = vm.host(); let c = a.first().map(|v| v.as_str()).unwrap_or_default(); Ok(Value::Str(h.console_command(vm, me, &c))) },
        "ParseKillMessage" => |_, a| {
            let (k, v, m) = (a[0].as_str(), a[1].as_str(), a[2].as_str());
            Ok(Value::Str(m.replace("%k", &k).replace("%o", &v)))
        },
        "Encompasses" => |vm, a| {
            // Without brush geometry, approximate a volume by its collision cylinder.
            let me = this(vm)?;
            let Some(o) = a.first().and_then(|v| v.as_obj()) else { return Ok(Value::Bool(false)) };
            let (p, q) = (vm.actor_loc(me), vm.actor_loc(o));
            let r = vm.get_prop(me, "CollisionRadius").map(|v| v.as_float()).unwrap_or(0.0);
            let h = vm.get_prop(me, "CollisionHeight").map(|v| v.as_float()).unwrap_or(0.0);
            Ok(Value::Bool(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt() <= r && (p[2] - q[2]).abs() <= h))
        },
    );
    // Sound and animation.
    reg!(vm,
        "PlaySound" => |vm, a| { let me = vm.native_self(); let h = vm.host(); h.play_sound(vm, me, &sound_req(a)); Ok(Value::Void) },
        "PlayOwnedSound" => |vm, a| { let me = vm.native_self(); let h = vm.host(); h.play_sound(vm, me, &sound_req(a)); Ok(Value::Void) },
        "DemoPlaySound" => |vm, a| { let me = vm.native_self(); let h = vm.host(); h.play_sound(vm, me, &sound_req(a)); Ok(Value::Void) },
        "PlayFlashbangSound" => |vm, a| { let me = vm.native_self(); let h = vm.host(); h.play_sound(vm, me, &sound_req(a)); Ok(Value::Void) },
        "GetSoundDuration" => |vm, a| {
            let s = a.first().and_then(|v| v.as_resource()).map(str::to_string).unwrap_or_default();
            let h = vm.host();
            Ok(Value::Float(h.sound_duration(vm, &s)))
        },
        "PlayAnim" => |vm, a| { let me = this(vm)?; let h = vm.host(); Ok(Value::Bool(h.play_anim(vm, me, &anim_req(a, false)))) },
        "LoopAnim" => |vm, a| { let me = this(vm)?; let h = vm.host(); Ok(Value::Bool(h.play_anim(vm, me, &anim_req(a, true)))) },
        "TweenAnim" => |vm, a| {
            let me = this(vm)?;
            let req = AnimRequest { sequence: a[0].as_name(), rate: 0.0, tween: arg_f(a, 1), channel: arg_i(a, 2), looping: false };
            let h = vm.host();
            Ok(Value::Bool(h.play_anim(vm, me, &req)))
        },
        "StopAnimating" => |vm, _| { let me = this(vm)?; let h = vm.host(); h.stop_anims(vm, me); Ok(Value::Void) },
        "IsAnimating" => |vm, a| { let me = this(vm)?; let h = vm.host(); Ok(Value::Bool(h.anim_channel(vm, me, arg_i(a, 0)).is_some_and(|c| c.playing))) },
        "HasAnim" => |vm, a| { let me = this(vm)?; let h = vm.host(); let s = a[0].as_name(); Ok(Value::Bool(h.has_anim(vm, me, &s))) },
        "AnimIsInGroup" => |vm, a| { let me = this(vm)?; let h = vm.host(); let g = a[1].as_name(); Ok(Value::Bool(h.anim_in_group(vm, me, arg_i(a, 0), &g))) },
        "GetAnimParams" => |vm, a| {
            let me = this(vm)?;
            let h = vm.host();
            let c = h.anim_channel(vm, me, arg_i(a, 0)).unwrap_or_default();
            a[1] = Value::Name(if c.sequence.is_empty() { "None".into() } else { c.sequence });
            a[2] = Value::Float(c.frame);
            a[3] = Value::Float(c.rate);
            Ok(Value::Void)
        },
    );
    // Colors (Actor operators).
    crate::natives::register_color(vm);
    // Platform / level queries with fixed answers in this engine.
    reg!(vm,
        "IsDemoBuild" => |_, _| Ok(Value::Bool(false)),
        "IsSteamDeck" => |_, _| Ok(Value::Bool(false)),
        "GetLocalURL" => |vm, _| { let h = vm.host(); Ok(Value::Str(h.map_name(vm))) },
        "GetAddressURL" => |_, _| Ok(Value::Str(String::new())),
        // Date split as (days since 2000-01-01, seconds into the day, milliseconds); the
        // signature has no hour, so the minute argument is the minute of the day.
        "GetMillisecondsFrom" => |_, a| {
            let (y, m, d) = (arg_i(a, 0) as i64, arg_i(a, 1) as i64, arg_i(a, 2) as i64);
            Ok(Value::Vector([days_from_2000(y, m, d) as f32, (arg_i(a, 3) as i64 * 60 + arg_i(a, 4) as i64) as f32, arg_i(a, 5) as f32]))
        },
    );
}

/// Days from 2000-01-01 to a civil date (proleptic Gregorian).
fn days_from_2000(y: i64, m: i64, d: i64) -> i64 {
    let days = |y: i64, m: i64, d: i64| {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy
    };
    days(y, m.clamp(1, 12), d.max(1)) - days(2000, 1, 1)
}

#[cfg(test)]
mod date_tests {
    #[test]
    fn days_from_2000() {
        assert_eq!(super::days_from_2000(2000, 1, 1), 0);
        assert_eq!(super::days_from_2000(2000, 3, 1), 60);
        assert_eq!(super::days_from_2000(2001, 1, 1), 366);
    }
}

/// Unit vector of a rotation (for hosts).
pub fn rot_dir(r: [i32; 3]) -> [f32; 3] {
    let (p, y) = (urad(r[0]), urad(r[1]));
    [p.cos() * y.cos(), p.cos() * y.sin(), p.sin()]
}

/// Outcome of [`run_headless`].
#[derive(Debug, Default)]
pub struct WorldReport {
    pub frames: usize,
    /// (class, actor or None if the spawn failed, state name after the run).
    pub spawned: Vec<(String, Option<ObjId>, String)>,
    pub live_actors: usize,
    /// Unimplemented natives `"Class.Function"` with call counts, most frequent first.
    pub missing_natives: Vec<(String, u64)>,
    /// Script errors with counts, most frequent first.
    pub script_errors: Vec<(String, u64)>,
    /// Lines logged by scripts (`Log`/`Warn`) and VM warnings.
    pub log_lines: usize,
}

impl std::fmt::Display for WorldReport {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "WORLD: {} frames, {} live actors, {} log lines", self.frames, self.live_actors, self.log_lines)?;
        for (c, a, s) in &self.spawned {
            match a {
                Some(a) => writeln!(f, "  spawned {c:<32} #{a:<5} state {s}")?,
                None => writeln!(f, "  spawned {c:<32} FAILED ({s})")?,
            }
        }
        let calls: u64 = self.missing_natives.iter().map(|x| x.1).sum();
        writeln!(f, "MISSING NATIVES: {} distinct, {calls} calls", self.missing_natives.len())?;
        for (n, c) in &self.missing_natives {
            writeln!(f, "  {c:>7}  {n}")?;
        }
        writeln!(f, "SCRIPT ERRORS: {} distinct", self.script_errors.len())?;
        for (n, c) in &self.script_errors {
            writeln!(f, "  {c:>7}  {n}")?;
        }
        Ok(())
    }
}

fn sorted(m: &std::collections::HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut v: Vec<_> = m.iter().map(|(k, c)| (k.clone(), *c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Headless world run: [`Vm::init_world`] with `game` (default `Engine.GameInfo`), spawns each
/// `"Package.Class"` (spread out along X) with the begin-play events, then ticks `frames` times.
/// Set `vm.missing_native_policy` to `Lenient` first to collect every missing native.
pub fn run_headless(vm: &mut Vm, game: Option<&str>, classes: &[String], frames: usize, dt: f32) -> WorldReport {
    let mut rep = WorldReport { frames, ..Default::default() };
    if let Err(e) = vm.init_world(game) {
        vm.record_error("init_world", &e);
    }
    for (k, name) in classes.iter().enumerate() {
        match vm.find_class_by_name(name) {
            Ok(c) => {
                let p = SpawnParams { location: [k as f32 * 256.0, 0.0, 0.0], ..Default::default() };
                let a = vm.spawn_actor(c, p, true);
                rep.spawned.push((name.clone(), a, if a.is_some() { String::new() } else { "abstract, not an actor, or destroyed".into() }));
            }
            Err(e) => rep.spawned.push((name.clone(), None, e.to_string())),
        }
    }
    vm.cleanup_destroyed();
    for _ in 0..frames {
        vm.tick_world(dt);
    }
    for s in rep.spawned.iter_mut() {
        if let Some(a) = s.1 {
            s.2 = if vm.objects[a].deleted { "(destroyed)".into() } else { vm.state_name(a) };
        }
    }
    rep.live_actors = vm.live_actor_count();
    rep.missing_natives = sorted(&vm.missing_natives);
    rep.script_errors = sorted(&vm.script_errors);
    rep.log_lines = vm.log.len();
    rep
}
