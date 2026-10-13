//! BSP world geometry (UModel) decoding: vectors, points, nodes, surfaces and the vertex pool.
//!
//! Record layouts were determined by parsing shipped maps and checking invariants
//! (unit-length node normals, in-range child links, in-range surface/point/vector references).

use crate::package::{ObjRef, Package};
use crate::props::begin_object;
use crate::reader::Reader;
use crate::{Error, Result};

/// Surface flag bits used by the viewer.
pub mod pf {
    pub const INVISIBLE: u32 = 0x0000_0001;
    pub const MASKED: u32 = 0x0000_0002;
    pub const TRANSLUCENT: u32 = 0x0000_0004;
    /// Surface of a non-solid brush: drawn but never collides.
    pub const NOT_SOLID: u32 = 0x0000_0008;
    /// Surface of a semi-solid brush: collides but does not cut the BSP.
    pub const SEMISOLID: u32 = 0x0000_0020;
    pub const MODULATED: u32 = 0x0000_0040;
    pub const FAKE_BACKDROP: u32 = 0x0000_0080;
    pub const TWO_SIDED: u32 = 0x0000_0100;
    pub const PORTAL: u32 = 0x0400_0000;
}

#[derive(Debug, Clone, Copy)]
pub struct Node {
    pub plane: [f32; 4],
    pub vert_pool: u32,
    pub surf: i32,
    pub back: i32,
    pub front: i32,
    pub num_verts: u8,
    /// Zone numbers on the back [0] and front [1] side of the node plane.
    pub zone: [u8; 2],
    /// Render section (index into `Model::sections`), or -1.
    pub section: i32,
    /// First vertex of this node's polygon in its section's vertex array.
    pub first_vertex: i32,
    /// Index into `Model::light_maps`, or -1 when the surface is unlit / not lightmapped.
    pub light_map: i32,
    /// Node flags byte.
    pub flags: u8,
    /// Next node lying in the same plane (-1 = none).
    pub coplanar: i32,
    /// Start of this node's collision hull in `Model::leaf_hulls` (-1 = none).
    pub collision_bound: i32,
    /// Index into `Model::bounds` (-1 = none).
    pub render_bound: i32,
    /// Leaf on the back [0] and front [1] side (-1 = none).
    pub leaf: [i32; 2],
}

/// Render vertex of a BSP section: position, normal, diffuse UV and lightmap UV.
#[derive(Debug, Clone, Copy)]
pub struct BspVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub lightmap_uv: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct Section {
    pub vertices: Vec<BspVertex>,
    pub material: ObjRef,
    pub poly_flags: u32,
    /// Index into `Model::light_map_textures`, or -1.
    pub light_map_texture: i32,
}

/// Placement of one surface's lightmap inside a lightmap texture.
#[derive(Debug, Clone)]
pub struct LightMap {
    /// Index into `Model::light_map_textures`.
    pub texture: i32,
    /// Texel offset of this lightmap inside the texture.
    pub offset: [i32; 2],
    pub size: [i32; 2],
    /// World to lightmap-texel transform (rows of a 4x4 matrix, row major as stored).
    pub world_to_lightmap: [f32; 16],
    /// All seven compact fields as stored (FLightMap +0x08..+0x20: atlas, iSurf, iZone, atlas
    /// offset X/Y, SizeX, SizeY).
    pub fields: [i32; 7],
    /// World position of texel (0, 0) and the world step per texel along X and Y.
    pub origin: [f32; 3],
    pub x_axis: [f32; 3],
    pub y_axis: [f32; 3],
    /// Static lights that reach this lightmap, with the editor's per-texel visibility.
    pub lights: Vec<LightBitmap>,
}

/// One static light's visibility over a lightmap (saved by the editor's light rebuild). UE2 builds
/// lightmaps saved without texel data from these at load (FLightMap::GetTextureData).
#[derive(Debug, Clone)]
pub struct LightBitmap {
    pub actor: ObjRef,
    /// One bit per texel (LSB first), `stride` bytes per row, rows from `min[1]`.
    pub bits: Vec<u8>,
    pub rows: i32,
    pub stride: i32,
    /// Inclusive texel rectangle the light reaches.
    pub min: [i32; 2],
    pub max: [i32; 2],
}

impl LightBitmap {
    /// Whether texel (x, y) of the lightmap sees the light.
    pub fn visible(&self, x: i32, y: i32) -> bool {
        let (bx, by) = (x - self.min[0], y - self.min[1]);
        if bx < 0 || by < 0 || by >= self.rows || bx >= self.stride * 8 {
            return false;
        }
        let i = (by * self.stride + bx / 8) as usize;
        self.bits.get(i).is_some_and(|b| b >> (bx % 8) & 1 != 0)
    }
}

/// A lightmap atlas texture (BGRA8 data plus size), from the static lightmap texture record.
#[derive(Debug, Clone)]
pub struct LightMapTexture {
    pub width: u32,
    pub height: u32,
    /// Mip chain, largest first; each tightly packed BGRA8 (or the stored format).
    pub mips: Vec<Vec<u8>>,
    pub format: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct Surf {
    pub material: ObjRef,
    pub flags: u32,
    pub base: u32,
    pub normal: u32,
    pub texture_u: u32,
    pub texture_v: u32,
    pub actor: ObjRef,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub vectors: Vec<[f32; 3]>,
    pub points: Vec<[f32; 3]>,
    pub nodes: Vec<Node>,
    pub surfs: Vec<Surf>,
    /// Vertex pool: point index per entry.
    pub verts: Vec<i32>,
    /// Zone actor per zone number (index 0 is usually None or the LevelInfo). Empty if unreadable.
    pub zones: Vec<ObjRef>,
    /// Absolute package offset of the data following the zone table.
    pub tail: usize,
    /// Render sections (vertex buffers grouped by material and lightmap texture). Empty if unread.
    pub sections: Vec<Section>,
    pub light_maps: Vec<LightMap>,
    pub light_map_textures: Vec<LightMapTexture>,
    /// Bounding boxes referenced by `Node::render_bound`.
    pub bounds: Vec<([f32; 3], [f32; 3])>,
    /// Collision hulls: per node a list of node indices (bit 30 = plane flipped) ending in -1,
    /// then the hull's bounding box as 6 raw f32 bits (min xyz, max xyz). See `Model::hull`.
    pub leaf_hulls: Vec<i32>,
    pub leaves: Vec<Leaf>,
    /// Light lists of the leaves (UModel::Lights): `Leaf::permeating` indexes a run of light
    /// actors ending in a null entry, the lights the editor found reaching that leaf. UE2 lights
    /// dynamic actors (no saved vertex lighting) with the lights of the leaf they stand in.
    pub lights: Vec<ObjRef>,
    /// Whether the space outside every brush is empty (true for subtractive levels). Decides the
    /// solidity of BSP regions reached only through non-CSG nodes.
    pub root_outside: bool,
}

/// Flag on `leaf_hulls` node entries whose plane is used negated.
pub const HULL_FLIP: i32 = 0x4000_0000;

/// A BSP leaf (convex region between nodes).
#[derive(Debug, Clone, Copy)]
pub struct Leaf {
    pub zone: i32,
    pub permeating: i32,
    pub volumetric: i32,
    pub visible_zones: u64,
}

/// Polys ref, bounds, leaf hulls, leaves, lights and the root-outside flag: the data right after
/// the zone table.
type Collision = (Vec<([f32; 3], [f32; 3])>, Vec<i32>, Vec<Leaf>, Vec<ObjRef>, bool);

fn read_collision(r: &mut Reader) -> Result<Collision> {
    let _polys = r.compact()?;
    let bounds = r.array(|r| {
        let min = r.vec3()?;
        let max = r.vec3()?;
        let _valid = r.u8()?;
        Ok((min, max))
    })?;
    let hulls = r.array(|r| r.i32())?;
    let leaves = r.array(|r| Ok(Leaf { zone: r.compact()?, permeating: r.compact()?, volumetric: r.compact()?, visible_zones: r.u64()? }))?;
    let lights = r.array(|r| Ok(ObjRef::from_raw(r.compact()?)))?;
    let root_outside = r.i32()? != 0;
    Ok((bounds, hulls, leaves, lights, root_outside))
}

fn node(r: &mut Reader) -> Result<Node> {
    let plane = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
    let _zone_mask = r.u64()?;
    let flags = r.u8()?;
    r.skip(28)?;
    let vert_pool = r.compact()?;
    let surf = r.compact()?;
    let back = r.compact()?;
    let front = r.compact()?;
    let coplanar = r.compact()?;
    let collision_bound = r.compact()?;
    let render_bound = r.compact()?;
    r.skip(32)?;
    let zone0 = r.u8()?;
    let zone1 = r.u8()?;
    let num_verts = r.u8()?;
    let leaf = [r.i32()?, r.i32()?];
    let section = r.i32()?;
    let first_vertex = r.i32()?;
    let light_map = r.i32()?;
    if vert_pool < 0 {
        return Err(Error::Corrupt("negative vertex pool index"));
    }
    Ok(Node {
        plane,
        vert_pool: vert_pool as u32,
        surf,
        back,
        front,
        num_verts,
        zone: [zone0, zone1],
        section,
        first_vertex,
        light_map,
        flags,
        coplanar,
        collision_bound,
        render_bound,
        leaf,
    })
}

fn surf(r: &mut Reader) -> Result<Surf> {
    let material = ObjRef::from_raw(r.compact()?);
    let flags = r.u32()?;
    let base = r.compact()? as u32;
    let normal = r.compact()? as u32;
    let texture_u = r.compact()? as u32;
    let texture_v = r.compact()? as u32;
    let _brush_poly = r.compact()?;
    let actor = ObjRef::from_raw(r.compact()?);
    r.skip(16)?; // plane
    let _lightmap_scale = r.f32()?;
    Ok(Surf { material, flags, base, normal, texture_u, texture_v, actor })
}

pub fn read_model(pkg: &Package, export: usize) -> Result<Model> {
    let (mut r, _props) = begin_object(pkg, export)?;
    r.skip(25 + 16)?; // bounding box (min, max, valid) + bounding sphere
    let vectors = r.array(|r| r.vec3())?;
    let points = r.array(|r| r.vec3())?;
    let nodes = r.array(node)?;
    let surfs = r.array(surf)?;
    let verts = r.array(|r| {
        let p = r.compact()?;
        let _side = r.compact()?;
        Ok(p)
    })?;
    // Zone table: shared side count, zone count, then per zone the zone actor (compact ref),
    // two int-length-prefixed bitsets (connectivity, visibility) and a float. Index = node zone number.
    let zones = (|| -> Result<Vec<ObjRef>> {
        let _shared_sides = r.i32()?;
        let n = r.i32()?;
        // Usually at most 64, but some maps store more entries (suburbs-3: 71).
        if !(0..=256).contains(&n) {
            return Err(Error::Corrupt("zone count"));
        }
        let mut z = Vec::with_capacity(n as usize);
        for _ in 0..n {
            z.push(ObjRef::from_raw(r.compact()?));
            for _ in 0..2 {
                let len = r.i32()?;
                if !(0..=1024).contains(&len) {
                    return Err(Error::Corrupt("zone bitset length"));
                }
                r.skip(len as usize)?;
            }
            r.skip(4)?;
        }
        Ok(z)
    })()
    .unwrap_or_default();
    let tail = r.pos();
    let (bounds, leaf_hulls, leaves, lights, root_outside) = read_collision(&mut r.clone()).unwrap_or_default();
    let (sections, light_maps, light_map_textures) = read_render_data(&mut r).unwrap_or_default();
    let m = Model { vectors, points, nodes, surfs, verts, zones, tail, sections, light_maps, light_map_textures, bounds, leaf_hulls, leaves, lights, root_outside };
    m.validate()?;
    Ok(m)
}

/// Reads the render data that follows the zone table: Polys ref, bounds, leaf hulls, leaves,
/// lights, then the render sections, lightmaps and lightmap textures.
fn read_render_data(r: &mut Reader) -> Result<(Vec<Section>, Vec<LightMap>, Vec<LightMapTexture>)> {
    let _polys = r.compact()?;
    // Bounds: FBox (min, max, valid byte).
    let nb = r.count()?;
    r.skip(nb * 25)?;
    // Leaf hulls: ints.
    let nh = r.count()?;
    r.skip(nh * 4)?;
    // Leaves: iZone, iPermeating, iVolumetric (compacts) and a 64-bit visible-zone mask.
    let nl = r.count()?;
    for _ in 0..nl {
        r.compact()?;
        r.compact()?;
        r.compact()?;
        r.skip(8)?;
    }
    // Lights: actor refs.
    let nlights = r.count()?;
    for _ in 0..nlights {
        r.compact()?;
    }
    let _root_outside = r.i32()?;
    let _linked = r.i32()?;
    let sections = r.array(|r| {
        let vertices = r.array(|r| {
            let position = r.vec3()?;
            let uv = [r.f32()?, r.f32()?];
            let lightmap_uv = [r.f32()?, r.f32()?];
            let normal = r.vec3()?;
            Ok(BspVertex { position, normal, uv, lightmap_uv })
        })?;
        let _revision = r.i32()?;
        let material = ObjRef::from_raw(r.compact()?);
        let poly_flags = r.u32()?;
        let _num_nodes = r.i32()?;
        let light_map_texture = r.i32()?;
        Ok(Section { vertices, material, poly_flags, light_map_texture })
    })?;
    let light_maps = r.array(|r| {
        let mut c = [0i32; 7];
        for v in &mut c {
            *v = r.compact()?;
        }
        let mut m = [0f32; 16];
        for v in &mut m {
            *v = r.f32()?;
        }
        let origin = r.vec3()?;
        let x_axis = r.vec3()?;
        let y_axis = r.vec3()?;
        // Per-light visibility bitmaps (FLightBitmap: actor, bits, ?, rows, stride, texel rect).
        let lights = r.array(|r| {
            let actor = ObjRef::from_raw(r.compact()?);
            let n = r.count()?;
            let bits = r.bytes(n)?.to_vec();
            let _unknown = r.i32()?;
            let rows = r.i32()?;
            let stride = r.i32()?;
            let min = [r.i32()?, r.i32()?];
            let max = [r.i32()?, r.i32()?];
            Ok(LightBitmap { actor, bits, rows, stride, min, max })
        })?;
        // FLightMap: Level ref, then Revision (the old parser took that for the atlas index).
        r.compact()?;
        let _revision = r.i32()?;
        Ok(LightMap {
            texture: c[0],
            offset: [c[3], c[4]],
            size: [c[5], c[6]],
            fields: c,
            world_to_lightmap: m,
            origin,
            x_axis,
            y_axis,
            lights,
        })
    })?;
    if std::env::var_os("UPKG_LM_DEBUG").is_some() {
        eprintln!("render data: {} sections, {} light maps, textures array at {}", sections.len(), light_maps.len(), r.pos());
    }
    let light_map_textures = r.array(|r| {
        let _object = r.compact()?;
        let n = r.count()?;
        r.skip(n * 4)?;
        r.skip(8 + 4)?;
        let mut mips = Vec::new();
        for _ in 0..2 {
            let d = r.lazy_bytes()?;
            mips.push(d.to_vec());
        }
        let format = r.u8()?;
        let width = r.i32()? as u32;
        let height = r.i32()? as u32;
        let _revision = r.i32()?;
        mips.retain(|m| !m.is_empty());
        Ok(LightMapTexture { width, height, mips, format })
    })?;
    Ok((sections, light_maps, light_map_textures))
}

/// Absolute package offset just after the zone table (start of the remaining model data).
pub fn zone_table_end(pkg: &Package, export: usize) -> Result<usize> {
    Ok(read_model(pkg, export)?.tail)
}

impl Model {
    /// The light actors permeating leaf `leaf` (its `permeating` run in `lights`, up to the
    /// terminating null entry). Empty for leaves with no list (-1).
    pub fn leaf_lights(&self, leaf: usize) -> &[ObjRef] {
        let Some(start) = self.leaves.get(leaf).and_then(|l| usize::try_from(l.permeating).ok()) else { return &[] };
        let Some(rest) = self.lights.get(start..) else { return &[] };
        let end = rest.iter().position(|r| *r == ObjRef::Null).unwrap_or(rest.len());
        &rest[..end]
    }

    fn validate(&self) -> Result<()> {
        let (np, nv) = (self.points.len(), self.vectors.len());
        for n in &self.nodes {
            if n.surf < 0 || n.surf as usize >= self.surfs.len() {
                return Err(Error::Corrupt("node surface out of range"));
            }
            let end = n.vert_pool as usize + n.num_verts as usize;
            if end > self.verts.len() {
                return Err(Error::Corrupt("node vertex range out of pool"));
            }
            if self.verts[n.vert_pool as usize..end].iter().any(|&p| p < 0 || p as usize >= np) {
                return Err(Error::Corrupt("node vertex references missing point"));
            }
            let nn = self.nodes.len() as i32;
            if [n.back, n.front, n.coplanar].iter().any(|&c| c < -1 || c >= nn) {
                return Err(Error::Corrupt("node child link out of range"));
            }
        }
        for s in &self.surfs {
            if s.base as usize >= np || [s.normal, s.texture_u, s.texture_v].iter().any(|&v| v as usize >= nv) {
                return Err(Error::Corrupt("surface reference out of range"));
            }
        }
        Ok(())
    }

    /// Checks the collision data (hull lists, leaves, bounds) against the node array.
    /// Returns the number of nodes with a well-formed hull.
    pub fn validate_collision(&self) -> Result<usize> {
        let mut hulls = 0;
        for n in &self.nodes {
            if n.collision_bound >= 0 {
                self.hull(n).ok_or(Error::Corrupt("malformed collision hull"))?;
                hulls += 1;
            }
            if n.render_bound >= self.bounds.len() as i32 {
                return Err(Error::Corrupt("node render bound out of range"));
            }
            if n.leaf.iter().any(|&l| l < -1 || l >= self.leaves.len() as i32) {
                return Err(Error::Corrupt("node leaf out of range"));
            }
        }
        if self.leaves.iter().any(|l| l.zone < 0 || l.zone as usize >= self.zones.len().max(1)) {
            return Err(Error::Corrupt("leaf zone out of range"));
        }
        Ok(hulls)
    }

    /// Collision hull of a node: the bevel planes (node index, negated) and the hull bounding box.
    pub fn hull(&self, n: &Node) -> Option<(Vec<(usize, bool)>, [f32; 3], [f32; 3])> {
        if n.collision_bound < 0 {
            return None;
        }
        let mut i = n.collision_bound as usize;
        let mut planes = Vec::new();
        loop {
            let v = *self.leaf_hulls.get(i)?;
            i += 1;
            if v == -1 {
                break;
            }
            let idx = (v & !HULL_FLIP) as usize;
            if v < 0 || idx >= self.nodes.len() {
                return None;
            }
            planes.push((idx, v & HULL_FLIP != 0));
        }
        let b = self.leaf_hulls.get(i..i + 6)?;
        let f = |k: usize| f32::from_bits(b[k] as u32);
        Some((planes, [f(0), f(1), f(2)], [f(3), f(4), f(5)]))
    }

    /// Polygon (in Unreal coordinates) for a node.
    pub fn node_polygon(&self, n: &Node) -> impl Iterator<Item = [f32; 3]> + '_ {
        let s = n.vert_pool as usize;
        self.verts[s..s + n.num_verts as usize].iter().map(|&p| self.points[p as usize])
    }

    /// Number of BSP nodes that carry renderable polygons.
    pub fn polygon_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.num_verts >= 3).count()
    }
}

/// Picks the level's world model: the Model export with the most serialized data.
/// (Brush models hold editor polygons only and are far smaller.)
pub fn find_world_model(pkg: &Package) -> Option<usize> {
    pkg.exports_of_class("Model").max_by_key(|&i| pkg.exports[i].size)
}
