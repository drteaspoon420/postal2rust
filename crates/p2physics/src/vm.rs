//! Optional glue between [`PhysWorld`] and the uscript VM (feature `vm`). A host keeps one
//! [`VmBridge`] next to its world and:
//!
//! 1. registers actors with [`VmBridge::add_actor`] (or [`VmBridge::bind`] for actors the map
//!    loader already created, via `LoadedMap::by_export` and the host's export-to-object table),
//! 2. before physics, copies script-side changes in with [`VmBridge::pull`] (Location, Rotation,
//!    Velocity, Acceleration, Physics, collision flags and size, pawn movement fields),
//! 3. calls the movement / trace natives on the world (or `tick`),
//! 4. copies results back with [`VmBridge::push`] and delivers queued events with
//!    [`VmBridge::deliver_events`].
//!
//! Pawn events go to the Controller first as in UE2 (`NotifyLanded`, `NotifyHitWall`,
//! `NotifyBump`, `NotifyPhysicsVolumeChange`, `NotifyHeadVolumeChange`); when it returns true the
//! pawn event is skipped.

use crate::actor::{Actor, PawnInfo};
use crate::types::{ActorId, Event, HitActor, Physics};
use crate::world::PhysWorld;
use glam::Vec3;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use uscript::vm::{ClassId, ObjId, Value, Vm};

/// Property names the bridge reads and writes, in [`Slots`] order.
const PROPS: &[&str] = &[
    "Location", "Rotation", "Velocity", "Acceleration", "Physics", "CollisionRadius", "CollisionHeight",
    "bCollideActors", "bCollideWorld", "bBlockActors", "bBlockPlayers", "bBlockZeroExtentTraces",
    "bBlockNonZeroExtentTraces", "bProjTarget", "bWorldGeometry", "bStatic", "bHidden", "bBounce", "Mass",
    "Buoyancy", "bRotateToDesired", "RotationRate", "DesiredRotation", "KeyNum", "PrevKeyNum", "PhysAlpha",
    "PhysRate", "MoveTime", "bInterpolating", "OldPos", "OldRot", "BasePos", "BaseRot", "KeyPos", "KeyRot",
    "GroundSpeed", "WaterSpeed", "AirSpeed", "LadderSpeed", "AccelRate", "JumpZ", "AirControl", "WalkingPct",
    "CrouchedPct", "CrouchRadius", "CrouchHeight", "BaseEyeHeight", "bCanWalkOffLedges", "bAvoidLedges",
    "bStopAtLedges", "bCanSwim", "bCanFly", "bCanCrouch", "bCanClimbLadders", "bIsWalking", "bWantsToCrouch",
    "Controller", "Base", "Touching", "PhysicsVolume", "HeadVolume", "Floor", "bIsCrouched", "Owner",
    "bFixedRotationDir",
];

/// Indices into [`PROPS`].
#[allow(non_camel_case_types, dead_code)]
#[derive(Clone, Copy)]
enum P {
    Location, Rotation, Velocity, Acceleration, Physics, CollisionRadius, CollisionHeight,
    bCollideActors, bCollideWorld, bBlockActors, bBlockPlayers, bBlockZeroExtentTraces,
    bBlockNonZeroExtentTraces, bProjTarget, bWorldGeometry, bStatic, bHidden, bBounce, Mass,
    Buoyancy, bRotateToDesired, RotationRate, DesiredRotation, KeyNum, PrevKeyNum, PhysAlpha,
    PhysRate, MoveTime, bInterpolating, OldPos, OldRot, BasePos, BaseRot, KeyPos, KeyRot,
    GroundSpeed, WaterSpeed, AirSpeed, LadderSpeed, AccelRate, JumpZ, AirControl, WalkingPct,
    CrouchedPct, CrouchRadius, CrouchHeight, BaseEyeHeight, bCanWalkOffLedges, bAvoidLedges,
    bStopAtLedges, bCanSwim, bCanFly, bCanCrouch, bCanClimbLadders, bIsWalking, bWantsToCrouch,
    Controller, Base, Touching, PhysicsVolume, HeadVolume, Floor, bIsCrouched, Owner,
    bFixedRotationDir,
}

/// Slot index of every [`PROPS`] name in one class (resolved once per class, so the per-frame
/// pull/push does no name hashing).
struct Slots(Vec<Option<usize>>);

impl Slots {
    fn new(vm: &Vm, class: ClassId) -> Slots {
        let c = &vm.classes[class];
        Slots(PROPS.iter().map(|n| c.slot(n)).collect())
    }
}

/// Typed reads of one object through its class's [`Slots`].
struct Obj<'a> {
    slots: &'a [Value],
    s: &'a Slots,
}

impl Obj<'_> {
    fn get(&self, p: P) -> Option<&Value> {
        self.s.0[p as usize].and_then(|i| self.slots.get(i))
    }
    fn f(&self, p: P) -> Option<f32> {
        self.get(p).map(|v| v.as_float())
    }
    fn b(&self, p: P) -> Option<bool> {
        self.get(p).map(|v| v.as_bool())
    }
    fn byte(&self, p: P) -> Option<u8> {
        self.get(p).map(|v| v.as_int() as u8)
    }
    fn v(&self, p: P) -> Option<Vec3> {
        v3(self.get(p))
    }
    fn rot(&self, p: P) -> Option<[i32; 3]> {
        match self.get(p) {
            Some(Value::Rotator(r)) => Some(*r),
            _ => None,
        }
    }
    fn obj(&self, p: P) -> Option<ObjId> {
        self.get(p).and_then(|v| v.as_obj())
    }
}

#[derive(Default, Clone)]
pub struct VmBridge {
    to_phys: HashMap<ObjId, ActorId>,
    to_obj: HashMap<ActorId, ObjId>,
    /// Zone actor objects by map export (for ZoneChange); the LevelInfo is used for zone 0.
    pub zone_objects: HashMap<u32, ObjId>,
    /// Level static mesh actors baked into the level collision, by map export: a trace that hits
    /// one returns that actor to script (UE2 Trace returns the StaticMeshActor, e.g. a
    /// UrineBucket grave mound or a breakable prop), not the LevelInfo.
    pub level_actors: HashMap<u32, ObjId>,
    /// Per-class slot tables (shared by clones).
    slots: Rc<RefCell<HashMap<ClassId, Rc<Slots>>>>,
    /// Bound objects in object order (rebuilt after add/remove), for `pull_dynamic`.
    order: Option<Vec<ObjId>>,
    /// The Physics value last pushed to each object. A different value in the VM means script
    /// called SetPhysics since (e.g. PlayDying's PHYS_KarmaRagDoll); push leaves it for the next
    /// pull instead of overwriting it with the physics actor's stale mode.
    pushed_physics: Rc<RefCell<HashMap<ObjId, u8>>>,
}

fn v3(v: Option<&Value>) -> Option<Vec3> {
    match v {
        Some(Value::Vector(a)) => Some(Vec3::from(*a)),
        _ => None,
    }
}

fn vv(v: Vec3) -> Value {
    Value::Vector(v.to_array())
}

impl VmBridge {
    pub fn new() -> VmBridge {
        VmBridge::default()
    }

    pub fn phys(&self, obj: ObjId) -> Option<ActorId> {
        self.to_phys.get(&obj).copied()
    }

    pub fn obj(&self, id: ActorId) -> Option<ObjId> {
        self.to_obj.get(&id).copied()
    }

    /// Associates an existing physics actor with a VM object.
    pub fn bind(&mut self, world: &mut PhysWorld, obj: ObjId, id: ActorId) {
        if let Some(a) = world.get_mut(id) {
            a.user = obj as u64;
        }
        self.to_phys.insert(obj, id);
        self.to_obj.insert(id, obj);
        self.order = None;
    }

    /// Creates a cylinder physics actor for a VM actor from its current properties (pawns get
    /// [`PawnInfo`] from the Pawn fields).
    pub fn add_actor(&mut self, vm: &Vm, world: &mut PhysWorld, obj: ObjId) -> ActorId {
        if let Some(id) = self.phys(obj) {
            return id;
        }
        if let Some(id) = world.find_user(obj as u64).filter(|&i| !self.to_obj.contains_key(&i)) {
            self.bind(world, obj, id);
            return id;
        }
        let class = vm.classes[vm.objects[obj].class].name.clone();
        let mut a = Actor::new(class, Vec3::from(vm.actor_loc(obj)));
        a.user = obj as u64;
        if vm.class_is_a(vm.objects[obj].class, "Pawn") {
            a.pawn = Some(PawnInfo::default());
            a.can_be_base = false;
        }
        self.read_into(vm, obj, &mut a);
        if let Some(p) = a.pawn.as_mut() {
            p.stand_radius = a.collision_radius;
            p.stand_height = a.collision_height;
        }
        // UE2 Karma: a KActor whose KParams.KStartEnabled is false starts asleep and is not
        // simulated until something wakes it (impulse, damage, KWake). Summoned barrels hang where
        // they appear (FIDELITY #27). A non-zero Velocity wakes it here (see `phys_karma`).
        let karma_asleep = a.physics == Physics::Karma
            && !vm
                .get_prop(obj, "KParams")
                .and_then(|v| v.as_obj())
                .and_then(|kp| vm.get_prop(kp, "KStartEnabled"))
                .is_some_and(|v| v.as_bool());
        // The physics mode read from script is what physics now holds (see `pushed_physics`).
        // Without this record, a pawn spawned at runtime (Spawn registers it before its
        // begin-play) lost the PHYS_Falling its controller's Possess -> SetMovementPhysics set:
        // `world.add` queues it as moved, and the next push_moved (any script call into physics)
        // took the missing record as "not changed by script" and wrote the stale PHYS_None back.
        // PawnSpawner pawns (Habib's wives behind the Lucky Ganesh) then never fell, never got
        // NotifyLanded, and stood forever in InitFall.
        self.pushed_physics.borrow_mut().insert(obj, a.physics as u8);
        let id = world.add(a);
        if karma_asleep {
            world.actor_mut(id).sleeping = true;
        }
        self.to_phys.insert(obj, id);
        self.to_obj.insert(id, obj);
        self.order = None;
        id
    }

    /// Removes the physics actor of a destroyed VM actor.
    pub fn remove_actor(&mut self, world: &mut PhysWorld, obj: ObjId) {
        if let Some(id) = self.to_phys.remove(&obj) {
            self.to_obj.remove(&id);
            self.pushed_physics.borrow_mut().remove(&obj);
            world.remove(id);
            self.order = None;
        }
    }

    /// The slot table of `obj`'s class (built on first use).
    fn slots_of(&self, vm: &Vm, obj: ObjId) -> Rc<Slots> {
        let class = vm.objects[obj].class;
        self.slots.borrow_mut().entry(class).or_insert_with(|| Rc::new(Slots::new(vm, class))).clone()
    }

    fn read_into(&self, vm: &Vm, obj: ObjId, a: &mut Actor) {
        let s = self.slots_of(vm, obj);
        let o = Obj { slots: &vm.objects[obj].slots, s: &s };
        a.location = o.v(P::Location).unwrap_or(a.location);
        a.rotation = o.rot(P::Rotation).unwrap_or(a.rotation);
        a.velocity = o.v(P::Velocity).unwrap_or(a.velocity);
        a.acceleration = o.v(P::Acceleration).unwrap_or(a.acceleration);
        if let Some(p) = o.byte(P::Physics) {
            a.physics = Physics::from_u8(p);
        }
        a.collision_radius = o.f(P::CollisionRadius).unwrap_or(a.collision_radius);
        a.collision_height = o.f(P::CollisionHeight).unwrap_or(a.collision_height);
        a.collide_actors = o.b(P::bCollideActors).unwrap_or(a.collide_actors);
        a.collide_world = o.b(P::bCollideWorld).unwrap_or(a.collide_world);
        a.block_actors = o.b(P::bBlockActors).unwrap_or(a.block_actors);
        a.block_players = o.b(P::bBlockPlayers).unwrap_or(a.block_players);
        a.block_zero_extent = o.b(P::bBlockZeroExtentTraces).unwrap_or(a.block_zero_extent);
        a.block_nonzero_extent = o.b(P::bBlockNonZeroExtentTraces).unwrap_or(a.block_nonzero_extent);
        a.proj_target = o.b(P::bProjTarget).unwrap_or(a.proj_target);
        a.world_geometry = o.b(P::bWorldGeometry).unwrap_or(a.world_geometry);
        a.is_static = o.b(P::bStatic).unwrap_or(a.is_static);
        a.hidden = o.b(P::bHidden).unwrap_or(a.hidden);
        a.bounce = o.b(P::bBounce).unwrap_or(a.bounce);
        a.mass = o.f(P::Mass).unwrap_or(a.mass);
        a.buoyancy = o.f(P::Buoyancy).unwrap_or(a.buoyancy);
        a.rotate_to_desired = o.b(P::bRotateToDesired).unwrap_or(a.rotate_to_desired);
        a.rotation_rate = o.rot(P::RotationRate).unwrap_or(a.rotation_rate);
        a.fixed_rotation_dir = o.b(P::bFixedRotationDir).unwrap_or(a.fixed_rotation_dir);
        if !a.fixed_rotation_dir {
            a.spin_axis = None;
        }
        a.desired_rotation = o.rot(P::DesiredRotation).unwrap_or(a.desired_rotation);
        if let Some(m) = a.mover.as_mut() {
            // Mover.InterpolateTo / KeyFrameReached run in script: mirror their state.
            m.key_num = o.byte(P::KeyNum).unwrap_or(m.key_num).min(7);
            m.prev_key_num = o.byte(P::PrevKeyNum).unwrap_or(m.prev_key_num).min(7);
            m.phys_alpha = o.f(P::PhysAlpha).unwrap_or(m.phys_alpha);
            m.phys_rate = o.f(P::PhysRate).unwrap_or(m.phys_rate);
            m.move_time = o.f(P::MoveTime).unwrap_or(m.move_time);
            m.interpolating = o.b(P::bInterpolating).unwrap_or(m.interpolating);
            m.old_pos = o.v(P::OldPos).unwrap_or(m.old_pos);
            m.old_rot = o.rot(P::OldRot).unwrap_or(m.old_rot);
            m.base_pos = o.v(P::BasePos).unwrap_or(m.base_pos);
            m.base_rot = o.rot(P::BaseRot).unwrap_or(m.base_rot);
            if let Some(Value::Array(arr)) = o.get(P::KeyPos) {
                for (k, v) in arr.iter().take(8).enumerate() {
                    if let Value::Vector(v) = v {
                        m.key_pos[k] = Vec3::from(*v);
                    }
                }
            }
            if let Some(Value::Array(arr)) = o.get(P::KeyRot) {
                for (k, r) in arr.iter().take(8).enumerate() {
                    if let Value::Rotator(r) = r {
                        m.key_rot[k] = *r;
                    }
                }
            }
        }
        if let Some(p) = a.pawn.as_mut() {
            p.ground_speed = o.f(P::GroundSpeed).unwrap_or(p.ground_speed);
            p.water_speed = o.f(P::WaterSpeed).unwrap_or(p.water_speed);
            p.air_speed = o.f(P::AirSpeed).unwrap_or(p.air_speed);
            p.ladder_speed = o.f(P::LadderSpeed).unwrap_or(p.ladder_speed);
            p.accel_rate = o.f(P::AccelRate).unwrap_or(p.accel_rate);
            p.jump_z = o.f(P::JumpZ).unwrap_or(p.jump_z);
            p.air_control = o.f(P::AirControl).unwrap_or(p.air_control);
            p.walking_pct = o.f(P::WalkingPct).unwrap_or(p.walking_pct);
            p.crouched_pct = o.f(P::CrouchedPct).unwrap_or(p.crouched_pct);
            p.crouch_radius = o.f(P::CrouchRadius).unwrap_or(p.crouch_radius);
            p.crouch_height = o.f(P::CrouchHeight).unwrap_or(p.crouch_height);
            p.base_eye_height = o.f(P::BaseEyeHeight).unwrap_or(p.base_eye_height);
            p.can_walk_off_ledges = o.b(P::bCanWalkOffLedges).unwrap_or(p.can_walk_off_ledges);
            p.avoid_ledges = o.b(P::bAvoidLedges).unwrap_or(p.avoid_ledges);
            p.stop_at_ledges = o.b(P::bStopAtLedges).unwrap_or(p.stop_at_ledges);
            p.can_swim = o.b(P::bCanSwim).unwrap_or(p.can_swim);
            p.can_fly = o.b(P::bCanFly).unwrap_or(p.can_fly);
            p.can_crouch = o.b(P::bCanCrouch).unwrap_or(p.can_crouch);
            p.can_climb_ladders = o.b(P::bCanClimbLadders).unwrap_or(p.can_climb_ladders);
            p.is_walking = o.b(P::bIsWalking).unwrap_or(p.is_walking);
            p.wants_to_crouch = o.b(P::bWantsToCrouch).unwrap_or(p.wants_to_crouch);
            let ctrl = o.obj(P::Controller);
            p.is_player = ctrl.is_some_and(|c| vm.class_is_a(vm.objects[c].class, "PlayerController"));
        }
    }

    /// Copies script-side state of `obj` into its physics actor (relinking if it moved). A
    /// `Base` set by script (the VM's SetBase) is mirrored without events.
    pub fn pull(&mut self, vm: &Vm, world: &mut PhysWorld, obj: ObjId) {
        let Some(id) = self.phys(obj) else { return };
        let (base, owner) = {
            let s = self.slots_of(vm, obj);
            let o = Obj { slots: &vm.objects[obj].slots, s: &s };
            (o.obj(P::Base).and_then(|b| self.phys(b)), o.obj(P::Owner).and_then(|b| self.phys(b)))
        };
        let Some(a) = world.get_mut(id) else { return };
        let before = (a.location, a.rotation, a.collision_radius, a.collision_height);
        let (v_before, phys_before) = (a.velocity, a.physics);
        self.read_into(vm, obj, a);
        if std::env::var_os("P2GAME_JUMP_TRACE").is_some() && a.is_player() && a.physics == Physics::Falling && (phys_before != Physics::Falling || v_before.z != a.velocity.z) {
            println!("PULL {} phys {:?}->{:?} vz {:.0}->{:.0} stack {}", vm.object_name(obj), phys_before, a.physics, v_before.z, a.velocity.z, vm.call_stack_summary());
        }
        // Physics now matches script again (see `pushed_physics`).
        self.pushed_physics.borrow_mut().insert(obj, a.physics as u8);
        if base != Some(id) {
            a.base = base;
        }
        // Script ownership (e.g. a pawn's head, spawned with the pawn as Owner): traces from the
        // owner skip it, so a shooter's instant-hit traces don't hit their own head.
        if owner != Some(id) {
            a.owner = owner;
        }
        let after = (a.location, a.rotation, a.collision_radius, a.collision_height);
        if before != after {
            world.relink(id);
        }
    }

    /// Pulls every bound actor.
    pub fn pull_all(&mut self, vm: &Vm, world: &mut PhysWorld) {
        let objs: Vec<ObjId> = self.to_phys.keys().copied().collect();
        for o in objs {
            if vm.objects.get(o).is_some_and(|x| !x.deleted) {
                self.pull(vm, world, o);
            }
        }
    }

    /// Pulls every bound actor that physics can move (non-static actors and movers), in object
    /// order. Destroyed objects are skipped.
    pub fn pull_dynamic(&mut self, vm: &Vm, world: &mut PhysWorld) {
        let order = match self.order.take() {
            Some(o) => o,
            None => {
                let mut o: Vec<ObjId> = self.to_phys.keys().copied().collect();
                o.sort_unstable();
                o
            }
        };
        for &o in &order {
            let Some(id) = self.phys(o) else { continue };
            if world.get(id).is_some_and(|a| !a.is_static || a.is_mover()) && vm.objects.get(o).is_some_and(|x| !x.deleted) {
                self.pull(vm, world, o);
            }
        }
        self.order = Some(order);
    }

    /// Pushes only the actors physics moved since the last call. Used after a single script call
    /// into physics (SetLocation, Move, ...): pushing every active actor there overwrote state
    /// script had just set on other actors this frame. A dead NPC's blood spout (PersonPawn.Tick
    /// -> SnapSpout -> SetLocation every tick) reset the player's DoJump Velocity/Physics before
    /// physics ever saw them, so jumps and door bumps did nothing while a corpse bled nearby.
    pub fn push_moved(&self, vm: &mut Vm, world: &mut PhysWorld) {
        let mut ids = world.take_moved();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            self.push(vm, world, id);
        }
    }

    /// Pushes the actors physics moved since the last call plus every actor with physics
    /// other than PHYS_None (velocity and state may change without moving).
    pub fn push_changed(&self, vm: &mut Vm, world: &mut PhysWorld) {
        let mut ids = world.take_moved();
        for &id in self.to_obj.keys() {
            // Sleeping actors (idle pawns, resting bodies) have nothing new to push.
            if world.get(id).is_some_and(|a| a.physics != Physics::None && !a.sleeping) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            self.push(vm, world, id);
        }
    }

    /// Copies physics results for `id` back to its VM object: Location, Rotation, Velocity,
    /// Physics, Base, Touching, PhysicsVolume, HeadVolume, Floor and bIsCrouched.
    pub fn push(&self, vm: &mut Vm, world: &PhysWorld, id: ActorId) {
        let (Some(obj), Some(a)) = (self.obj(id), world.get(id)) else { return };
        let s = self.slots_of(vm, obj);
        let script_physics = s.0[P::Physics as usize].and_then(|i| vm.objects[obj].slots.get(i)).map(|v| v.as_int() as u8);
        if std::env::var_os("P2GAME_JUMP_TRACE").is_some() && a.is_player() {
            let sv = s.0[P::Velocity as usize].and_then(|i| vm.objects[obj].slots.get(i)).map(|v| v.as_vec()[2]).unwrap_or(0.0);
            if sv > 100.0 && a.velocity.z < 1.0 {
                println!("PUSHOVER {} script vz {sv:.0} phys {:?} vz {:.0} script phys {script_physics:?} stack {}", vm.object_name(obj), a.physics, a.velocity.z, vm.call_stack_summary());
            }
        }
        let slots = &mut vm.objects[obj].slots;
        let mut set = |p: P, v: Value| {
            if let Some(slot) = s.0[p as usize].and_then(|i| slots.get_mut(i)) {
                *slot = v;
            }
        };
        set(P::Location, vv(a.location));
        set(P::Rotation, Value::Rotator(a.rotation));
        set(P::Velocity, vv(a.velocity));
        {
            let mut last = self.pushed_physics.borrow_mut();
            let changed_by_script = matches!((last.get(&obj), script_physics), (Some(l), Some(cur)) if *l != cur);
            if !changed_by_script {
                set(P::Physics, Value::Byte(a.physics as u8));
                last.insert(obj, a.physics as u8);
            }
        }
        set(P::CollisionRadius, Value::Float(a.collision_radius));
        set(P::CollisionHeight, Value::Float(a.collision_height));
        set(P::Base, Value::Object(a.base.and_then(|b| self.obj(b))));
        set(P::Touching, Value::Array(a.touching.iter().filter_map(|t| self.obj(*t)).map(|o| Value::Object(Some(o))).collect()));
        if let Some(v) = a.physics_volume.and_then(|v| self.obj(v)) {
            set(P::PhysicsVolume, Value::Object(Some(v)));
        }
        if let Some(p) = &a.pawn {
            if let Some(v) = a.head_volume.and_then(|v| self.obj(v)) {
                set(P::HeadVolume, Value::Object(Some(v)));
            }
            set(P::Floor, vv(p.floor));
            set(P::bIsCrouched, Value::Bool(p.is_crouched));
        }
        if let Some(m) = &a.mover {
            set(P::KeyNum, Value::Byte(m.key_num));
            set(P::PrevKeyNum, Value::Byte(m.prev_key_num));
            set(P::PhysAlpha, Value::Float(m.phys_alpha));
            set(P::bInterpolating, Value::Bool(m.interpolating));
        }
    }

    /// Pushes every bound actor.
    pub fn push_all(&self, vm: &mut Vm, world: &PhysWorld) {
        for &id in self.to_obj.keys() {
            self.push(vm, world, id);
        }
    }

    /// Delivers queued physics events to script (pushing state first so script sees it).
    pub fn deliver_events(&self, vm: &mut Vm, world: &mut PhysWorld) {
        let events = world.drain_events();
        if events.is_empty() {
            return;
        }
        self.push_all(vm, world);
        self.deliver(vm, events);
    }

    /// Delivers already-drained events to script. The world is not borrowed, so script run by
    /// the events may call back into a host that owns the world.
    pub fn deliver(&self, vm: &mut Vm, events: Vec<Event>) {
        let o = |id: ActorId| Value::Object(self.obj(id));
        for e in events {
            let Some(target) = self.obj(e.target()) else { continue };
            if vm.objects.get(target).is_none_or(|x| x.deleted) {
                continue;
            }
            let ctrl = vm.get_prop(target, "Controller").and_then(|v| v.as_obj());
            let notify = |vm: &mut Vm, name: &str, args: Vec<Value>| -> bool { ctrl.is_some_and(|c| vm.fire_event(c, name, args).is_some_and(|r| r.as_bool())) };
            match e {
                Event::Touch { other, .. } | Event::UnTouch { other, .. } | Event::Attach { other, .. } | Event::Detach { other, .. } | Event::EncroachedBy { other, .. } | Event::ActorEnteredVolume { other, .. } | Event::ActorLeavingVolume { other, .. } | Event::PawnEnteredVolume { other, .. } | Event::PawnLeavingVolume { other, .. } => {
                    // UE2 never sends these with a None/destroyed Other (an earlier handler in the
                    // batch may have destroyed it: DoorBufferPoint.Touch Accessed None on 42 maps).
                    if self.obj(other).is_none_or(|x| vm.objects.get(x).is_none_or(|x| x.deleted)) {
                        continue;
                    }
                    vm.fire_event(target, e.name(), vec![o(other)]);
                }
                Event::EncroachingOn { other, .. } => {
                    vm.fire_event(target, "EncroachingOn", vec![o(other)]);
                }
                Event::Bump { other, other_velocity, .. } => {
                    // Script reads Other.Velocity in Bump (Window.Bump breaks the glass above
                    // BreakPct of GroundSpeed): show it the speed at the contact, as UE2 calls
                    // Bump inside the move, then restore what physics settled on.
                    let ob = self.obj(other).filter(|&x| vm.objects.get(x).is_some_and(|x| !x.deleted));
                    let saved = ob.and_then(|x| vm.get_prop(x, "Velocity").cloned());
                    if let Some(x) = ob {
                        vm.set_prop(x, "Velocity", vv(other_velocity));
                    }
                    if !notify(vm, "NotifyBump", vec![o(other)]) {
                        vm.fire_event(target, "Bump", vec![o(other)]);
                    }
                    if let (Some(x), Some(s)) = (ob, saved) {
                        if !vm.objects[x].deleted && vm.get_prop(x, "Velocity").is_some_and(|v| *v == vv(other_velocity)) {
                            vm.set_prop(x, "Velocity", s);
                        }
                    }
                }
                Event::HitWall { normal, wall, .. } => {
                    let w = wall.and_then(|w| self.obj(w)).or(vm.level);
                    if !notify(vm, "NotifyHitWall", vec![vv(normal), Value::Object(w)]) {
                        vm.fire_event(target, "HitWall", vec![vv(normal), Value::Object(w)]);
                    }
                }
                Event::Landed { normal, .. } => {
                    if !notify(vm, "NotifyLanded", vec![vv(normal)]) {
                        vm.fire_event(target, "Landed", vec![vv(normal)]);
                    }
                }
                Event::PhysicsVolumeChange { volume, .. } => {
                    let v = Value::Object(volume.and_then(|v| self.obj(v)));
                    if !notify(vm, "NotifyPhysicsVolumeChange", vec![v.clone()]) {
                        vm.fire_event(target, "PhysicsVolumeChange", vec![v]);
                    }
                }
                Event::HeadVolumeChange { volume, .. } => {
                    let v = Value::Object(volume.and_then(|v| self.obj(v)));
                    if !notify(vm, "NotifyHeadVolumeChange", vec![v.clone()]) {
                        vm.fire_event(target, "HeadVolumeChange", vec![v]);
                    }
                }
                Event::MayFall { .. } => {
                    if let Some(c) = ctrl {
                        vm.fire_event(c, "MayFall", vec![]);
                    }
                }
                Event::ZoneChange { zone_export, .. } => {
                    let z = zone_export.and_then(|e| self.zone_objects.get(&e).copied()).or(vm.level);
                    vm.fire_event(target, "ZoneChange", vec![Value::Object(z)]);
                }
                Event::StartCrouch { height_adjust, .. } | Event::EndCrouch { height_adjust, .. } => {
                    vm.fire_event(target, e.name(), vec![Value::Float(height_adjust)]);
                }
                Event::KImpact { other, pos, velocity, normal, .. } => {
                    vm.fire_event(target, "KImpact", vec![Value::Object(other.and_then(|x| self.obj(x))), vv(pos), vv(velocity), vv(normal)]);
                }
                Event::Falling { .. } | Event::BaseChange { .. } | Event::KeyFrameReached { .. } | Event::InterpolateEnd { .. } | Event::EndedRotation { .. } | Event::FellOutOfWorld { .. } => {
                    vm.fire_event(target, e.name(), vec![]);
                }
            }
        }
    }

    /// The VM object a trace hit (the LevelInfo for world geometry).
    pub fn hit_object(&self, vm: &Vm, h: HitActor) -> Option<ObjId> {
        match h {
            HitActor::World => vm.level,
            HitActor::Actor(a) => self.obj(a),
        }
    }

    /// The VM object a trace hit, resolving level static mesh actors in `level_actors`.
    pub fn trace_hit_object(&self, vm: &Vm, h: &crate::TraceHit) -> Option<ObjId> {
        if let Some(o) = h.level_export.and_then(|e| self.level_actors.get(&e).copied()).filter(|&o| !vm.objects[o].deleted) {
            return Some(o);
        }
        self.hit_object(vm, h.actor)
    }

    /// Rows for the collision `foreach` iterators (`TouchingActors`, `TraceActors`,
    /// `RadiusActors`, `VisibleActors`, `VisibleCollidingActors`, `CollidingActors`,
    /// `BasedActors`), in the shape `Host::iterate` returns; None for other iterators. The class
    /// filter (`args[0]`) is applied here.
    pub fn iterate(&self, vm: &mut Vm, world: &mut PhysWorld, this: Option<ObjId>, name: &str, args: &[Value]) -> Option<Vec<Vec<Value>>> {
        let me = this.and_then(|t| self.phys(t));
        let here = this.map(|t| Vec3::from(vm.actor_loc(t))).unwrap_or(Vec3::ZERO);
        let loc = |k: usize| match args.get(k) {
            Some(Value::Vector(v)) if *v != [0.0; 3] => Vec3::from(*v),
            _ => here,
        };
        let f = |k: usize| args.get(k).map(|v| v.as_float()).unwrap_or(0.0);
        let ids: Vec<ActorId>;
        let mut rows_extra: Option<Vec<Vec<Value>>> = None;
        match name.to_ascii_lowercase().as_str() {
            "touchingactors" => ids = me.map(|m| world.touching_actors(m)).unwrap_or_default(),
            "basedactors" => ids = me.map(|m| world.based_actors(m)).unwrap_or_default(),
            "radiusactors" => ids = world.radius_actors(loc(3), f(2)),
            "collidingactors" => ids = world.colliding_actors(loc(3), f(2)),
            "visibleactors" => ids = world.visible_actors(loc(3), f(2)),
            "visiblecollidingactors" => ids = world.visible_colliding_actors(loc(3), f(2), args.get(4).is_some_and(|v| v.as_bool())),
            "traceactors" => {
                let end = v3(args.get(4)).unwrap_or(here);
                let start = loc(5);
                let ext = v3(args.get(6)).unwrap_or(Vec3::ZERO);
                let hits = world.trace_actors(end, start, ext, me);
                ids = Vec::new();
                rows_extra = Some(hits.into_iter().filter_map(|h| Some((self.hit_object(vm, h.actor)?, h))).map(|(o, h)| vec![Value::Object(Some(o)), vv(h.location), vv(h.normal)]).collect());
            }
            _ => return None,
        }
        let base = args.first().and_then(|v| v.as_class());
        let keep = |vm: &Vm, o: ObjId| !vm.objects[o].deleted && base.is_none_or(|b| vm.is_child_of(vm.objects[o].class, b));
        if let Some(rows) = rows_extra {
            return Some(rows.into_iter().filter(|r| r[0].as_obj().is_some_and(|o| keep(vm, o))).collect());
        }
        Some(ids.into_iter().filter_map(|i| self.obj(i)).filter(|&o| keep(vm, o)).map(|o| vec![Value::Object(Some(o))]).collect())
    }
}
