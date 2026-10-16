//! The collision world of one map: BSP polygons, blocking static mesh actors and terrain
//! heightfields, queried by swept traces. Everything is in Unreal units and Unreal axes
//! (X forward, Y right, Z up).

use crate::bsp::Bsp;
use crate::bvh::Bvh;
use crate::defaults::{PawnParams, ScriptDefaults};
use crate::geom::{Aabb, Shape, sweep_impl};
use crate::heightfield::Heightfield;
use glam::{Mat3, Vec3};
use std::collections::HashMap;
use std::sync::Arc;
use upkg::library::Library;
use upkg::model::{find_world_model, pf, read_model};
use upkg::staticmesh::read_static_mesh;
use upkg::{ObjRef, Package, Value, begin_object};

/// What a triangle came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Bsp,
    /// Static mesh actor (map export index).
    Actor(u32),
    Terrain(u16),
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    /// Fraction of the trace at contact.
    pub t: f32,
    /// Shape centre at contact.
    pub location: Vec3,
    pub normal: Vec3,
    pub start_solid: bool,
    /// Push-out distance along `normal` for start-solid hits.
    pub depth: f32,
    pub source: Source,
    /// Material of the surface hit (index into [`World::materials`]), when known: the BSP
    /// surface's material, the static mesh section's, or the terrain layer weighted highest there.
    pub material: Option<u32>,
}

/// Which geometry a trace collides with.
#[derive(Clone, Copy, Debug)]
pub struct TraceFlags {
    pub world: bool,
    pub actors: bool,
    pub terrain: bool,
}

impl TraceFlags {
    pub const ALL: TraceFlags = TraceFlags { world: true, actors: true, terrain: true };
}

#[derive(Default, Debug, Clone)]
pub struct WorldStats {
    pub bsp_tris: usize,
    pub bsp_nonsolid_skipped: usize,
    pub actor_tris: usize,
    pub blocking_actors: usize,
    pub nonblocking_actors: usize,
    pub actors_from_script_flags: usize,
    pub meshes_with_collision_tris: usize,
    pub meshes_render_fallback: usize,
    pub terrains: usize,
    /// Mover actors left out of the static geometry (`BuildOptions::skip_movers`).
    pub movers_skipped: usize,
    /// Non-static static mesh actors left out (`BuildOptions::skip_dynamic`).
    pub dynamic_skipped: usize,
    pub load_ms: u128,
}

/// Options for [`World::build_with`].
#[derive(Clone, Copy, Debug, Default)]
pub struct BuildOptions {
    /// Leave movers out of the static triangle set (a dynamic physics layer moves them).
    pub skip_movers: bool,
    /// Leave out every non-static (`bStatic=False`) blocking static mesh actor: windows, props,
    /// KActors, cars, DayBlockers. Script can break, hide, move or destroy them, so the physics
    /// layer gives them their own mesh colliders (UE2 collides them on their StaticMesh, which a
    /// static triangle set could never switch off when a window breaks).
    pub skip_dynamic: bool,
}

pub struct World {
    tris: Vec<[Vec3; 3]>,
    sources: Vec<Source>,
    /// Material index per triangle (`NO_MATERIAL` when unknown).
    tri_mat: Vec<u32>,
    /// Full object paths of the materials surfaces use ("Package.Group.Name").
    pub materials: Vec<String>,
    bvh: Bvh,
    pub bsp: Bsp,
    /// Zone actor (map export index) per BSP zone number; index 0 is the level itself.
    pub zones: Vec<Option<u32>>,
    pub terrains: Vec<Heightfield>,
    pub player_starts: Vec<(Vec3, f32)>,
    pub pawn: PawnParams,
    pub stats: WorldStats,
    /// Static mesh actors (map export index) destroyed at runtime; their triangles stop colliding.
    /// Level-start script removes the meshes of other days and holidays (PrepActorsByGroup).
    removed: std::sync::RwLock<std::collections::HashSet<u32>>,
}

/// Collision triangles of a StaticMesh in mesh space: its stored collision triangles when it
/// has any (second value `Some(true)`), else its render sections (`Some(false)`); sections and
/// collision materials with `EnableCollision` off are skipped. `None` if the mesh fails to load.
pub fn mesh_collision_tris(sm_obj: &upkg::library::Obj) -> (Vec<[Vec3; 3]>, Option<bool>) {
    let (t, _, kind) = mesh_collision_tris_mat(sm_obj);
    (t, kind)
}

/// [`mesh_collision_tris`] plus each triangle's section (material slot) index and the mesh's
/// section material references (resolve through the mesh's package).
pub fn mesh_collision_tris_mat(sm_obj: &upkg::library::Obj) -> (Vec<[Vec3; 3]>, (Vec<u16>, Vec<ObjRef>), Option<bool>) {
    let Ok(sm) = read_static_mesh(&sm_obj.pkg, sm_obj.export) else { return (vec![], (vec![], vec![]), None) };
    let p: Vec<Vec3> = sm.positions.iter().map(|&v| Vec3::from(v)).collect();
    let mut out = Vec::new();
    let mut slot = Vec::new();
    if !sm.collision_tris.is_empty() {
        for t in &sm.collision_tris {
            if sm.material_collision.get(t.material.max(0) as usize).copied().unwrap_or(true) {
                out.push(t.indices.map(|k| p[k as usize]));
                slot.push(t.material.max(0) as u16);
            }
        }
        (out, (slot, sm.materials), Some(true))
    } else {
        for s in 0..sm.sections.len() {
            if !sm.material_collision.get(s).copied().unwrap_or(true) {
                continue;
            }
            for t in sm.section_indices(s).chunks_exact(3) {
                out.push([p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]]);
                slot.push(s as u16);
            }
        }
        (out, (slot, sm.materials), Some(false))
    }
}

/// Interns material paths for [`World::materials`].
#[derive(Default)]
struct MatTable {
    paths: Vec<String>,
    index: HashMap<String, u32>,
}

impl MatTable {
    fn intern(&mut self, path: String) -> u32 {
        if let Some(&i) = self.index.get(&path) {
            return i;
        }
        let i = self.paths.len() as u32;
        self.index.insert(path.clone(), i);
        self.paths.push(path);
        i
    }

    /// Index for a material reference made from `pkg` (resolved to the defining package, so the
    /// path names the real object), or `NO_MATERIAL`.
    fn of(&mut self, lib: &Library, pkg: &Arc<Package>, r: ObjRef) -> u32 {
        if r == ObjRef::Null {
            return NO_MATERIAL;
        }
        match lib.resolve(pkg, r) {
            Some(o) => self.intern(o.pkg.full_path(ObjRef::Export(o.export))),
            None => NO_MATERIAL,
        }
    }
}

/// `World::tri_mat` value for triangles with no known material.
const NO_MATERIAL: u32 = u32::MAX;

/// World placement of a map actor from its saved properties: (Location, rotation x scale, PrePivot),
/// so a mesh-space vertex `v` lands at `loc + m * (v - pre)`.
pub fn actor_transform(props: &upkg::Properties) -> (Vec3, Mat3, Vec3) {
    let loc = Vec3::from(props.vector("Location").unwrap_or([0.0; 3]));
    let rot = props.rotator("Rotation").unwrap_or([0; 3]);
    let ds = props.float("DrawScale").unwrap_or(1.0);
    let ds3 = match props.get("DrawScale3D") {
        Some(Value::Vector(v)) => Vec3::from(*v),
        _ => Vec3::ONE,
    };
    let pre = match props.get("PrePivot") {
        Some(Value::Vector(v)) => Vec3::from(*v),
        _ => Vec3::ZERO,
    };
    (loc, rotator_matrix(rot) * Mat3::from_diagonal(ds3 * ds), pre)
}

/// Rotation matrix for an Unreal rotator (pitch, yaw, roll in 65536ths of a turn).
pub fn rotator_matrix(r: [i32; 3]) -> Mat3 {
    let k = std::f32::consts::TAU / 65536.0;
    let (sp, cp) = (r[0] as f32 * k).sin_cos();
    let (sy, cy) = (r[1] as f32 * k).sin_cos();
    let (sr, cr) = (r[2] as f32 * k).sin_cos();
    let x = Vec3::new(cp * cy, cp * sy, sp);
    let y = Vec3::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp);
    let z = Vec3::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp);
    Mat3::from_cols(x, y, z)
}

impl World {
    /// Builds the collision world for a map package. `game_dir` is used to read class defaults
    /// from the script packages (collision flags and player size).
    pub fn build(lib: &Library, map: &Arc<Package>) -> upkg::Result<World> {
        Self::build_with(lib, map, BuildOptions::default())
    }

    pub fn build_with(lib: &Library, map: &Arc<Package>, opts: BuildOptions) -> upkg::Result<World> {
        let t0 = std::time::Instant::now();
        lib.insert(map.clone());
        let mut stats = WorldStats::default();
        let mut tris: Vec<[Vec3; 3]> = Vec::new();
        let mut sources = Vec::new();
        let mut tri_mat: Vec<u32> = Vec::new();
        let mut mats = MatTable::default();

        // --- BSP polygons (solid and semi-solid brushes) ---
        let mut bsp = Bsp::empty();
        let mut zones = Vec::new();
        if let Some(mi) = find_world_model(map) {
            let m = read_model(map, mi)?;
            bsp = Bsp::new(&m);
            zones = m.zones.iter().map(|z| if let ObjRef::Export(e) = z { Some(*e as u32) } else { None }).collect();
            let mut surf_mat: HashMap<u32, u32> = HashMap::new();
            for n in &m.nodes {
                if n.num_verts < 3 {
                    continue;
                }
                let s = m.surfs[n.surf as usize];
                if s.flags & (pf::NOT_SOLID | pf::PORTAL) != 0 {
                    stats.bsp_nonsolid_skipped += 1;
                    continue;
                }
                let mi = *surf_mat.entry(n.surf as u32).or_insert_with(|| mats.of(lib, map, s.material));
                let poly: Vec<Vec3> = m.node_polygon(n).map(Vec3::from).collect();
                for k in 1..poly.len() - 1 {
                    tris.push([poly[0], poly[k], poly[k + 1]]);
                    sources.push(Source::Bsp);
                    tri_mat.push(mi);
                }
            }
            stats.bsp_tris = tris.len();
        }

        // --- Actors ---
        let mut defaults = ScriptDefaults::new(&lib.root);
        let pawn = defaults.pawn_params();
        let mut mesh_cache: HashMap<(String, usize), Arc<(Vec<[Vec3; 3]>, Vec<u16>, Vec<u32>)>> = HashMap::new();
        let mut player_starts = Vec::new();
        for i in 0..map.exports.len() {
            let class = map.export_class(i).to_string();
            if class == "Class" || class.ends_with("Property") || class == "Function" || class == "Polys" || class == "Model" || class == "Texture" {
                continue;
            }
            if class.eq_ignore_ascii_case("PlayerStart") {
                if let Ok((_, p)) = begin_object(map, i) {
                    if let Some(loc) = p.vector("Location") {
                        let yaw = p.rotator("Rotation").map(|r| r[1]).unwrap_or(0) as f32 * std::f32::consts::TAU / 65536.0;
                        player_starts.push((Vec3::from(loc), yaw));
                    }
                }
                continue;
            }
            let Ok((_, props)) = begin_object(map, i) else { continue };
            let Some(sm_ref) = props.object("StaticMesh").filter(|r| *r != ObjRef::Null) else { continue };
            let cf = defaults.class_flags(&class);
            let collide = props.bool("bCollideActors").unwrap_or(cf.collide_actors);
            let block = props.bool("bBlockPlayers").unwrap_or(cf.block_players) || props.bool("bBlockActors").unwrap_or(cf.block_actors);
            let nonzero = props.bool("bBlockNonZeroExtentTraces").unwrap_or(cf.block_nonzero_extent);
            if cf.from_script {
                stats.actors_from_script_flags += 1;
            }
            if !(collide && block && nonzero) {
                stats.nonblocking_actors += 1;
                continue;
            }
            let Some(sm_obj) = lib.resolve(map, sm_ref) else { continue };
            if opts.skip_movers && cf.is_mover {
                stats.movers_skipped += 1;
                continue;
            }
            if opts.skip_dynamic && !cf.is_mover && !props.bool("bStatic").unwrap_or(cf.is_static) {
                stats.dynamic_skipped += 1;
                continue;
            }
            let local = mesh_cache
                .entry(sm_obj.key())
                .or_insert_with(|| {
                    let (t, (slot, refs), kind) = mesh_collision_tris_mat(&sm_obj);
                    match kind {
                        Some(true) => stats.meshes_with_collision_tris += 1,
                        Some(false) => stats.meshes_render_fallback += 1,
                        None => {}
                    }
                    let slot_mat: Vec<u32> = refs.iter().map(|&r| mats.of(lib, &sm_obj.pkg, r)).collect();
                    Arc::new((t, slot, slot_mat))
                })
                .clone();
            if local.0.is_empty() {
                continue;
            }
            // A placed mesh's `Skins` override its section materials.
            let skin = |k: usize| match props.get_at("Skins", k as u32) {
                Some(Value::Object(r)) if *r != ObjRef::Null => Some(*r),
                _ => None,
            };
            let slot_mat: Vec<u32> = local.2.iter().enumerate().map(|(k, &m)| skin(k).map(|r| mats.of(lib, map, r)).unwrap_or(m)).collect();
            let (loc, m, pre) = actor_transform(&props);
            for (t, &s) in local.0.iter().zip(&local.1) {
                tris.push(t.map(|v| m * (v - pre) + loc));
                sources.push(Source::Actor(i as u32));
                tri_mat.push(slot_mat.get(s as usize).copied().unwrap_or(NO_MATERIAL));
            }
            stats.actor_tris += local.0.len();
            stats.blocking_actors += 1;
        }

        // --- Terrain ---
        let mut terrains = Vec::new();
        for ti in upkg::terrain::find_terrains(map) {
            if let Ok(t) = upkg::terrain::read_terrain(lib, map, ti) {
                let mut hf = Heightfield::new(&t);
                // Surface layers for HitMaterial: each layer's material and alpha map (a layer
                // without one covers the whole terrain).
                for l in &t.layers {
                    let mi = mats.of(lib, map, l.texture);
                    let alpha = if l.alpha_map == ObjRef::Null { None } else { lib.resolve(map, l.alpha_map).and_then(|o| upkg::terrain::read_alpha_map(&o).ok()) };
                    hf.layers.push((mi, alpha));
                }
                terrains.push(hf);
            }
        }
        stats.terrains = terrains.len();

        let bvh = Bvh::build(&tris);
        stats.load_ms = t0.elapsed().as_millis();
        Ok(World { tris, sources, tri_mat, materials: mats.paths, bvh, bsp, zones, terrains, player_starts, pawn, stats, removed: Default::default() })
    }

    /// An empty world containing only the given triangles (tests and tools).
    pub fn from_triangles(tris: Vec<[Vec3; 3]>) -> World {
        let sources = vec![Source::Bsp; tris.len()];
        let tri_mat = vec![NO_MATERIAL; tris.len()];
        let bvh = Bvh::build(&tris);
        World { tris, sources, tri_mat, materials: vec![], bvh, bsp: Bsp::empty(), zones: vec![], terrains: vec![], player_starts: vec![], pawn: PawnParams::default(), stats: WorldStats::default(), removed: Default::default() }
    }

    /// Path of a hit's material ("Package.Group.Name").
    pub fn material_path(&self, m: Option<u32>) -> Option<&str> {
        self.materials.get(m? as usize).map(|s| s.as_str())
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Bounds of all world triangles (BSP, meshes; terrain excluded). None for an empty world.
    pub fn bounds(&self) -> Option<Aabb> {
        let mut it = self.tris.iter().flatten();
        let first = *it.next()?;
        let (mut lo, mut hi) = (first, first);
        for p in it {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        Some(Aabb { min: lo, max: hi })
    }

    /// Stops the triangles of a level static mesh actor (map export index) from colliding.
    pub fn remove_actor(&self, export: u32) {
        if let Ok(mut r) = self.removed.write() {
            r.insert(export);
        }
    }

    /// Map export indices of the static mesh actors baked into this collision.
    pub fn actor_exports(&self) -> std::collections::HashSet<u32> {
        self.sources.iter().filter_map(|s| if let Source::Actor(e) = s { Some(*e) } else { None }).collect()
    }

    fn is_removed(&self, export: u32) -> bool {
        self.removed.read().is_ok_and(|r| !r.is_empty() && r.contains(&export))
    }

    /// Sweeps `shape` from `start` to `end`; returns the first blocking contact.
    pub fn trace(&self, start: Vec3, end: Vec3, shape: &Shape, flags: TraceFlags) -> Option<Hit> {
        let d = end - start;
        let ext = shape.extent();
        let qb = Aabb::swept(start, end, ext + Vec3::splat(0.01));
        let (faces, sedges) = shape.features();
        let mut best: Option<Hit> = None;
        let consider = |tri: &[Vec3; 3], src: Source, mat: u32, best: &mut Option<Hit>| {
            if let Some(h) = sweep_impl(start, shape, &faces, &sedges, d, tri) {
                let better = match best {
                    None => true,
                    Some(b) => h.t < b.t || (h.t == b.t && h.start_solid && (!b.start_solid || h.depth > b.depth)),
                };
                if better {
                    let material = (mat != NO_MATERIAL).then_some(mat);
                    *best = Some(Hit { t: h.t, location: start + d * h.t, normal: h.normal, start_solid: h.start_solid, depth: h.depth, source: src, material });
                }
            }
        };
        if flags.world || flags.actors {
            self.bvh.query_segment(start, d, ext + Vec3::splat(0.01), |i| {
                let src = self.sources[i as usize];
                let wanted = match src {
                    Source::Bsp => flags.world,
                    Source::Actor(e) => flags.actors && !self.is_removed(e),
                    Source::Terrain(_) => flags.terrain,
                };
                if wanted {
                    consider(&self.tris[i as usize], src, self.tri_mat.get(i as usize).copied().unwrap_or(NO_MATERIAL), &mut best);
                }
            });
        }
        if flags.terrain {
            for (k, hf) in self.terrains.iter().enumerate() {
                hf.for_tris(&qb, |t| consider(&t, Source::Terrain(k as u16), NO_MATERIAL, &mut best));
            }
        }
        // Terrain: the material is the layer with the most weight under the hit point.
        if let Some(h) = best.as_mut() {
            if let Source::Terrain(k) = h.source {
                h.material = self.terrains.get(k as usize).and_then(|hf| hf.layer_at(h.location)).filter(|&m| m != NO_MATERIAL);
            }
        }
        best
    }

    /// Zero-extent line trace.
    pub fn line(&self, start: Vec3, end: Vec3) -> Option<Hit> {
        self.trace(start, end, &Shape::POINT, TraceFlags::ALL)
    }

    /// Calls `f` for every world triangle (BSP, level static meshes, terrain) whose bounds may
    /// overlap `b` (for decal clipping and similar queries).
    pub fn for_tris_in(&self, b: &Aabb, mut f: impl FnMut(&[Vec3; 3])) {
        self.bvh.query(b, |i| f(&self.tris[i as usize]));
        for hf in &self.terrains {
            hf.for_tris(b, |t| f(&t));
        }
    }

    /// As `for_tris_in`, limited to the sources in `flags` (world = BSP, actors = static mesh
    /// actors, terrain). Projectors use it for bProjectBSP / bProjectStaticMesh / bProjectTerrain.
    pub fn for_tris_in_flags(&self, b: &Aabb, flags: TraceFlags, mut f: impl FnMut(&[Vec3; 3])) {
        if flags.world || flags.actors {
            self.bvh.query(b, |i| {
                let wanted = match self.sources[i as usize] {
                    Source::Bsp => flags.world,
                    Source::Actor(e) => flags.actors && !self.is_removed(e),
                    Source::Terrain(_) => flags.terrain,
                };
                if wanted {
                    f(&self.tris[i as usize]);
                }
            });
        }
        if flags.terrain {
            for hf in &self.terrains {
                hf.for_tris(b, |t| f(&t));
            }
        }
    }

    /// True if `shape` placed at `p` overlaps any geometry.
    pub fn encroaches(&self, p: Vec3, shape: &Shape) -> bool {
        self.penetration(p, shape).is_some()
    }

    /// Deepest overlap of `shape` at `p` with any geometry: (push-out normal, depth). A
    /// zero-length sweep reports every overlapping triangle as start-solid.
    pub fn penetration(&self, p: Vec3, shape: &Shape) -> Option<(Vec3, f32)> {
        self.trace(p, p, shape, TraceFlags::ALL).filter(|h| h.start_solid).map(|h| (h.normal, h.depth))
    }
}
