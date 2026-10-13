//! TerrainInfo actors: heightmap terrain with alpha-blended texture layers and decoration layers.
//!
//! Layout notes (from inspection of the shipped maps):
//! - `TerrainMap` references a G16 texture: one little-endian u16 height per vertex, row-major,
//!   X along world X, Y along world Y.
//! - World position of heightmap vertex (x, y, h):
//!   `Location + ((x - W/2) * Scale.X, (y - H/2) * Scale.Y, (h - 32768) * Scale.Z / 256)`.
//! - `Layers[i]` and `DecoLayers` elements are tagged property lists (struct properties).
//! - `QuadVisibilityBitmap` / `EdgeTurnBitmap` are int arrays holding one bit per heightmap quad.

use crate::library::{Library, Obj};
use crate::package::{ObjRef, Package};
use crate::props::{Properties, Value, begin_object, read_properties};
use crate::reader::Reader;
use crate::texture::{TexFormat, read_texture};
use crate::{Error, Result};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TerrainLayer {
    /// Material drawn by this layer.
    pub texture: ObjRef,
    /// Per-vertex blend weight texture (None = full weight everywhere).
    pub alpha_map: ObjRef,
    pub u_scale: f32,
    pub v_scale: f32,
    pub u_pan: f32,
    pub v_pan: f32,
    /// 0 = XY (top-down), 1 = XZ, 2 = YZ.
    pub map_axis: u8,
    /// Texture rotation in Unreal angle units (65536 = full turn).
    pub rotation: f32,
}

#[derive(Debug, Clone, Default)]
pub struct DecoLayer {
    pub show_on_terrain: i32,
    pub static_mesh: Option<ObjRef>,
    pub scale_map: Option<ObjRef>,
    pub density_map: Option<ObjRef>,
    pub color_map: Option<ObjRef>,
    /// (min, max) for X, Y, Z.
    pub scale_multiplier: [(f32, f32); 3],
    pub fadeout_radius: (f32, f32),
    pub density_multiplier: (f32, f32),
    pub max_per_quad: i32,
    pub seed: i32,
    pub align_to_terrain: bool,
    pub random_yaw: bool,
}

#[derive(Debug, Clone)]
pub struct Terrain {
    pub name: String,
    pub location: [f32; 3],
    pub scale: [f32; 3],
    pub heightmap: ObjRef,
    /// Heightmap size in vertices.
    pub width: u32,
    pub height: u32,
    pub heights: Vec<u16>,
    pub layers: Vec<TerrainLayer>,
    pub deco_layers: Vec<DecoLayer>,
    /// One bit per quad (index y * width + x); set = visible. None = all visible.
    pub quad_visibility: Option<Vec<u32>>,
    /// One bit per quad; set = triangulate along the other diagonal.
    pub edge_turn: Option<Vec<u32>>,
    pub inverted: bool,
}

impl Terrain {
    pub fn height_at(&self, x: u32, y: u32) -> u16 {
        let (x, y) = (x.min(self.width - 1), y.min(self.height - 1));
        self.heights[(y * self.width + x) as usize]
    }

    /// World (Unreal units) position of heightmap vertex (x, y).
    pub fn vertex(&self, x: u32, y: u32) -> [f32; 3] {
        let h = self.height_at(x, y) as f32;
        [
            self.location[0] + (x as f32 - (self.width / 2) as f32) * self.scale[0],
            self.location[1] + (y as f32 - (self.height / 2) as f32) * self.scale[1],
            self.location[2] + (h - 32768.0) * self.scale[2] / 256.0,
        ]
    }

    fn bit(bits: &Option<Vec<u32>>, idx: usize, default: bool) -> bool {
        match bits {
            Some(v) => v.get(idx / 32).map(|w| w & (1 << (idx % 32)) != 0).unwrap_or(default),
            None => default,
        }
    }

    /// Whether the quad with top-left vertex (x, y) is drawn (false = hole).
    pub fn quad_visible(&self, x: u32, y: u32) -> bool {
        Self::bit(&self.quad_visibility, (y * self.width + x) as usize, true)
    }

    pub fn edge_turned(&self, x: u32, y: u32) -> bool {
        Self::bit(&self.edge_turn, (y * self.width + x) as usize, false)
    }
}

/// Export indices of every TerrainInfo actor in a map.
pub fn find_terrains(pkg: &Package) -> Vec<usize> {
    pkg.exports_of_class("TerrainInfo").collect()
}

fn nested(pkg: &Package, raw: &[u8]) -> Option<Properties> {
    let mut r = Reader::new(raw);
    read_properties(&mut r, pkg).ok()
}

fn range(pkg: &Package, props: &Properties, name: &str) -> (f32, f32) {
    match props.get(name) {
        Some(Value::Struct { raw, .. }) => nested(pkg, raw).map(|p| (p.float("Min").unwrap_or(0.0), p.float("Max").unwrap_or(0.0))).unwrap_or_default(),
        _ => (0.0, 0.0),
    }
}

fn int_bits(props: &Properties, name: &str) -> Option<Vec<u32>> {
    let Some(Value::Array { raw }) = props.get(name) else { return None };
    let mut r = Reader::new(raw);
    let v = r.array(|r| r.u32()).ok()?;
    (!v.is_empty()).then_some(v)
}

fn read_layer(pkg: &Package, raw: &[u8]) -> Option<TerrainLayer> {
    let p = nested(pkg, raw)?;
    Some(TerrainLayer {
        texture: p.object("Texture").unwrap_or(ObjRef::Null),
        alpha_map: p.object("AlphaMap").unwrap_or(ObjRef::Null),
        u_scale: p.float("UScale").unwrap_or(1.0),
        v_scale: p.float("VScale").unwrap_or(1.0),
        u_pan: p.float("UPan").unwrap_or(0.0),
        v_pan: p.float("VPan").unwrap_or(0.0),
        map_axis: p.byte("TextureMapAxis").unwrap_or(0),
        rotation: p.float("TextureRotation").unwrap_or(0.0),
    })
}

fn read_deco(pkg: &Package, p: &Properties) -> DecoLayer {
    let obj = |n: &str| p.object(n).filter(|o| *o != ObjRef::Null);
    let mut scale = [(1.0, 1.0); 3];
    if let Some(Value::Struct { raw, .. }) = p.get("ScaleMultiplier") {
        if let Some(rv) = nested(pkg, raw) {
            for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                if rv.get(axis).is_some() {
                    scale[i] = range(pkg, &rv, axis);
                }
            }
        }
    }
    DecoLayer {
        show_on_terrain: p.int("ShowOnTerrain").unwrap_or(0),
        static_mesh: obj("StaticMesh"),
        scale_map: obj("ScaleMap"),
        density_map: obj("DensityMap"),
        color_map: obj("ColorMap"),
        scale_multiplier: scale,
        fadeout_radius: range(pkg, p, "FadeoutRadius"),
        density_multiplier: range(pkg, p, "DensityMultiplier"),
        max_per_quad: p.int("MaxPerQuad").unwrap_or(0),
        seed: p.int("Seed").unwrap_or(0),
        align_to_terrain: p.int("AlignToTerrain").unwrap_or(0) != 0,
        random_yaw: p.int("RandomYaw").unwrap_or(0) != 0,
    }
}

fn read_deco_layers(pkg: &Package, props: &Properties) -> Vec<DecoLayer> {
    let Some(Value::Array { raw }) = props.get("DecoLayers") else { return vec![] };
    let mut r = Reader::new(raw);
    let Ok(n) = r.count() else { return vec![] };
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        match read_properties(&mut r, pkg) {
            Ok(p) => out.push(read_deco(pkg, &p)),
            Err(_) => break,
        }
    }
    out
}

/// Decodes a heightmap texture's top mip into raw 16-bit heights.
pub fn read_heightmap(obj: &Obj) -> Result<(u32, u32, Vec<u16>)> {
    let t = read_texture(&obj.pkg, obj.export)?;
    let mip = t.mips.first().ok_or(Error::Corrupt("heightmap without mips"))?;
    let (w, h) = (mip.width, mip.height);
    let px = (w * h) as usize;
    let heights = match t.format {
        TexFormat::G16 if mip.data.len() >= px * 2 => mip.data.chunks_exact(2).take(px).map(|c| u16::from_le_bytes([c[0], c[1]])).collect(),
        // 8-bit heightmaps (older terrain format) scaled up to the 16-bit range.
        TexFormat::P8 | TexFormat::L8 if mip.data.len() >= px => mip.data[..px].iter().map(|&b| (b as u16) << 8).collect(),
        f => return Err(Error::Unsupported(format!("heightmap format {f:?}"))),
    };
    Ok((w, h, heights))
}

/// Reads a TerrainInfo actor, resolving its heightmap through the library.
pub fn read_terrain(lib: &Library, map: &Arc<Package>, export: usize) -> Result<Terrain> {
    let (_, props) = begin_object(map, export)?;
    let heightmap = props.object("TerrainMap").filter(|o| *o != ObjRef::Null).ok_or(Error::Corrupt("terrain without TerrainMap"))?;
    let hm_obj = lib.resolve(map, heightmap).ok_or_else(|| Error::Unsupported(format!("unresolved heightmap {}", map.full_path(heightmap))))?;
    let (width, height, heights) = read_heightmap(&hm_obj)?;
    if width < 2 || height < 2 {
        return Err(Error::Corrupt("heightmap too small"));
    }
    let mut layers = Vec::new();
    for i in 0..32 {
        if let Some(Value::Struct { raw, .. }) = props.get_at("Layers", i) {
            if let Some(l) = read_layer(map, raw) {
                if l.texture != ObjRef::Null {
                    layers.push(l);
                }
            }
        }
    }
    Ok(Terrain {
        name: map.name(map.exports[export].name).to_string(),
        location: props.vector("Location").unwrap_or([0.0; 3]),
        scale: props.vector("TerrainScale").unwrap_or([64.0, 64.0, 64.0]),
        heightmap,
        width,
        height,
        heights,
        layers,
        deco_layers: read_deco_layers(map, &props),
        quad_visibility: int_bits(&props, "QuadVisibilityBitmap"),
        edge_turn: int_bits(&props, "EdgeTurnBitmap"),
        inverted: props.bool("Inverted").unwrap_or(false),
    })
}

/// Saved static lighting of one TerrainSector: the vertex block it covers and, per static light
/// that reaches it, one visibility bit per vertex (the editor's shadow test, LSB first, row-major
/// over (quads_x + 1) * (quads_y + 1) vertices). UE2 rebuilds terrain vertex colours from these
/// at load (UTerrainSector::StaticLight).
#[derive(Debug, Clone)]
pub struct SectorLighting {
    pub offset: (u32, u32),
    pub quads: (u32, u32),
    /// (light actor export, visibility bits)
    pub lights: Vec<(usize, Vec<u8>)>,
}

impl SectorLighting {
    pub fn visible(bits: &[u8], i: usize) -> bool {
        bits.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)
    }
}

/// Reads the TerrainSector exports that belong to the TerrainInfo `terrain` (export index).
/// Layout after the tagged properties: compact terrain ref, QuadsX, QuadsY, OffsetX, OffsetY
/// (i32), FBox bounds (25 bytes), then TArray of { compact light ref, TArray<u8> bits }.
pub fn read_sector_lighting(pkg: &Package, terrain: usize) -> Vec<SectorLighting> {
    let mut out = Vec::new();
    for i in pkg.exports_of_class("TerrainSector").collect::<Vec<_>>() {
        let parse = || -> Option<SectorLighting> {
            let (r0, _) = begin_object(pkg, i).ok()?;
            let start = r0.pos();
            let end = pkg.exports[i].offset + pkg.exports[i].size;
            let mut r = Reader::new(pkg.data.get(start..end)?);
            if ObjRef::from_raw(r.compact().ok()?) != ObjRef::Export(terrain) {
                return None;
            }
            let qx = r.i32().ok()?;
            let qy = r.i32().ok()?;
            let ox = r.i32().ok()?;
            let oy = r.i32().ok()?;
            for _ in 0..25 {
                r.u8().ok()?;
            }
            let n = r.count().ok()?;
            let mut lights = Vec::with_capacity(n);
            for _ in 0..n {
                let l = ObjRef::from_raw(r.compact().ok()?);
                let bits = r.array(|r| r.u8()).ok()?;
                if let ObjRef::Export(e) = l {
                    lights.push((e, bits));
                }
            }
            Some(SectorLighting { offset: (ox.max(0) as u32, oy.max(0) as u32), quads: (qx.max(0) as u32, qy.max(0) as u32), lights })
        };
        if let Some(s) = parse() {
            out.push(s);
        }
    }
    out
}

/// Decodes a layer alpha map to one weight byte per texel.
///
/// Paletted alpha maps carry the weight in the palette's red channel; other formats in alpha
/// (or luminance for single-channel formats).
pub fn read_alpha_map(obj: &Obj) -> Result<(u32, u32, Vec<u8>)> {
    let t = read_texture(&obj.pkg, obj.export)?;
    if t.format == TexFormat::G16 {
        let (w, h, _) = read_heightmap(obj)?;
        let mip = &t.mips[0];
        return Ok((w, h, mip.data.chunks_exact(2).take((w * h) as usize).map(|c| c[1]).collect()));
    }
    let (w, h, rgba) = t.decode_rgba(0)?;
    let ch = match t.format {
        TexFormat::P8 | TexFormat::L8 => 0,
        _ => 3,
    };
    Ok((w, h, rgba.chunks_exact(4).map(|p| p[ch]).collect()))
}
