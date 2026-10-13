//! Geometry primitives: axis-aligned boxes and an exact swept box versus triangle test.
//!
//! The swept test uses the separating axis theorem over time: for a convex shape translating
//! linearly against a static triangle, the set of times at which they overlap is the intersection
//! of the per-axis overlap intervals. The axis set is the triangle normal, the shape's face
//! normals, every shape-edge x triangle-edge product and the three in-plane edge normals (the
//! last three keep the test exact when the shape has zero extent, i.e. for line traces).
//!
//! Shapes: an axis-aligned box (what UE2 uses for actor-vs-world traces, extent = radius,
//! radius, half height) and an upright cylinder approximated by a regular prism whose polygon
//! circumscribes the circle.

use glam::Vec3;

/// Number of sides of the prism that stands in for a collision cylinder.
pub const CYL_SIDES: usize = 16;

/// A convex collision shape centred on the trace position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// Axis-aligned box with the given half extent (zero = a line trace).
    Box(Vec3),
    /// Upright cylinder.
    Cylinder { radius: f32, half_height: f32 },
}

impl Shape {
    pub const POINT: Shape = Shape::Box(Vec3::ZERO);

    /// Half extent of the shape's bounding box.
    pub fn extent(&self) -> Vec3 {
        match *self {
            Shape::Box(e) => e,
            Shape::Cylinder { radius, half_height } => Vec3::new(radius / cyl_cos(), radius / cyl_cos(), half_height),
        }
    }

    /// Half width of the shape projected on unit axis `a`.
    fn radius_along(&self, a: Vec3) -> f32 {
        match *self {
            Shape::Box(e) => e.x * a.x.abs() + e.y * a.y.abs() + e.z * a.z.abs(),
            Shape::Cylinder { radius, half_height } => {
                let r = radius / cyl_cos();
                let m = cyl_dirs(0.0).map(|(c, s)| (a.x * c + a.y * s).abs()).into_iter().fold(0.0f32, f32::max);
                half_height * a.z.abs() + r * m
            }
        }
    }

    /// Face normals and edge directions (axes; sign does not matter).
    pub(crate) fn features(&self) -> (Vec<Vec3>, Vec<Vec3>) {
        match self {
            Shape::Box(_) => (vec![Vec3::X, Vec3::Y, Vec3::Z], vec![Vec3::X, Vec3::Y, Vec3::Z]),
            Shape::Cylinder { .. } => {
                let half = std::f32::consts::PI / CYL_SIDES as f32;
                // Side normals point between vertices; ring edges run perpendicular to them.
                let mut faces = vec![Vec3::Z];
                let mut edges = vec![Vec3::Z];
                for (c, s) in cyl_dirs(half).into_iter().take(CYL_SIDES / 2) {
                    faces.push(Vec3::new(c, s, 0.0));
                    edges.push(Vec3::new(-s, c, 0.0));
                }
                (faces, edges)
            }
        }
    }
}

fn cyl_cos() -> f32 {
    (std::f32::consts::PI / CYL_SIDES as f32).cos()
}

/// (cos, sin) of the prism vertex directions, rotated by `offset`.
fn cyl_dirs(offset: f32) -> [(f32, f32); CYL_SIDES] {
    std::array::from_fn(|k| {
        let t = offset + k as f32 * std::f32::consts::TAU / CYL_SIDES as f32;
        (t.cos(), t.sin())
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb { min: Vec3::splat(f32::INFINITY), max: Vec3::splat(f32::NEG_INFINITY) };

    pub fn from_points(pts: &[Vec3]) -> Self {
        let mut b = Self::EMPTY;
        for p in pts {
            b.grow(*p);
        }
        b
    }

    pub fn grow(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(self, o: Aabb) -> Aabb {
        Aabb { min: self.min.min(o.min), max: self.max.max(o.max) }
    }

    pub fn overlaps(&self, o: &Aabb) -> bool {
        self.min.cmple(o.max).all() && self.max.cmpge(o.min).all()
    }

    pub fn expand(self, e: Vec3) -> Aabb {
        Aabb { min: self.min - e, max: self.max + e }
    }

    pub fn is_valid(&self) -> bool {
        self.min.cmple(self.max).all()
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Bounds of a box of half extent `e` swept from `a` to `b`.
    pub fn swept(a: Vec3, b: Vec3, e: Vec3) -> Aabb {
        Aabb { min: a.min(b) - e, max: a.max(b) + e }
    }
}

/// Result of a swept box against one triangle.
#[derive(Clone, Copy, Debug)]
pub struct TriHit {
    /// Fraction of the motion at first contact (0 when the box starts overlapping).
    pub t: f32,
    /// Contact normal, pointing from the triangle toward the box.
    pub normal: Vec3,
    /// The box already overlapped the triangle at t = 0.
    pub start_solid: bool,
    /// For start-solid hits: distance to push the box along `normal` to separate.
    pub depth: f32,
}

/// Sweeps a box (centre `c`, half extent `e`) by `d` against triangle `tri`.
pub fn sweep_box_tri(c: Vec3, e: Vec3, d: Vec3, tri: &[Vec3; 3]) -> Option<TriHit> {
    sweep_shape_tri(c, &Shape::Box(e), d, tri)
}

/// Sweeps `shape` (centred at `c`) by `d` against triangle `tri`.
pub fn sweep_shape_tri(c: Vec3, shape: &Shape, d: Vec3, tri: &[Vec3; 3]) -> Option<TriHit> {
    let (faces, sedges) = shape.features();
    sweep_impl(c, shape, &faces, &sedges, d, tri)
}

/// Same as [`sweep_shape_tri`] with the shape features precomputed (hot loop).
pub(crate) fn sweep_impl(c: Vec3, shape: &Shape, faces: &[Vec3], sedges: &[Vec3], d: Vec3, tri: &[Vec3; 3]) -> Option<TriHit> {
    let v = [tri[0] - c, tri[1] - c, tri[2] - c];
    let edges = [v[1] - v[0], v[2] - v[1], v[0] - v[2]];
    let n = edges[0].cross(edges[1]);
    if n.length_squared() < 1e-10 {
        return None;
    }
    let mut enter = f32::NEG_INFINITY;
    let mut exit = f32::INFINITY;
    let mut enter_n = Vec3::ZERO;
    let mut min_pen = f32::INFINITY;
    let mut pen_n = Vec3::ZERO;

    // Returns false as soon as the axis proves the pair never overlaps during the sweep.
    let mut axis = |a: Vec3| -> bool {
        let l2 = a.length_squared();
        if l2 < 1e-12 {
            return true;
        }
        let a = a / l2.sqrt();
        let (p0, p1, p2) = (v[0].dot(a), v[1].dot(a), v[2].dot(a));
        let tmin = p0.min(p1).min(p2);
        let tmax = p0.max(p1).max(p2);
        let r = shape.radius_along(a);
        let s = d.dot(a);
        // Penetration at t = 0 if overlapping: push along -a by (r - tmin) or along +a by (tmax + r).
        let (o_pos, o_neg) = (r - tmin, tmax + r);
        if o_pos < o_neg {
            if o_pos < min_pen {
                min_pen = o_pos;
                pen_n = -a;
            }
        } else if o_neg < min_pen {
            min_pen = o_neg;
            pen_n = a;
        }
        if s.abs() < 1e-9 {
            return tmin <= r && tmax >= -r;
        }
        let t1 = (tmin - r) / s;
        let t2 = (tmax + r) / s;
        let (te, tx) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
        if te > enter {
            enter = te;
            enter_n = if s > 0.0 { -a } else { a };
        }
        exit = exit.min(tx);
        enter <= exit && enter <= 1.0 && exit >= 0.0
    };

    if !axis(n) {
        return None;
    }
    for b in faces {
        if !axis(*b) {
            return None;
        }
    }
    for ed in &edges {
        for b in sedges {
            if !axis(ed.cross(*b)) {
                return None;
            }
        }
        if !axis(n.cross(*ed)) {
            return None;
        }
    }
    if enter > exit || enter > 1.0 || exit < 0.0 {
        return None;
    }
    if enter < 0.0 {
        // Overlapping at the start. Moving out along the push-out direction is allowed.
        if d.dot(pen_n) > 0.0 {
            return None;
        }
        return Some(TriHit { t: 0.0, normal: pen_n, start_solid: true, depth: min_pen.max(0.0) });
    }
    Some(TriHit { t: enter, normal: enter_n, start_solid: false, depth: 0.0 })
}

/// The 12 triangles of an axis-aligned box.
pub fn box_triangles(b: &Aabb) -> Vec<[Vec3; 3]> {
    let c = |i: u32| Vec3::new(if i & 1 != 0 { b.max.x } else { b.min.x }, if i & 2 != 0 { b.max.y } else { b.min.y }, if i & 4 != 0 { b.max.z } else { b.min.z });
    let faces = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
    let mut out = Vec::with_capacity(12);
    for f in faces {
        out.push([c(f[0]), c(f[1]), c(f[2])]);
        out.push([c(f[0]), c(f[2]), c(f[3])]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor() -> [Vec3; 3] {
        [Vec3::new(-100.0, -100.0, 0.0), Vec3::new(100.0, -100.0, 0.0), Vec3::new(0.0, 100.0, 0.0)]
    }

    #[test]
    fn box_falls_onto_floor() {
        let h = sweep_box_tri(Vec3::new(0.0, 0.0, 100.0), Vec3::new(10.0, 10.0, 20.0), Vec3::new(0.0, 0.0, -200.0), &floor()).unwrap();
        assert!(!h.start_solid);
        assert!((h.t - 0.4).abs() < 1e-4, "t={}", h.t);
        assert!((h.normal - Vec3::Z).length() < 1e-4);
    }

    #[test]
    fn line_misses_outside_edge() {
        // A point passing just outside the triangle's slanted edge must not hit.
        let h = sweep_box_tri(Vec3::new(60.0, 10.0, 10.0), Vec3::ZERO, Vec3::new(0.0, 0.0, -20.0), &floor());
        assert!(h.is_none());
        let h = sweep_box_tri(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, Vec3::new(0.0, 0.0, -20.0), &floor()).unwrap();
        assert!((h.t - 0.5).abs() < 1e-4);
    }

    #[test]
    fn start_solid_reports_push_out() {
        let h = sweep_box_tri(Vec3::new(0.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 20.0), Vec3::new(0.0, 0.0, -1.0), &floor()).unwrap();
        assert!(h.start_solid);
        assert!((h.depth - 15.0).abs() < 1e-3);
        assert!((h.normal - Vec3::Z).length() < 1e-4);
        // Moving away is allowed.
        assert!(sweep_box_tri(Vec3::new(0.0, 0.0, 5.0), Vec3::new(10.0, 10.0, 20.0), Vec3::new(0.0, 0.0, 30.0), &floor()).is_none());
    }

    #[test]
    fn sliding_parallel_does_not_hit() {
        let h = sweep_box_tri(Vec3::new(0.0, 0.0, 20.1), Vec3::new(10.0, 10.0, 20.0), Vec3::new(50.0, 0.0, 0.0), &floor());
        assert!(h.is_none());
    }

    #[test]
    fn cylinder_misses_box_corner_region() {
        // A wall corner at 45 degrees: the box (corner reaches r*sqrt2) hits, the cylinder does not.
        // Wall plane x + y = 30, at distance 21.21 from the origin.
        let wall = [Vec3::new(50.0, -20.0, -50.0), Vec3::new(50.0, -20.0, 50.0), Vec3::new(-20.0, 50.0, 0.0)];
        let c = Vec3::ZERO;
        let d = Vec3::new(0.5, 0.5, 0.0);
        let cyl = Shape::Cylinder { radius: 20.0, half_height: 10.0 };
        assert!(sweep_shape_tri(c, &cyl, d, &wall).is_none());
        assert!(sweep_shape_tri(c, &Shape::Box(Vec3::new(20.0, 20.0, 10.0)), d, &wall).is_some());
        // Moving further, the cylinder hits close to radius 20 from the plane.
        let h = sweep_shape_tri(c, &cyl, Vec3::new(10.0, 10.0, 0.0), &wall).unwrap();
        let moved = 14.142 * h.t; // distance along the plane normal
        let gap = 21.213 - moved;
        assert!((20.0..20.5).contains(&gap), "gap {gap}");
    }
}
