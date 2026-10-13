//! Physics-side state of one actor: the UE2 Actor fields physics reads and writes, plus the
//! per-kind data of pawns, movers and volumes.

use crate::shape::{BrushModel, PlacedBrush};
use crate::types::{ActorId, Physics};
use glam::{Mat3, Vec3};
use p2collide::world::rotator_matrix;
use p2collide::{Aabb, Shape, TriMesh};
use std::sync::Arc;

/// Collision geometry of an actor.
pub enum Collider {
    /// Upright cylinder of `collision_radius` x `collision_height` (pawns, pickups, projectiles,
    /// decorations, and static mesh actors with `bUseCylinderCollision`).
    Cylinder,
    /// Static mesh collision triangles in mesh space, placed at
    /// `location + R(rotation) * diag(scale) * (v - pivot)`.
    Mesh { local: Arc<Vec<[Vec3; 3]>>, scale: Vec3, pivot: Vec3, placed: TriMesh },
    /// A brush Model (volumes, blocking volumes, brush movers), placed at
    /// `location + R(rotation) * diag(scale) * (v - pivot)`.
    Brush { model: Arc<BrushModel>, scale: Vec3, pivot: Vec3, placed: PlacedBrush },
}

impl Collider {
    pub fn mesh(local: Arc<Vec<[Vec3; 3]>>, scale: Vec3, pivot: Vec3) -> Collider {
        Collider::Mesh { local, scale, pivot, placed: TriMesh::default() }
    }

    pub fn brush(model: Arc<BrushModel>, scale: Vec3, pivot: Vec3) -> Collider {
        let placed = PlacedBrush::new(model.clone(), Vec3::ZERO, Mat3::IDENTITY, pivot);
        Collider::Brush { model, scale, pivot, placed }
    }

    /// Re-places mesh and brush geometry for a new transform.
    pub(crate) fn place(&mut self, location: Vec3, rotation: [i32; 3]) {
        match self {
            Collider::Cylinder => {}
            Collider::Mesh { local, scale, pivot, placed } => {
                let m = rotator_matrix(rotation) * Mat3::from_diagonal(*scale);
                *placed = TriMesh::transformed(local, location, m, *pivot);
            }
            Collider::Brush { model, scale, pivot, placed } => {
                let m = rotator_matrix(rotation) * Mat3::from_diagonal(*scale);
                *placed = PlacedBrush::new(model.clone(), location, m, *pivot);
            }
        }
    }

    pub fn is_cylinder(&self) -> bool {
        matches!(self, Collider::Cylinder)
    }
}

/// `PhysicsVolume` settings (also used for the level's default physics volume).
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeInfo {
    /// The actor is a PhysicsVolume (can become an actor's `PhysicsVolume`). Plain volumes
    /// (BlockingVolume, triggers) only touch.
    pub physics_volume: bool,
    pub priority: i32,
    pub gravity: Vec3,
    pub ground_friction: f32,
    pub fluid_friction: f32,
    pub terminal_velocity: f32,
    pub zone_velocity: Vec3,
    pub water: bool,
    pub pain_causing: bool,
    pub damage_per_sec: f32,
    /// LadderVolume: climbing direction (unit) and the direction a climber faces.
    pub ladder: Option<LadderInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LadderInfo {
    pub climb_dir: Vec3,
    pub look_dir: Vec3,
}

impl Default for VolumeInfo {
    /// Shipped `PhysicsVolume` defaults.
    fn default() -> Self {
        VolumeInfo {
            physics_volume: true,
            priority: 0,
            gravity: Vec3::new(0.0, 0.0, -1500.0),
            ground_friction: 8.0,
            fluid_friction: 0.3,
            terminal_velocity: 2500.0,
            zone_velocity: Vec3::ZERO,
            water: false,
            pain_causing: false,
            damage_per_sec: 0.0,
            ladder: None,
        }
    }
}

/// How a mover reacts when it would move into an actor (`Mover.MoverEncroachType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EncroachType {
    #[default]
    Stop,
    Return,
    Crush,
    Ignore,
}

impl EncroachType {
    pub fn from_u8(b: u8) -> EncroachType {
        match b {
            1 => EncroachType::Return,
            2 => EncroachType::Crush,
            3 => EncroachType::Ignore,
            _ => EncroachType::Stop,
        }
    }
}

/// Keyframed mover state (`Mover` fields used by PHYS_MovingBrush).
#[derive(Clone, Debug, PartialEq)]
pub struct MoverInfo {
    pub key_pos: [Vec3; 8],
    pub key_rot: [[i32; 3]; 8],
    pub base_pos: Vec3,
    pub base_rot: [i32; 3],
    pub key_num: u8,
    pub prev_key_num: u8,
    pub num_keys: u8,
    pub move_time: f32,
    /// Interpolation progress 0..1 and rate per second (`PhysAlpha`, `PhysRate`).
    pub phys_alpha: f32,
    pub phys_rate: f32,
    pub old_pos: Vec3,
    pub old_rot: [i32; 3],
    /// `MV_GlideByTime` (smooth start and stop) instead of linear.
    pub glide: bool,
    pub encroach: EncroachType,
    /// `bInterpolating`: the move is in progress.
    pub interpolating: bool,
}

impl Default for MoverInfo {
    fn default() -> Self {
        MoverInfo {
            key_pos: [Vec3::ZERO; 8],
            key_rot: [[0; 3]; 8],
            base_pos: Vec3::ZERO,
            base_rot: [0; 3],
            key_num: 0,
            prev_key_num: 0,
            num_keys: 2,
            move_time: 1.0,
            phys_alpha: 0.0,
            phys_rate: 1.0,
            old_pos: Vec3::ZERO,
            old_rot: [0; 3],
            glide: true,
            encroach: EncroachType::Stop,
            interpolating: false,
        }
    }
}

/// Pawn movement parameters and state.
#[derive(Clone, Debug, PartialEq)]
pub struct PawnInfo {
    pub ground_speed: f32,
    pub water_speed: f32,
    pub air_speed: f32,
    pub ladder_speed: f32,
    pub accel_rate: f32,
    pub jump_z: f32,
    pub air_control: f32,
    pub walking_pct: f32,
    pub crouched_pct: f32,
    pub crouch_radius: f32,
    pub crouch_height: f32,
    pub base_eye_height: f32,
    pub max_step_height: f32,
    pub walkable_floor_z: f32,
    pub can_walk_off_ledges: bool,
    pub avoid_ledges: bool,
    pub stop_at_ledges: bool,
    pub can_swim: bool,
    pub can_fly: bool,
    pub can_crouch: bool,
    pub can_climb_ladders: bool,
    /// Controlled by a player (blocks with `bBlockPlayers`, walks off ledges by default).
    pub is_player: bool,
    pub is_walking: bool,
    pub wants_to_crouch: bool,
    pub is_crouched: bool,
    /// Normal of the floor while walking (`Pawn.Floor`).
    pub floor: Vec3,
    /// `OnLadder`: the LadderVolume being climbed.
    pub on_ladder: Option<ActorId>,
    /// Uncrouched collision size (restored when standing up).
    pub stand_radius: f32,
    pub stand_height: f32,
}

impl Default for PawnInfo {
    /// The shipped `Pawn` defaults (as read for PostalDude in p2collide).
    fn default() -> Self {
        PawnInfo {
            ground_speed: 450.0,
            water_speed: 300.0,
            air_speed: 450.0,
            ladder_speed: 200.0,
            accel_rate: 2048.0,
            jump_z: 550.0,
            air_control: 0.05,
            walking_pct: 0.3,
            crouched_pct: 0.5,
            crouch_radius: 25.0,
            crouch_height: 40.0,
            base_eye_height: 64.0,
            max_step_height: 35.0,
            walkable_floor_z: 0.7,
            can_walk_off_ledges: false,
            avoid_ledges: false,
            stop_at_ledges: false,
            can_swim: true,
            can_fly: false,
            can_crouch: true,
            can_climb_ladders: true,
            is_player: false,
            is_walking: false,
            wants_to_crouch: false,
            is_crouched: false,
            floor: Vec3::Z,
            on_ladder: None,
            stand_radius: 0.0,
            stand_height: 0.0,
        }
    }
}

/// A path for PHYS_Interpolating: points visited in order at `rate` segments per second.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InterpPath {
    pub points: Vec<(Vec3, [i32; 3])>,
    pub rate: f32,
    /// Segment index plus fraction.
    pub pos: f32,
}

/// One actor in the physics world.
pub struct Actor {
    /// Opaque handle of the host's object (e.g. the VM `ObjId`).
    pub user: u64,
    /// Script class name (diagnostics and simple class tests).
    pub class: String,
    pub location: Vec3,
    pub rotation: [i32; 3],
    pub velocity: Vec3,
    pub acceleration: Vec3,
    pub physics: Physics,
    pub collision_radius: f32,
    pub collision_height: f32,
    pub collide_actors: bool,
    pub collide_world: bool,
    pub block_actors: bool,
    pub block_players: bool,
    pub block_zero_extent: bool,
    pub block_nonzero_extent: bool,
    pub proj_target: bool,
    pub world_geometry: bool,
    pub is_static: bool,
    pub hidden: bool,
    pub bounce: bool,
    pub mass: f32,
    pub buoyancy: f32,
    pub rotation_rate: [i32; 3],
    pub desired_rotation: [i32; 3],
    pub rotate_to_desired: bool,
    pub fixed_rotation_dir: bool,
    /// World-space spin axis (unit) for a `bFixedRotationDir` actor: it then turns about this axis
    /// at |RotationRate| instead of by the per-component rate (gibs spinning about the axis the
    /// hit's torque gives; script damping of RotationRate still slows it). Cleared when
    /// `bFixedRotationDir` goes off.
    pub spin_axis: Option<Vec3>,
    /// Can be a base for other actors standing on it (`bCanBeBaseForPawns` for pawns; always for
    /// movers and world geometry).
    pub can_be_base: bool,
    pub owner: Option<ActorId>,
    pub base: Option<ActorId>,
    /// Actors currently touching this one (`Touching`).
    pub touching: Vec<ActorId>,
    pub physics_volume: Option<ActorId>,
    pub head_volume: Option<ActorId>,
    /// BSP zone number (`Region.ZoneNumber`).
    pub zone: usize,
    pub collider: Collider,
    /// Static mesh geometry answering traces for an actor that moves as a cylinder: a KActor
    /// (DT_StaticMesh) is hit by line checks on its mesh in UE2, so a
    /// pistol shot at eye level hits a barrel taller than its Karma cylinder (FIDELITY #27).
    pub trace_collider: Option<Collider>,
    pub volume: Option<VolumeInfo>,
    pub mover: Option<MoverInfo>,
    pub pawn: Option<PawnInfo>,
    pub interp: Option<InterpPath>,
    /// Was the actor encroached/pushed this frame (diagnostics).
    pub pushed: u32,
    /// Resting rigid body (PHYS_Karma): skipped by the tick until its Velocity is set again
    /// (impulse, damage, script) or what it rests on moves.
    pub sleeping: bool,
}

impl Actor {
    /// A collision-less actor of `class` at `location` (enable collision with the flags).
    pub fn new(class: impl Into<String>, location: Vec3) -> Actor {
        Actor {
            user: 0,
            class: class.into(),
            location,
            rotation: [0; 3],
            velocity: Vec3::ZERO,
            acceleration: Vec3::ZERO,
            physics: Physics::None,
            collision_radius: 22.0,
            collision_height: 22.0,
            collide_actors: false,
            collide_world: false,
            block_actors: false,
            block_players: false,
            block_zero_extent: true,
            block_nonzero_extent: true,
            proj_target: false,
            world_geometry: false,
            is_static: false,
            hidden: false,
            bounce: false,
            mass: 100.0,
            buoyancy: 0.0,
            rotation_rate: [0; 3],
            desired_rotation: [0; 3],
            rotate_to_desired: false,
            fixed_rotation_dir: false,
            spin_axis: None,
            can_be_base: true,
            owner: None,
            base: None,
            touching: Vec::new(),
            physics_volume: None,
            head_volume: None,
            zone: 0,
            collider: Collider::Cylinder,
            trace_collider: None,
            volume: None,
            mover: None,
            pawn: None,
            interp: None,
            pushed: 0,
            sleeping: false,
        }
    }

    /// A walking pawn with `info` and its standing cylinder.
    pub fn pawn(class: impl Into<String>, location: Vec3, radius: f32, height: f32, info: PawnInfo) -> Actor {
        let mut a = Actor::new(class, location);
        a.collision_radius = radius;
        a.collision_height = height;
        a.collide_actors = true;
        a.collide_world = true;
        a.block_actors = true;
        a.block_players = true;
        a.proj_target = true;
        a.can_be_base = false;
        a.physics = Physics::Falling;
        a.pawn = Some(PawnInfo { stand_radius: radius, stand_height: height, ..info });
        a
    }

    pub fn is_pawn(&self) -> bool {
        self.pawn.is_some()
    }

    pub fn is_player(&self) -> bool {
        self.pawn.as_ref().is_some_and(|p| p.is_player)
    }

    pub fn is_mover(&self) -> bool {
        self.mover.is_some()
    }

    pub fn is_volume(&self) -> bool {
        self.volume.is_some()
    }

    /// Cylinder shape for sweeps (radius/height clamped above zero).
    pub fn cylinder(&self) -> Shape {
        Shape::Cylinder { radius: self.collision_radius.max(0.01), half_height: self.collision_height.max(0.01) }
    }

    /// World bounds of the collision geometry.
    pub fn bounds(&self) -> Aabb {
        let b = match &self.collider {
            Collider::Cylinder => {
                let e = Vec3::new(self.collision_radius, self.collision_radius, self.collision_height);
                Aabb { min: self.location - e, max: self.location + e }
            }
            Collider::Mesh { placed, .. } => placed.bounds,
            Collider::Brush { placed, .. } => placed.bounds(),
        };
        match &self.trace_collider {
            Some(Collider::Mesh { placed, .. }) if !placed.is_empty() => b.union(placed.bounds),
            _ => b,
        }
    }

    /// Eye position for HeadVolume (`Location + BaseEyeHeight` for pawns).
    pub fn head(&self) -> Vec3 {
        let eye = self.pawn.as_ref().map(|p| if p.is_crouched { 0.8 * p.crouch_height } else { p.base_eye_height }).unwrap_or(0.0);
        self.location + Vec3::Z * eye
    }
}
