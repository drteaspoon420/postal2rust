//! Level collision for particles (`UseCollision`) through `p2collide`.

use crate::sim::Collider;
use bevy::prelude::*;
use p2collide::{Shape, TraceFlags, World};
use std::sync::Arc;

/// The level a particle system collides with. Insert as a resource (`FxCollision`) to make
/// `UseCollision` emitters bounce off map geometry.
#[derive(Resource, Clone)]
pub struct FxCollision(pub Arc<World>);

impl Collider for World {
    fn trace(&self, start: Vec3, end: Vec3, extent: Vec3) -> Option<(Vec3, Vec3)> {
        if (end - start).length_squared() < 1e-6 {
            return None;
        }
        let shape = Shape::Box(extent);
        let h = World::trace(self, start, end, &shape, TraceFlags::ALL)?;
        if h.start_solid {
            return None;
        }
        Some((h.location, h.normal))
    }
}
