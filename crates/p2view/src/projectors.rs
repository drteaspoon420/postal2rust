//! Level-static Projector actors (decals painted onto BSP: graffiti, stains, cracks).
//!
//! Each projector's frustum is treated as an orthographic box (FOV 0/1 in the shipped maps):
//! BSP polygons facing the projector and inside the box are clipped to it and re-textured
//! with the projected material, producing decal meshes drawn with a small depth bias.

use bevy::math::{Mat3, Vec3};
use upkg::library::Library;
use upkg::model::{Model, pf};
use upkg::{ObjRef, Package, begin_object};

/// A decal mesh produced by one projector.
pub struct DecalData {
    pub export: usize,
    pub material: ObjRef,
    /// EProjectorBlending of MaterialBlendingOp / FrameBufferBlendingOp (0 None, 1 Modulate,
    /// 2 AlphaBlend, 3 Add).
    pub material_op: u8,
    pub framebuffer_op: u8,
    /// Unreal-space triangles: positions, normals, UVs.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

fn rot_axes(r: [i32; 3]) -> (Vec3, Vec3, Vec3) {
    let k = std::f32::consts::TAU / 65536.0;
    let (sp, cp) = (r[0] as f32 * k).sin_cos();
    let (sy, cy) = (r[1] as f32 * k).sin_cos();
    let (sr, cr) = (r[2] as f32 * k).sin_cos();
    let x = Vec3::new(cp * cy, cp * sy, sp);
    let y = Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
    let z = Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp);
    (x, y, z)
}

/// Sutherland-Hodgman clip of a polygon (projector-local coordinates) to min <= p[axis] <= max.
fn clip(mut poly: Vec<Vec3>, axis: usize, min: f32, max: f32) -> Vec<Vec3> {
    // Each pass keeps the side where sign * p[axis] <= bound.
    for (sign, bound) in [(1.0f32, max), (-1.0f32, -min)] {
        let inside = |p: &Vec3| sign * p[axis] <= bound;
        let mut out = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (bound - sign * a[axis]) / (sign * (b[axis] - a[axis]));
                out.push(a + (b - a) * t);
            }
        }
        poly = out;
        if poly.len() < 3 {
            return vec![];
        }
    }
    poly
}

/// Builds decal geometry for every level-static Projector that projects onto BSP.
pub fn build_projectors(lib: &Library, map: &std::sync::Arc<Package>, model: &Model) -> Vec<DecalData> {
    let mut out = Vec::new();
    for i in map.exports_of_class("Projector") {
        let Ok((_, p)) = begin_object(map, i) else { continue };
        if p.bool("bProjectBSP") == Some(false) || p.bool("bHidden") == Some(true) {
            continue;
        }
        let Some(mat) = p.object("ProjTexture").filter(|o| *o != ObjRef::Null) else { continue };
        // Projected image size: the material's texture size times DrawScale.
        let size = lib
            .resolve(map, mat)
            .and_then(|m| lib.material_texture(&m))
            .and_then(|m| m.texture)
            .and_then(|t| begin_object(&t.pkg, t.export).ok().map(|(_, tp)| (tp.int("USize").unwrap_or(256) as f32, tp.int("VSize").unwrap_or(256) as f32)))
            .unwrap_or((256.0, 256.0));
        let ds = p.float("DrawScale").unwrap_or(1.0);
        let (hw, hh) = (size.0 * ds * 0.5, size.1 * ds * 0.5);
        let depth = p.int("MaxTraceDistance").unwrap_or(1000) as f32;
        let loc = Vec3::from(p.vector("Location").unwrap_or([0.0; 3]));
        let (fx, fy, fz) = rot_axes(p.rotator("Rotation").unwrap_or([0; 3]));
        let to_local = Mat3::from_cols(fx, fy, fz).transpose();
        let mut d = DecalData {
            export: i,
            material: mat,
            material_op: p.byte("MaterialBlendingOp").unwrap_or(0),
            framebuffer_op: p.byte("FrameBufferBlendingOp").unwrap_or(0),
            positions: vec![], normals: vec![], uvs: vec![], indices: vec![] };
        for n in &model.nodes {
            if n.num_verts < 3 {
                continue;
            }
            let s = model.surfs[n.surf as usize];
            if s.flags & (pf::INVISIBLE | pf::FAKE_BACKDROP | pf::PORTAL) != 0 {
                continue;
            }
            let nrm = Vec3::new(n.plane[0], n.plane[1], n.plane[2]);
            // Only surfaces facing back toward the projector.
            if nrm.dot(fx) > -0.1 {
                continue;
            }
            let local: Vec<Vec3> = model.node_polygon(n).map(|q| to_local * (Vec3::from(q) - loc)).collect();
            let mut poly = clip(local, 0, 0.0, depth);
            if poly.is_empty() {
                continue;
            }
            poly = clip(poly, 1, -hw, hw);
            if poly.is_empty() {
                continue;
            }
            poly = clip(poly, 2, -hh, hh);
            if poly.len() < 3 {
                continue;
            }
            let start = d.positions.len() as u32;
            for q in &poly {
                // Lift slightly off the surface to avoid z-fighting.
                let w = loc + fx * q.x + fy * q.y + fz * q.z + nrm * 0.15;
                d.positions.push(w.to_array());
                d.normals.push(nrm.to_array());
                d.uvs.push([0.5 + q.y / (2.0 * hw), 0.5 - q.z / (2.0 * hh)]);
            }
            for k in 1..poly.len() as u32 - 1 {
                d.indices.extend([start, start + k, start + k + 1]);
            }
        }
        if !d.indices.is_empty() {
            out.push(d);
        }
    }
    out
}
