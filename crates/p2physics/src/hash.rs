//! A uniform-grid spatial hash over actor bounding boxes (XY cells, unbounded Z), so traces,
//! touch updates and radius queries only look at actors near the query.

use crate::types::ActorId;
use glam::Vec3;
use p2collide::Aabb;
use std::collections::HashMap;

/// Large actors (more cells than this) go in a separate list checked by every query.
const MAX_CELLS: i64 = 64;

pub struct SpatialHash {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<ActorId>>,
    /// Actors too large to bucket.
    big: Vec<ActorId>,
    /// Where each actor is stored: (min cell, max cell) or None for `big`.
    placed: HashMap<ActorId, Option<((i32, i32), (i32, i32))>>,
    stamp: Vec<u32>,
    epoch: u32,
}

impl SpatialHash {
    pub fn new(cell: f32) -> SpatialHash {
        SpatialHash { cell, cells: HashMap::new(), big: Vec::new(), placed: HashMap::new(), stamp: Vec::new(), epoch: 0 }
    }

    fn key(&self, x: f32, y: f32) -> (i32, i32) {
        ((x / self.cell).floor() as i32, (y / self.cell).floor() as i32)
    }

    pub fn insert(&mut self, id: ActorId, b: Aabb) {
        self.remove(id);
        let (lo, hi) = (self.key(b.min.x, b.min.y), self.key(b.max.x, b.max.y));
        let n = (hi.0 as i64 - lo.0 as i64 + 1) * (hi.1 as i64 - lo.1 as i64 + 1);
        if !b.is_valid() || n > MAX_CELLS {
            self.big.push(id);
            self.placed.insert(id, None);
            return;
        }
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                self.cells.entry((x, y)).or_default().push(id);
            }
        }
        self.placed.insert(id, Some((lo, hi)));
    }

    pub fn remove(&mut self, id: ActorId) {
        match self.placed.remove(&id) {
            Some(Some((lo, hi))) => {
                for x in lo.0..=hi.0 {
                    for y in lo.1..=hi.1 {
                        if let Some(v) = self.cells.get_mut(&(x, y)) {
                            v.retain(|&a| a != id);
                            if v.is_empty() {
                                self.cells.remove(&(x, y));
                            }
                        }
                    }
                }
            }
            Some(None) => self.big.retain(|&a| a != id),
            None => {}
        }
    }

    /// Calls `f` once for every actor whose stored box may overlap `b`.
    pub fn query(&mut self, b: Aabb, mut f: impl FnMut(ActorId)) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.epoch = 1;
        }
        let epoch = self.epoch;
        let visit = |stamp: &mut Vec<u32>, id: ActorId, f: &mut dyn FnMut(ActorId)| {
            let i = id as usize;
            if stamp.len() <= i {
                stamp.resize(i + 1, 0);
            }
            if stamp[i] != epoch {
                stamp[i] = epoch;
                f(id);
            }
        };
        for &id in &self.big {
            visit(&mut self.stamp, id, &mut f);
        }
        let (lo, hi) = (self.key(b.min.x, b.min.y), self.key(b.max.x, b.max.y));
        let n = (hi.0 as i64 - lo.0 as i64 + 1) * (hi.1 as i64 - lo.1 as i64 + 1);
        if n > self.cells.len() as i64 * 2 {
            // Huge query: walk the occupied cells instead.
            for (k, v) in &self.cells {
                if k.0 >= lo.0 && k.0 <= hi.0 && k.1 >= lo.1 && k.1 <= hi.1 {
                    for &id in v {
                        visit(&mut self.stamp, id, &mut f);
                    }
                }
            }
            return;
        }
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                if let Some(v) = self.cells.get(&(x, y)) {
                    for &id in v {
                        visit(&mut self.stamp, id, &mut f);
                    }
                }
            }
        }
    }

    pub fn query_vec(&mut self, b: Aabb) -> Vec<ActorId> {
        let mut out = Vec::new();
        self.query(b, |a| out.push(a));
        out
    }

    pub fn len(&self) -> usize {
        self.placed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }
}

/// Bounds of a point swept by `d` with half extent `e`.
pub fn swept_box(p: Vec3, d: Vec3, e: Vec3) -> Aabb {
    Aabb::swept(p, p + d, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_query_remove() {
        let mut h = SpatialHash::new(256.0);
        h.insert(1, Aabb { min: Vec3::new(0.0, 0.0, 0.0), max: Vec3::new(10.0, 10.0, 10.0) });
        h.insert(2, Aabb { min: Vec3::new(1000.0, 0.0, 0.0), max: Vec3::new(1010.0, 10.0, 10.0) });
        h.insert(3, Aabb { min: Vec3::splat(-1e6), max: Vec3::splat(1e6) });
        let mut q = h.query_vec(Aabb { min: Vec3::splat(-5.0), max: Vec3::splat(5.0) });
        q.sort();
        assert_eq!(q, vec![1, 3]);
        h.insert(1, Aabb { min: Vec3::new(990.0, 0.0, 0.0), max: Vec3::new(1000.0, 10.0, 10.0) });
        let mut q = h.query_vec(Aabb { min: Vec3::new(995.0, 0.0, 0.0), max: Vec3::new(1005.0, 5.0, 5.0) });
        q.sort();
        assert_eq!(q, vec![1, 2, 3]);
        h.remove(3);
        assert_eq!(h.query_vec(Aabb { min: Vec3::splat(-5.0), max: Vec3::splat(5.0) }), Vec::<u32>::new());
    }
}
