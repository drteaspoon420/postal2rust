//! Terrain zone clipping. UE2 draws a TerrainInfo only from zones flagged `bTerrainZone`
//! (ZoneInfo.Terrains, filled at load for terrain zones; zones without a ZoneInfo use the
//! LevelInfo's flag). Heightmap parts that run under or through buildings are therefore never
//! seen indoors. We draw the terrain as one mesh, so those parts showed through interior floors
//! and walls (suburbs-1 Habib's shop hallway: a wavy dirt band halfway up the wall).
//!
//! `TerrainClip` splits terrain triangles along the world BSP planes and keeps only the pieces
//! in terrain zones (or in solid space, which no camera sees into).

use bevy::math::Vec3;
use upkg::model::Model;
use upkg::{ObjRef, Package, begin_object};

pub struct TerrainClip<'a> {
    model: &'a Model,
    /// Per zone number: whether terrain is drawn there.
    keep_zone: Vec<bool>,
}

/// A clipped polygon vertex: Unreal-space position and barycentric weights of the source triangle.
type CVert = (Vec3, [f32; 3]);

const EPS: f32 = 0.05;

impl<'a> TerrainClip<'a> {
    /// None when clipping does not apply (no zone draws terrain, so the flags are not trusted).
    pub fn new(map: &Package, model: &'a Model) -> Option<Self> {
        if std::env::var_os("P2VIEW_NO_TERRAIN_CLIP").is_some() {
            return None;
        }
        let flag = |r: ObjRef| match r {
            ObjRef::Export(i) => begin_object(map, i).ok().and_then(|(_, p)| p.bool("bTerrainZone")).unwrap_or(false),
            _ => false,
        };
        let level = map.exports_of_class("LevelInfo").next().map(|i| flag(ObjRef::Export(i))).unwrap_or(false);
        let keep_zone: Vec<bool> = model.zones.iter().map(|&z| if z == ObjRef::Null { level } else { flag(z) }).collect();
        if !keep_zone.iter().any(|&k| k) {
            return None;
        }
        Some(Self { model, keep_zone })
    }

    fn keep_leaf(&self, leaf: i32) -> bool {
        if leaf < 0 {
            return true;
        }
        let Some(l) = self.model.leaves.get(leaf as usize) else { return true };
        self.keep_zone.get(l.zone as usize).copied().unwrap_or(true)
    }

    /// Clips one triangle (Unreal space). None = keep it whole; Some(pieces) = the kept pieces
    /// as triangles of barycentric weights (empty when nothing is kept).
    pub fn clip_tri(&self, tri: [Vec3; 3]) -> Option<Vec<[[f32; 3]; 3]>> {
        let start: Vec<CVert> = vec![(tri[0], [1.0, 0.0, 0.0]), (tri[1], [0.0, 1.0, 0.0]), (tri[2], [0.0, 0.0, 1.0])];
        let mut kept: Vec<Vec<CVert>> = Vec::new();
        let mut dropped = false;
        let mut stack: Vec<(usize, Vec<CVert>)> = vec![(0, start)];
        while let Some((ni, poly)) = stack.pop() {
            let Some(n) = self.model.nodes.get(ni) else {
                kept.push(poly);
                continue;
            };
            let plane = Vec3::new(n.plane[0], n.plane[1], n.plane[2]);
            let d: Vec<f32> = poly.iter().map(|(p, _)| plane.dot(*p) - n.plane[3]).collect();
            let any_front = d.iter().any(|&x| x > EPS);
            let any_back = d.iter().any(|&x| x < -EPS);
            let (front, back) = if any_front && any_back {
                split(&poly, &d)
            } else if any_back {
                (Vec::new(), poly)
            } else {
                // All in front or on the plane: the point query treats d >= 0 as front.
                (poly, Vec::new())
            };
            for (side, child, leaf) in [(front, n.front, n.leaf[1]), (back, n.back, n.leaf[0])] {
                if side.len() < 3 {
                    continue;
                }
                if child >= 0 {
                    stack.push((child as usize, side));
                } else if self.keep_leaf(leaf) {
                    kept.push(side);
                } else {
                    dropped = true;
                }
            }
        }
        if !dropped {
            return None;
        }
        let mut out = Vec::new();
        for poly in kept {
            for k in 1..poly.len() - 1 {
                out.push([poly[0].1, poly[k].1, poly[k + 1].1]);
            }
        }
        Some(out)
    }

    /// Whether a point (Unreal space) is in a zone that draws terrain (or in solid space).
    pub fn keep_point(&self, p: Vec3) -> bool {
        let mut i = 0usize;
        for _ in 0..4096 {
            let Some(n) = self.model.nodes.get(i) else { return true };
            let front = n.plane[0] * p.x + n.plane[1] * p.y + n.plane[2] * p.z - n.plane[3] >= 0.0;
            let child = if front { n.front } else { n.back };
            if child < 0 {
                return self.keep_leaf(n.leaf[front as usize]);
            }
            i = child as usize;
        }
        true
    }
}

/// Splits a convex polygon by signed distances `d` into (front, back), both keeping its winding.
fn split(poly: &[CVert], d: &[f32]) -> (Vec<CVert>, Vec<CVert>) {
    let (mut f, mut b) = (Vec::new(), Vec::new());
    for i in 0..poly.len() {
        let j = (i + 1) % poly.len();
        let (pi, di) = (poly[i], d[i]);
        if di >= -EPS {
            f.push(pi);
        }
        if di <= EPS {
            b.push(pi);
        }
        let dj = d[j];
        if (di > EPS && dj < -EPS) || (di < -EPS && dj > EPS) {
            let t = di / (di - dj);
            let pj = poly[j];
            let w = [pi.1[0] + (pj.1[0] - pi.1[0]) * t, pi.1[1] + (pj.1[1] - pi.1[1]) * t, pi.1[2] + (pj.1[2] - pi.1[2]) * t];
            let v = (pi.0 + (pj.0 - pi.0) * t, w);
            f.push(v);
            b.push(v);
        }
    }
    (f, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_area_and_winding() {
        let poly: Vec<CVert> = vec![(Vec3::new(0.0, 0.0, 0.0), [1.0, 0.0, 0.0]), (Vec3::new(2.0, 0.0, 0.0), [0.0, 1.0, 0.0]), (Vec3::new(0.0, 2.0, 0.0), [0.0, 0.0, 1.0])];
        // Plane x = 1.
        let d: Vec<f32> = poly.iter().map(|(p, _)| p.x - 1.0).collect();
        let (f, b) = split(&poly, &d);
        let area = |p: &[CVert]| (1..p.len() - 1).map(|k| (p[k].0 - p[0].0).cross(p[k + 1].0 - p[0].0).z * 0.5).sum::<f32>();
        assert!((area(&f) - 0.5).abs() < 1e-4, "front {}", area(&f));
        assert!((area(&b) - 1.5).abs() < 1e-4, "back {}", area(&b));
        for (p, w) in f.iter().chain(b.iter()) {
            let r = poly[0].0 * w[0] + poly[1].0 * w[1] + poly[2].0 * w[2];
            assert!((r - *p).length() < 1e-4);
        }
    }
}
