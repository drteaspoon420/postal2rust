//! UE2 actor physics for POSTAL 2, independent of Bevy and of the script VM.
//!
//! - [`PhysWorld`] holds the level's static collision (a [`p2collide::World`]) and dynamic
//!   actors ([`Actor`]: cylinders, static mesh colliders, brush volumes) in a spatial hash.
//! - Movement natives: [`PhysWorld::move_by`] (`Move`), [`PhysWorld::move_smooth`]
//!   (`MoveSmooth`), [`PhysWorld::set_location`], [`PhysWorld::set_rotation`],
//!   [`PhysWorld::set_collision`], [`PhysWorld::set_collision_size`], [`PhysWorld::set_base`],
//!   [`PhysWorld::set_physics`].
//! - Physics modes per tick ([`PhysWorld::tick`]): walking, falling, swimming, flying, ladder,
//!   projectile, rotating, interpolating, moving brush (movers) and a Karma stand-in.
//! - Trace natives: [`PhysWorld::trace`], [`PhysWorld::fast_trace`], [`PhysWorld::trace_actors`],
//!   [`PhysWorld::visible_actors`], [`PhysWorld::colliding_actors`], [`PhysWorld::radius_actors`].
//! - Script events (Touch, UnTouch, Bump, HitWall, Landed, Falling, BaseChange,
//!   EncroachingOn/EncroachedBy, PhysicsVolumeChange, ZoneChange, HeadVolumeChange, ...) are
//!   queued as [`Event`]s for the host to deliver ([`PhysWorld::drain_events`]).
//! - [`load::LoadedMap`] builds the world for a map: level geometry, movers and volumes.
//!
//! Units are Unreal units, axes are Unreal (X forward, Y right, Z up), rotators are
//! (pitch, yaw, roll) in 65536ths of a turn.

pub mod actor;
pub mod hash;
pub mod load;
pub mod modes;
pub mod shape;
pub mod types;
#[cfg(feature = "vm")]
pub mod vm;
pub mod world;

pub use actor::{Actor, Collider, EncroachType, InterpPath, LadderInfo, MoverInfo, PawnInfo, VolumeInfo};
pub use load::LoadedMap;
pub use types::{ActorId, Event, HitActor, MoveHit, Physics, TraceHit};
#[cfg(feature = "vm")]
pub use vm::VmBridge;
pub use world::{MoveResult, PhysWorld};
