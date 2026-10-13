//! A self-contained triangle collider with its own BVH: used for dynamic colliders (movers, brush
//! volumes) that are rebuilt when they move, and for static mesh collision extracted on its own.

use crate::bvh::Bvh;
use crate::geom::{Aabb, Shape, sweep_impl};
use glam::{Mat3, Vec3};

/// Contact of a sweep against a [`TriMesh`].
#[derive(Clone, Copy, Debug)]
pub struct MeshHit {
    pub t: f32,
    pub normal: Vec3,
    pub start_solid: bool,
    pub depth: f32,
}

pub struct TriMesh {
    tris: Vec<[Vec3; 3]>,
    bvh: Bvh,
    pub bounds: Aabb,
}

impl Default for TriMesh {
    fn default() -> Self {
        TriMesh { tris: Vec::new(), bvh: Bvh::default(), bounds: Aabb::EMPTY }
    }
}

impl TriMesh {
    pub fn new(tris: Vec<[Vec3; 3]>) -> TriMesh {
        let bvh = Bvh::build(&tris);
        let bounds = if tris.is_empty() { Aabb::EMPTY } else { bvh.bounds() };
        TriMesh { tris, bvh, bounds }
    }

    /// Transforms local triangles: `world = loc + m * (v - pivot)`.
    pub fn transformed(local: &[[Vec3; 3]], loc: Vec3, m: Mat3, pivot: Vec3) -> TriMesh {
        TriMesh::new(local.iter().map(|t| t.map(|v| loc + m * (v - pivot))).collect())
    }

    pub fn triangles(&self) -> &[[Vec3; 3]] {
        &self.tris
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Sweeps `shape` from `start` to `end`; first contact.
    pub fn trace(&self, start: Vec3, end: Vec3, shape: &Shape) -> Option<MeshHit> {
        if self.tris.is_empty() {
            return None;
        }
        let d = end - start;
        let qb = Aabb::swept(start, end, shape.extent() + Vec3::splat(0.01));
        if !qb.overlaps(&self.bounds) {
            return None;
        }
        let (faces, sedges) = shape.features();
        let mut best: Option<MeshHit> = None;
        self.bvh.query(&qb, |i| {
            if let Some(h) = sweep_impl(start, shape, &faces, &sedges, d, &self.tris[i as usize]) {
                let better = match best {
                    None => true,
                    Some(b) => h.t < b.t || (h.t == b.t && h.start_solid && (!b.start_solid || h.depth > b.depth)),
                };
                if better {
                    best = Some(MeshHit { t: h.t, normal: h.normal, start_solid: h.start_solid, depth: h.depth });
                }
            }
        });
        best
    }

    /// True if `shape` at `p` intersects a triangle (does not detect full containment).
    pub fn overlaps(&self, p: Vec3, shape: &Shape) -> bool {
        self.penetration(p, shape).is_some()
    }

    /// Push-out (normal, depth) for `shape` at `p` if it intersects the mesh surface (a
    /// zero-length sweep reports every overlapping triangle as start-solid).
    pub fn penetration(&self, p: Vec3, shape: &Shape) -> Option<(Vec3, f32)> {
        self.trace(p, p, shape).filter(|h| h.start_solid).map(|h| (h.normal, h.depth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::box_triangles;

    #[test]
    fn box_mesh_blocks_and_overlaps() {
        let m = TriMesh::new(box_triangles(&Aabb { min: Vec3::new(100.0, -50.0, -50.0), max: Vec3::new(120.0, 50.0, 50.0) }));
        let s = Shape::Cylinder { radius: 10.0, half_height: 20.0 };
        let h = m.trace(Vec3::ZERO, Vec3::new(200.0, 0.0, 0.0), &s).unwrap();
        assert!((h.t * 200.0 - 90.0).abs() < 1.0, "t {}", h.t);
        assert!(h.normal.x < -0.9);
        assert!(m.overlaps(Vec3::new(95.0, 0.0, 0.0), &s));
        assert!(!m.overlaps(Vec3::new(80.0, 0.0, 0.0), &s));
    }
}
