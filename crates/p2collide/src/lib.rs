//! Collision for POSTAL 2 maps, read from the user's own install.
//!
//! - [`world::World`] gathers the map's collision geometry: BSP polygons (non-solid brushes
//!   skipped), the collision triangles of every blocking static mesh actor (flags from the map
//!   and the original class defaults) and terrain heightfields.
//! - [`world::World::trace`] sweeps a point, box or upright cylinder ([`geom::Shape`]) and
//!   returns the first contact. Triangle tests are exact swept SAT ([`geom`]); a BVH prunes them.
//! - [`bsp::Bsp`] classifies points and segments against the BSP tree (solid/empty).
//! - [`walker::Walker`] is a UE2-style walking pawn (step-up, sliding, falling, jump, crouch).
//!
//! Units are Unreal units with Unreal axes (X forward, Y right, Z up).

pub mod bsp;
pub mod bvh;
pub mod defaults;
pub mod geom;
pub mod heightfield;
pub mod trimesh;
pub mod walker;
pub mod world;

pub use defaults::PawnParams;
pub use geom::{Aabb, Shape};
pub use trimesh::{MeshHit, TriMesh};
pub use walker::{MoveInput, Physics, Walker};
pub use world::{BuildOptions, Hit, Source, TraceFlags, World};
