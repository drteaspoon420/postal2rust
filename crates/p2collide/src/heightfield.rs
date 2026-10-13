//! Terrain heightfield collision: triangles are generated on demand for the grid cells under a
//! query box, with holes and diagonal flips matching the rendered terrain.

use crate::geom::Aabb;
use glam::Vec3;
use upkg::terrain::Terrain;

pub struct Heightfield {
    pub name: String,
    location: Vec3,
    scale: Vec3,
    width: u32,
    height: u32,
    heights: Vec<u16>,
    visibility: Option<Vec<u32>>,
    edge_turn: Option<Vec<u32>>,
    pub bounds: Aabb,
    /// Surface layers in draw order: (material index in the world's table, alpha map
    /// (width, height, weights) or None for full coverage).
    pub layers: Vec<(u32, Option<(u32, u32, Vec<u8>)>)>,
}

fn bit(bits: &Option<Vec<u32>>, idx: usize, default: bool) -> bool {
    match bits {
        Some(v) => v.get(idx / 32).map(|w| w & (1 << (idx % 32)) != 0).unwrap_or(default),
        None => default,
    }
}

impl Heightfield {
    pub fn new(t: &Terrain) -> Heightfield {
        let mut hf = Heightfield {
            name: t.name.clone(),
            location: Vec3::from(t.location),
            scale: Vec3::from(t.scale),
            width: t.width,
            height: t.height,
            heights: t.heights.clone(),
            visibility: t.quad_visibility.clone(),
            edge_turn: t.edge_turn.clone(),
            bounds: Aabb::EMPTY,
            layers: Vec::new(),
        };
        let (lo, hi) = hf.heights.iter().fold((u16::MAX, 0u16), |(lo, hi), &h| (lo.min(h), hi.max(h)));
        let a = hf.vertex_h(0, 0, lo);
        let b = hf.vertex_h(t.width - 1, t.height - 1, hi);
        hf.bounds = Aabb::from_points(&[a, b, hf.vertex_h(0, 0, hi), hf.vertex_h(t.width - 1, t.height - 1, lo)]);
        hf
    }

    fn vertex_h(&self, x: u32, y: u32, h: u16) -> Vec3 {
        Vec3::new(
            self.location.x + (x as f32 - (self.width / 2) as f32) * self.scale.x,
            self.location.y + (y as f32 - (self.height / 2) as f32) * self.scale.y,
            self.location.z + (h as f32 - 32768.0) * self.scale.z / 256.0,
        )
    }

    pub fn vertex(&self, x: u32, y: u32) -> Vec3 {
        self.vertex_h(x, y, self.heights[(y * self.width + x) as usize])
    }

    /// Grid coordinate (fractional) of a world X/Y.
    fn grid(&self, p: Vec3) -> (f32, f32) {
        ((p.x - self.location.x) / self.scale.x + (self.width / 2) as f32, (p.y - self.location.y) / self.scale.y + (self.height / 2) as f32)
    }

    /// The layer seen on top at world point `p`: layers draw in order, each blending over the ones
    /// below by its alpha, so the visible one is the last with a substantial weight there (the
    /// first full-coverage layer is the base).
    pub fn layer_at(&self, p: Vec3) -> Option<u32> {
        let (gx, gy) = self.grid(p);
        let (u, v) = (gx / self.width.max(1) as f32, gy / self.height.max(1) as f32);
        let mut top = None;
        for (m, alpha) in &self.layers {
            let w = match alpha {
                None => 255,
                Some((aw, ah, data)) => {
                    let x = ((u * *aw as f32) as i64).clamp(0, *aw as i64 - 1) as u32;
                    let y = ((v * *ah as f32) as i64).clamp(0, *ah as i64 - 1) as u32;
                    data.get((y * aw + x) as usize).copied().unwrap_or(0)
                }
            };
            if w >= 128 {
                top = Some(*m);
            }
        }
        top.or_else(|| self.layers.first().map(|l| l.0))
    }

    /// Calls `f` with every terrain triangle in the grid cells overlapping `b` (XY).
    pub fn for_tris(&self, b: &Aabb, mut f: impl FnMut([Vec3; 3])) {
        if !self.bounds.overlaps(b) || self.width < 2 || self.height < 2 {
            return;
        }
        let (gx0, gy0) = self.grid(b.min);
        let (gx1, gy1) = self.grid(b.max);
        let (x0, x1) = (gx0.min(gx1).floor(), gx0.max(gx1).ceil());
        let (y0, y1) = (gy0.min(gy1).floor(), gy0.max(gy1).ceil());
        let cx = |v: f32| v.clamp(0.0, (self.width - 1) as f32) as u32;
        let cy = |v: f32| v.clamp(0.0, (self.height - 1) as f32) as u32;
        let (x0, x1, y0, y1) = (cx(x0), cx(x1), cy(y0), cy(y1));
        for y in y0..y1.min(self.height - 1) {
            for x in x0..x1.min(self.width - 1) {
                let idx = (y * self.width + x) as usize;
                if !bit(&self.visibility, idx, true) {
                    continue;
                }
                let (a, bb, c, d) = (self.vertex(x, y), self.vertex(x + 1, y), self.vertex(x, y + 1), self.vertex(x + 1, y + 1));
                if bit(&self.edge_turn, idx, false) {
                    f([a, bb, d]);
                    f([a, d, c]);
                } else {
                    f([a, bb, c]);
                    f([bb, d, c]);
                }
            }
        }
    }

    /// Terrain surface height under (x, y), if the point is over a visible quad.
    pub fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        let mut best = None;
        let b = Aabb { min: Vec3::new(x, y, f32::MIN), max: Vec3::new(x, y, f32::MAX) };
        self.for_tris(&b, |t| {
            let p = Vec3::new(x, y, 0.0);
            let n = (t[1] - t[0]).cross(t[2] - t[0]);
            if n.z.abs() < 1e-6 {
                return;
            }
            // Barycentric inside test in XY.
            let s = |a: Vec3, b: Vec3| (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
            let (e0, e1, e2) = (s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0]));
            if (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0) {
                let z = t[0].z - (n.x * (x - t[0].x) + n.y * (y - t[0].y)) / n.z;
                best = Some(best.map_or(z, |b: f32| b.max(z)));
            }
        });
        best
    }
}
