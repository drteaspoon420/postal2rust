//! Builds renderable scene data from a map package, independent of the Bevy ECS.
//!
//! Coordinate convention: Unreal is left-handed Z-up (X forward, Y right). Bevy is right-handed
//! Y-up. Swapping Y and Z converts between them (a single reflection fixes handedness), and
//! everything is scaled from Unreal units to roughly metres.

use bevy::math::{Mat3, Mat4, Vec3};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::model::{find_world_model, pf, read_model};
use upkg::staticmesh::read_static_mesh;
use upkg::texture::{TexFormat, read_texture};
use upkg::{ObjRef, Package, Value, begin_object};

/// Unreal units per metre (1 UU is about 1.9 cm).
pub const UU_PER_M: f32 = 52.5;

#[inline]
pub fn to_bevy(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], p[1]) / UU_PER_M
}

#[inline]
fn dir_to_bevy(p: [f32; 3]) -> Vec3 {
    Vec3::new(p[0], p[2], p[1])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bc1,
    Bc2,
    Bc3,
    Rgba8,
}

pub struct TexData {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Mip levels, largest first.
    pub mips: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Blend {
    #[default]
    Opaque,
    Masked,
    Translucent,
    /// Framebuffer multiplied by the texture, unlit (UE2 modulate projectors: window grilles).
    Modulate,
    /// Texture added to the framebuffer, unlit (UE2 additive projectors: rotating alarm lights).
    Additive,
    /// Framebuffer multiplied by (1 - texture), unlit (Shader OB_Darken: sign grime). The
    /// texture is stored inverted so this draws as a multiply.
    Darken,
}

#[derive(Clone)]
pub struct MatData {
    pub texture: Option<u32>,
    pub blend: Blend,
    pub two_sided: bool,
    /// Texture size in texels (used for BSP UV scaling).
    pub size: (f32, f32),
    /// TexPanner/TexRotator/TexOscillator wrappers (outermost first), animated each frame.
    pub uv_anim: Vec<upkg::material::Material>,
    /// Animated texture (UE2 AnimNext frame chain): texture ids of every frame, starting with
    /// `texture`, and the playback rate in frames per second. Empty for still textures.
    pub frames: Vec<u32>,
    pub frame_rate: f32,
}

#[derive(Default)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Optional second UV set (terrain layers: alpha map coordinates).
    pub uvs_b: Vec<[f32; 2]>,
    /// Optional per-vertex RGBA.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub material: u32,
    /// Static mesh sections: source vertex index in the StaticMesh for each vertex (used to apply
    /// per-actor baked vertex lighting). Empty for other meshes.
    pub source: Vec<u32>,
    /// BSP: lightmap atlas index (into `SceneData::lightmaps`) for this batch, if lightmapped.
    pub lightmap: Option<u32>,
}

/// A decoded BSP lightmap atlas (RGBA8, linear light where 128 = 1.0 like UE2's modulate-2x).
pub struct LightmapData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One terrain layer drawn as its own mesh; layers after the first blend over it using their
/// alpha map, sampled per pixel through UV1 (heightmap-normalised coordinates).
pub struct TerrainLayerMesh {
    pub mesh: MeshData,
    pub texture: Option<u32>,
    /// Draw order within the terrain (0 = opaque base layer).
    pub order: u32,
    /// Blend weight map (width, height, one byte per texel); None = full weight.
    pub alpha: Option<(u32, u32, Vec<u8>)>,
}

/// A decoration (grass, weeds, rocks) scattered over a terrain from a DecoLayer.
pub struct DecoInstance {
    pub mesh: u32,
    pub material: u32,
    pub transform: Mat4,
    /// Fade-out band (metres): visible up to .0, gone after .1.
    pub fade: (f32, f32),
}

/// The level's sky zone: its contents render from a second camera placed at `origin`
/// that copies the main camera's rotation only.
pub struct SkyData {
    pub origin: Vec3,
    pub world: Vec<MeshData>,
    pub instances: Vec<Instance>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightShape {
    Point,
    Spot,
}

/// A map light converted to Bevy space.
pub struct LightData {
    pub shape: LightShape,
    pub position: Vec3,
    pub direction: Vec3,
    /// sRGB-ish colour 0..1.
    pub color: [f32; 3],
    /// Unreal LightBrightness (nominally 0-255).
    pub brightness: f32,
    /// Influence radius in metres.
    pub range: f32,
    /// Spot half-angle in radians.
    pub cone: f32,
    /// Time modulation (pulse, flicker, ...), with LightPeriod and LightPhase.
    pub modulation: upkg::lighting::LightType,
    pub period: u8,
    pub phase: u8,
}

/// Combined level sunlight (all LE_Sunlight lights merged).
pub struct SunData {
    /// Direction the light travels, Bevy space.
    pub direction: Vec3,
    pub color: [f32; 3],
    pub brightness: f32,
    /// Sunlights grouped by direction: (direction, normalised colour, summed brightness).
    /// Maps light terrain from several directions (Church: 3), merging them onto one direction
    /// left slopes facing away from the brightest sun near black (FIDELITY #37).
    pub parts: Vec<(Vec3, [f32; 3], f32)>,
}

/// Zone ambient light and distance fog (metres).
pub struct ZoneData {
    pub ambient: [f32; 3],
    pub ambient_brightness: f32,
    /// Ambient for dynamically lit geometry (terrain zone on outdoor levels, else as above).
    pub dynamic_ambient: [f32; 3],
    pub dynamic_ambient_brightness: f32,
    pub fog: Option<([u8; 3], f32, f32)>,
}

pub struct Instance {
    pub mesh: u32,
    pub material: u32,
    pub transform: Mat4,
    /// Distance (metres) beyond which the original game culls this actor; 0 = never.
    pub cull_distance: f32,
    /// Export index (in the map package) of the actor this instance came from.
    pub export: usize,
    /// Baked per-vertex lighting for this placement (StaticMeshInstance colours, RGBA8 in the
    /// source StaticMesh's vertex order; map through `MeshData::source`).
    pub light: Option<Arc<Vec<[u8; 4]>>>,
    /// Actor AmbientGlow (0-254, same 0-255 scale as the baked colours): added to its lighting.
    pub glow: f32,
    /// Actor bUnlit: drawn fullbright, ignoring all lighting.
    pub unlit: bool,
    /// Ambient (0-255 scale, colour * brightness) of the zone the actor was saved in, if it
    /// sets one: UE2 adds that zone's ambient to the baked vertex light at render time.
    pub zone_ambient: Option<[f32; 3]>,
}

/// A Mover actor (doors, lifts, lockers). In the shipped maps every mover is a static mesh,
/// rendered at keyframe 0 as a regular instance; this keeps its keyframes for animation.
#[derive(Clone, Debug)]
pub struct MoverData {
    pub export: usize,
    pub class: String,
    pub tag: Option<String>,
    /// Base location (metres, Bevy space) and rotation (Unreal rotator) of keyframe 0.
    pub base_pos: Vec3,
    pub base_rot: [i32; 3],
    /// Keyframe offsets relative to the base: (position delta in metres, Bevy space; rotator).
    pub keys: Vec<(Vec3, [i32; 3])>,
    /// Seconds to move between keyframes.
    pub move_time: f32,
}

#[derive(Default)]
pub struct Stats {
    pub bsp_polys: usize,
    pub static_mesh_actors: usize,
    pub unique_meshes: usize,
    pub textures: usize,
    pub bc_textures: usize,
    pub unresolved: usize,
    pub terrains: usize,
    pub terrain_tris: usize,
    pub deco_instances: usize,
    pub sky_polys: usize,
    pub sky_actors: usize,
    pub lightmaps: usize,
    pub lightmapped_polys: usize,
    pub baked_actors: usize,
    pub projectors: usize,
    pub lights: usize,
    pub load_ms: u128,
}

pub struct SceneData {
    pub textures: Vec<TexData>,
    pub materials: Vec<MatData>,
    /// Static mesh sections shared between instances.
    pub meshes: Vec<MeshData>,
    pub instances: Vec<Instance>,
    /// Mover actors (their meshes are also in `instances`, keyed by `export`).
    pub movers: Vec<MoverData>,
    /// World BSP, one batched mesh per material.
    pub world: Vec<MeshData>,
    /// BSP lightmap atlases referenced by `MeshData::lightmap`.
    pub lightmaps: Vec<LightmapData>,
    /// Terrain layer meshes, in draw order per terrain.
    pub terrain: Vec<TerrainLayerMesh>,
    pub deco: Vec<DecoInstance>,
    pub sky: Option<SkyData>,
    pub lights: Vec<LightData>,
    pub sun: Option<SunData>,
    pub zone: Option<ZoneData>,
    pub spawn: (Vec3, Vec3),
    pub spawn_found: bool,
    pub stats: Stats,
}

pub struct Options {
    /// Upload DXT textures compressed (GPU block compression) instead of decoding to RGBA.
    pub gpu_bc: bool,
    /// Bake level Projector decals into the scene. p2game leaves them to the script (every
    /// Projector calls AttachProjector in PostBeginPlay, drawn by p2fx); baking them too drew
    /// each decal twice.
    pub projectors: bool,
}

pub struct Builder<'l> {
    pub lib: &'l Library,
    mat_ids: HashMap<(String, usize), u32>,
    tex_ids: HashMap<(String, usize, String, usize), u32>,
    tex_objs: Vec<(Obj, Option<Obj>, bool)>,
    pub materials: Vec<MatData>,
    mesh_ids: HashMap<(String, usize), Vec<(u32, u32)>>,
    pub meshes: Vec<MeshData>,
    stats: Stats,
}

/// Zone number (index into `Model::zones`) of the BSP leaf containing an Unreal-space point.
fn point_zone(m: &upkg::model::Model, p: [f32; 3]) -> Option<usize> {
    let mut i = 0usize;
    for _ in 0..4096 {
        let n = m.nodes.get(i)?;
        let d = n.plane[0] * p[0] + n.plane[1] * p[1] + n.plane[2] * p[2] - n.plane[3];
        let front = d >= 0.0;
        let child = if front { n.front } else { n.back };
        if child < 0 {
            let leaf = n.leaf[front as usize];
            return (leaf >= 0).then(|| m.leaves.get(leaf as usize).map(|l| l.zone as usize)).flatten();
        }
        i = child as usize;
    }
    None
}

/// The zone actor an actor was last in (its saved `Region.Zone`), if recorded.
fn actor_zone(map: &Package, props: &upkg::Properties) -> Option<ObjRef> {
    let Some(Value::Struct { raw, .. }) = props.get("Region") else { return None };
    let mut r = upkg::Reader::new(raw);
    let p = upkg::read_properties(&mut r, map).ok()?;
    p.object("Zone")
}

impl<'l> Builder<'l> {
    fn default_material(&mut self) -> u32 {
        self.material_for(None)
    }

    /// Material id for a resolved material object (None = untextured grey).
    pub(crate) fn material_for(&mut self, mat: Option<Obj>) -> u32 {
        let key = mat.as_ref().map(|m| m.key()).unwrap_or_default();
        if let Some(&id) = self.mat_ids.get(&key) {
            return id;
        }
        let info = mat.as_ref().and_then(|m| self.lib.material_texture(m));
        let mut data = MatData { texture: None, blend: Blend::Opaque, two_sided: false, size: (256.0, 256.0), uv_anim: Vec::new(), frames: Vec::new(), frame_rate: 0.0 };
        if let Some(info) = info {
            data.two_sided = info.two_sided;
            data.uv_anim = info.uv_anim.clone();
            let op_blend = match info.op {
                Some(upkg::library::BlendOp::Modulate) => Some(Blend::Modulate),
                Some(upkg::library::BlendOp::Darken) => Some(Blend::Darken),
                Some(upkg::library::BlendOp::Brighten | upkg::library::BlendOp::Translucent) => Some(Blend::Additive),
                None => None,
            };
            let invert = op_blend == Some(Blend::Darken);
            if info.blended {
                data.blend = Blend::Translucent;
            } else if info.masked {
                data.blend = Blend::Masked;
            }
            if let Some(tex) = info.texture {
                if let Ok((_, props)) = begin_object(&tex.pkg, tex.export) {
                    data.size = (props.int("USize").unwrap_or(256) as f32, props.int("VSize").unwrap_or(256) as f32);
                    if props.bool("bMasked") == Some(true) && data.blend == Blend::Opaque {
                        data.blend = Blend::Masked;
                    }
                    if props.bool("bAlphaTexture") == Some(true) && data.blend == Blend::Opaque {
                        data.blend = Blend::Translucent;
                    }
                    if props.bool("bTwoSided") == Some(true) {
                        data.two_sided = true;
                    }
                }
                let tid = self.texture_id(&tex, info.opacity.clone(), invert);
                data.texture = Some(tid);
                // Animated textures (TV static, arcade screens): UE2 advances along AnimNext each
                // tick, wrapping to the head when a frame has none, at MaxFrameRate (else
                // MinFrameRate). suburbs-3's big-screen TV (ben_skins fuzzyScreen_layered ->
                // staticFuzz01_ben -> 02 -> 03, 12 fps) showed only the first frame.
                let (frames, rate) = self.anim_chain(&tex);
                if frames.len() > 1 && std::env::var_os("P2VIEW_NO_TEX_ANIM").is_none() {
                    data.frames = frames.iter().map(|f| self.texture_id(f, info.opacity.clone(), invert)).collect();
                    data.frame_rate = rate;
                }
            }
            if let Some(b) = op_blend {
                data.blend = b;
            }
        } else if mat.is_some() {
            self.stats.unresolved += 1;
        }
        let id = self.materials.len() as u32;
        self.materials.push(data);
        self.mat_ids.insert(key, id);
        id
    }

    /// Texture id for a texture object (with optional opacity texture / inversion), decoded once.
    fn texture_id(&mut self, tex: &Obj, opacity: Option<Obj>, invert: bool) -> u32 {
        let (k0, k1) = tex.key();
        let (o0, o1) = opacity.as_ref().map(|o| o.key()).unwrap_or_default();
        let tk = (k0, k1, if invert { format!("{o0}#inv") } else { o0 }, o1);
        *self.tex_ids.entry(tk).or_insert_with(|| {
            self.tex_objs.push((tex.clone(), opacity, invert));
            (self.tex_objs.len() - 1) as u32
        })
    }

    /// A texture's AnimNext frame chain (head first, at most 64 frames) and its frame rate:
    /// the head's MaxFrameRate, else MinFrameRate, else 10 fps (as p2canvas eval.rs).
    fn anim_chain(&self, head: &Obj) -> (Vec<Obj>, f32) {
        let mut frames = vec![head.clone()];
        let mut rate = 0.0;
        let mut cur = head.clone();
        while frames.len() < 64 {
            let Ok((_, props)) = begin_object(&cur.pkg, cur.export) else { break };
            if frames.len() == 1 {
                let max = props.float("MaxFrameRate").unwrap_or(0.0);
                let min = props.float("MinFrameRate").unwrap_or(0.0);
                rate = if max > 0.0 { max } else if min > 0.0 { min } else { 10.0 };
            }
            let Some(next) = props.object("AnimNext").filter(|o| *o != ObjRef::Null).and_then(|o| self.lib.resolve(&cur.pkg, o)) else { break };
            if next.key() == head.key() || frames.iter().any(|f| f.key() == next.key()) || !next.class().eq_ignore_ascii_case("Texture") {
                break;
            }
            frames.push(next.clone());
            cur = next;
        }
        (frames, rate)
    }

    /// Converts a StaticMesh into shared per-section meshes; returns (mesh id, default material id) per section.
    pub(crate) fn static_mesh(&mut self, obj: &Obj) -> Vec<(u32, u32)> {
        if let Some(v) = self.mesh_ids.get(&obj.key()) {
            return v.clone();
        }
        let mut out = Vec::new();
        if let Ok(sm) = read_static_mesh(&obj.pkg, obj.export) {
            let positions: Vec<[f32; 3]> = sm.positions.iter().map(|&p| to_bevy(p).to_array()).collect();
            let normals: Vec<[f32; 3]> = sm.normals.iter().map(|&n| dir_to_bevy(n).normalize_or_zero().to_array()).collect();
            let uvs = if sm.uvs.len() == positions.len() { sm.uvs.clone() } else { vec![[0.0, 0.0]; positions.len()] };
            // Decide winding once per mesh by comparing triangle normals with vertex normals.
            let mut score = 0.0f32;
            for t in sm.indices.chunks_exact(3) {
                let (a, b, c) = (Vec3::from(positions[t[0] as usize]), Vec3::from(positions[t[1] as usize]), Vec3::from(positions[t[2] as usize]));
                let n = Vec3::from(normals[t[0] as usize]) + Vec3::from(normals[t[1] as usize]) + Vec3::from(normals[t[2] as usize]);
                score += (b - a).cross(c - a).dot(n).signum();
            }
            let flip = score < 0.0;
            for s in 0..sm.sections.len() {
                let idx = sm.section_indices(s);
                if idx.is_empty() {
                    continue;
                }
                // Compact each section to its own vertex range so sections render independently.
                let mut remap: HashMap<u16, u32> = HashMap::new();
                let mut md = MeshData::default();
                for t in idx.chunks_exact(3) {
                    let order = if flip { [t[0], t[2], t[1]] } else { [t[0], t[1], t[2]] };
                    for v in order {
                        let id = *remap.entry(v).or_insert_with(|| {
                            md.positions.push(positions[v as usize]);
                            md.normals.push(normals[v as usize]);
                            md.uvs.push(uvs[v as usize]);
                            md.source.push(v as u32);
                            (md.positions.len() - 1) as u32
                        });
                        md.indices.push(id);
                    }
                }
                let mat = sm.materials.get(s).copied().unwrap_or(ObjRef::Null);
                let mat_obj = self.lib.resolve(&obj.pkg, mat);
                md.material = self.material_for(mat_obj);
                let mid = self.meshes.len() as u32;
                out.push((mid, md.material));
                self.meshes.push(md);
            }
            self.stats.unique_meshes += 1;
        } else {
            self.stats.unresolved += 1;
        }
        self.mesh_ids.insert(obj.key(), out.clone());
        out
    }
}

fn rotator_matrix(r: [i32; 3]) -> Mat3 {
    let k = std::f32::consts::TAU / 65536.0;
    let (sp, cp) = (r[0] as f32 * k).sin_cos();
    let (sy, cy) = (r[1] as f32 * k).sin_cos();
    let (sr, cr) = (r[2] as f32 * k).sin_cos();
    // Columns are the rotated X, Y, Z axes expressed in world space (Unreal convention).
    let x = Vec3::new(cp * cy, cp * sy, sp);
    let y = Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
    let z = Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp);
    Mat3::from_cols(x, y, z)
}

/// Axis swap Y<->Z (its own inverse).
const SWAP: Mat3 = Mat3::from_cols(Vec3::X, Vec3::Z, Vec3::Y);

fn object_array(props: &upkg::Properties, name: &str) -> Vec<ObjRef> {
    let Some(Value::Array { raw }) = props.get(name) else { return vec![] };
    let mut r = upkg::Reader::new(raw);
    let Ok(n) = r.count() else { return vec![] };
    (0..n).map_while(|_| r.compact().ok().map(ObjRef::from_raw)).collect()
}

pub fn build_scene(lib: &Library, map: &Arc<Package>, opts: &Options) -> upkg::Result<SceneData> {
    let t0 = std::time::Instant::now();
    lib.insert(map.clone());
    let mut b = Builder {
        lib,
        mat_ids: HashMap::new(),
        tex_ids: HashMap::new(),
        tex_objs: Vec::new(),
        materials: Vec::new(),
        mesh_ids: HashMap::new(),
        meshes: Vec::new(),
        stats: Stats::default(),
    };
    b.default_material();

    // --- World BSP, batched per (material, lightmap atlas). Sky zone polygons go to the sky. ---
    let mut world: HashMap<(u32, i32), MeshData> = HashMap::new();
    let mut sky_world: HashMap<(u32, i32), MeshData> = HashMap::new();
    let model = find_world_model(map).map(|mi| read_model(map, mi)).transpose()?;
    let sky_zone = model.as_ref().and_then(|m| m.zones.iter().position(|z| map.class_of(*z).eq_ignore_ascii_case("SkyZoneInfo")));
    let sky_ref = sky_zone.and_then(|i| model.as_ref().map(|m| m.zones[i]));
    let sky_origin = sky_ref.and_then(|r| match r {
        ObjRef::Export(e) => begin_object(map, e).ok().and_then(|(_, p)| p.vector("Location")).map(to_bevy),
        _ => None,
    });
    if let Some(model) = &model {
        for n in &model.nodes {
            if n.num_verts < 3 {
                continue;
            }
            let s = model.surfs[n.surf as usize];
            if s.flags & (pf::INVISIBLE | pf::FAKE_BACKDROP | pf::PORTAL) != 0 {
                continue;
            }
            let in_sky = sky_zone.is_some_and(|z| n.zone[0] as usize == z || n.zone[1] as usize == z);
            let mat_obj = lib.resolve(map, s.material);
            let mat = b.material_for(mat_obj);
            if s.flags & pf::TWO_SIDED != 0 {
                b.materials[mat as usize].two_sided = true;
            }
            let (usz, vsz) = b.materials[mat as usize].size;
            let base = Vec3::from(model.points[s.base as usize]);
            let tu = Vec3::from(model.vectors[s.texture_u as usize]);
            let tv = Vec3::from(model.vectors[s.texture_v as usize]);
            let normal = dir_to_bevy([n.plane[0], n.plane[1], n.plane[2]]).normalize_or_zero();
            // Lightmap: the node's render-section vertices carry atlas UVs in polygon order.
            let section = (n.light_map >= 0).then(|| model.sections.get(n.section as usize)).flatten();
            let lm_tex = section.map(|sec| sec.light_map_texture).filter(|&t| t >= 0 && (t as usize) < model.light_map_textures.len()).unwrap_or(-1);
            let target = if in_sky { &mut sky_world } else { &mut world };
            let md = target.entry((mat, lm_tex)).or_insert_with(|| MeshData { material: mat, lightmap: (lm_tex >= 0).then_some(lm_tex as u32), ..Default::default() });
            let start = md.positions.len() as u32;
            let poly: Vec<[f32; 3]> = model.node_polygon(n).collect();
            for (k, p) in poly.iter().enumerate() {
                let d = Vec3::from(*p) - base;
                md.positions.push(to_bevy(*p).to_array());
                md.normals.push(normal.to_array());
                md.uvs.push([d.dot(tu) / usz, d.dot(tv) / vsz]);
                if lm_tex >= 0 {
                    let uv = section.and_then(|sec| sec.vertices.get(n.first_vertex as usize + k)).map(|v| v.lightmap_uv).unwrap_or([0.0, 0.0]);
                    md.uvs_b.push(uv);
                }
            }
            // Fan triangulation, oriented so the geometric normal matches the plane normal.
            // Use the summed fan normal: BSP polygons often start with collinear vertices (a split
            // edge), which made a single-triangle test degenerate and culled the whole polygon.
            let p0 = to_bevy(poly[0]);
            let tri_n = (1..poly.len() - 1).fold(Vec3::ZERO, |acc, k| acc + (to_bevy(poly[k]) - p0).cross(to_bevy(poly[k + 1]) - p0));
            let flip = tri_n.dot(normal) < 0.0;
            for k in 1..(poly.len() as u32 - 1) {
                if flip {
                    md.indices.extend([start, start + k + 1, start + k]);
                } else {
                    md.indices.extend([start, start + k, start + k + 1]);
                }
            }
            b.stats.bsp_polys += 1;
            if in_sky {
                b.stats.sky_polys += 1;
            }
        }
    }

    // --- Actors ---
    // Sky zone box (Bevy space): actors placed inside it belong to the sky sub-scene even when their
    // saved Region names another zone (sky cylinders are often stored outside the BSP, iLeaf -1).
    let sky_box = sky_world.values().flat_map(|m| m.positions.iter()).fold(None, |acc: Option<(Vec3, Vec3)>, p| {
        let p = Vec3::from(*p);
        Some(acc.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))))
    });
    let mut instances = Vec::new();
    let mut movers = Vec::new();
    let mut sky_instances = Vec::new();
    let mut spawn = (Vec3::new(0.0, 2.0, 0.0), Vec3::X);
    let mut spawn_found = false;
    let mut light_cache: HashMap<usize, Option<upkg::lighting::MapLight>> = HashMap::new();
    // World BSP for the leaf light lists of unbaked meshes.
    let bsp = model.as_ref().map(p2collide::bsp::Bsp::new);
    // Zone ambient per zone actor, for static meshes' baked lighting (LevelInfo included).
    let zone_amb: HashMap<ObjRef, [f32; 3]> = upkg::lighting::read_zones(map)
        .iter()
        .filter(|z| z.ambient_brightness > 0)
        .map(|z| {
            let c = upkg::lighting::hsv_to_rgb(z.ambient_hue, z.ambient_saturation);
            let b = if std::env::var_os("P2VIEW_MESH_AMB_LINEAR").is_some() { z.ambient_brightness as f32 } else { (z.ambient_brightness as f32 * 102.0).sqrt() }; // FGetHSV sqrt brightness curve (as terrain), normalised at suburbs-3's terrain zone 102 where spawn.rs's 0.65 was calibrated; LowerParadise (51) meshes were ~30% dark
            (ObjRef::Export(z.export), [c[0] * b, c[1] * b, c[2] * b])
        })
        .collect();
    for i in 0..map.exports.len() {
        let class = map.export_class(i);
        if class.eq_ignore_ascii_case("PlayerStart") {
            if let Ok((_, p)) = begin_object(map, i) {
                if let Some(loc) = p.vector("Location") {
                    let yaw = p.rotator("Rotation").map(|r| r[1]).unwrap_or(0) as f32 * std::f32::consts::TAU / 65536.0;
                    spawn = (to_bevy(loc), Vec3::new(yaw.cos(), 0.0, yaw.sin()));
                    spawn_found = true;
                }
            }
            continue;
        }
        // Cheap pre-filter: only actors whose class name suggests they can carry a static mesh are parsed fully.
        if class == "Class" || class.ends_with("Property") || class == "Function" || class == "Polys" || class == "Model" {
            continue;
        }
        let Ok((_, props)) = begin_object(map, i) else { continue };
        let Some(sm_ref) = props.object("StaticMesh") else { continue };
        if props.bool("bHidden") == Some(true) {
            continue;
        }
        let Some(sm_obj) = lib.resolve(map, sm_ref) else {
            b.stats.unresolved += 1;
            continue;
        };
        let sections = b.static_mesh(&sm_obj);
        if sections.is_empty() {
            continue;
        }
        let loc = props.vector("Location").unwrap_or([0.0; 3]);
        let rot = props.rotator("Rotation").unwrap_or([0; 3]);
        let ds = props.float("DrawScale").unwrap_or(1.0);
        let ds3 = match props.get("DrawScale3D") {
            Some(Value::Vector(v)) => Vec3::from(*v),
            _ => Vec3::ONE,
        };
        let lin = rotator_matrix(rot) * Mat3::from_diagonal(ds3 * ds);
        let lin_b = SWAP * lin * SWAP;
        let transform = Mat4::from_cols(
            lin_b.x_axis.extend(0.0),
            lin_b.y_axis.extend(0.0),
            lin_b.z_axis.extend(0.0),
            to_bevy(loc).extend(1.0),
        );
        let skins = object_array(&props, "Skins");
        let cull = props.float("CullDistance").unwrap_or(0.0) / UU_PER_M;
        let in_sky = sky_ref.is_some() && actor_zone(map, &props) == sky_ref
            || sky_box.is_some_and(|(lo, hi)| {
                // World-space centre of the mesh bounds.
                let (mut a, mut c) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
                for (mesh, _) in &sections {
                    for p in &b.meshes[*mesh as usize].positions {
                        let w = transform.transform_point3(Vec3::from(*p));
                        a = a.min(w);
                        c = c.max(w);
                    }
                }
                let mid = (a + c) * 0.5;
                mid.cmpge(lo).all() && mid.cmple(hi).all()
            });
        // Baked vertex lighting from the actor's StaticMeshInstance (unless lit specially).
        // Streams whose alpha is zero everywhere were never computed by the editor: ignore them.
        // (Checked: such streams are all-black RGB too, smi_stats; keeping them darkens night
        // statics far below the original, which lights them dynamically.)
        let light = match props.object("StaticMeshInstance") {
            Some(ObjRef::Export(e)) if props.bool("bUnlit") != Some(true) => upkg::staticmesh::read_instance_colors(map, e)
                .ok()
                .and_then(|c| if c.iter().any(|v| v[3] != 0) { Some(c) } else { instance_static_light(map, e, &sm_obj, lin, Vec3::from(loc), &mut light_cache) })
                .map(Arc::new),
            // No StaticMeshInstance (movers/doors, non-static decorations): UE2 lights the actor
            // per actor, zone ambient + AmbientGlow + the lights that reach its location
            // (bSpecialLit: only bSpecialLit lights). Bevy's dynamic lights ignored the glow and
            // special-lit flag (slaughterhouse freezer door 73 vs orig 134, MilitaryBase3 door 58 vs 28).
            None if props.bool("bUnlit") != Some(true) && std::env::var_os("P2VIEW_ACTOR_DYNAMIC").is_none() => {
actor_static_light(&sm_obj, lin, Vec3::from(loc), props.bool("bSpecialLit") == Some(true), model.as_ref(), bsp.as_ref(), &mut light_cache, map).map(Arc::new)
            }
            _ => None,
        };
        if light.is_some() {
            b.stats.baked_actors += 1;
        }
        // 255 means pulsing in UE2; treat it as no glow.
        let glow = props.byte("AmbientGlow").filter(|&g| g < 255).unwrap_or(0) as f32;
        let zone_ambient = actor_zone(map, &props).and_then(|zr| zone_amb.get(&zr).copied());
        for (s, (mesh, default_mat)) in sections.iter().enumerate() {
            let material = match skins.get(s).copied().filter(|r| *r != ObjRef::Null) {
                Some(r) => {
                    let o = lib.resolve(map, r);
                    b.material_for(o)
                }
                None => *default_mat,
            };
            let inst = Instance { mesh: *mesh, material, transform, cull_distance: cull, export: i, light: light.clone(), glow, unlit: props.bool("bUnlit") == Some(true), zone_ambient };
            if in_sky {
                sky_instances.push(inst);
            } else {
                instances.push(inst);
            }
        }
        if in_sky {
            b.stats.sky_actors += 1;
        }
        if class.to_ascii_lowercase().contains("mover") {
            let keys = (0..8u32)
                .filter_map(|k| {
                    let p = match props.get_at("KeyPos", k) {
                        Some(Value::Vector(v)) => Some(*v),
                        _ => None,
                    };
                    let r = match props.get_at("KeyRot", k) {
                        Some(Value::Rotator(v)) => Some(*v),
                        _ => None,
                    };
                    (k == 0 || p.is_some() || r.is_some()).then(|| (to_bevy(p.unwrap_or([0.0; 3])), r.unwrap_or([0; 3])))
                })
                .collect();
            movers.push(MoverData {
                export: i,
                class: class.to_string(),
                tag: match props.get("Tag") {
                    Some(Value::Name(n)) => Some(n.clone()),
                    _ => None,
                },
                base_pos: to_bevy(props.vector("BasePos").unwrap_or(loc)),
                base_rot: props.rotator("BaseRot").unwrap_or(rot),
                keys,
                move_time: props.float("MoveTime").unwrap_or(1.0),
            });
        }
        b.stats.static_mesh_actors += 1;
    }

    // --- Terrain and its decoration layers ---
    let mut terrain = Vec::new();
    let mut deco = Vec::new();
    let terrain_clip = model.as_ref().and_then(|m| crate::terrain_clip::TerrainClip::new(map, m));
    for ti in upkg::terrain::find_terrains(map) {
        match upkg::terrain::read_terrain(lib, map, ti) {
            Ok(t) => {
                let t0 = std::time::Instant::now();
                let lit = terrain_static_light(map, ti, &t);
                if std::env::var_os("P2VIEW_TERRAIN_TIMING").is_some() {
                    eprintln!("terrain static light: {:?} in {:.1} ms", lit.as_ref().map(|l| l.len()), t0.elapsed().as_secs_f32() * 1000.0);
                }
                let layers = build_terrain(&mut b, map, &t, lit.as_deref(), terrain_clip.as_ref());
                b.stats.terrains += 1;
                b.stats.terrain_tris += layers.iter().map(|l| l.mesh.indices.len() / 3).sum::<usize>();
                terrain.extend(layers);
                deco.extend(crate::outdoor::build_deco(&mut b, map, &t, terrain_clip.as_ref()));
            }
            Err(e) => eprintln!("terrain {} failed: {e}", map.name(map.exports[ti].name)),
        }
    }
    b.stats.deco_instances = deco.len();

    // --- Lights and zone settings ---
    let (lights, sun) = build_lights(map, spawn.0);
    b.stats.lights = lights.len() + sun.is_some() as usize;
    let zones = upkg::lighting::read_zones(map);
    // Ambient for dynamically lit geometry (terrain, unbaked meshes, pawns): outdoor levels light
    // those from their terrain zone (suburbs-3: LevelInfo 23, terrain ZoneInfo 102).
    // The zone containing the player start (BSP point query), if it sets ambient; else the
    // first terrain zone. ngt-suburbs-3 has two terrain zones (ambient 11 at the start, 36 elsewhere).
    let start_ue = [spawn.0.x * UU_PER_M, spawn.0.z * UU_PER_M, spawn.0.y * UU_PER_M];
    let start_zone = model.as_ref().and_then(|m| point_zone(m, start_ue)).and_then(|zi| model.as_ref().and_then(|m| m.zones.get(zi).copied()));
    let amb_zone = start_zone
        .and_then(|zr| zones.iter().find(|z| ObjRef::Export(z.export) == zr && z.ambient_brightness > 0))
        .or_else(|| zones.iter().find(|z| z.terrain_zone && !z.sky_zone && z.ambient_brightness > 0));
    let zone = upkg::lighting::primary_zone(&zones).map(|z| {
        let a = amb_zone.unwrap_or(z);
        ZoneData {
            ambient: upkg::lighting::hsv_to_rgb(z.ambient_hue, z.ambient_saturation),
            ambient_brightness: z.ambient_brightness as f32,
            dynamic_ambient: upkg::lighting::hsv_to_rgb(a.ambient_hue, a.ambient_saturation),
            dynamic_ambient_brightness: a.ambient_brightness as f32,
            fog: (z.distance_fog && z.fog_end > z.fog_start)
                .then(|| ([z.fog_color[0], z.fog_color[1], z.fog_color[2]], z.fog_start / UU_PER_M, z.fog_end / UU_PER_M)),
        }
    });

    // Projector decals: extra translucent batches on top of the BSP (materials registered before
    // texture decoding below).
    if let Some(model) = &model {
        for d in crate::projectors::build_projectors(lib, map, model).into_iter().filter(|_| opts.projectors && std::env::var_os("P2VIEW_NO_PROJECTORS").is_none()) {
            let mat_obj = lib.resolve(map, d.material);
            let base = b.material_for(mat_obj);
            // Projected decals blend over the surface with the projector's own ops (EProjectorBlending
            // PB_None/Modulate/AlphaBlend/Add): FrameBufferBlendingOp Add draws additively, a Modulate
            // material or framebuffer op multiplies what is below (unlit: the surface keeps its own
            // lighting). Drawing them all as lit alpha decals turned MilitaryBase's grey cell-window
            // grille into a bright white grid on the floor (FIDELITY #52).
            let mut md = b.materials[base as usize].clone();
            md.blend = match (d.material_op, d.framebuffer_op) {
                (_, 3) => Blend::Additive,
                (1, _) | (_, 1) => Blend::Modulate,
                _ => Blend::Translucent,
            };
            md.two_sided = true;
            let mat = b.materials.len() as u32;
            b.materials.push(md);
            world.insert(
                (u32::MAX - b.stats.projectors as u32, -1),
                MeshData {
                    positions: d.positions.iter().map(|&p| to_bevy(p).to_array()).collect(),
                    normals: d.normals.iter().map(|&n| dir_to_bevy(n).normalize_or_zero().to_array()).collect(),
                    uvs: d.uvs,
                    indices: d.indices,
                    material: mat,
                    ..Default::default()
                },
            );
            b.stats.projectors += 1;
        }
    }

    // --- Textures, decoded in parallel ---
    let gpu_bc = opts.gpu_bc;
    let textures: Vec<TexData> = b.tex_objs.par_iter().map(|(o, op, invert)| match (op, invert) {
        (_, true) => {
            let mut t = load_texture(o, false);
            if t.format == PixelFormat::Rgba8 {
                for m in t.mips.iter_mut() {
                    for p in m.chunks_exact_mut(4) {
                        p[0] = 255 - p[0];
                        p[1] = 255 - p[1];
                        p[2] = 255 - p[2];
                    }
                }
            }
            t
        }
        (Some(op), _) => load_texture_opacity(o, op),
        (None, _) => load_texture(o, gpu_bc),
    }).collect();
    b.stats.textures = textures.len();
    b.stats.bc_textures = textures.iter().filter(|t| t.format != PixelFormat::Rgba8).count();

    let mut world: Vec<MeshData> = world.into_values().collect();
    world.sort_by_key(|m| (m.material, m.lightmap));
    b.stats.lightmapped_polys = world.iter().filter(|m| m.lightmap.is_some()).map(|m| m.indices.len() / 3).sum();
    // Lightmap atlases (stored DXT1 in these maps), decoded in parallel.
    let mut lightmaps: Vec<LightmapData> = model
        .as_ref()
        .map(|m| {
            m.light_map_textures
                .par_iter()
                .map(|t| {
                    let fmt = TexFormat::from_byte(t.format);
                    if std::env::var_os("P2VIEW_LM_DEBUG").is_some() {
                        eprintln!("lightmap atlas {}x{} format {} ({fmt:?}) mips {}", t.width, t.height, t.format, t.mips.len());
                    }
                    // Some maps (mpdgt-monsoon) carry atlas headers with nonsense dimensions; a
                    // grey fill of that size was gigabytes and took 10+ s to build and upload.
                    // Real atlases are at most 1024 square, so anything past 4096 is mid-gray 1x1.
                    // Atlases saved without texel data (MilitaryBase, homeatnight, the movie maps: both
                    // mips empty, header garbage) are built at runtime by UE2 from the lights
                    // (FLightMap::GetTextureData); rebuilt below from the saved light visibility.
                    // A black 1x1 marks them unusable (spawn lights those surfaces dynamically) if
                    // the rebuild is disabled (P2VIEW_LM_REBUILD=0).
                    if t.width == 0 || t.height == 0 || t.width > 4096 || t.height > 4096 || t.mips.is_empty() {
                        return LightmapData { width: 1, height: 1, rgba: vec![0, 0, 0, 255] };
                    }
                    let rgba = t
                        .mips
                        .first()
                        .and_then(|d| upkg::texture::decode_raw(fmt, d, t.width, t.height).ok())
                        .unwrap_or_else(|| vec![128; (t.width.max(1) * t.height.max(1) * 4) as usize]);
                    LightmapData { width: t.width.max(1), height: t.height.max(1), rgba }
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(m) = model.as_ref().filter(|_| std::env::var("P2VIEW_LM_REBUILD").as_deref() != Ok("0")) {
        rebuild_dataless_lightmaps(map, m, &mut lightmaps);
    }
    b.stats.lightmaps = lightmaps.len();
    let sky = sky_origin.map(|origin| {
        let mut w: Vec<MeshData> = sky_world.into_values().collect();
        w.sort_by_key(|m| m.material);
        SkyData { origin, world: w, instances: sky_instances }
    });
    b.stats.load_ms = t0.elapsed().as_millis();
    Ok(SceneData {
        textures,
        materials: b.materials,
        meshes: b.meshes,
        instances,
        movers,
        world,
        lightmaps,
        terrain,
        deco,
        sky,
        lights,
        sun,
        zone,
        spawn,
        spawn_found,
        stats: b.stats,
    })
}

/// Maximum number of point/spot lights spawned (nearest to the start position win).
pub const MAX_LIGHTS: usize = 160;

fn build_lights(map: &Package, origin: Vec3) -> (Vec<LightData>, Option<SunData>) {
    use upkg::lighting::LightKind;
    let all = upkg::lighting::read_lights(map);
    // Sunlight: brightness-weighted colour sum over all LE_Sunlight lights, direction of the brightest.
    // UE2 sunlight only lights its own zone, so maps repeat the same sun set per zone (Industry:
    // the 158/60/32/30 + 2x40 set ~8 times). Count each identical sun once: summing them made
    // Industry's static meshes and pawns fullbright.
    let mut seen = std::collections::HashSet::new();
    let suns: Vec<_> = all
        .iter()
        .filter(|l| l.kind == LightKind::Sun)
        .filter(|l| std::env::var_os("P2VIEW_SUN_NODEDUP").is_some() || seen.insert((l.rotation, l.hue, l.saturation, l.brightness.to_bits())))
        .collect();
    let sun = suns.iter().max_by(|a, b| a.brightness.total_cmp(&b.brightness)).map(|main| {
        let mut c = Vec3::ZERO;
        for s in &suns {
            c += Vec3::from(s.color()) * s.brightness;
        }
        let peak = c.max_element().max(1e-3);
        // Suns within ~10 degrees act as one (suburbs-3: 158 + 62 + 60 a few degrees apart; split
        // up, the shaded side got as much light as the sunlit side and the terrain looked flat).
        let same = std::env::var("P2VIEW_SUN_GROUP_DOT").ok().and_then(|v| v.parse().ok()).unwrap_or(0.985f32);
        let mut groups: Vec<(Vec3, Vec3)> = Vec::new();
        for s in &suns {
            let d = dir_to_bevy(s.direction()).normalize_or_zero();
            match groups.iter_mut().find(|g| g.0.dot(d) > same) {
                Some(g) => g.1 += Vec3::from(s.color()) * s.brightness,
                None => groups.push((d, Vec3::from(s.color()) * s.brightness)),
            }
        }
        let parts = groups
            .into_iter()
            .map(|(d, c)| {
                let p = c.max_element().max(1e-3);
                (d, (c / p).to_array(), p)
            })
            .collect();
        SunData { direction: dir_to_bevy(main.direction()).normalize_or_zero(), color: (c / peak).to_array(), brightness: peak, parts }
    });
    let mut lights: Vec<LightData> = all
        .iter()
        .filter(|l| l.kind != LightKind::Sun)
        .map(|l| LightData {
            shape: if l.kind == LightKind::Spot { LightShape::Spot } else { LightShape::Point },
            position: to_bevy(l.location),
            direction: dir_to_bevy(l.direction()).normalize_or_zero(),
            color: l.color(),
            brightness: l.brightness,
            range: l.world_radius() / UU_PER_M,
            cone: (l.cone as f32 / 255.0 * std::f32::consts::FRAC_PI_2).clamp(0.05, 1.5),
            modulation: l.light_type,
            period: l.period,
            phase: l.phase,
        })
        .collect();
    lights.sort_by(|a, b| a.position.distance_squared(origin).total_cmp(&b.position.distance_squared(origin)));
    lights.truncate(MAX_LIGHTS);
    (lights, sun)
}

/// Rebuilds a static mesh actor's vertex lighting from the per-light visibility bits saved in
/// its StaticMeshInstance, for instances whose colour stream was never baked (alpha 0; most of
/// Industry). Same light model as the terrain: per visible light, sun 2 * max(0, -L.N) or point
/// falloff, colour * brightness / 255 * 0.5, accumulated in the 0-255 modulate-2x scale that
/// baked streams use (the zone ambient is added later, as for baked streams).
fn instance_static_light(
    map: &Package,
    smi: usize,
    mesh: &upkg::library::Obj,
    lin: Mat3,
    loc: Vec3,
    cache: &mut HashMap<usize, Option<upkg::lighting::MapLight>>,
) -> Option<Vec<[u8; 4]>> {
    if std::env::var_os("P2VIEW_NO_SMI_RELIGHT").is_some() {
        return None;
    }
    let lights = upkg::staticmesh::read_instance_lights(map, smi).ok()?;
    if lights.is_empty() {
        return None;
    }
    let sm = upkg::staticmesh::read_static_mesh(&mesh.pkg, mesh.export).ok()?;
    let nrm_m = lin.inverse().transpose();
    let n = sm.positions.len();
    let mut acc = vec![Vec3::ZERO; n];
    for (r, bits) in &lights {
        let ObjRef::Export(le) = *r else { continue };
        let l = cache.entry(le).or_insert_with(|| begin_object(map, le).ok().and_then(|(_, p)| upkg::lighting::light_from_props(map.export_class(le), le, &p)));
        let Some(l) = l else { continue };
        let color = Vec3::from(upkg::lighting::hsv_to_rgb(l.hue, l.saturation)) * (l.brightness / 255.0) * 0.5;
        for v in 0..n {
            if !upkg::terrain::SectorLighting::visible(bits, v) {
                continue;
            }
            let p = lin * Vec3::from(sm.positions[v]) + loc;
            let nn = (nrm_m * Vec3::from(sm.normals.get(v).copied().unwrap_or([0.0, 0.0, 1.0]))).normalize_or_zero();
            let i = if l.kind == upkg::lighting::LightKind::Sun {
                (-2.0 * Vec3::from(l.direction()).dot(nn)).max(0.0)
            } else {
                let d = Vec3::from(l.location) - p;
                let dist = d.length().max(1e-3);
                let f = (dist / l.world_radius()).min(1.0);
                2.0 * (2.0 * f * f * f - 3.0 * f * f + 1.0) * (d / dist).dot(nn).max(0.0) * l.spot((-d / dist).to_array())
            };
            acc[v] += color * i;
        }
    }
    Some(acc.iter().map(|c| [(c.x.clamp(0.0, 1.0) * 255.0) as u8, (c.y.clamp(0.0, 1.0) * 255.0) as u8, (c.z.clamp(0.0, 1.0) * 255.0) as u8, 255]).collect())
}

/// Per-vertex lighting (modulate-2x bytes, as a saved StaticMeshInstance stream) of a static mesh
/// actor with no saved lighting (movers, doors), as UE2 lights a dynamic actor: the lights of the
/// BSP leaf at its location (UModel::Lights run at Leaf.iPermeating, saved by the editor's light
/// rebuild), only those whose bSpecialLit matches the actor's, with the same falloff and N.L as
/// the baked path. Zone ambient and AmbientGlow are added later with the baked streams.
fn actor_static_light(
    mesh: &upkg::library::Obj,
    lin: Mat3,
    loc: Vec3,
    special: bool,
    model: Option<&upkg::model::Model>,
    bsp: Option<&p2collide::bsp::Bsp>,
    lights: &mut HashMap<usize, Option<upkg::lighting::MapLight>>,
    map: &Package,
) -> Option<Vec<[u8; 4]>> {
    let (model, bsp) = (model?, bsp?);
    let leaf = bsp.point_leaf(loc)?;
    let refs = model.leaf_lights(leaf);
    let sm = upkg::staticmesh::read_static_mesh(&mesh.pkg, mesh.export).ok()?;
    if sm.positions.is_empty() {
        return None;
    }
    let trace = std::env::var_os("P2VIEW_ACTOR_LIGHT_TRACE").is_some();
    let nrm_m = lin.inverse().transpose();
    let mut acc = vec![Vec3::ZERO; sm.positions.len()];
    let mut list: Vec<upkg::lighting::MapLight> = refs
        .iter()
        .filter_map(|r| match *r {
            ObjRef::Export(le) => lights.entry(le).or_insert_with(|| begin_object(map, le).ok().and_then(|(_, p)| upkg::lighting::light_from_props(map.export_class(le), le, &p))).clone(),
            _ => None,
        })
        .filter(|l| l.special_lit == special)
        .collect();
    // UE2 draws a dynamic actor with at most the 8 most significant lights (D3D MaxActiveLights):
    // strongest first at the actor's location (brightness x distance falloff). All 11 freezer
    // lights put the slaughterhouse door at 149 vs orig 136; 8 gives 137.
    let strength = |l: &upkg::lighting::MapLight| {
        if l.kind == upkg::lighting::LightKind::Sun {
            return l.brightness;
        }
        let f = (Vec3::from(l.location).distance(loc) / l.world_radius()).min(1.0);
        l.brightness * (2.0 * f * f * f - 3.0 * f * f + 1.0) * l.spot((loc - Vec3::from(l.location)).normalize_or_zero().to_array())
    };
    list.sort_by(|a, b| strength(b).total_cmp(&strength(a)));
    list.truncate(std::env::var("P2VIEW_ACTOR_MAX_LIGHTS").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(8));
    for l in &list {
        let le = l.export;
        if trace {
            eprintln!("actor light: mesh {} at {loc:?} leaf {leaf} <- {} {} {:?} b {} r {} effect {} cone {} dir {} rot {:?}", mesh.pkg.name(mesh.pkg.exports[mesh.export].name), l.class, le, l.location, l.brightness, l.world_radius(), l.light_effect, l.cone, l.directional, l.rotation);
        }
        let color = Vec3::from(upkg::lighting::hsv_to_rgb(l.hue, l.saturation)) * (l.brightness / 255.0) * 0.5;
        for (v, p) in sm.positions.iter().enumerate() {
            let wp = lin * Vec3::from(*p) + loc;
            let nn = (nrm_m * Vec3::from(sm.normals.get(v).copied().unwrap_or([0.0, 0.0, 1.0]))).normalize_or_zero();
            let i = if l.kind == upkg::lighting::LightKind::Sun {
                (-2.0 * Vec3::from(l.direction()).dot(nn)).max(0.0)
            } else {
                let d = Vec3::from(l.location) - wp;
                let dist = d.length().max(1e-3);
                let f = (dist / l.world_radius()).min(1.0);
                2.0 * (2.0 * f * f * f - 3.0 * f * f + 1.0) * (d / dist).dot(nn).max(0.0) * l.spot((-d / dist).to_array())
            };
            acc[v] += color * i;
        }
    }
    if trace {
        let mean = acc.iter().copied().sum::<Vec3>() / acc.len() as f32 * 255.0;
        eprintln!("actor light: mesh {} at {loc:?} leaf {leaf}: {} lights, mean {mean:?}", mesh.pkg.name(mesh.pkg.exports[mesh.export].name), refs.len());
    }
    Some(acc.iter().map(|c| [(c.x.clamp(0.0, 1.0) * 255.0) as u8, (c.y.clamp(0.0, 1.0) * 255.0) as u8, (c.z.clamp(0.0, 1.0) * 255.0) as u8, 255]).collect())
}

/// Builds the atlases saved without texel data (left as 1x1 placeholders above) the way UE2 does
/// at load: zone ambient plus every static light the editor saved as visible per texel
/// (upkg::lightmap). MilitaryBase's prison: walls 19 / grille floor 26 in the original, ~30 / 57
/// with the dynamic-light fallback (#52).
fn rebuild_dataless_lightmaps(map: &Package, model: &upkg::model::Model, lightmaps: &mut [LightmapData]) {
    let t0 = std::time::Instant::now();
    let dataless: Vec<usize> = model.light_map_textures.iter().enumerate().filter(|(_, t)| t.mips.is_empty() || t.width == 0 || t.height == 0 || t.width > 4096 || t.height > 4096).map(|(i, _)| i).collect();
    if dataless.is_empty() {
        return;
    }
    let zones = upkg::lighting::read_zones(map);
    let ambient = |zi: i32| -> [f32; 3] {
        let z = match model.zones.get(zi.max(0) as usize) {
            Some(ObjRef::Export(e)) => zones.iter().find(|z| z.export == *e),
            _ => None,
        }
        .or_else(|| zones.iter().find(|z| z.class.eq_ignore_ascii_case("LevelInfo")));
        z.map(|z| upkg::lightmap::zone_ambient(z.ambient_hue, z.ambient_saturation, z.ambient_brightness, upkg::lightmap::AMBIENT_SCALE)).unwrap_or([0.0; 3])
    };
    let mut cache: HashMap<ObjRef, Option<upkg::lighting::MapLight>> = HashMap::new();
    let mut light = |r: ObjRef| -> Option<upkg::lighting::MapLight> {
        cache
            .entry(r)
            .or_insert_with(|| match r {
                ObjRef::Export(e) => begin_object(map, e).ok().and_then(|(_, p)| upkg::lighting::light_from_props(map.export_class(e), e, &p)),
                _ => None,
            })
            .clone()
    };
    let mut built = 0;
    for &ti in &dataless {
        let (w, h) = upkg::lightmap::infer_atlas_size(model, ti as i32);
        if w <= 1 || h <= 1 {
            continue;
        }
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for lm in model.light_maps.iter().filter(|l| l.texture == ti as i32) {
            upkg::lightmap::build_lightmap(model, lm, &mut rgba, w, &mut light, &ambient, upkg::lightmap::LIGHT_SCALE);
            built += 1;
        }
        lightmaps[ti] = LightmapData { width: w, height: h, rgba };
    }
    eprintln!("scene: rebuilt {} dataless lightmap atlases ({built} lightmaps) from saved light visibility in {} ms", dataless.len(), t0.elapsed().as_millis());
}

/// UE2 hue/saturation colour (FGetHSV).
fn ue2_hue(hue: u8, saturation: u8) -> Vec3 {
    Vec3::from(upkg::lighting::hsv_to_rgb(hue, saturation))
}

/// UE2 terrain static lighting rebuilt from the saved TerrainSector light lists (as
/// UTerrainSector::StaticLight does at load): per vertex, the terrain zone's ambient plus every
/// light the editor found visible from that vertex (sunlight: 2 * max(0, -L.N); point lights:
/// UE2 falloff), accumulated as an 8-bit colour. Returns the linear light factor per heightmap
/// vertex, or None when the map has no saved terrain lighting (dynamic lighting then).
fn terrain_static_light(map: &Package, terrain_export: usize, t: &upkg::terrain::Terrain) -> Option<Vec<[f32; 4]>> {
    if std::env::var_os("P2VIEW_TERRAIN_DYNAMIC").is_some() {
        return None;
    }
    let sectors = upkg::terrain::read_sector_lighting(map, terrain_export);
    // No sectors: no saved terrain lighting, light dynamically. Sectors that exist but list no
    // lights mean the editor found none visible: UE2 lights those vertices with the zone ambient
    // only (ngt-Junkyard: moonlit gravel 9 in the original, 18 with our dynamic suns).
    if sectors.is_empty() || (sectors.iter().all(|s| s.lights.is_empty()) && std::env::var_os("P2VIEW_TERRAIN_EMPTY_DYNAMIC").is_some()) {
        return None;
    }
    let (w, h) = (t.width, t.height);
    let n = (w * h) as usize;
    let world: Vec<Vec3> = (0..n as u32).map(|i| Vec3::from(t.vertex(i % w, i / w))).collect();
    let normal = |i: usize| -> Vec3 {
        let (x, y) = (i as u32 % w, i as u32 / w);
        let dx = world[(y * w + (x + 1).min(w - 1)) as usize] - world[(y * w + x.saturating_sub(1)) as usize];
        let dy = world[((y + 1).min(h - 1) * w + x) as usize] - world[(y.saturating_sub(1) * w + x) as usize];
        let nrm = dx.cross(dy).normalize_or_zero();
        if nrm.z < 0.0 { -nrm } else { nrm }
    };
    // Terrain zone ambient: the zone the TerrainInfo actor is in (its saved Region.Zone), else
    // the first terrain zone.
    let zones = upkg::lighting::read_zones(map);
    let own = begin_object(map, terrain_export).ok().and_then(|(_, p)| actor_zone(map, &p));
    let amb = zones
        .iter()
        .find(|z| own == Some(ObjRef::Export(z.export)))
        .or_else(|| zones.iter().find(|z| z.terrain_zone && !z.sky_zone))
        .or_else(|| upkg::lighting::primary_zone(&zones))
        // FGetHSV(hue, sat, brightness) scales by sqrt(brightness * 1.4 / 255): the night map's
        // ambient 11 is ~0.25, the day map's 102 ~0.75 (linear would be 0.04 and 0.4).
        .map(|z| (ue2_hue(z.ambient_hue, z.ambient_saturation) * (z.ambient_brightness as f32 * 1.4 / 255.0).sqrt().min(1.0)).to_array())
        .unwrap_or([0.0; 3]);
    let amb_scale: f32 = std::env::var("P2VIEW_TERRAIN_AMB").ok().and_then(|v| v.parse().ok()).unwrap_or(0.45);
    let light_scale: f32 = std::env::var("P2VIEW_TERRAIN_LIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let mut lights: HashMap<usize, Option<upkg::lighting::MapLight>> = HashMap::new();
    let mut acc: Vec<Vec3> = vec![Vec3::from(amb) * amb_scale; n];
    let mut covered = vec![false; n];
    for s in &sectors {
        let (qx, qy) = s.quads;
        let row = qx + 1;
        let count = (row * (qy + 1)) as usize;
        let mut sector_acc = vec![Vec3::ZERO; count];
        for (le, bits) in &s.lights {
            let l = lights
                .entry(*le)
                .or_insert_with(|| begin_object(map, *le).ok().and_then(|(_, p)| upkg::lighting::light_from_props(map.export_class(*le), *le, &p)));
            let Some(l) = l else { continue };
            // UE2 light colour (FGetHSV: linear hue ramp blended toward white by saturation).
            let rgb = ue2_hue(l.hue, l.saturation);
            // SampleIntensity gets a 0.5 scale: the vertex colour is drawn modulate-2x (128 = 1.0).
            let color = rgb * (l.brightness / 255.0) * 0.5 * light_scale;
            for j in 0..count {
                if !upkg::terrain::SectorLighting::visible(bits, j) {
                    continue;
                }
                let (gx, gy) = (s.offset.0 + j as u32 % row, s.offset.1 + j as u32 / row);
                if gx >= w || gy >= h {
                    continue;
                }
                let gi = (gy * w + gx) as usize;
                let nrm = normal(gi);
                let intensity = if std::env::var_os("P2VIEW_TERRAIN_VIS").is_some() {
                    if l.brightness >= 150.0 { 4.0 } else { 0.0 }
                } else if l.kind == upkg::lighting::LightKind::Sun {
                    (-2.0 * Vec3::from(l.direction()).dot(nrm)).max(0.0)
                } else {
                    let d = Vec3::from(l.location) - world[gi];
                    let dist = d.length().max(1e-3);
                    let f = (dist / l.world_radius()).min(1.0);
                    2.0 * (2.0 * f * f * f - 3.0 * f * f + 1.0) * (d / dist).dot(nrm).max(0.0)
                };
                sector_acc[j] += color * intensity;
            }
        }
        for j in 0..count {
            let (gx, gy) = (s.offset.0 + j as u32 % row, s.offset.1 + j as u32 / row);
            if gx < w && gy < h {
                let gi = (gy * w + gx) as usize;
                if !covered[gi] {
                    covered[gi] = true;
                    acc[gi] += sector_acc[j];
                }
            }
        }
    }
    // 8-bit colour (per-channel saturation, as FColor), drawn modulate-2x with the texture in
    // gamma space: linear factor = (2 * byte / 255)^2.2. Gain 0.8 and ambient 0.8 are
    // calibrated on the suburbs-3 start view (sunlit ground 139 vs 138, shed shadow core 63 vs
    // 65, hill in shade 54 vs 55).
    let gain: f32 = std::env::var("P2VIEW_TERRAIN_GAIN").ok().and_then(|v| v.parse().ok()).unwrap_or(0.75);
    if std::env::var_os("P2VIEW_TERRAIN_TIMING").is_some() {
        let lit_n = acc.iter().filter(|c| c.max_element() > Vec3::from(amb).max_element() * amb_scale + 0.01).count();
        let with_lights = sectors.iter().filter(|s| !s.lights.is_empty()).count();
        eprintln!("terrain light: {} sectors ({with_lights} with lights), {lit_n}/{n} vertices lit, ambient {amb:?}, terrain at {:?} scale {:?} {}x{}", sectors.len(), t.location, t.scale, w, h);
        if let Ok(p) = std::env::var("P2VIEW_TERRAIN_PROBE") {
            let v: Vec<f32> = p.split(',').filter_map(|s| s.parse().ok()).collect();
            let (x, y) = (((v[0] - t.location[0]) / t.scale[0] + w as f32 / 2.0) as u32, ((v[1] - t.location[1]) / t.scale[1] + h as f32 / 2.0) as u32);
            for s in sectors.iter().filter(|s| x >= s.offset.0 && x <= s.offset.0 + s.quads.0 && y >= s.offset.1 && y <= s.offset.1 + s.quads.1) {
                eprintln!("  probe vertex ({x},{y}) sector off {:?} lights {:?}", s.offset, s.lights.iter().map(|(e, b)| (map.name(map.exports[*e].name).to_string(), b.iter().map(|x| x.count_ones()).sum::<u32>())).collect::<Vec<_>>());
                if let Some((_, b)) = s.lights.first() {
                    for r in 0..=s.quads.1 {
                        let line: String = (0..=s.quads.0).map(|c| if upkg::terrain::SectorLighting::visible(b, (r * (s.quads.0 + 1) + c) as usize) { '#' } else { '.' }).collect();
                        eprintln!("    {line}");
                    }
                }
            }
            for dy in 0..3u32 {
                let row: Vec<String> = (0..6u32).map(|dx| { let c = acc[((y + dy) * w + x + dx) as usize]; format!("{:.2},{:.2},{:.2}", c.x, c.y, c.z) }).collect();
                eprintln!("  {row:?}");
            }
        }
    }
    // Clamp only the final modulate-2x value (as the original's 8-bit output does), not the light
    // sum at 1.0: clipping the sum desaturated strongly lit warm ground toward the cooler channels
    // (parade's road came out greenish grey, orig brown).
    let ceil: f32 = std::env::var("P2VIEW_TERRAIN_CEIL").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0);
    Some(acc.iter().map(|c| { let f = |v: f32| (v.clamp(0.0, ceil) * 2.0 * gain).powf(2.2); [f(c.x), f(c.y), f(c.z), 1.0] }).collect())
}

/// Builds one mesh per terrain layer. Layer 0 is the opaque base; later layers carry their alpha
/// map weight in vertex alpha. Triangles where a layer has zero weight are dropped.
fn build_terrain(b: &mut Builder, map: &Arc<Package>, t: &upkg::terrain::Terrain, lit: Option<&[[f32; 4]]>, clip: Option<&crate::terrain_clip::TerrainClip>) -> Vec<TerrainLayerMesh> {
    let (w, h) = (t.width, t.height);
    let grid_n = (w * h) as usize;
    let mut world: Vec<Vec3> = (0..grid_n as u32).map(|i| Vec3::from(t.vertex(i % w, i / w))).collect();
    // Central-difference normals in Unreal space, then converted.
    let mut normals: Vec<[f32; 3]> = (0..grid_n as u32)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let dx = world[(y * w + (x + 1).min(w - 1)) as usize] - world[(y * w + x.saturating_sub(1)) as usize];
            let dy = world[((y + 1).min(h - 1) * w + x) as usize] - world[(y.saturating_sub(1) * w + x) as usize];
            let mut nrm = dx.cross(dy).normalize_or_zero();
            if nrm.z < 0.0 {
                nrm = -nrm;
            }
            dir_to_bevy(nrm.to_array()).normalize_or_zero().to_array()
        })
        .collect();
    // Quad triangulation shared by all layers so blended layers are exactly coplanar.
    let mut tris: Vec<[u32; 3]> = Vec::with_capacity(((w - 1) * (h - 1) * 2) as usize);
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            if !t.quad_visible(x, y) {
                continue;
            }
            let (a, bb, c, d) = (y * w + x, y * w + x + 1, (y + 1) * w + x, (y + 1) * w + x + 1);
            if t.edge_turned(x, y) {
                tris.push([a, bb, d]);
                tris.push([a, d, c]);
            } else {
                tris.push([a, bb, c]);
                tris.push([bb, d, c]);
            }
        }
    }
    // Terrain is only drawn from terrain zones (see terrain_clip.rs): cut the triangles along the
    // BSP and drop the pieces inside buildings. Cut vertices interpolate the grid vertex data;
    // their alpha-map coordinates come from the heightmap-space position (see `alpha_uv`).
    let mut grid_xy: Vec<[f32; 2]> = (0..grid_n as u32).map(|i| [(i % w) as f32, (i / w) as f32]).collect();
    let mut lit_v: Option<Vec<[f32; 4]>> = lit.map(|l| l.to_vec());
    if let Some(c) = clip {
        let mut out: Vec<[u32; 3]> = Vec::with_capacity(tris.len());
        let (mut cut, mut gone) = (0usize, 0usize);
        for tri in &tris {
            let p = [world[tri[0] as usize], world[tri[1] as usize], world[tri[2] as usize]];
            match c.clip_tri(p) {
                None => out.push(*tri),
                Some(pieces) => {
                    if pieces.is_empty() {
                        gone += 1;
                    } else {
                        cut += 1;
                    }
                    for piece in pieces {
                        let mut idx = [0u32; 3];
                        for (k, wts) in piece.iter().enumerate() {
                            // Reuse an original corner when the weight is (almost) one-hot.
                            if let Some(c) = (0..3).find(|&c| wts[c] > 0.9999) {
                                idx[k] = tri[c];
                                continue;
                            }
                            let mix3 = |a: Vec3, bb: Vec3, cc: Vec3| a * wts[0] + bb * wts[1] + cc * wts[2];
                            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                            world.push(mix3(world[i0], world[i1], world[i2]));
                            let nn = mix3(Vec3::from(normals[i0]), Vec3::from(normals[i1]), Vec3::from(normals[i2])).normalize_or_zero();
                            normals.push(nn.to_array());
                            let g = |i: usize| Vec3::new(grid_xy[i][0], grid_xy[i][1], 0.0);
                            let gxy = mix3(g(i0), g(i1), g(i2));
                            grid_xy.push([gxy.x, gxy.y]);
                            if let Some(l) = lit_v.as_mut() {
                                let m4 = |k: usize| l[i0][k] * wts[0] + l[i1][k] * wts[1] + l[i2][k] * wts[2];
                                let v = [m4(0), m4(1), m4(2), m4(3)];
                                l.push(v);
                            }
                            idx[k] = (world.len() - 1) as u32;
                        }
                        out.push(idx);
                    }
                }
            }
        }
        if std::env::var_os("P2VIEW_TERRAIN_TIMING").is_some() {
            eprintln!("terrain clip: {} tris, {cut} cut, {gone} removed, {} out", tris.len(), out.len());
        }
        tris = out;
    }
    let lit = lit_v.as_deref();
    let n = world.len();
    let positions: Vec<[f32; 3]> = world.iter().map(|p| to_bevy(p.to_array()).to_array()).collect();
    // Orient triangles to face up (+Y in Bevy).
    if let Some(tri) = tris.first() {
        let (p0, p1, p2) = (Vec3::from(positions[tri[0] as usize]), Vec3::from(positions[tri[1] as usize]), Vec3::from(positions[tri[2] as usize]));
        if (p1 - p0).cross(p2 - p0).y < 0.0 {
            for tri in &mut tris {
                tri.swap(1, 2);
            }
        }
    }
    // Bounding corners, appended unreferenced to every layer so all layer meshes share one AABB
    // centre: transparent sorting then orders layers purely by their depth bias.
    let (lo, hi) = positions.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(Vec3::from(*p)), hi.max(Vec3::from(*p))));

    let mut out = Vec::new();
    for (li, layer) in t.layers.iter().enumerate() {
        let mat_obj = b.lib.resolve(map, layer.texture);
        let mat = b.material_for(mat_obj);
        let texture = b.materials[mat as usize].texture;
        // Per-layer weight from the alpha map. The map itself goes to the GPU for per-pixel
        // splatting; per-vertex samples are only used to drop triangles with zero weight.
        let alpha = if li == 0 {
            None
        } else {
            match b.lib.resolve(map, layer.alpha_map).and_then(|o| upkg::terrain::read_alpha_map(&o).ok()) {
                Some(a) => Some(a),
                None if layer.alpha_map == ObjRef::Null => None,
                None => continue,
            }
        };
        let weights: Vec<f32> = match &alpha {
            None => vec![1.0; n],
            Some((aw, ah, a)) => (0..n)
                .map(|i| {
                    // Cut vertices only serve to keep their triangle (zero-weight culling below);
                    // the splat itself samples the alpha map per pixel.
                    if i >= grid_n {
                        return 1.0;
                    }
                    let (x, y) = (grid_xy[i][0] as u64, grid_xy[i][1] as u64);
                    let ax = ((x * *aw as u64) / w as u64).min(*aw as u64 - 1);
                    let ay = ((y * *ah as u64) / h as u64).min(*ah as u64 - 1);
                    a[(ay * *aw as u64 + ax) as usize] as f32 / 255.0
                })
                .collect(),
        };
        // Alpha map coordinates: vertex (x, y) samples the texel centre the engine maps it to.
        let (aw, ah) = alpha.as_ref().map(|(aw, ah, _)| (*aw as f32, *ah as f32)).unwrap_or((w as f32, h as f32));
        let alpha_uv = |vi: usize| -> [f32; 2] {
            let [x, y] = grid_xy[vi];
            [x / w as f32 + 0.5 / aw, y / h as f32 + 0.5 / ah]
        };
        // Texture coordinates are in heightmap space: one texture repeat per UScale (VScale)
        // heightmap quads, i.e. UScale * TerrainScale.X world units. The mapping plane follows
        // TextureMapAxis and the coordinates are rotated by TextureRotation.
        let (su, sv) = (if layer.u_scale == 0.0 { 1.0 } else { layer.u_scale }, if layer.v_scale == 0.0 { 1.0 } else { layer.v_scale });
        let rot = layer.rotation * std::f32::consts::TAU / 65536.0;
        let (sr, cr) = rot.sin_cos();
        let quad = t.scale[0].abs().max(1.0);
        let uv_of = |p: Vec3| -> [f32; 2] {
            let p = (p - Vec3::from(t.location)) / quad;
            let (a, c) = match layer.map_axis {
                1 => (p.x, -p.z),
                2 => (p.y, -p.z),
                _ => (p.x, p.y),
            };
            let (ra, rc) = (a * cr - c * sr, a * sr + c * cr);
            [(ra + layer.u_pan) / su, (rc + layer.v_pan) / sv]
        };
        let mut md = MeshData { material: mat, ..Default::default() };
        let mut remap = vec![u32::MAX; n];
        for tri in &tris {
            if li > 0 && tri.iter().all(|&v| weights[v as usize] <= 0.0) {
                continue;
            }
            for &v in tri {
                let vi = v as usize;
                if remap[vi] == u32::MAX {
                    remap[vi] = md.positions.len() as u32;
                    md.positions.push(positions[vi]);
                    md.normals.push(normals[vi]);
                    md.uvs.push(uv_of(world[vi]));
                    md.uvs_b.push(alpha_uv(vi));
                    if let Some(l) = lit {
                        md.colors.push(l[vi]);
                    }
                }
                md.indices.push(remap[vi]);
            }
        }
        if md.indices.is_empty() {
            continue;
        }
        for corner in [lo, hi] {
            md.positions.push(corner.to_array());
            md.normals.push([0.0, 1.0, 0.0]);
            md.uvs.push([0.0, 0.0]);
            md.uvs_b.push([0.0, 0.0]);
            if lit.is_some() {
                md.colors.push([0.0, 0.0, 0.0, 1.0]);
            }
        }
        out.push(TerrainLayerMesh { mesh: md, texture, order: li as u32, alpha });
    }
    out
}

fn placeholder(name: &str) -> TexData {
    // Magenta/black checker: visible marker for textures that could not be decoded.
    let mut px = Vec::with_capacity(8 * 8 * 4);
    for y in 0..8 {
        for x in 0..8 {
            px.extend(if (x + y) % 2 == 0 { [255, 0, 255, 255] } else { [0, 0, 0, 255] });
        }
    }
    TexData { name: name.to_string(), width: 8, height: 8, format: PixelFormat::Rgba8, mips: vec![px] }
}

/// RGBA8 texture whose alpha comes from a Shader Opacity texture (every mip).
fn load_texture_opacity(o: &Obj, op: &Obj) -> TexData {
    let mut t = load_texture(o, false);
    if !matches!(t.format, PixelFormat::Rgba8) {
        return t;
    }
    let Ok(ot) = read_texture(&op.pkg, op.export) else { return t };
    let Ok((sw, sh, src)) = ot.decode_rgba(0) else { return t };
    let (mut w, mut h) = (t.width, t.height);
    for m in t.mips.iter_mut() {
        upkg::texture::merge_alpha(m, w, h, &src, sw, sh);
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    t
}

fn load_texture(o: &Obj, gpu_bc: bool) -> TexData {
    let Ok(t) = read_texture(&o.pkg, o.export) else { return placeholder(o.name()) };
    let base = t.mips.first();
    let (w, h) = base.map(|m| (m.width, m.height)).unwrap_or((0, 0));
    let bc = match t.format {
        TexFormat::Dxt1 => Some(PixelFormat::Bc1),
        TexFormat::Dxt3 => Some(PixelFormat::Bc2),
        TexFormat::Dxt5 => Some(PixelFormat::Bc3),
        _ => None,
    };
    if let (true, Some(fmt)) = (gpu_bc && w % 4 == 0 && h % 4 == 0 && w > 0, bc) {
        // Pass block-compressed mips straight through: no decode, 4-8x less GPU memory.
        let mut mips = Vec::new();
        let (mut ew, mut eh) = (w, h);
        for m in &t.mips {
            let need = t.format.data_size(ew, eh).unwrap_or(usize::MAX);
            if m.width != ew || m.height != eh || m.data.len() < need {
                break;
            }
            mips.push(m.data[..need].to_vec());
            if ew == 1 && eh == 1 {
                break;
            }
            ew = (ew / 2).max(1);
            eh = (eh / 2).max(1);
        }
        if !mips.is_empty() {
            return TexData { name: t.name.clone(), width: w, height: h, format: fmt, mips };
        }
    }
    // CPU decode path: decode every valid mip level to RGBA8.
    let mut mips = Vec::new();
    let (mut ew, mut eh) = (w, h);
    for level in 0..t.mips.len() {
        let m = &t.mips[level];
        if m.width != ew || m.height != eh {
            break;
        }
        match t.decode_rgba(level) {
            Ok((_, _, px)) => mips.push(px),
            Err(_) => break,
        }
        if ew == 1 && eh == 1 {
            break;
        }
        ew = (ew / 2).max(1);
        eh = (eh / 2).max(1);
    }
    if mips.is_empty() {
        return placeholder(&t.name);
    }
    TexData { name: t.name.clone(), width: w, height: h, format: PixelFormat::Rgba8, mips }
}
