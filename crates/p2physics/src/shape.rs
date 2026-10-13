//! Collision primitives used between actors: upright cylinders (pawns, pickups, projectiles,
//! decorations), triangle meshes (static mesh movers and blocking static mesh actors) and brushes
//! (volumes and blocking volumes: a brush Model with its own BSP for point containment plus its
//! polygons for surface overlap).

use glam::{Mat3, Vec3};
use p2collide::bsp::Bsp;
use p2collide::{Aabb, Shape, TriMesh};
use std::sync::Arc;

/// Contact of a swept shape against one actor collider.
#[derive(Clone, Copy, Debug)]
pub struct Contact {
    pub t: f32,
    pub normal: Vec3,
    pub start_solid: bool,
    pub depth: f32,
}

/// Swept upright cylinder (or point when `r` and `h` are zero) from `p` by `d` against a static
/// upright cylinder at `c`. Both are treated as their Minkowski sum: a point against a cylinder of
/// radius `r + cr` and half height `h + ch`, which is exact for two upright cylinders.
pub fn sweep_cylinder(p: Vec3, d: Vec3, r: f32, h: f32, c: Vec3, cr: f32, ch: f32) -> Option<Contact> {
    let rr = r + cr;
    let hh = h + ch;
    let o = p - c;
    let inside_xy = o.x * o.x + o.y * o.y < rr * rr;
    let inside_z = o.z.abs() < hh;
    if inside_xy && inside_z {
        // Overlapping at the start: push out through the nearest side.
        let horiz = rr - (o.x * o.x + o.y * o.y).sqrt();
        let vert = hh - o.z.abs();
        let (n, depth) = if vert < horiz {
            (Vec3::Z * o.z.signum().max(0.0) * 2.0 - Vec3::Z, vert)
        } else {
            let n2 = Vec3::new(o.x, o.y, 0.0).normalize_or(Vec3::X);
            (n2, horiz)
        };
        if d.dot(n) > 0.0 {
            return None; // moving out
        }
        return Some(Contact { t: 0.0, normal: n, start_solid: true, depth });
    }
    // Slab interval in Z.
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    let mut n_enter = Vec3::ZERO;
    if d.z.abs() < 1e-9 {
        if !inside_z {
            return None;
        }
    } else {
        let a = (-hh - o.z) / d.z;
        let b = (hh - o.z) / d.z;
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        if lo > t0 {
            t0 = lo;
            n_enter = if d.z > 0.0 { -Vec3::Z } else { Vec3::Z };
        }
        t1 = t1.min(hi);
    }
    // Circle interval in XY.
    let a = d.x * d.x + d.y * d.y;
    let b = 2.0 * (o.x * d.x + o.y * d.y);
    let cc = o.x * o.x + o.y * o.y - rr * rr;
    if a < 1e-12 {
        if cc >= 0.0 {
            return None;
        }
    } else {
        let disc = b * b - 4.0 * a * cc;
        if disc < 0.0 {
            return None;
        }
        let s = disc.sqrt();
        let lo = (-b - s) / (2.0 * a);
        let hi = (-b + s) / (2.0 * a);
        if lo > t0 {
            t0 = lo;
            let q = o + d * lo;
            n_enter = Vec3::new(q.x, q.y, 0.0).normalize_or(Vec3::X);
        }
        t1 = t1.min(hi);
    }
    if t0 > t1 || t0 > 1.0 || t1 < 0.0 || n_enter == Vec3::ZERO {
        return None;
    }
    Some(Contact { t: t0.max(0.0), normal: n_enter, start_solid: false, depth: 0.0 })
}

/// True if two upright cylinders overlap (touching within `slop` counts).
pub fn cylinders_overlap(a: Vec3, ar: f32, ah: f32, b: Vec3, br: f32, bh: f32, slop: f32) -> bool {
    let o = a - b;
    let rr = ar + br + slop;
    o.x * o.x + o.y * o.y <= rr * rr && o.z.abs() <= ah + bh + slop
}

/// A brush in local space (shared by every actor using the same Model).
pub struct BrushModel {
    pub bsp: Bsp,
    /// Polygon triangles in brush space.
    pub tris: Vec<[Vec3; 3]>,
    pub local_bounds: Aabb,
}

impl BrushModel {
    pub fn from_model(m: &upkg::model::Model) -> BrushModel {
        let mut tris = Vec::new();
        for n in &m.nodes {
            if n.num_verts < 3 {
                continue;
            }
            let poly: Vec<Vec3> = m.node_polygon(n).map(Vec3::from).collect();
            for k in 1..poly.len() - 1 {
                tris.push([poly[0], poly[k], poly[k + 1]]);
            }
        }
        let pts: Vec<Vec3> = m.points.iter().map(|&p| Vec3::from(p)).collect();
        BrushModel { bsp: Bsp::new(m), tris, local_bounds: Aabb::from_points(&pts) }
    }

    /// An axis-aligned box brush (tests and script-spawned volumes).
    pub fn aabb(min: Vec3, max: Vec3) -> BrushModel {
        let b = Aabb { min, max };
        BrushModel { bsp: Bsp::empty(), tris: p2collide::geom::box_triangles(&b), local_bounds: b }
    }

    fn contains_local(&self, p: Vec3) -> bool {
        if self.bsp.is_empty() {
            // Box brush.
            return p.cmpge(self.local_bounds.min).all() && p.cmple(self.local_bounds.max).all();
        }
        p.cmpge(self.local_bounds.min - 0.01).all() && p.cmple(self.local_bounds.max + 0.01).all() && self.bsp.point_solid(p)
    }
}

/// A brush placed in the world: `world = location + m * (local - pivot)`.
pub struct PlacedBrush {
    pub model: Arc<BrushModel>,
    pub location: Vec3,
    pub m: Mat3,
    pub pivot: Vec3,
    inv: Mat3,
    pub mesh: TriMesh,
}

impl PlacedBrush {
    pub fn new(model: Arc<BrushModel>, location: Vec3, m: Mat3, pivot: Vec3) -> PlacedBrush {
        let mesh = TriMesh::transformed(&model.tris, location, m, pivot);
        PlacedBrush { inv: m.inverse(), model, location, m, pivot, mesh }
    }

    pub fn bounds(&self) -> Aabb {
        self.mesh.bounds
    }

    pub fn contains_point(&self, p: Vec3) -> bool {
        if !self.mesh.bounds.expand(Vec3::splat(0.01)).overlaps(&Aabb { min: p, max: p }) {
            return false;
        }
        self.model.contains_local(self.inv * (p - self.location) + self.pivot)
    }

    /// True if an upright cylinder at `p` touches the brush: its centre is inside, or the
    /// cylinder intersects a brush polygon.
    pub fn overlaps_cylinder(&self, p: Vec3, r: f32, h: f32) -> bool {
        let shape = Shape::Cylinder { radius: r.max(0.01), half_height: h.max(0.01) };
        if !self.mesh.bounds.overlaps(&Aabb { min: p - shape.extent(), max: p + shape.extent() }) {
            return false;
        }
        self.contains_point(p) || self.mesh.overlaps(p, &shape)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cylinder_sweep_side_and_top() {
        // Approach from -X: contact when centres are r1 + r2 apart.
        let c = sweep_cylinder(Vec3::new(-100.0, 0.0, 0.0), Vec3::new(200.0, 0.0, 0.0), 10.0, 20.0, Vec3::ZERO, 15.0, 30.0).unwrap();
        assert!((c.t * 200.0 - 75.0).abs() < 1e-3, "t {}", c.t);
        assert!((c.normal - -Vec3::X).length() < 1e-4);
        // Fall onto the top.
        let c = sweep_cylinder(Vec3::new(5.0, 0.0, 100.0), Vec3::new(0.0, 0.0, -100.0), 10.0, 20.0, Vec3::ZERO, 15.0, 30.0).unwrap();
        assert!((c.t * 100.0 - 50.0).abs() < 1e-3);
        assert_eq!(c.normal, Vec3::Z);
        // Miss to the side.
        assert!(sweep_cylinder(Vec3::new(-100.0, 30.0, 0.0), Vec3::new(200.0, 0.0, 0.0), 10.0, 20.0, Vec3::ZERO, 15.0, 30.0).is_none());
        // Start overlapping, moving away: no hit; moving in: start solid.
        assert!(sweep_cylinder(Vec3::new(-20.0, 0.0, 0.0), Vec3::new(-10.0, 0.0, 0.0), 10.0, 20.0, Vec3::ZERO, 15.0, 30.0).is_none());
        let c = sweep_cylinder(Vec3::new(-20.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0), 10.0, 20.0, Vec3::ZERO, 15.0, 30.0).unwrap();
        assert!(c.start_solid && (c.depth - 5.0).abs() < 1e-3);
    }

    #[test]
    fn box_brush_contains() {
        let b = PlacedBrush::new(Arc::new(BrushModel::aabb(Vec3::splat(-50.0), Vec3::splat(50.0))), Vec3::new(1000.0, 0.0, 0.0), Mat3::IDENTITY, Vec3::ZERO);
        assert!(b.contains_point(Vec3::new(1010.0, 0.0, 0.0)));
        assert!(!b.contains_point(Vec3::new(900.0, 0.0, 0.0)));
        assert!(b.overlaps_cylinder(Vec3::new(1060.0, 0.0, 0.0), 20.0, 20.0));
        assert!(!b.overlaps_cylinder(Vec3::new(1080.0, 0.0, 0.0), 20.0, 20.0));
    }
}
