//! Point and line queries against the level's BSP tree (solid/empty classification).
//!
//! Each node splits space by its plane (`dot(n, p) - w`, front when >= 0). Regions are
//! classified as in a CSG BSP: crossing a CSG node into its front side makes the region empty
//! (outside), into its back side makes it solid; non-CSG nodes (flag bit 0) inherit the parent's
//! state. The state at the root comes from the model's `root_outside` flag.

use glam::Vec3;
use upkg::model::Model;

/// Node flag: node does not take part in CSG solidity (added by non-solid / semi-solid brushes).
pub const NF_NOT_CSG: u8 = 0x01;

#[derive(Clone, Copy, Debug)]
struct BNode {
    normal: Vec3,
    w: f32,
    front: i32,
    back: i32,
    csg: bool,
    /// Leaf index reached through the back and front side ([back, front]), -1 if none.
    leaf: [i32; 2],
}

pub struct Bsp {
    nodes: Vec<BNode>,
    root_outside: bool,
    /// Zone number of every leaf.
    leaf_zone: Vec<i32>,
}

impl Bsp {
    /// A BSP with no nodes (everything is empty space).
    pub fn empty() -> Bsp {
        Bsp { nodes: Vec::new(), root_outside: true, leaf_zone: Vec::new() }
    }

    pub fn new(m: &Model) -> Bsp {
        let nodes = m
            .nodes
            .iter()
            .map(|n| BNode {
                normal: Vec3::new(n.plane[0], n.plane[1], n.plane[2]),
                w: n.plane[3],
                front: n.front,
                back: n.back,
                csg: n.flags & NF_NOT_CSG == 0,
                leaf: n.leaf,
            })
            .collect();
        Bsp { nodes, root_outside: m.root_outside, leaf_zone: m.leaves.iter().map(|l| l.zone).collect() }
    }

    /// Leaf containing `p` (the `iLeaf` of a UE2 PointRegion), or None outside any leaf.
    pub fn point_leaf(&self, p: Vec3) -> Option<usize> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut i = 0i32;
        loop {
            let n = &self.nodes[i as usize];
            let side = (n.normal.dot(p) - n.w >= 0.0) as usize;
            let next = if side == 1 { n.front } else { n.back };
            if next < 0 {
                return usize::try_from(n.leaf[side]).ok();
            }
            i = next;
        }
    }

    /// Zone number at `p` (index into the world model's zone table; 0 = no zone).
    pub fn point_zone(&self, p: Vec3) -> usize {
        self.point_leaf(p).and_then(|l| self.leaf_zone.get(l)).map(|&z| z.max(0) as usize).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// True when `p` lies inside solid world geometry.
    pub fn point_solid(&self, p: Vec3) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let mut outside = self.root_outside;
        let mut i = 0i32;
        loop {
            let n = &self.nodes[i as usize];
            let next = if n.normal.dot(p) - n.w >= 0.0 {
                outside = outside || n.csg;
                n.front
            } else {
                outside = outside && !n.csg;
                n.back
            };
            if next < 0 {
                return !outside;
            }
            i = next;
        }
    }

    /// First point where the segment `a -> b` enters solid space: (fraction, surface normal).
    /// Returns fraction 0 with a zero normal when `a` itself is in solid.
    pub fn line_check(&self, a: Vec3, b: Vec3) -> Option<(f32, Vec3)> {
        if self.nodes.is_empty() {
            return None;
        }
        self.line(0, a, b, 0.0, 1.0, self.root_outside, Vec3::ZERO, 0)
    }

    #[allow(clippy::too_many_arguments)]
    fn line(&self, node: i32, a: Vec3, b: Vec3, t0: f32, t1: f32, outside: bool, cross_n: Vec3, depth: u32) -> Option<(f32, Vec3)> {
        if node < 0 {
            return (!outside).then_some((t0, cross_n));
        }
        if depth > 4096 {
            return None;
        }
        let n = &self.nodes[node as usize];
        let (front_out, back_out) = (outside || n.csg, outside && !n.csg);
        let da = n.normal.dot(a) - n.w;
        let db = n.normal.dot(b) - n.w;
        if da >= 0.0 && db >= 0.0 {
            return self.line(n.front, a, b, t0, t1, front_out, cross_n, depth + 1);
        }
        if da < 0.0 && db < 0.0 {
            return self.line(n.back, a, b, t0, t1, back_out, cross_n, depth + 1);
        }
        let f = da / (da - db);
        let pm = a + (b - a) * f;
        let tm = t0 + (t1 - t0) * f;
        if da >= 0.0 {
            self.line(n.front, a, pm, t0, tm, front_out, cross_n, depth + 1)
                .or_else(|| self.line(n.back, pm, b, tm, t1, back_out, n.normal, depth + 1))
        } else {
            self.line(n.back, a, pm, t0, tm, back_out, cross_n, depth + 1)
                .or_else(|| self.line(n.front, pm, b, tm, t1, front_out, -n.normal, depth + 1))
        }
    }
}
