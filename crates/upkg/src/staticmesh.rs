//! StaticMesh decoding (render data only: sections, vertices, UVs, colours, indices).
//!
//! Layout determined by inspecting the shipped .usx data; each stream is a compact-counted
//! array followed by an i32 revision number.

use crate::package::{ObjRef, Package};
use crate::props::{Properties, begin_object, read_properties};
use crate::reader::Reader;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct Section {
    pub first_index: u32,
    pub min_vertex: u32,
    pub max_vertex: u32,
    pub num_triangles: u32,
}

#[derive(Debug, Clone)]
pub struct StaticMesh {
    pub name: String,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub sections: Vec<Section>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// First UV channel (one entry per vertex), if present.
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[u8; 4]>,
    pub indices: Vec<u16>,
    /// Material per section (same order as `sections`).
    pub materials: Vec<ObjRef>,
    /// Per-section `EnableCollision` from the `Materials` array (true when absent).
    pub material_collision: Vec<bool>,
    /// Collision triangles: three indices into `positions` plus the material (section) index.
    /// Empty for meshes saved without collision data.
    pub collision_tris: Vec<CollisionTri>,
    /// Simplified collision Model (brush) when the mesh has one, else Null.
    pub collision_model: ObjRef,
    pub props: Properties,
}

#[derive(Debug, Clone, Copy)]
pub struct CollisionTri {
    pub indices: [u32; 3],
    pub material: i32,
}

/// Data after the render index buffer: wireframe indices, collision Model ref and collision
/// triangles (four planes then v0, v1, v2, material as compact indices).
fn read_collision(r: &mut Reader, num_verts: usize) -> Result<(ObjRef, Vec<CollisionTri>)> {
    let _wire = r.array(|r| r.u16())?;
    let _rev = r.i32()?;
    let model = ObjRef::from_raw(r.compact()?);
    let tris = r.array(|r| {
        r.skip(64)?; // face plane + three edge planes
        let indices = [r.compact()? as u32, r.compact()? as u32, r.compact()? as u32];
        let material = r.compact()?;
        Ok(CollisionTri { indices, material })
    })?;
    if tris.iter().any(|t| t.indices.iter().any(|&i| i as usize >= num_verts)) {
        return Err(Error::Corrupt("collision triangle vertex out of range"));
    }
    Ok((model, tris))
}

fn bbox(r: &mut Reader) -> Result<([f32; 3], [f32; 3])> {
    let min = r.vec3()?;
    let max = r.vec3()?;
    let _valid = r.u8()?;
    Ok((min, max))
}

/// Parses the `Materials` property: a dynamic array whose struct elements are stored as tagged
/// property lists, each holding a `Material` object reference.
fn read_materials(pkg: &Package, props: &Properties) -> (Vec<ObjRef>, Vec<bool>) {
    let Some(crate::Value::Array { raw }) = props.get("Materials") else { return (vec![], vec![]) };
    let mut r = Reader::new(raw);
    let Ok(n) = r.count() else { return (vec![], vec![]) };
    let mut out = Vec::with_capacity(n);
    let mut coll = Vec::with_capacity(n);
    for _ in 0..n {
        match read_properties(&mut r, pkg) {
            Ok(p) => {
                out.push(p.object("Material").unwrap_or(ObjRef::Null));
                coll.push(p.bool("EnableCollision").unwrap_or(true));
            }
            Err(_) => break,
        }
    }
    (out, coll)
}

pub fn read_static_mesh(pkg: &Package, export: usize) -> Result<StaticMesh> {
    let (mut r, props) = begin_object(pkg, export)?;
    // Primitive bounds.
    let (bounds_min, bounds_max) = bbox(&mut r)?;
    let _sphere = (r.vec3()?, r.f32()?);

    let sections = r.array(|r| {
        let _is_strip = r.i32()?;
        let first_index = r.u16()? as u32;
        let min_vertex = r.u16()? as u32;
        let max_vertex = r.u16()? as u32;
        let num_triangles = r.u16()? as u32;
        let _num_primitives = r.u16()?;
        Ok(Section { first_index, min_vertex, max_vertex, num_triangles })
    })?;
    let _render_bounds = bbox(&mut r)?;

    // Vertex stream: position + normal.
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let n = r.count()?;
    positions.reserve(n);
    normals.reserve(n);
    for _ in 0..n {
        positions.push(r.vec3()?);
        normals.push(r.vec3()?);
    }
    let _rev = r.i32()?;

    // Two colour streams (vertex colour and alpha); keep the first.
    let colors = r.array(|r| {
        let b = r.bytes(4)?;
        Ok([b[2], b[1], b[0], b[3]])
    })?;
    let _rev = r.i32()?;
    let _alpha = r.array(|r| r.bytes(4).map(|_| ()))?;
    let _rev = r.i32()?;

    // UV streams.
    let uv_streams = r.count()?;
    let mut uvs = Vec::new();
    for s in 0..uv_streams {
        let stream = r.array(|r| Ok([r.f32()?, r.f32()?]))?;
        let _coord_index = r.i32()?;
        let _rev = r.i32()?;
        if s == 0 {
            uvs = stream;
        }
    }

    let indices = r.array(|r| r.u16())?;
    let _rev = r.i32()?;
    let (collision_model, collision_tris) = read_collision(&mut r, positions.len()).unwrap_or((ObjRef::Null, Vec::new()));

    // A few shipped meshes (16 of 3,174) declare more section triangles than the index buffer
    // holds. Clamp each section to the indices actually present so the mesh still renders.
    let mut sections = sections;
    for s in &mut sections {
        let avail = indices.len().saturating_sub(s.first_index as usize) / 3;
        if s.num_triangles as usize > avail {
            s.num_triangles = avail as u32;
        }
    }

    let (materials, material_collision) = read_materials(pkg, &props);
    let mesh = StaticMesh {
        name: pkg.name(pkg.exports[export].name).to_string(),
        bounds_min,
        bounds_max,
        materials,
        material_collision,
        collision_tris,
        collision_model,
        sections,
        positions,
        normals,
        uvs,
        colors,
        indices,
        props,
    };
    mesh.validate()?;
    Ok(mesh)
}

impl StaticMesh {
    fn validate(&self) -> Result<()> {
        let nv = self.positions.len();
        if !self.uvs.is_empty() && self.uvs.len() != nv {
            return Err(Error::Corrupt("uv count != vertex count"));
        }
        if self.indices.iter().any(|&i| i as usize >= nv) {
            return Err(Error::Corrupt("index out of range"));
        }
        for s in &self.sections {
            if (s.first_index + s.num_triangles * 3) as usize > self.indices.len() {
                return Err(Error::Corrupt("section exceeds index buffer"));
            }
        }
        Ok(())
    }

    /// Triangle index range for a section.
    pub fn section_indices(&self, s: usize) -> &[u16] {
        let sec = self.sections[s];
        &self.indices[sec.first_index as usize..(sec.first_index + sec.num_triangles * 3) as usize]
    }
}
 
/// Baked per-vertex lighting of a placed StaticMeshActor (its StaticMeshInstance object): one
/// colour per vertex of the referenced mesh, in mesh vertex order, returned as RGBA. Empty if the
/// actor has no baked colours.
pub fn read_instance_colors(pkg: &Package, export: usize) -> Result<Vec<[u8; 4]>> {
    let (mut r, _) = begin_object(pkg, export)?;
    r.array(|r| {
        let b = r.bytes(4)?;
        // Stored R, G, B, A. Read as BGRA, suburbs-3 foliage under the warm orange sun came out
        // blue (leaves ~20,29,39) and trees rendered teal instead of golden.
        Ok([b[0], b[1], b[2], b[3]])
    })
}

/// Static light visibility saved in a StaticMeshInstance after its colour stream: an int32,
/// then per light { compact light actor, TArray<u8> visibility bits (one per mesh vertex, LSB
/// first), int32 }. UE2 builds the vertex lighting from these at load when the colour stream
/// was not baked (alpha 0 everywhere; most of Industry).
pub fn read_instance_lights(pkg: &Package, export: usize) -> Result<Vec<(ObjRef, Vec<u8>)>> {
    let (mut r, _) = begin_object(pkg, export)?;
    let n = r.count()?;
    r.skip(n * 4)?;
    let _ = r.i32()?;
    let nl = r.count()?;
    let mut out = Vec::with_capacity(nl);
    for _ in 0..nl {
        let actor = ObjRef::from_raw(r.compact()?);
        let nb = r.count()?;
        let bits = r.bytes(nb)?.to_vec();
        let _applied = r.i32()?;
        out.push((actor, bits));
    }
    Ok(out)
}
