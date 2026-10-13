//! Script-driven particle systems: the bridge between the uscript VM's Emitter actors and the
//! p2fx simulation.
//!
//! The VM is the source of truth for every Emitter actor (spawned by script or placed in the
//! map). [`FxWorld`] keeps, per live Emitter actor, a [`VmFx`]: the sub-emitter definitions read
//! from the actor's `Emitters` array (ParticleEmitter sub-objects) plus a [`SystemState`]. Each
//! frame [`FxWorld::sync`]:
//! 1. picks up new Emitter actors (via [`FxHost::actor_spawned`] or a scan),
//! 2. copies script edits back from the VM sub-objects into the definitions (Disabled,
//!    ParticlesPerSecond, LineStart/LineEnd, StartVelocityRange, ...: every field scripts touch),
//! 3. ticks the simulation at the actor's Location/Rotation (Owner),
//! 4. writes runtime state back (ActiveParticles, LineStart/LineEnd from SpawnParticleLength),
//! 5. destroys actors whose AutoDestroy system finished (`Destroy()` through the VM, like the
//!    engine), and drops systems of destroyed actors.
//!
//! The natives the engine implements for emitters live here too ([`FxHost`] implements
//! `uscript::host::Host::native`): `Emitter.Kill`, `ParticleEmitter.SpawnParticle`,
//! `SuperSpriteEmitter.SetProjectionNormal / SetMaxParticles / SetLiveParticleTimes /
//! SpawnParticleLength`, `StripEmitter.ForceInit / ForceSpawn / SetProjectionNormal /
//! SetMaxParticles`, the fluid helpers `Fluid.PointToLineDist / VectorsInFuzz` and
//! `P2Emitter.ThickLineCylinderCollide / FireHurtRadius`, and `Projector.AttachProjector /
//! DetachProjector / AbandonProjector` (dynamic decals, see [`crate::decal`]).
//!
//! Rendering is decoupled: [`FxWorld::snapshot`] produces Send data ([`FxFrame`]) that a Bevy
//! front end draws with [`crate::FxVmPlugin`]. Headless runs simply never take snapshots.

use crate::decal::{DecalSpec, Decals};
use crate::load::{FxEmitter, FxLibrary};
use crate::sim::{ActorSettings, Owner, SystemState, rotator_matrix};
use bevy::math::Vec3;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use upkg::emitter::{EmitterKind, ParticleEmitter, Range, RangeVector};
use uscript::host::{Host, NativeCall};
use uscript::vm::{ObjId, Value, Vm};

/// One script-visible Emitter actor and its simulation.
pub struct VmFx {
    pub actor: ObjId,
    /// The actor's `Emitters[i]` sub-objects (None: empty slot).
    pub objs: Vec<Option<ObjId>>,
    /// Definitions (decoded once from the package, then kept in sync with script edits).
    pub defs: Vec<ParticleEmitter>,
    /// Textures / meshes per sub-emitter (from the package decode).
    pub res: Vec<Arc<FxEmitter>>,
    pub settings: ActorSettings,
    pub state: SystemState,
    pub owner: Owner,
    last_loc: Option<Vec3>,
    /// Values last written to / read from the VM per sub-emitter (to detect script edits).
    seen: Vec<Mirror>,
    pub class: String,
    /// Stable id for render entities.
    pub id: u64,
    /// Actor bHidden (not drawn, still simulated).
    pub hidden: bool,
    /// Attached to a weapon held by a player pawn: drawn in the first-person view layer (the
    /// engine draws such effects with the weapon, in front of the world).
    pub first_person: bool,
}

/// Script-editable fields as last mirrored to the VM.
#[derive(Clone, Default, PartialEq)]
struct Mirror {
    disabled: bool,
    pps: f32,
    ipps: f32,
    respawn: bool,
    auto_spawn: bool,
    allow: bool,
    line_start: [f32; 3],
    line_end: [f32; 3],
    max_particles: i32,
}

/// Every Emitter actor of the current level.
pub struct FxWorld {
    pub lib: Arc<FxLibrary>,
    pub systems: HashMap<ObjId, VmFx>,
    /// Actors announced by `actor_spawned`, adopted on the next sync (after their begin-play).
    pending: Vec<ObjId>,
    /// Emitter class cache: class name -> is an Emitter subclass with particle sub-emitters.
    emitter_class: HashMap<String, bool>,
    /// Per (package path of the sub-object) decoded emitter, shared by every actor using it.
    decoded: HashMap<String, Option<Arc<FxEmitter>>>,
    pub decals: Decals,
    pub collision: Option<Arc<p2collide::World>>,
    /// The level package (sub-objects of placed Emitter actors live there).
    pub map: Option<Arc<upkg::Package>>,
    next_id: u64,
    pub stats: FxStats,
    scanned: bool,
    /// Camera placement requested by a driver (Unreal eye, target), applied by the renderer.
    pub camera: Option<([f32; 3], [f32; 3])>,
}

#[derive(Default, Debug, Clone)]
pub struct FxStats {
    pub adopted: u64,
    pub auto_destroyed: u64,
    pub killed: u64,
    pub natives: u64,
    pub decals: u64,
    pub unresolved: u64,
    pub classes: HashMap<String, u64>,
}

fn f_of(v: Option<&Value>) -> f32 {
    v.map(|v| v.as_float()).unwrap_or(0.0)
}
fn b_of(v: Option<&Value>) -> bool {
    v.is_some_and(|v| v.as_bool())
}
fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    match v {
        Value::Struct(fs) => fs.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v),
        _ => None,
    }
}
fn range_of(v: Option<&Value>) -> Option<Range> {
    let v = v?;
    Some(Range { min: f_of(field(v, "Min")), max: f_of(field(v, "Max")) })
}
fn rv_of(v: Option<&Value>) -> Option<RangeVector> {
    let v = v?;
    Some(RangeVector { x: range_of(field(v, "X"))?, y: range_of(field(v, "Y"))?, z: range_of(field(v, "Z"))? })
}
impl FxWorld {
    pub fn new(lib: Arc<FxLibrary>) -> Self {
        Self {
            lib,
            systems: HashMap::new(),
            pending: Vec::new(),
            emitter_class: HashMap::new(),
            decoded: HashMap::new(),
            decals: Decals::default(),
            collision: None,
            map: None,
            next_id: 1,
            stats: FxStats::default(),
            scanned: false,
            camera: None,
        }
    }

    /// True if the actor's class derives from Engine.Emitter.
    fn is_emitter(&mut self, vm: &Vm, a: ObjId) -> bool {
        let c = vm.objects[a].class;
        let name = &vm.classes[c].name;
        if let Some(&b) = self.emitter_class.get(name) {
            return b;
        }
        let b = vm.class_is_a(c, "Emitter");
        self.emitter_class.insert(name.clone(), b);
        b
    }

    /// Decodes the package export behind a ParticleEmitter sub-object. Sub-objects of placed
    /// actors have an `Outer` (set by the level loader) and live in the map; class-default ones
    /// live in the package of a class in the actor's class chain.
    fn decode_sub(&mut self, vm: &Vm, obj: ObjId, actor: ObjId) -> Option<Arc<FxEmitter>> {
        let name = vm.get_prop(obj, "Name").map(|v| v.as_name()).unwrap_or_default();
        let class = vm.classes[vm.objects[obj].class].name.clone();
        let in_map = matches!(vm.get_prop(obj, "Outer"), Some(Value::Object(Some(_))));
        let mut chain_pkgs: Vec<String> = Vec::new();
        let mut c = Some(vm.objects[actor].class);
        while let Some(k) = c {
            let p = vm.classes[k].package.clone();
            if !chain_pkgs.iter().any(|x| x.eq_ignore_ascii_case(&p)) {
                chain_pkgs.push(p);
            }
            c = vm.classes[k].super_;
        }
        let key = if in_map { format!("map:{class}:{name}") } else { format!("{}:{class}:{name}", chain_pkgs.join("/")) };
        if let Some(d) = self.decoded.get(&key) {
            return d.clone();
        }
        let mut found = None;
        let mut pkgs: Vec<Arc<upkg::Package>> = if in_map {
            self.map.iter().cloned().collect()
        } else {
            chain_pkgs.iter().filter_map(|p| self.lib.lib.package(p)).collect()
        };
        // Placed emitters whose sub-objects were saved at the map's top level (outer = the map
        // package, e.g. Startup.SpriteEmitter22, the main menu glow) have no actor Outer.
        if !in_map {
            pkgs.extend(self.map.iter().cloned());
        }
        for pkg in pkgs {
            if let Some(e) = pkg.find_export(&name, Some(&class)) {
                found = self.lib.load_sub_emitter(&upkg::library::Obj { pkg, export: e }).map(Arc::new);
                break;
            }
        }
        self.decoded.insert(key, found.clone());
        found
    }

    /// Builds the simulation for an Emitter actor from the VM state.
    fn adopt(&mut self, vm: &mut Vm, a: ObjId) {
        if self.systems.contains_key(&a) || vm.objects[a].deleted {
            return;
        }
        let objs: Vec<Option<ObjId>> = match vm.get_prop(a, "Emitters") {
            Some(Value::Array(v)) => v.iter().map(|x| x.as_obj()).collect(),
            _ => vec![],
        };
        let mut defs = Vec::new();
        let mut res = Vec::new();
        let mut kept = Vec::new();
        for o in objs.iter() {
            let Some(o) = *o else { continue };
            let Some(fx) = self.decode_sub(vm, o, a) else {
                self.stats.unresolved += 1;
                if std::env::var_os("P2FX_DEBUG").is_some() {
                    eprintln!("p2fx unresolved sub {} ({}) of {} outer {:?}", vm.object_name(o), vm.classes[vm.objects[o].class].name, vm.object_name(a), vm.get_prop(o, "Outer"));
                }
                continue;
            };
            let mut d = fx.data.clone();
            read_script_fields(vm, o, &mut d);
            // A script may assign another texture before the first sync (xMuzzleFlashEmitter.
            // SetEmitterTexture right after Spawn: the pistol's pistol_corona replaces the class
            // default machine_gun_corona).
            let fx = match vm.get_prop(o, "Texture").and_then(|v| v.as_resource()).map(str::to_string) {
                Some(path) if !fx.texture.as_ref().is_some_and(|t| path.rsplit('.').next().is_some_and(|leaf| t.name.rsplit('.').next().is_some_and(|l| l.eq_ignore_ascii_case(leaf)))) => match self.lib.texture_by_path(&path) {
                    Some(t) => Arc::new(FxEmitter { data: fx.data.clone(), texture: Some(t), mesh: fx.mesh.clone() }),
                    None => fx,
                },
                _ => fx,
            };
            defs.push(d);
            res.push(fx);
            kept.push(Some(o));
        }
        let class = vm.classes[vm.objects[a].class].name.clone();
        if defs.is_empty() {
            if std::env::var_os("P2FX_DEBUG").is_some() {
                eprintln!("p2fx skip {}: no emitters ({:?})", vm.object_name(a), vm.get_prop(a, "Emitters"));
            }
            return;
        }
        let settings = ActorSettings {
            auto_destroy: b_of(vm.get_prop(a, "AutoDestroy")),
            auto_reset: b_of(vm.get_prop(a, "AutoReset")),
            global_offset_range: rv_of(vm.get_prop(a, "GlobalOffsetRange")).unwrap_or_default(),
            time_till_reset_range: range_of(vm.get_prop(a, "TimeTillResetRange")).unwrap_or_default(),
        };
        let id = self.next_id;
        self.next_id += 1;
        let state = SystemState::new(&defs, &settings, 0x5eed_0000 ^ id.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let seen = defs.iter().map(mirror_of).collect();
        let hidden = b_of(vm.get_prop(a, "bHidden"));
        // Muzzle flashes attach to the first-person weapon (Base = a Weapon whose Instigator is
        // the local player's pawn).
        let first_person = vm.get_prop(a, "Base").and_then(|v| v.as_obj()).is_some_and(|b| vm.class_is_a(vm.objects[b].class, "Weapon"));
        // Screenshot aid: `P2FX_FP_LIFE_SCALE=6` stretches first-person effects (muzzle flashes
        // live ~0.05 s, shorter than the screenshot readback latency).
        if first_person {
            if let Some(k) = std::env::var("P2FX_FP_LIFE_SCALE").ok().and_then(|s| s.parse::<f32>().ok()) {
                // The actor's LifeSpan (set by SetEmitterLifetime) ends it first otherwise.
                let ls = f_of(vm.get_prop(a, "LifeSpan"));
                if ls > 0.0 {
                    vm.set_prop(a, "LifeSpan", Value::Float(ls * k));
                }
                for (d, o) in defs.iter_mut().zip(&kept) {
                    d.lifetime_range.min *= k;
                    d.lifetime_range.max *= k;
                    // Also in the VM object, which is re-read every frame.
                    if let Some(o) = o {
                        let r = Value::Struct(vec![("Min".into(), Value::Float(d.lifetime_range.min)), ("Max".into(), Value::Float(d.lifetime_range.max))]);
                        vm.set_prop(*o, "LifetimeRange", r);
                    }
                }
            }
        }
        let mut fx = VmFx { actor: a, objs: kept, defs, res, settings, state, owner: Owner::default(), last_loc: None, seen, class: class.clone(), id, hidden, first_person };
        fx.update_owner(vm, 0.0);
        if std::env::var_os("P2FX_DEBUG").is_some() {
            let base = vm.get_prop(a, "Base").and_then(|v| v.as_obj()).map(|b| vm.object_name(b));
            let owner = vm.get_prop(a, "Owner").and_then(|v| v.as_obj()).map(|b| vm.object_name(b));
            eprintln!("p2fx adopt {} at {:?} base {base:?} owner {owner:?} emitters {}", vm.object_name(a), fx.owner.location, fx.defs.len());
            for (o, d) in fx.objs.iter().zip(&fx.defs) {
                if let Some(o) = o {
                    eprintln!("    sub {} tex {:?} style {:?} disabled {} max {} size {:?} life {:?} scale {:?} colorscale {:?} vel {:?} acc {:?} loss {:?}", vm.object_name(*o), vm.get_prop(*o, "Texture"), d.draw_style, d.disabled, d.max_particles, d.start_size_range.x, d.lifetime_range, d.size_scale, d.use_color_scale.then_some(&d.color_scale), d.start_velocity_range, d.acceleration, d.velocity_loss_range);
                }
            }
        }
        *self.stats.classes.entry(class).or_default() += 1;
        self.stats.adopted += 1;
        self.systems.insert(a, fx);
    }

    /// Queues an actor announced by the VM (adopted at the next sync, after its begin-play).
    /// Its `Emitters` sub-objects come from the class defaults, where the VM keeps one shared
    /// instance per export; the engine instances them per actor, so they are duplicated here
    /// (before PreBeginPlay) and script edits stay local to this actor.
    pub fn spawned(&mut self, vm: &mut Vm, a: ObjId) {
        if !self.is_emitter(vm, a) {
            return;
        }
        if let Some(Value::Array(list)) = vm.get_prop(a, "Emitters").cloned() {
            let copied: Vec<Value> = list
                .iter()
                .map(|v| match v.as_obj() {
                    Some(o) if !vm.objects[o].deleted => {
                        let (class, slots) = (vm.objects[o].class, vm.objects[o].slots.clone());
                        let n = vm.new_object(class);
                        vm.objects[n].slots = slots;
                        Value::Object(Some(n))
                    }
                    _ => v.clone(),
                })
                .collect();
            vm.set_prop(a, "Emitters", Value::Array(copied));
        }
        self.pending.push(a);
    }

    /// Per-frame update (see module docs).
    pub fn sync(&mut self, vm: &mut Vm, dt: f32) {
        if !self.scanned {
            self.scanned = true;
            let all: Vec<ObjId> = vm.actors.clone();
            for a in all {
                if !vm.objects[a].deleted && self.is_emitter(vm, a) {
                    self.pending.push(a);
                }
            }
        }
        for a in std::mem::take(&mut self.pending) {
            self.adopt(vm, a);
        }
        self.systems.retain(|&a, _| vm.objects.get(a).is_some_and(|o| !o.deleted));
        let collider = self.collision.clone();
        let mut finished = Vec::new();
        for fx in self.systems.values_mut() {
            fx.pull_script_edits(vm);
            fx.update_owner(vm, dt);
            if dt > 0.0 {
                let c = collider.as_deref().map(|w| w as &dyn crate::sim::Collider);
                fx.state.tick_with(&fx.defs, &fx.settings, &fx.owner, dt, c);
            }
            fx.push_runtime(vm);
            if fx.state.finished {
                finished.push(fx.actor);
            }
        }
        for a in finished {
            self.systems.remove(&a);
            self.stats.auto_destroyed += 1;
            if !vm.objects[a].deleted {
                vm.destroy_actor(a);
            }
        }
        self.decals.tick(dt);
    }

    fn system_of(&mut self, vm: &mut Vm, a: ObjId) -> Option<&mut VmFx> {
        if !self.systems.contains_key(&a) && self.is_emitter(vm, a) {
            self.adopt(vm, a);
        }
        self.systems.get_mut(&a)
    }

    /// Finds the system and sub-emitter index for a ParticleEmitter sub-object.
    fn find_sub(&mut self, vm: &mut Vm, obj: ObjId) -> Option<(ObjId, usize)> {
        // Owner first (scripts set Emitters[i].Owner), else search.
        if let Some(Value::Object(Some(o))) = vm.get_prop(obj, "Owner") {
            let o = *o;
            if let Some(fx) = self.system_of(vm, o) {
                if let Some(i) = fx.objs.iter().position(|x| *x == Some(obj)) {
                    return Some((o, i));
                }
            }
        }
        // Pending actors may not be adopted yet.
        for a in std::mem::take(&mut self.pending) {
            self.adopt(vm, a);
        }
        self.systems.iter().find_map(|(&a, fx)| fx.objs.iter().position(|x| *x == Some(obj)).map(|i| (a, i)))
    }

    /// Script natives handled here (see module docs). None: not ours.
    pub fn native(&mut self, vm: &mut Vm, call: &NativeCall, args: &mut [Value]) -> Option<Value> {
        let name = call.name.to_ascii_lowercase();
        let this = call.this;
        let v = |k: usize| args.get(k).map(|x| x.as_vec()).map(Vec3::from).unwrap_or(Vec3::ZERO);
        let f = |k: usize| args.get(k).map(|x| x.as_float()).unwrap_or(0.0);
        let r = match name.as_str() {
            "kill" if call.class.eq_ignore_ascii_case("Emitter") => {
                let a = this?;
                if let Some(fx) = self.system_of(vm, a) {
                    fx.state.kill();
                    fx.settings.auto_destroy = true;
                } else {
                    vm.destroy_actor(a);
                }
                self.stats.killed += 1;
                Value::Void
            }
            "spawnparticle" => {
                let o = this?;
                let n = args.first().map(|x| x.as_int()).unwrap_or(1).max(0) as usize;
                if let Some((a, i)) = self.find_sub(vm, o) {
                    let fx = self.systems.get_mut(&a)?;
                    fx.pull_script_edits(vm);
                    fx.state.spawn_now(&fx.defs, i, &fx.owner, n);
                }
                Value::Void
            }
            "setprojectionnormal" => {
                let o = this?;
                let n = v(0);
                vm.set_prop(o, "ProjectionNormal", Value::Vector(n.to_array()));
                if let Some((a, i)) = self.find_sub(vm, o) {
                    self.systems.get_mut(&a)?.defs[i].projection_normal = n.normalize_or_zero().to_array();
                }
                Value::Void
            }
            "setmaxparticles" => {
                let o = this?;
                let n = args.first().map(|x| x.as_int()).unwrap_or(0);
                vm.set_prop(o, "MaxParticles", Value::Int(n));
                if let Some((a, i)) = self.find_sub(vm, o) {
                    let fx = self.systems.get_mut(&a)?;
                    fx.state.set_max_particles(&mut fx.defs, i, n);
                    fx.seen[i].max_particles = n;
                }
                Value::Void
            }
            "setliveparticletimes" => {
                let o = this?;
                let t = f(0);
                if let Some((a, i)) = self.find_sub(vm, o) {
                    self.systems.get_mut(&a)?.state.emitters[i].set_live_particle_times(t);
                }
                Value::Void
            }
            "spawnparticlelength" => {
                let o = this?;
                let (s, e, size) = (v(0), v(1), f(2));
                if let Some((a, i)) = self.find_sub(vm, o) {
                    let fx = self.systems.get_mut(&a)?;
                    fx.pull_script_edits(vm);
                    let num = fx.defs.len();
                    let owner = fx.owner;
                    let def = &mut fx.defs[i];
                    let st = &mut fx.state.emitters[i];
                    st.spawn_length(def, &mut fx.state.rng, &owner, num, s, e, size.max(0.5));
                    fx.state.finished = false;
                    fx.push_runtime(vm);
                }
                Value::Void
            }
            "forceinit" | "forcespawn" => Value::Void,
            "pointtolinedist" => {
                // (start, end, point, out perpdist, out u)
                let (a, b, p) = (v(0), v(1), v(2));
                let es = b - a;
                let dir = es.normalize_or_zero();
                if dir == Vec3::ZERO {
                    return Some(Value::Bool(false));
                }
                let ps = p - a;
                let perp = (-ps).cross(dir).length();
                let u = ps.dot(es) / es.length_squared();
                if args.len() > 4 {
                    args[3] = Value::Float(perp);
                    args[4] = Value::Float(u);
                }
                Value::Bool(true)
            }
            "vectorsinfuzz" => {
                let (a, b, z) = (v(0), v(1), f(2));
                Value::Bool((a - b).abs().max_element() < z)
            }
            "thicklinecylindercollide" => {
                Value::Bool(thick_line_cylinder(v(0), v(1), f(2), f(3), v(4), f(5), f(6)))
            }
            "firehurtradius" => {
                let a = this?;
                self.fire_hurt_radius(vm, a, args);
                Value::Void
            }
            "attachprojector" => {
                let a = this?;
                if let Some(spec) = DecalSpec::from_vm(vm, a, &self.lib) {
                    if let Some(w) = self.collision.clone() {
                        self.decals.attach(a, spec, &w);
                        self.stats.decals += 1;
                    }
                }
                Value::Void
            }
            "abandonprojector" => {
                let a = this?;
                let life = args.first().map(|x| x.as_float()).filter(|l| *l > 0.0);
                self.decals.abandon(a, life);
                Value::Void
            }
            "detachprojector" => {
                let a = this?;
                self.decals.detach(a);
                Value::Void
            }
            "attachactor" | "detachactor" => Value::Void,
            _ => return None,
        };
        self.stats.natives += 1;
        Some(r)
    }

    /// `P2Emitter.FireHurtRadius(Damage, Radius, DamageType, LineStart, LineEnd, DefColRadius,
    /// DefColHeight)`: TakeDamage on every actor within Radius of the emitter, except FluidPuddle
    /// subclasses must also touch the thick line.
    fn fire_hurt_radius(&mut self, vm: &mut Vm, me: ObjId, args: &[Value]) {
        let dmg = args.first().map(|x| x.as_float()).unwrap_or(0.0);
        let radius = args.get(1).map(|x| x.as_float()).unwrap_or(0.0);
        let dt = args.get(2).cloned().unwrap_or(Value::Class(None));
        let ls = Vec3::from(args.get(3).map(|x| x.as_vec()).unwrap_or_default());
        let le = Vec3::from(args.get(4).map(|x| x.as_vec()).unwrap_or_default());
        let (lr, lh) = (args.get(5).map(|x| x.as_float()).unwrap_or(0.0), args.get(6).map(|x| x.as_float()).unwrap_or(0.0));
        let here = Vec3::from(vm.actor_loc(me));
        let instigator = vm.get_prop(me, "Instigator").cloned().unwrap_or(Value::Object(None));
        let victims: Vec<ObjId> = vm
            .actors
            .iter()
            .copied()
            .filter(|&o| o != me && !vm.objects[o].deleted)
            .filter(|&o| (Vec3::from(vm.actor_loc(o)) - here).length() <= radius + f_of(vm.get_prop(o, "CollisionRadius")))
            .collect();
        for o in victims {
            if vm.objects[o].deleted {
                continue;
            }
            let c = vm.objects[o].class;
            if !vm.class_is_a(c, "FluidPuddle") {
                let loc = Vec3::from(vm.actor_loc(o));
                let (cr, ch) = (f_of(vm.get_prop(o, "CollisionRadius")), f_of(vm.get_prop(o, "CollisionHeight")));
                if !thick_line_cylinder(ls, le, lr, lh, loc, cr, ch) {
                    continue;
                }
            }
            let hl = vm.actor_loc(o);
            vm.fire_event(o, "TakeDamage", vec![Value::Int(dmg as i32), instigator.clone(), Value::Vector(hl), Value::Vector([0.0; 3]), dt.clone()]);
        }
    }

    /// Render snapshot of every live system (Send).
    pub fn snapshot(&self, out: &mut FxFrame) {
        out.systems.clear();
        for fx in self.systems.values() {
            if fx.hidden {
                continue;
            }
            out.systems.push(SysSnap { id: fx.id, class: fx.class.clone(), defs: fx.defs.clone(), res: fx.res.clone(), state: fx.state.clone(), owner: fx.owner, first_person: fx.first_person });
        }
        out.decals = self.decals.draws();
        out.camera = self.camera;
    }
}

/// ThickLineCylinderCollide: does a cylinder (ActorLoc, radius, half-height) touch the thick
/// segment startpt..endpt (radius lineradius, half-height lineheight)? Tests the end points, then
/// the perpendicular XY distance to the segment and the Z overlap.
pub fn thick_line_cylinder(a: Vec3, b: Vec3, lr: f32, lh: f32, p: Vec3, r: f32, h: f32) -> bool {
    let xy = |v: Vec3| bevy::math::Vec2::new(v.x, v.y);
    let (a2, b2, p2) = (xy(a), xy(b), xy(p));
    if (p2 - a2).length() <= r + lr && (a.z - p.z).abs() < h + lh {
        return true;
    }
    if (p2 - b2).length() <= r + lr && (b.z - p.z).abs() < h + lh {
        return true;
    }
    let d = b2 - a2;
    let l2 = d.length_squared();
    if l2 <= 0.0 {
        return false;
    }
    let t = ((p2 - a2).dot(d) / l2).clamp(0.0, 1.0);
    let q = a2 + d * t;
    let z = a.z + (b.z - a.z) * t;
    (p2 - q).length() <= r + lr && (z - p.z).abs() < h + lh
}

fn mirror_of(d: &ParticleEmitter) -> Mirror {
    Mirror {
        disabled: d.disabled,
        pps: d.particles_per_second,
        ipps: d.initial_particles_per_second,
        respawn: d.respawn_dead_particles,
        auto_spawn: d.automatic_initial_spawning,
        allow: d.allow_particle_spawn,
        line_start: d.line_start,
        line_end: d.line_end,
        max_particles: d.max_particles,
    }
}

/// Reads every field scripts are known to change from a ParticleEmitter sub-object.
fn read_script_fields(vm: &Vm, o: ObjId, d: &mut ParticleEmitter) {
    let g = |n: &str| vm.get_prop(o, n);
    if let Some(r) = rv_of(g("StartVelocityRange")) {
        d.start_velocity_range = r;
    }
    if let Some(r) = rv_of(g("StartLocationRange")) {
        d.start_location_range = r;
    }
    if let Some(r) = rv_of(g("StartSizeRange")) {
        d.start_size_range = r;
    }
    if let Some(r) = range_of(g("SphereRadiusRange")) {
        d.sphere_radius_range = r;
    }
    if let Some(r) = range_of(g("LifetimeRange")) {
        d.lifetime_range = r;
    }
    if let Some(r) = rv_of(g("VelocityLossRange")) {
        d.velocity_loss_range = r;
    }
    if let Some(v) = g("Acceleration") {
        d.acceleration = v.as_vec();
    }
    if let Some(v) = g("StartLocationOffset") {
        d.start_location_offset = v.as_vec();
    }
    if let Some(v) = g("ProjectionNormal") {
        let n = v.as_vec();
        if n != [0.0; 3] {
            d.projection_normal = n;
        }
    }
    if let Some(v) = g("MaxAbsVelocity") {
        d.max_abs_velocity = v.as_vec();
    }
    if let Some(v) = g("FadeOutStartTime") {
        d.fade_out_start_time = v.as_float();
    }
    if let Some(v) = g("FadeInEndTime") {
        d.fade_in_end_time = v.as_float();
    }
    if let Some(v) = g("FadeOut") {
        d.fade_out = v.as_bool();
    }
    if let Some(v) = g("UseColorScale") {
        d.use_color_scale = v.as_bool();
    }
    if let Some(Value::Array(keys)) = g("ColorScale") {
        if keys.len() == d.color_scale.len() {
            for (k, kv) in d.color_scale.iter_mut().zip(keys) {
                k.time = f_of(field(kv, "RelativeTime"));
                if let Some(Value::Struct(c)) = field(kv, "Color") {
                    let ch = |n: &str| c.iter().find(|(x, _)| x.eq_ignore_ascii_case(n)).map(|(_, v)| v.as_int().clamp(0, 255) as u8).unwrap_or(0);
                    k.color = [ch("R"), ch("G"), ch("B"), ch("A")];
                }
            }
        }
    }
    if let Some(Value::Array(keys)) = g("SizeScale") {
        if keys.len() == d.size_scale.len() {
            for (k, kv) in d.size_scale.iter_mut().zip(keys) {
                k.time = f_of(field(kv, "RelativeTime"));
                k.size = f_of(field(kv, "RelativeSize"));
            }
        }
    }
    if let Some(r) = rv_of(g("StartSpinRange")) {
        d.start_spin_range = r;
    }
    if let Some(r) = rv_of(g("SpinsPerSecondRange")) {
        d.spins_per_second_range = r;
    }
    if let Some(v) = g("SpinParticles") {
        d.spin_particles = v.as_bool();
    }
    if let Some(v) = g("LocationShapeExtend") {
        d.location_shape_extend = v.as_int() as u8;
    }
    if let Some(v) = g("Disabled") {
        d.disabled = v.as_bool();
    }
    if let Some(v) = g("ParticlesPerSecond") {
        d.particles_per_second = v.as_float();
    }
    if let Some(v) = g("InitialParticlesPerSecond") {
        d.initial_particles_per_second = v.as_float();
    }
    if let Some(v) = g("RespawnDeadParticles") {
        d.respawn_dead_particles = v.as_bool();
    }
    if let Some(v) = g("AutomaticInitialSpawning") {
        d.automatic_initial_spawning = v.as_bool();
    }
    if let Some(v) = g("AllowParticleSpawn") {
        d.allow_particle_spawn = v.as_bool();
    }
    if let Some(v) = g("LineStart") {
        d.line_start = v.as_vec();
    }
    if let Some(v) = g("LineEnd") {
        d.line_end = v.as_vec();
    }
    if let Some(v) = g("MaxParticles") {
        d.max_particles = v.as_int();
    }
}

impl VmFx {
    /// Copies script edits made since the last frame into the definitions and runtime state.
    fn pull_script_edits(&mut self, vm: &Vm) {
        for (i, o) in self.objs.iter().enumerate() {
            let Some(o) = *o else { continue };
            if vm.objects.get(o).is_none() {
                continue;
            }
            let before = self.seen[i].clone();
            let d = &mut self.defs[i];
            read_script_fields(vm, o, d);
            let now = mirror_of(d);
            if now == before {
                continue;
            }
            let st = &mut self.state.emitters[i];
            if now.disabled != before.disabled {
                st.disabled = now.disabled;
            }
            if now.pps != before.pps {
                st.particles_per_second = now.pps;
            }
            if now.ipps != before.ipps {
                st.initial_particles_per_second = now.ipps;
            }
            if now.respawn != before.respawn {
                st.respawn_dead_particles = now.respawn;
            }
            if now.auto_spawn != before.auto_spawn {
                st.automatic_initial_spawning = now.auto_spawn;
            }
            if now.allow != before.allow {
                st.allow_particle_spawn = now.allow;
            }
            if now.max_particles != before.max_particles && st.initialized {
                self.state.set_max_particles(&mut self.defs, i, now.max_particles);
            }
            self.seen[i] = now;
        }
        // Actor-level: AutoDestroy set by script (SelfDestroy / SlowlyDestroy).
        if b_of(vm.get_prop(self.actor, "AutoDestroy")) && !self.settings.auto_destroy {
            self.settings.auto_destroy = true;
            self.state.auto_destroy = true;
        }
        self.hidden = b_of(vm.get_prop(self.actor, "bHidden"));
    }

    /// Writes runtime values scripts read back (ActiveParticles, LineStart/LineEnd).
    fn push_runtime(&mut self, vm: &mut Vm) {
        for (i, o) in self.objs.iter().enumerate() {
            let Some(o) = *o else { continue };
            let st = &self.state.emitters[i];
            let d = &self.defs[i];
            vm.set_prop(o, "ActiveParticles", Value::Int(st.active_particles as i32));
            if d.kind == EmitterKind::SuperSprite || d.kind == EmitterKind::Strip {
                vm.set_prop(o, "LineStart", Value::Vector(d.line_start));
                vm.set_prop(o, "LineEnd", Value::Vector(d.line_end));
            }
            self.seen[i] = mirror_of(d);
        }
    }

    fn update_owner(&mut self, vm: &Vm, dt: f32) {
        let loc = Vec3::from(vm.actor_loc(self.actor));
        let rot = vm.actor_rot(self.actor);
        let vel = match self.last_loc {
            Some(p) if dt > 0.0 => (loc - p) / dt,
            _ => Vec3::ZERO,
        };
        self.last_loc = Some(loc);
        self.owner = Owner { location: loc, rotation: rotator_matrix(rot), velocity: vel };
    }
}

/// Render data of one system for a frame.
#[derive(Clone)]
pub struct SysSnap {
    pub id: u64,
    pub class: String,
    /// Draw in the first-person overlay layer (see [`VmFx::first_person`]).
    pub first_person: bool,
    pub defs: Vec<ParticleEmitter>,
    pub res: Vec<Arc<FxEmitter>>,
    pub state: SystemState,
    pub owner: Owner,
}

/// Everything the renderer needs for one frame (Send).
#[derive(Clone, Default)]
pub struct FxFrame {
    pub systems: Vec<SysSnap>,
    pub decals: Vec<Arc<crate::decal::DecalDraw>>,
    /// Requested camera (Unreal eye, target).
    pub camera: Option<([f32; 3], [f32; 3])>,
}

/// `uscript::host::Host` extension: announces new actors to [`FxWorld`] and implements the
/// effect natives. Push into the game host's extension list; call [`FxWorld::sync`] per frame.
pub struct FxHost {
    pub world: Rc<RefCell<FxWorld>>,
}

impl Host for FxHost {
    fn actor_spawned(&self, vm: &mut Vm, actor: ObjId) {
        if let Ok(mut w) = self.world.try_borrow_mut() {
            w.spawned(vm, actor);
        }
    }

    fn actor_destroyed(&self, _vm: &mut Vm, actor: ObjId) {
        if let Ok(mut w) = self.world.try_borrow_mut() {
            w.systems.remove(&actor);
            w.decals.owner_destroyed(actor);
        }
    }

    fn native(&self, vm: &mut Vm, call: &NativeCall, args: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        let interesting = matches!(
            call.class.to_ascii_lowercase().as_str(),
            "emitter" | "particleemitter" | "superspriteemitter" | "stripemitter" | "fluid" | "p2emitter" | "projector"
        );
        if !interesting {
            return None;
        }
        // Natives may re-enter script (FireHurtRadius -> TakeDamage -> Spawn), so the world is
        // only borrowed when free; a nested call falls back to the VM default.
        let mut w = self.world.try_borrow_mut().ok()?;
        if call.name.eq_ignore_ascii_case("FireHurtRadius") {
            drop(w);
            let me = call.this?;
            // Collect without holding the borrow across script calls.
            let mut tmp = FxWorld::new(self.world.borrow().lib.clone());
            tmp.fire_hurt_radius(vm, me, args);
            return Some(Ok(Value::Void));
        }
        w.native(vm, call, args).map(Ok)
    }
}

/// Per-level shared handle: what a game host keeps to sync and to render.
pub type SharedFx = Rc<RefCell<FxWorld>>;
