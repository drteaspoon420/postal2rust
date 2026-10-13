//! Shared types: actor ids, the UE2 `EPhysics` enum, hit results and physics events.

use glam::Vec3;

/// Index of an actor in a [`crate::PhysWorld`]. Slots are reused after removal.
pub type ActorId = u32;

/// UE2 `Actor.EPhysics`, in script enum order (the byte stored in `Actor.Physics`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Physics {
    #[default]
    None = 0,
    Walking,
    Falling,
    Swimming,
    Flying,
    Rotating,
    Projectile,
    Interpolating,
    MovingBrush,
    Spider,
    Trailer,
    Ladder,
    RootMotion,
    Karma,
    KarmaRagDoll,
}

impl Physics {
    pub fn from_u8(b: u8) -> Physics {
        use Physics::*;
        const ALL: [Physics; 15] = [None, Walking, Falling, Swimming, Flying, Rotating, Projectile, Interpolating, MovingBrush, Spider, Trailer, Ladder, RootMotion, Karma, KarmaRagDoll];
        ALL.get(b as usize).copied().unwrap_or(None)
    }
}

/// What a trace or move hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HitActor {
    /// Level geometry (BSP, static meshes baked into the level, terrain). Script sees the LevelInfo.
    World,
    Actor(ActorId),
}

impl HitActor {
    pub fn actor(self) -> Option<ActorId> {
        match self {
            HitActor::Actor(a) => Some(a),
            HitActor::World => None,
        }
    }
}

/// Result of a trace native.
#[derive(Clone, Copy, Debug)]
pub struct TraceHit {
    pub actor: HitActor,
    /// Fraction of the trace at contact.
    pub t: f32,
    /// Trace shape centre at contact (`HitLocation`).
    pub location: Vec3,
    pub normal: Vec3,
    /// Level material hit (index into the collision world's material table), if known.
    pub material: Option<u32>,
    /// Map export of the level static mesh actor hit (`Source::Actor`), if that is what was hit:
    /// in UE2 `Trace` returns that StaticMeshActor, not the LevelInfo.
    pub level_export: Option<u32>,
}

/// Contact during a sweep of an actor.
#[derive(Clone, Copy, Debug)]
pub struct MoveHit {
    pub actor: HitActor,
    pub t: f32,
    pub normal: Vec3,
    pub start_solid: bool,
    pub depth: f32,
}

/// Script events produced by physics, in the order they happened. The host delivers them to
/// script after the native or tick that produced them returns (see [`crate::PhysWorld::drain_events`]).
/// Each event is called on `actor`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Touch { actor: ActorId, other: ActorId },
    UnTouch { actor: ActorId, other: ActorId },
    /// `Bump(Other)`. `other_velocity`: Other's Velocity at the contact, before the blocked move
    /// cut it (UE2 sends Bump mid-move, so Window.Bump sees the speed the pawn hit the glass at).
    Bump { actor: ActorId, other: ActorId, other_velocity: Vec3 },
    /// `HitWall(HitNormal, Wall)`; `wall` None means the level.
    HitWall { actor: ActorId, normal: Vec3, wall: Option<ActorId> },
    Landed { actor: ActorId, normal: Vec3 },
    Falling { actor: ActorId },
    /// Controller event for pawns about to walk off a ledge they may not leave.
    MayFall { actor: ActorId },
    BaseChange { actor: ActorId },
    /// `Attach(Other)` on the new base.
    Attach { actor: ActorId, other: ActorId },
    /// `Detach(Other)` on the old base.
    Detach { actor: ActorId, other: ActorId },
    EncroachingOn { actor: ActorId, other: ActorId },
    EncroachedBy { actor: ActorId, other: ActorId },
    PhysicsVolumeChange { actor: ActorId, volume: Option<ActorId> },
    HeadVolumeChange { actor: ActorId, volume: Option<ActorId> },
    /// Called on the volume.
    ActorEnteredVolume { actor: ActorId, other: ActorId },
    ActorLeavingVolume { actor: ActorId, other: ActorId },
    PawnEnteredVolume { actor: ActorId, other: ActorId },
    PawnLeavingVolume { actor: ActorId, other: ActorId },
    /// `ZoneChange(NewZone)`: `zone` is the BSP zone number; `zone_export` the zone actor's map
    /// export (None = the LevelInfo).
    ZoneChange { actor: ActorId, zone: usize, zone_export: Option<u32> },
    /// Mover reached its destination keyframe (PHYS_MovingBrush ends).
    KeyFrameReached { actor: ActorId },
    /// PHYS_Interpolating reached the end of its path.
    InterpolateEnd { actor: ActorId },
    /// `bRotateToDesired` rotation finished.
    EndedRotation { actor: ActorId },
    StartCrouch { actor: ActorId, height_adjust: f32 },
    EndCrouch { actor: ActorId, height_adjust: f32 },
    /// Karma body impact above its threshold.
    KImpact { actor: ActorId, other: Option<ActorId>, pos: Vec3, velocity: Vec3, normal: Vec3 },
    FellOutOfWorld { actor: ActorId },
}

impl Event {
    /// The actor the event is called on.
    pub fn target(&self) -> ActorId {
        use Event::*;
        match *self {
            Touch { actor, .. } | UnTouch { actor, .. } | Bump { actor, .. } | HitWall { actor, .. } | Landed { actor, .. } | Falling { actor } | MayFall { actor } | BaseChange { actor } | Attach { actor, .. } | Detach { actor, .. } | EncroachingOn { actor, .. } | EncroachedBy { actor, .. } | PhysicsVolumeChange { actor, .. } | HeadVolumeChange { actor, .. } | ActorEnteredVolume { actor, .. } | ActorLeavingVolume { actor, .. } | PawnEnteredVolume { actor, .. } | PawnLeavingVolume { actor, .. } | ZoneChange { actor, .. } | KeyFrameReached { actor } | InterpolateEnd { actor } | EndedRotation { actor } | StartCrouch { actor, .. } | EndCrouch { actor, .. } | KImpact { actor, .. } | FellOutOfWorld { actor } => actor,
        }
    }

    /// Script event name.
    pub fn name(&self) -> &'static str {
        use Event::*;
        match self {
            Touch { .. } => "Touch",
            UnTouch { .. } => "UnTouch",
            Bump { .. } => "Bump",
            HitWall { .. } => "HitWall",
            Landed { .. } => "Landed",
            Falling { .. } => "Falling",
            MayFall { .. } => "MayFall",
            BaseChange { .. } => "BaseChange",
            Attach { .. } => "Attach",
            Detach { .. } => "Detach",
            EncroachingOn { .. } => "EncroachingOn",
            EncroachedBy { .. } => "EncroachedBy",
            PhysicsVolumeChange { .. } => "PhysicsVolumeChange",
            HeadVolumeChange { .. } => "HeadVolumeChange",
            ActorEnteredVolume { .. } => "ActorEnteredVolume",
            ActorLeavingVolume { .. } => "ActorLeavingVolume",
            PawnEnteredVolume { .. } => "PawnEnteredVolume",
            PawnLeavingVolume { .. } => "PawnLeavingVolume",
            ZoneChange { .. } => "ZoneChange",
            KeyFrameReached { .. } => "KeyFrameReached",
            InterpolateEnd { .. } => "InterpolateEnd",
            EndedRotation { .. } => "EndedRotation",
            StartCrouch { .. } => "StartCrouch",
            EndCrouch { .. } => "EndCrouch",
            KImpact { .. } => "KImpact",
            FellOutOfWorld { .. } => "FellOutOfWorld",
        }
    }
}

/// Rotator helpers (pitch, yaw, roll in 65536ths of a turn).
pub mod rot {
    /// Signed shortest difference `to - from` in 16-bit angle units.
    pub fn delta(from: i32, to: i32) -> i32 {
        ((to - from) as i16) as i32
    }

    /// Steps `cur` toward `target` by at most `rate` (all in angle units), shortest way.
    pub fn approach(cur: i32, target: i32, rate: i32) -> i32 {
        let d = delta(cur, target);
        if d.abs() <= rate.abs() { target } else { cur + rate.abs() * d.signum() }
    }

    const K: f32 = std::f32::consts::TAU / 65536.0;

    /// Orientation of a rotator as a quaternion whose matrix columns are the rotator's X, Y, Z
    /// axes (UE2 GetAxes).
    pub fn to_quat(r: [i32; 3]) -> glam::Quat {
        let (sp, cp) = (r[0] as f32 * K).sin_cos();
        let (sy, cy) = (r[1] as f32 * K).sin_cos();
        let (sr, cr) = (r[2] as f32 * K).sin_cos();
        let x = glam::Vec3::new(cp * cy, cp * sy, sp);
        let y = glam::Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
        let z = glam::Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp);
        glam::Quat::from_mat3(&glam::Mat3::from_cols(x, y, z)).normalize()
    }

    /// Inverse of [`to_quat`] (angles wrapped to 0..65535).
    pub fn from_quat(q: glam::Quat) -> [i32; 3] {
        let (x, y) = (q * glam::Vec3::X, q * glam::Vec3::Y);
        let pitch = x.z.clamp(-1.0, 1.0).asin();
        let yaw = x.y.atan2(x.x);
        let (sp, cp) = pitch.sin_cos();
        let (sy, cy) = yaw.sin_cos();
        let y0 = glam::Vec3::new(-sy, cy, 0.0);
        let w = glam::Vec3::new(sp * cy, sp * sy, -cp);
        let roll = y.dot(w).atan2(y.dot(y0));
        [pitch, yaw, roll].map(|a| ((a / K).round() as i32) & 0xffff)
    }

    /// Unit direction of a rotator (X forward).
    pub fn dir(r: [i32; 3]) -> glam::Vec3 {
        let k = std::f32::consts::TAU / 65536.0;
        let (sp, cp) = (r[0] as f32 * k).sin_cos();
        let (sy, cy) = (r[1] as f32 * k).sin_cos();
        glam::Vec3::new(cp * cy, cp * sy, sp)
    }
}
