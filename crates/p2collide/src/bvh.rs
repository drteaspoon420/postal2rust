//! Bounding volume hierarchy over triangles (median split on the longest axis).

use crate::geom::Aabb;
use glam::Vec3;

#[derive(Clone, Copy, Debug)]
struct Node {
    bounds: Aabb,
    /// Leaf: first primitive in `order`; inner: index of the left child (right = left + 1).
    first: u32,
    /// Leaf primitive count (0 = inner node).
    count: u32,
}

#[derive(Default)]
pub struct Bvh {
    nodes: Vec<Node>,
    /// Primitive indices, grouped by leaf.
    order: Vec<u32>,
}

const LEAF: usize = 4;

impl Bvh {
    pub fn build(tris: &[[Vec3; 3]]) -> Bvh {
        let boxes: Vec<Aabb> = tris.iter().map(|t| Aabb::from_points(t)).collect();
        let centers: Vec<Vec3> = boxes.iter().map(|b| b.center()).collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes = vec![Node { bounds: Aabb::EMPTY, first: 0, count: 0 }];
        if !tris.is_empty() {
            let mut stack = vec![(0usize, 0usize, tris.len())];
            while let Some((ni, lo, hi)) = stack.pop() {
                let slice = &mut order[lo..hi];
                let bounds = slice.iter().fold(Aabb::EMPTY, |b, &i| b.union(boxes[i as usize]));
                if hi - lo <= LEAF {
                    nodes[ni] = Node { bounds, first: lo as u32, count: (hi - lo) as u32 };
                    continue;
                }
                let cb = slice.iter().fold(Aabb::EMPTY, |mut b, &i| {
                    b.grow(centers[i as usize]);
                    b
                });
                let ext = cb.max - cb.min;
                let axis = if ext.x >= ext.y && ext.x >= ext.z { 0 } else if ext.y >= ext.z { 1 } else { 2 };
                let mid = (hi - lo) / 2;
                slice.select_nth_unstable_by(mid, |&a, &b| centers[a as usize][axis].total_cmp(&centers[b as usize][axis]));
                let left = nodes.len();
                nodes.push(Node { bounds: Aabb::EMPTY, first: 0, count: 0 });
                nodes.push(Node { bounds: Aabb::EMPTY, first: 0, count: 0 });
                nodes[ni] = Node { bounds, first: left as u32, count: 0 };
                stack.push((left, lo, lo + mid));
                stack.push((left + 1, lo + mid, hi));
            }
        }
        Bvh { nodes, order }
    }

    /// Calls `f` for every primitive whose bounds may overlap `b`.
    pub fn query(&self, b: &Aabb, mut f: impl FnMut(u32)) {
        if self.order.is_empty() {
            return;
        }
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let n = &self.nodes[stack[sp] as usize];
            if !n.bounds.overlaps(b) {
                continue;
            }
            if n.count > 0 {
                for &i in &self.order[n.first as usize..(n.first + n.count) as usize] {
                    f(i);
                }
            } else if sp + 2 <= stack.len() {
                stack[sp] = n.first;
                stack[sp + 1] = n.first + 1;
                sp += 2;
            }
        }
    }

    /// Calls `f` for every primitive whose bounds, grown by `ext`, the segment `start + d * t`
    /// (t in 0..=1) may touch. Much tighter than [`Self::query`] with the swept box for long
    /// diagonal traces (line of sight, hitscan), whose box can cover most of a level.
    pub fn query_segment(&self, start: Vec3, d: Vec3, ext: Vec3, mut f: impl FnMut(u32)) {
        if self.order.is_empty() {
            return;
        }
        let inv = Vec3::new(1.0 / d.x, 1.0 / d.y, 1.0 / d.z);
        let hits = |b: &Aabb| {
            let (lo, hi) = (b.min - ext - start, b.max + ext - start);
            let (mut t0, mut t1) = (0.0f32, 1.0f32);
            for k in 0..3 {
                if d[k] == 0.0 {
                    // Parallel to this slab: inside it or never.
                    if lo[k] > 0.0 || hi[k] < 0.0 {
                        return false;
                    }
                    continue;
                }
                let (a, c) = (lo[k] * inv[k], hi[k] * inv[k]);
                let (a, c) = if a < c { (a, c) } else { (c, a) };
                t0 = t0.max(a);
                t1 = t1.min(c);
                if t0 > t1 {
                    return false;
                }
            }
            true
        };
        let mut stack = [0u32; 64];
        let mut sp = 1;
        while sp > 0 {
            sp -= 1;
            let n = &self.nodes[stack[sp] as usize];
            if !hits(&n.bounds) {
                continue;
            }
            if n.count > 0 {
                for &i in &self.order[n.first as usize..(n.first + n.count) as usize] {
                    f(i);
                }
            } else if sp + 2 <= stack.len() {
                stack[sp] = n.first;
                stack[sp + 1] = n.first + 1;
                sp += 2;
            }
        }
    }

    pub fn bounds(&self) -> Aabb {
        self.nodes.first().map(|n| n.bounds).unwrap_or(Aabb::EMPTY)
    }
}
