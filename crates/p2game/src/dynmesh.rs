//! Windowed rendering of static mesh actors spawned at runtime.
//!
//! p2view builds the static mesh actors that are saved in the map, and `rigs` handles skeletal
//! actors. This module covers everything else with `DrawType == DT_StaticMesh` that script
//! spawns while the game runs (summoned KActors such as `BarrelExplodable`, thrown or dropped
//! props, pickups). Each mesh section becomes one entity with a `StandardMaterial`. Every frame
//! the entities are placed from the actor's Location/Rotation/DrawScale, and they are hidden while
//! the actor is hidden or destroyed. If script changes `StaticMesh`, the entities are rebuilt.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use p2anim::rig::{c_mat, make_image, to_bevy};
use p2game::host::{actor_world, resolve_path};
use std::collections::{HashMap, HashSet};
use upkg::library::{Library, Obj};
use upkg::texture::read_texture;
use uscript::vm::{ObjId, Value, Vm};

/// `EDrawType::DT_StaticMesh`.
pub(crate) const DT_STATIC_MESH: u8 = 8;

/// Static lighting for runtime-drawn static meshes, as UE2 lights a non-moving actor: the zone
/// ambient plus each map light that reaches the actor's location (sunlight traced toward the sun,
/// point lights within radius and traced), with N.L per vertex, in the modulate-2x vertex scale.
pub struct ActorLighting {
    /// (light, is_sun, colour * brightness / 255 * 0.5, direction or location, radius)
    lights: Vec<(bool, Vec3, Vec3, f32, upkg::lighting::MapLight)>,
    /// Ambient (0-255 scale) per zone actor export, and the LevelInfo's as fallback.
    zone_ambient: HashMap<usize, [f32; 3]>,
    fallback_ambient: [f32; 3],
    /// Level actor -> zone actor export it was saved in (Region.Zone).
    actor_zone: HashMap<ObjId, usize>,
    /// Parsed mesh vertices/normals per static mesh path (bake input).
    #[allow(clippy::type_complexity)]
    geo_cache: std::sync::Mutex<HashMap<String, Option<std::sync::Arc<(Vec<[f32; 3]>, Vec<[f32; 3]>)>>>>,
}

impl ActorLighting {
    pub fn new(map: &upkg::Package, level_actors: &[(usize, ObjId)]) -> Self {
        let actor_zone = level_actors
            .iter()
            .filter_map(|&(e, o)| {
                let (_, p) = upkg::begin_object(map, e).ok()?;
                let Some(upkg::Value::Struct { raw, .. }) = p.get("Region") else { return None };
                let mut r = upkg::Reader::new(raw);
                match upkg::read_properties(&mut r, map).ok()?.object("Zone")? {
                    upkg::ObjRef::Export(z) => Some((o, z)),
                    _ => None,
                }
            })
            .collect();
        let mut seen = HashSet::new();
        let lights = upkg::lighting::read_lights(map)
            .into_iter()
            .filter(|l| l.kind != upkg::lighting::LightKind::Sun || seen.insert((l.rotation, l.hue, l.saturation, l.brightness.to_bits())))
            .map(|l| {
                let c = Vec3::from(upkg::lighting::hsv_to_rgb(l.hue, l.saturation)) * (l.brightness / 255.0) * 0.5;
                let sun = l.kind == upkg::lighting::LightKind::Sun;
                (sun, c, if sun { Vec3::from(l.direction()) } else { Vec3::from(l.location) }, l.world_radius(), l)
            })
            .collect();
        let zones = upkg::lighting::read_zones(map);
        let amb = |z: &upkg::lighting::ZoneLight| {
            let c = upkg::lighting::hsv_to_rgb(z.ambient_hue, z.ambient_saturation);
            let b = z.ambient_brightness as f32 * 0.65;
            [c[0] * b, c[1] * b, c[2] * b]
        };
        let zone_ambient = zones.iter().filter(|z| z.ambient_brightness > 0).map(|z| (z.export, amb(z))).collect();
        let fallback_ambient = upkg::lighting::primary_zone(&zones).map(amb).unwrap_or([0.0; 3]);
        ActorLighting { lights, zone_ambient, fallback_ambient, actor_zone, geo_cache: Default::default() }
    }

    /// Per-vertex light factors (linear, for the baked vertex-colour material) for a mesh whose
    /// Unreal-space vertices/normals are given, placed by `world` (Unreal space).
    fn vertex_light(&self, coll: Option<&p2collide::World>, a: ObjId, loc: Vec3, world: Mat4, pos: &[[f32; 3]], nrm: &[[f32; 3]]) -> Vec<[f32; 4]> {
        let amb = self.actor_zone.get(&a).and_then(|z| self.zone_ambient.get(z).copied()).unwrap_or(self.fallback_ambient);
        let visible = |to: Vec3| match coll {
            Some(w) => match w.line(loc + Vec3::Z * 8.0, to) {
                None => true,
                Some(h) => h.start_solid || h.t >= 0.98,
            },
            None => true,
        };
        // Lights that reach the actor (one trace per light, from the actor's origin).
        let reach: Vec<&(bool, Vec3, Vec3, f32, upkg::lighting::MapLight)> = self
            .lights
            .iter()
            .filter(|(sun, _, p, r, _)| if *sun { visible(loc - *p * 20000.0) } else { p.distance(loc) < *r && visible(*p) })
            .collect();
        if std::env::var_os("P2GAME_DYNMESH_LIGHTS").is_some() {
            for (_, c, p, r, l) in &reach {
                println!("dynmesh light actor {a:?} at {loc:?} <- {} {} c {c:.2?} at {p:?} r {r} effect {} cone {} spot@loc {:.2}", l.class, l.export, l.light_effect, l.cone, l.spot((loc - *p).normalize_or_zero().to_array()));
            }
            println!("dynmesh light actor {a:?} ambient {amb:?}");
        }
        let nm = Mat3::from_mat4(world).inverse().transpose();
        pos.iter()
            .zip(nrm)
            .map(|(p, n)| {
                let wp = world.transform_point3(Vec3::from(*p));
                let wn = (nm * Vec3::from(*n)).normalize_or_zero();
                let mut acc = Vec3::ZERO;
                for (sun, c, d, r, l) in &reach {
                    let i = if *sun {
                        (-2.0 * d.dot(wn)).max(0.0)
                    } else {
                        let v = *d - wp;
                        let dist = v.length().max(1e-3);
                        let f = (dist / r).min(1.0);
                        2.0 * (2.0 * f * f * f - 3.0 * f * f + 1.0) * (v / dist).dot(wn).max(0.0) * l.spot((-v / dist).to_array())
                    };
                    acc += *c * i;
                }
                let px = [(acc.x.clamp(0.0, 1.0) * 255.0) as u8, (acc.y.clamp(0.0, 1.0) * 255.0) as u8, (acc.z.clamp(0.0, 1.0) * 255.0) as u8, 255];
                p2view::baked::vertex_light(px, amb)
            })
            .collect()
    }
}

/// Render entities of one runtime static mesh actor.
struct DynActor {
    mesh_path: String,
    sections: Vec<Entity>,
    /// Pose and visibility last applied to the entities.
    last: Option<([f32; 3], [i32; 3], Option<u32>, bool)>,
}

#[derive(Default)]
pub struct DynMeshTracker {
    /// Actors present when the level loaded (rendered by p2view, never by this module).
    level_actors: HashSet<ObjId>,
    /// Level actors p2view did not draw but that have a static mesh (class-default StaticMesh).
    level_extra: HashSet<ObjId>,
    /// Static lighting for level_extra actors (built at level load).
    lighting: Option<std::sync::Arc<ActorLighting>>,
    /// Bake-ahead of the level pickups' vertex colours, started at level load on a worker (see
    /// [`DynMeshTracker::start_bake_ahead`]). Taken on the first frame that bakes.
    ahead: Option<std::thread::JoinHandle<Vec<(BakeKey, Vec<[f32; 4]>)>>>,
    /// Finished bake-ahead results, keyed by their exact inputs, and the collision world they were
    /// traced against (held so its address, part of the key, cannot be reused).
    ahead_done: HashMap<BakeKey, Vec<[f32; 4]>>,
    ahead_coll: Option<std::sync::Arc<p2collide::World>>,
    /// Highest actor id handled at load (everything at or below it is a level actor or skipped).
    floor: Option<ObjId>,
    actors: HashMap<ObjId, DynActor>,
    /// Per static mesh path: section meshes and materials (None if the mesh failed to load).
    cache: HashMap<String, Option<Vec<(Handle<Mesh>, Handle<StandardMaterial>)>>>,
    textures: HashMap<(usize, usize), Option<Handle<Image>>>,
    pub spawned: usize,
}

impl DynMeshTracker {
    /// Call when a level finishes loading. `rendered` are the level actors p2view already drew
    /// (their StaticMesh is saved in the map); other level actors that draw a static mesh (from
    /// their class defaults, e.g. ShovelPickup in the suburbs-3 shed) are drawn here.
    pub fn level_loaded(&mut self, vm: &Vm, rendered: &HashSet<ObjId>, player: Option<ObjId>) {
        self.actors.clear();
        self.floor = vm.actors.iter().copied().max();
        self.level_extra = vm.actors.iter().copied().filter(|a| !rendered.contains(a) && static_mesh_path(vm, *a, player).is_some()).collect();
        self.level_actors = vm.actors.iter().copied().filter(|a| !self.level_extra.contains(a)).collect();
    }

    /// Static lighting data for the level's pickups (zones and lights of the loaded map).
    pub fn set_lighting(&mut self, lighting: ActorLighting) {
        self.lighting = Some(std::sync::Arc::new(lighting));
        self.ahead = None;
        self.ahead_done.clear();
        self.ahead_coll = None;
    }

    /// Starts baking every level pickup's vertex colours on a worker thread, from the inputs the
    /// first frame will see (actor, mesh, transform, the level collision). The frame that draws
    /// them takes a result only when its own inputs are bit-identical (a pickup moved or swapped
    /// by level-start script is baked on the spot as before), and waits for the worker rather
    /// than show unbaked pickups, so the result is the same as without it.
    /// `P2GAME_NO_BAKE_AHEAD=1` turns it off.
    pub fn start_bake_ahead(&mut self, lib: &std::sync::Arc<Library>, vm: &Vm, map: &upkg::Package, level_actors: &[(usize, ObjId)], player: Option<ObjId>) {
        if std::env::var_os("P2GAME_NO_BAKE_AHEAD").is_some() || std::env::var_os("P2GAME_DYN_DYNAMIC").is_some() {
            return;
        }
        let Some(lit) = self.lighting.clone() else { return };
        // Candidates: level actors that draw a static mesh their map entry does not save (the
        // ones `level_loaded` will hand to this module). A wrong guess only costs worker time.
        let inputs: Vec<(ObjId, String, Vec3, Mat4)> = level_actors
            .iter()
            .filter(|(e, _)| !upkg::begin_object(map, *e).ok().is_some_and(|(_, p)| p.object("StaticMesh").is_some()))
            .filter_map(|&(_, a)| static_mesh_path(vm, a, player).map(|p| (a, p.to_string(), Vec3::from(vm.actor_loc(a)), actor_world(vm, a))))
            .collect();
        if inputs.is_empty() {
            return;
        }
        let lib = lib.clone();
        let coll = p2game::physics::level_collision();
        self.ahead_coll = coll.clone();
        let t0 = std::time::Instant::now();
        let h = std::thread::Builder::new().name("bake-ahead".into()).spawn(move || {
            let jobs: Vec<BakeJob> = inputs.iter().filter_map(|(a, path, loc, w)| bake_geo(&lib, path, &lit).map(|g| (*a, *loc, *w, g))).collect();
            let keys: Vec<BakeKey> = inputs.iter().filter(|(_, path, _, _)| bake_geo(&lib, path, &lit).is_some()).map(|(a, path, loc, w)| bake_key(*a, path, *loc, *w, coll.as_ref())).collect();
            let colors = bake_colors_with(&lit, coll.as_deref(), &jobs);
            if std::env::var_os("P2GAME_BOOT_TIMING").is_some() {
                println!("dynmesh: bake-ahead of {} pickups done in {:.0} ms", jobs.len(), t0.elapsed().as_secs_f64() * 1e3);
            }
            keys.into_iter().zip(colors).collect()
        });
        self.ahead = h.ok();
    }

    /// Results of the bake-ahead (waits for the worker if it is still running).
    fn take_ahead(&mut self) {
        if let Some(h) = self.ahead.take() {
            let t = std::time::Instant::now();
            if let Ok(r) = h.join() {
                self.ahead_done.extend(r);
            }
            let ms = t.elapsed().as_secs_f64() * 1e3;
            if ms > 1.0 && std::env::var_os("P2GAME_SPIKES").is_some() {
                println!("dynmesh: waited {ms:.1} ms for the bake-ahead");
            }
        }
    }

    pub fn level_extra_count(&self) -> usize {
        self.level_extra.len()
    }

    fn texture(&mut self, lib: &Library, images: &mut Assets<Image>, mat: &Obj) -> Option<Handle<Image>> {
        let key = (std::sync::Arc::as_ptr(&mat.pkg) as usize, mat.export);
        if let Some(h) = self.textures.get(&key) {
            return h.clone();
        }
        let h = lib.material_texture(mat).and_then(|mi| mi.texture.map(|t| (t, mi.opacity))).and_then(|(t, opacity)| {
            let tex = read_texture(&t.pkg, t.export).ok()?;
            // Shader Opacity texture (window glass): its alpha replaces the diffuse's.
            let op = opacity.and_then(|o| read_texture(&o.pkg, o.export).ok()?.decode_rgba(0).ok());
            let (w, h) = (tex.mips.first()?.width, tex.mips.first()?.height);
            let mut data = Vec::new();
            let mut levels = 0u32;
            let (mut ew, mut eh) = (w, h);
            for level in 0..tex.mips.len() {
                let m = &tex.mips[level];
                if m.width != ew || m.height != eh {
                    break;
                }
                let Ok((_, _, mut px)) = tex.decode_rgba(level) else { break };
                if let Some((sw, sh, src)) = &op {
                    upkg::texture::merge_alpha(&mut px, ew, eh, src, *sw, *sh);
                }
                data.extend_from_slice(&px);
                levels += 1;
                if ew == 1 && eh == 1 {
                    break;
                }
                ew = (ew / 2).max(1);
                eh = (eh / 2).max(1);
            }
            (levels > 0).then(|| images.add(make_image(w, h, levels, data)))
        });
        self.textures.insert(key, h.clone());
        h
    }

    /// Section meshes and materials of a static mesh (cached per path).
    pub(crate) fn sections(&mut self, lib: &Library, path: &str, skins: &[Option<Obj>], meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>, images: &mut Assets<Image>) -> Option<Vec<(Handle<Mesh>, Handle<StandardMaterial>)>> {
        // Keyed by the skin materials themselves: severed limbs and stumps share one mesh but
        // take each victim's body skin (Limb/Stump.Setup*: Skins[0] = the pawn's skin).
        let key = if skins.iter().any(|s| s.is_some()) {
            let ids: Vec<String> = skins.iter().map(|s| s.as_ref().map_or("-".to_string(), |o| format!("{}:{}", o.pkg.name(o.pkg.exports[o.export].name), o.export))).collect();
            format!("{path}#skins{}", ids.join(","))
        } else {
            path.to_string()
        };
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let built = (|| {
            let obj = resolve_path(lib, path, Some("StaticMesh"))?;
            let sm = upkg::staticmesh::read_static_mesh(&obj.pkg, obj.export).ok()?;
            if std::env::var_os("P2GAME_DYNMESH").is_some() {
                println!("dynmesh mesh {path} bounds {:?}..{:?} vert z max {:?}", sm.bounds_min, sm.bounds_max, sm.positions.iter().map(|p| p[2]).fold(f32::MIN, f32::max));
            }
            let pos: Vec<[f32; 3]> = sm.positions.iter().map(|p| to_bevy(*p).to_array()).collect();
            let nrm: Vec<[f32; 3]> = sm.normals.iter().map(|n| Vec3::new(n[0], n[2], n[1]).normalize_or_zero().to_array()).collect();
            let uvs = if sm.uvs.len() == pos.len() { sm.uvs.clone() } else { vec![[0.0; 2]; pos.len()] };
            let mut out = Vec::new();
            for s in 0..sm.sections.len() {
                // The Y/Z swap mirrors geometry: reverse the winding.
                let idx: Vec<u32> = sm.section_indices(s).chunks(3).filter(|t| t.len() == 3).flat_map(|t| [t[0] as u32, t[2] as u32, t[1] as u32]).collect();
                if idx.is_empty() {
                    continue;
                }
                // MAIN_WORLD too: bake_sections later copies these (cached across levels) to add
                // vertex colours, which needs the vertex data after the mesh has been extracted.
                let m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos.clone())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, nrm.clone())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone())
                    .with_inserted_indices(Indices::U32(idx));
                let mat_obj = skins.get(s).cloned().flatten().or_else(|| sm.materials.get(s).and_then(|r| lib.resolve(&obj.pkg, *r)));
                // A Shader with OutputBlending OB_Invisible (4) draws nothing. ProtestSignWeapon
                // hides its first-person sign this way (Skins[0] = P2R_Tex_D.Weapons.fake when
                // it is not on a bone); without this it showed as a grey prism (FIDELITY #48).
                if mat_obj.as_ref().is_some_and(|o| o.class().eq_ignore_ascii_case("Shader") && upkg::begin_object(&o.pkg, o.export).ok().and_then(|(_, p)| p.byte("OutputBlending")) == Some(4)) {
                    continue;
                }
                let tex = mat_obj.as_ref().and_then(|o| self.texture(lib, images, o));
                let info = mat_obj.as_ref().and_then(|o| lib.material_texture(o));
                let masked = info.as_ref().is_some_and(|mi| mi.masked);
                let blended = info.as_ref().is_some_and(|mi| mi.opacity.is_some());
                let two_sided = info.as_ref().is_some_and(|mi| mi.two_sided);
                // Non-alpha framebuffer ops (Shader OutputBlending / FinalBlend). The NPC
                // third-person muzzle flash (PistolMuzzleFlash: Timb_mesh mf_pistol, Shader
                // pistol_corona_add OB_Translucent, texture alpha 255 everywhere) was drawn
                // opaque and lit: a big brown card sticking out of the gun. UE2 adds these
                // (black = clear) unlit; modulate multiplies the framebuffer (2x).
                let op = info.as_ref().and_then(|mi| mi.op).filter(|_| std::env::var_os("P2GAME_DYNMESH_NO_OP").is_none());
                let additive = matches!(op, Some(upkg::library::BlendOp::Brighten | upkg::library::BlendOp::Translucent));
                let modulate = op == Some(upkg::library::BlendOp::Modulate);
                if std::env::var_os("P2GAME_DYNMESH").is_some() {
                    println!("dynmesh {path} section {s}: material {:?} class {:?} texture {} op {op:?}", mat_obj.as_ref().map(|o| o.pkg.name(o.pkg.exports[o.export].name).to_string()), mat_obj.as_ref().map(|o| o.class().to_string()), tex.is_some());
                }
                if additive || modulate {
                    let material = materials.add(StandardMaterial {
                        base_color: if modulate { Color::linear_rgb(4.595, 4.595, 4.595) } else { Color::WHITE },
                        base_color_texture: tex,
                        alpha_mode: if modulate { AlphaMode::Multiply } else { AlphaMode::Add },
                        unlit: true,
                        double_sided: true,
                        cull_mode: if two_sided { None } else { Some(bevy::render::render_resource::Face::Back) },
                        ..default()
                    });
                    out.push((meshes.add(m), material));
                    continue;
                }
                let material = materials.add(StandardMaterial {
                    base_color: if tex.is_some() { Color::WHITE } else { Color::srgb(0.5, 0.5, 0.5) },
                    base_color_texture: tex,
                    alpha_mode: if blended { AlphaMode::Blend } else if masked { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
                    // Blended one-sided sections (window glass: separate outer/inner pane
                    // sections) are back-face culled as in UE2, so each pane is drawn once.
                    double_sided: !blended || two_sided,
                    cull_mode: if blended && !two_sided { Some(bevy::render::render_resource::Face::Back) } else { None },
                    perceptual_roughness: 0.8,
                    // Glass draws after the other blended meshes. Bevy sorts translucents by mesh
                    // centre, so foliage behind a window (bAlphaTexture leaves, also blended) often
                    // sorted nearer and was drawn over the pane: jagged dark patches through the
                    // glass (playtest "weird artifacts through the windows"). Panes are mostly
                    // clear, so drawing them last is the smaller error. Same value as the sky haze
                    // (outdoor.rs); verified on suburbs-3 window views (out\glass\m1_sheet.png).
                    // P2GAME_GLASS_BIAS overrides.
                    depth_bias: if blended { std::env::var("P2GAME_GLASS_BIAS").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000.0) } else { 0.0 },
                    ..default()
                });
                out.push((meshes.add(m), material));
            }
            (!out.is_empty()).then_some(out)
        })();
        self.cache.insert(key, built.clone());
        built
    }
}

/// Bake input for one pickup: (actor, location, world matrix, mesh vertices/normals).
type BakeJob = (ObjId, Vec3, Mat4, std::sync::Arc<(Vec<[f32; 3]>, Vec<[f32; 3]>)>);

/// Exact inputs of one bake (actor, mesh path, location and matrix bits, collision world): two
/// bakes with the same key give the same colours.
type BakeKey = (ObjId, String, [u32; 3], [u32; 16], usize);

fn bake_key(a: ObjId, path: &str, loc: Vec3, w: Mat4, coll: Option<&std::sync::Arc<p2collide::World>>) -> BakeKey {
    let m = w.to_cols_array();
    (a, path.to_string(), loc.to_array().map(f32::to_bits), std::array::from_fn(|i| m[i].to_bits()), coll.map_or(0, |c| std::sync::Arc::as_ptr(c) as usize))
}

/// Parsed vertices/normals of a static mesh (cached per level in the lighting).
fn bake_geo(lib: &Library, path: &str, lit: &ActorLighting) -> Option<std::sync::Arc<(Vec<[f32; 3]>, Vec<[f32; 3]>)>> {
    lit.geo_cache.lock().unwrap_or_else(|p| p.into_inner()).entry(path.to_string()).or_insert_with(|| {
        let obj = resolve_path(lib, path, Some("StaticMesh"))?;
        let sm = upkg::staticmesh::read_static_mesh(&obj.pkg, obj.export).ok()?;
        Some(std::sync::Arc::new((sm.positions, sm.normals)))
    }).clone()
}

fn bake_job(lib: &Library, vm: &Vm, a: ObjId, path: &str, lit: &ActorLighting) -> Option<BakeJob> {
    // Parse each mesh once per level (suburbs-3 bakes ~470 pickups at load).
    let geo = bake_geo(lib, path, lit)?;
    // actor_world is in Unreal space, as are the mesh's own vertices and normals.
    Some((a, Vec3::from(vm.actor_loc(a)), actor_world(vm, a), geo))
}

fn bake_threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16)
}

/// Vertex colours for many pickups. The light visibility traces (~4 ms per actor, 364 lights
/// on suburbs-3) dominate, and each actor is independent, so spread them over threads.
fn bake_colors_with(lit: &ActorLighting, coll: Option<&p2collide::World>, jobs: &[BakeJob]) -> Vec<Vec<[f32; 4]>> {
    let threads = bake_threads();
    let chunk = jobs.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let hs: Vec<_> = jobs.chunks(chunk).map(|c| s.spawn(move || c.iter().map(|(a, loc, w, g)| lit.vertex_light(coll, *a, *loc, *w, &g.0, &g.1)).collect::<Vec<_>>())).collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// Replaces a static pickup's dynamically lit section entities' meshes/materials with
/// vertex-lit copies (baked material), lit once from the actor's place in the level.
fn bake_sections(commands: &mut Commands, colors: Vec<[f32; 4]>, sections: &[Entity]) {
    let entries = sections.to_vec();
    commands.queue(move |world: &mut World| {
        let mut out = Vec::new();
        for e in entries {
            let Some(mh) = world.get::<Mesh3d>(e).map(|m| m.0.clone()) else { continue };
            let Some(base) = world.get::<MeshMaterial3d<StandardMaterial>>(e).and_then(|m| world.resource::<Assets<StandardMaterial>>().get(&m.0).cloned()) else { continue };
            // Unlit framebuffer-op sections (additive flares, modulate) keep their own material.
            if base.unlit {
                continue;
            }
            let Some(mut mesh) = world.resource::<Assets<Mesh>>().get(&mh).cloned() else { continue };
            if mesh.count_vertices() != colors.len() {
                continue;
            }
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors.clone());
            let nh = world.resource_mut::<Assets<Mesh>>().add(mesh);
            if let Ok(mut ent) = world.get_entity_mut(e) {
                ent.insert(Mesh3d(nh));
                ent.remove::<MeshMaterial3d<StandardMaterial>>();
            }
            out.push((e, 0u32, base));
        }
        for (e, _, base) in out {
            let white = world.resource_mut::<Assets<Image>>().add(p2view::baked::lightmap_image(&p2view::scene::LightmapData { width: 1, height: 1, rgba: vec![128, 128, 128, 255] }));
            let mat = world.resource_mut::<Assets<p2view::baked::BakedMaterial>>().add(p2view::baked::BakedMaterial { base, extension: p2view::baked::BakedLight { lightmap: white } });
            if let Ok(mut ent) = world.get_entity_mut(e) {
                ent.insert(MeshMaterial3d(mat));
            }
        }
    });
}

fn static_mesh_path(vm: &Vm, a: ObjId, player: Option<ObjId>) -> Option<&str> {
    if !matches!(vm.get_prop(a, "DrawType"), Some(Value::Byte(DT_STATIC_MESH))) {
        return None;
    }
    // First-person-only meshes, and the player's own third-person attachments, are not drawn
    // in the world view (same rule as the skeletal rigs).
    let flag = |n: &str| vm.get_prop(a, n).is_some_and(|v| v.as_bool());
    if flag("bOnlyOwnerSee") || (flag("bOwnerNoSee") && player.is_some_and(|p| owned_by(vm, a, p))) {
        return None;
    }
    // Canvas-only props: MapScreen's HeadMarker/TrailMarker (ArrowBox/DashBox) and NewsScreen's
    // Paper are RawActors owned by the PlayerController that script draws through
    // Canvas.DrawActor, placed just in front of the camera while the screen runs. In the world
    // view they would hang there as an untextured block (FIDELITY #48).
    if vm.classes[vm.objects[a].class].name.eq_ignore_ascii_case("RawActor")
        && vm.get_prop(a, "Owner").and_then(|v| v.as_obj()).is_some_and(|o| vm.class_is_a(vm.objects[o].class, "PlayerController"))
    {
        return None;
    }
    // In first person the player's pawn is not drawn, and neither is what is attached to it (Base
    // chain): the third-person weapon in the Dude's hand (PistolAttachment, bOwnerNoSee false)
    // would otherwise float in front of the camera (FIDELITY #46). The behind view shows them.
    if let Some(p) = player {
        let pawn = vm.get_prop(p, "Pawn").and_then(|v| v.as_obj());
        if pawn.is_some_and(|pw| based_on(vm, a, pw)) && !crate::ui::third_person(vm, p) {
            return None;
        }
    }
    vm.get_prop(a, "StaticMesh").and_then(|v| v.as_resource())
}

/// True if `a`'s Base chain reaches `pawn`.
fn based_on(vm: &Vm, a: ObjId, pawn: ObjId) -> bool {
    let mut cur = vm.get_prop(a, "Base").and_then(|v| v.as_obj());
    for _ in 0..6 {
        let Some(c) = cur else { return false };
        if c == pawn {
            return true;
        }
        cur = vm.get_prop(c, "Base").and_then(|v| v.as_obj());
    }
    false
}

/// True if `a` is `who`, its pawn, or owned (transitively) by either.
fn owned_by(vm: &Vm, a: ObjId, who: ObjId) -> bool {
    let pawn = vm.get_prop(who, "Pawn").and_then(|v| v.as_obj());
    let mut cur = Some(a);
    for _ in 0..8 {
        let Some(c) = cur else { return false };
        if c == who || Some(c) == pawn {
            return true;
        }
        cur = vm.get_prop(c, "Owner").and_then(|v| v.as_obj());
    }
    false
}

/// Spawns/rebuilds/removes render entities for runtime static mesh actors and places them.
#[allow(clippy::too_many_arguments)]
pub fn update(
    commands: &mut Commands,
    lib: &Library,
    vm: &Vm,
    player: Option<ObjId>,
    tracker: &mut DynMeshTracker,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    tfs: &mut Query<(&mut Transform, &mut Visibility)>,
) {
    let floor = tracker.floor;
    // New or changed actors: only ids above the load floor (the object table only grows).
    let mut want: Vec<(ObjId, String)> = Vec::new();
    // Candidates that draw a static mesh this frame (the place pass below reuses it rather than
    // evaluating `static_mesh_path` a second time per actor; the VM does not change in between).
    let mut drawn: HashSet<ObjId> = HashSet::with_capacity(tracker.actors.len() + 8);
    // Candidates: actors spawned after the load (id above the floor) plus the level's extra mesh
    // actors. Testing the other ~5000 level actors against two hash sets every frame was a
    // measurable part of the frame; the floor test alone rejects them.
    let mut consider = |a: ObjId, want: &mut Vec<(ObjId, String)>| {
        if vm.objects.get(a).is_none_or(|o| o.deleted) || tracker.level_actors.contains(&a) {
            return;
        }
        if let Some(p) = static_mesh_path(vm, a, player) {
            drawn.insert(a);
            if tracker.actors.get(&a).is_none_or(|d| d.mesh_path != p) {
                want.push((a, p.to_string()));
            }
        }
    };
    for &a in &vm.actors {
        if floor.is_some_and(|f| a <= f) {
            continue;
        }
        consider(a, &mut want);
    }
    if floor.is_some() {
        for &a in &tracker.level_extra {
            consider(a, &mut want);
        }
    }
    if want.len() > 4 && std::env::var_os("P2GAME_SPIKES").is_some() {
        println!("dynmesh: {} new actors this frame ({} cached meshes)", want.len(), tracker.cache.len());
    }
    let mut bakes: Vec<(BakeJob, Vec<Entity>)> = Vec::new();
    for (a, path) in want {
        if let Some(old) = tracker.actors.remove(&a) {
            for e in old.sections {
                commands.entity(e).despawn();
            }
        }
        let skins: Vec<Option<Obj>> = match vm.get_prop(a, "Skins") {
            Some(Value::Array(s)) => s.iter().map(|v| v.as_resource().and_then(|p| resolve_path(lib, p, None))).collect(),
            _ => Vec::new(),
        };
        if std::env::var_os("P2GAME_DYNMESH").is_some() {
            println!("dynmesh skins of {} {:?} -> {:?}", vm.object_name(a), vm.get_prop(a, "Skins"), skins.iter().map(|s| s.is_some()).collect::<Vec<_>>());
        }
        let sections: Vec<Entity> = tracker
            .sections(lib, &path, &skins, meshes, materials, images)
            .unwrap_or_default()
            .into_iter()
            .map(|(m, mat)| commands.spawn((Mesh3d(m), MeshMaterial3d(mat), Transform::IDENTITY, Visibility::Hidden)).id())
            .collect();
        // Level pickups lying still get static (baked-style) vertex lighting, like the map's own
        // static meshes, instead of the dim dynamic lights.
        if tracker.level_extra.contains(&a) && std::env::var_os("P2GAME_DYN_DYNAMIC").is_none() {
            if let Some(lit) = tracker.lighting.as_ref() {
                if let Some(job) = bake_job(lib, vm, a, &path, lit) {
                    bakes.push((job, sections.clone()));
                }
            }
        }
        if std::env::var_os("P2GAME_DYNMESH").is_some() {
            println!("dynmesh {a} {} {path}", vm.classes[vm.objects[a].class].name);
        }
        tracker.spawned += 1;
        tracker.actors.insert(a, DynActor { mesh_path: path, sections, last: None });
    }
    if let (false, Some(lit)) = (bakes.is_empty(), tracker.lighting.clone()) {
        // Bake-ahead results for jobs whose inputs match exactly; the rest are baked here.
        tracker.take_ahead();
        let coll = p2game::physics::level_collision();
        let mut colors_for: Vec<Option<Vec<[f32; 4]>>> = Vec::with_capacity(bakes.len());
        let mut todo: Vec<BakeJob> = Vec::new();
        for (job, _) in &bakes {
            let hit = if tracker.ahead_done.is_empty() { None } else { tracker.ahead_done.remove(&bake_key(job.0, &tracker.actors[&job.0].mesh_path, job.1, job.2, coll.as_ref())) };
            if hit.is_none() {
                todo.push(job.clone());
            }
            colors_for.push(hit);
        }
        let n_ahead = bakes.len() - todo.len();
        if std::env::var_os("P2GAME_BAKE_VERIFY").is_some() && n_ahead > 0 {
            // Re-bake the bake-ahead hits here and compare bit for bit.
            let hits: Vec<(usize, BakeJob)> = colors_for.iter().zip(&bakes).enumerate().filter(|(_, (c, _))| c.is_some()).map(|(i, (_, (j, _)))| (i, j.clone())).collect();
            let jobs: Vec<BakeJob> = hits.iter().map(|(_, j)| j.clone()).collect();
            let sync = bake_colors_with(&lit, coll.as_deref(), &jobs);
            let bits = |v: &[[f32; 4]]| v.iter().flat_map(|c| c.map(f32::to_bits)).collect::<Vec<u32>>();
            let bad = hits.iter().zip(&sync).filter(|((i, _), s)| bits(colors_for[*i].as_deref().unwrap_or_default()) != bits(s)).count();
            println!("dynmesh: bake-ahead verify: {} of {n_ahead} hits {}", n_ahead - bad, if bad == 0 { "IDENTICAL" } else { "DIFFER" });
        }
        let mut fresh = bake_colors_with(&lit, coll.as_deref(), &todo).into_iter();
        if std::env::var_os("P2GAME_SPIKES").is_some() && n_ahead > 0 {
            println!("dynmesh: {n_ahead} of {} pickup bakes from the bake-ahead", bakes.len());
        }
        let dump = std::env::var_os("P2GAME_BAKE_DUMP");
        for (pre, (job, sections)) in colors_for.into_iter().zip(&bakes) {
            let colors = pre.unwrap_or_else(|| fresh.next().unwrap_or_default());
            if let Some(path) = &dump {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let bytes: Vec<u8> = colors.iter().flat_map(|c| c.iter().flat_map(|x| x.to_le_bytes())).collect();
                    let _ = writeln!(f, "{} {} {}", vm.object_name(job.0), colors.len(), bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
                }
            }
            if std::env::var_os("P2GAME_DYNMESH").is_some() && !colors.is_empty() {
                let n = colors.len() as f32;
                let (lo, hi) = colors.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), c| ([lo[0].min(c[0]), lo[1].min(c[1]), lo[2].min(c[2])], [hi[0].max(c[0]), hi[1].max(c[1]), hi[2].max(c[2])]));
                let avg = colors.iter().fold([0.0f32; 3], |a, c| [a[0] + c[0] / n, a[1] + c[1] / n, a[2] + c[2] / n]);
                println!("dynmesh bake {} avg {avg:.2?} min {lo:.2?} max {hi:.2?}", vm.object_name(job.0));
            }
            bake_sections(commands, colors, sections);
        }
    }
    // Place, hide, and drop destroyed actors.
    let c = c_mat();
    let ci = c.inverse();
    tracker.actors.retain(|&a, d| {
        let gone = vm.objects.get(a).is_none_or(|o| o.deleted) || !drawn.contains(&a);
        if gone {
            for &e in &d.sections {
                commands.entity(e).despawn();
            }
            return false;
        }
        let hidden = p2game::ai::render_hidden(vm, a);
        // Most of these never move (pickups lying in the level): skip the entity updates while
        // the actor's location, rotation, scale and visibility are unchanged.
        let key = (vm.actor_loc(a), vm.actor_rot(a), vm.get_prop(a, "DrawScale").map(|v| v.as_float().to_bits()), hidden);
        if d.last.as_ref() == Some(&key) {
            return true;
        }
        if std::env::var_os("P2GAME_DYNMESH").is_some() && vm.classes[vm.objects[a].class].name.contains("Shovel") {
            println!("dynmesh place {a} loc {:?} hidden {hidden} sections {}", vm.actor_loc(a), d.sections.len());
        }
        let tf = Transform::from_matrix(c * actor_world(vm, a) * ci);
        let mut applied = true;
        for &e in &d.sections {
            if let Ok((mut t, mut v)) = tfs.get_mut(e) {
                *t = tf;
                let want = if hidden { Visibility::Hidden } else { Visibility::Inherited };
                if *v != want {
                    *v = want;
                }
            } else {
                // Spawned this frame through Commands: not queryable yet, place it next frame.
                applied = false;
            }
        }
        if applied {
            d.last = Some(key);
        }
        true
    });
}
