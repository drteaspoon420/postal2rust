//! Outdoor rendering for p2view: per-pixel terrain splatting, terrain decoration layers, the sky
//! zone sub-scene and animated light effects.

use crate::scene::{Builder, DecoInstance, MeshData, SceneData, UU_PER_M, to_bevy};
use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{RenderLayers, VisibilityRange};
use bevy::camera::ClearColorConfig;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::NotShadowCaster;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use std::sync::Arc;
use upkg::Package;
use upkg::lighting::LightType;
use upkg::terrain::Terrain;

const SPLAT_SHADER: Handle<Shader> = uuid_handle!("6f1c2b0e-8a43-4d0a-9c55-2b7e4f9d1a31");

/// Render layer for the sky zone sub-scene.
pub const SKY_LAYER: usize = 1;

/// Upper bound on decoration instances per map.
pub const MAX_DECO: usize = 200_000;

pub struct OutdoorPlugin;

impl Plugin for OutdoorPlugin {
    fn build(&self, app: &mut App) {
        app.world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(SPLAT_SHADER.id(), Shader::from_wgsl(include_str!("terrain_splat.wgsl"), "p2view/terrain_splat.wgsl"))
            .expect("register terrain shader");
        app.add_plugins(MaterialPlugin::<SplatMaterial>::default()).add_systems(Update, (animate_lights, ue2_display, crate::uvanim::animate_uvs, crate::uvanim::animate_textures))
            .add_systems(PostUpdate, sync_sky_camera.before(bevy::camera::CameraUpdateSystems).before(bevy::transform::TransformSystems::Propagate))
            .add_systems(PostUpdate, fog_far_plane.after(bevy::camera::CameraUpdateSystems))
            .add_systems(PostUpdate, crate::zones::zone_cull.after(bevy::transform::TransformSystems::Propagate).before(bevy::camera::visibility::VisibilitySystems::CheckVisibility));
    }
}

/// Reverse-Z perspective matrix with a real far plane (depth 1 at `near`, 0 at `far`, clipped
/// beyond). Bevy's PerspectiveProjection is infinite and uses `far` only to cull whole entities,
/// which never trims the map-sized BSP and terrain batches.
fn far_clip_matrix(p: &PerspectiveProjection, far: f32) -> Mat4 {
    let f = 1.0 / (0.5 * p.fov).tan();
    let n = p.near;
    let a = n / (far - n);
    let b = n * far / (far - n);
    Mat4::from_cols(Vec4::new(f / p.aspect_ratio, 0.0, 0.0, 0.0), Vec4::new(0.0, f, 0.0, 0.0), Vec4::new(0.0, 0.0, a, -1.0), Vec4::new(0.0, 0.0, b, 0.0))
}

/// UE2 stops drawing the world at the zone's DistanceFogEnd and the sky zone shows through
/// beyond it. Without a far clip, geometry past the fog end (fully fog-coloured) was drawn as
/// flat grey silhouettes over the sky (intro.fuk: the far-off set pieces looked like floating
/// debris in the sky). Runs after Bevy's camera update and replaces the main camera's clip matrix.
fn fog_far_plane(mut cams: Query<(&DistanceFog, &Projection, &mut Camera), (With<Camera3d>, Without<SkyCamera>)>, sky: Query<(), With<SkyCamera>>) {
    if sky.is_empty() {
        return;
    }
    for (fog, proj, mut cam) in &mut cams {
        let FogFalloff::Linear { end, .. } = &fog.falloff else { continue };
        let Projection::Perspective(p) = proj else { continue };
        if *end > p.near * 2.0 {
            let m = far_clip_matrix(p, *end);
            if cam.computed.clip_from_view != m {
                cam.computed.clip_from_view = m;
            }
        }
    }
}

/// UE2 draws straight into an 8-bit sRGB framebuffer: no tonemapping curve. Bevy's default
/// TonyMcMapface compresses and desaturates mid-tones, so every 3D camera gets `Tonemapping::None`
/// (the baked shader already reproduces UE2's gamma-space modulate-2x lighting exactly).
fn ue2_display(mut cams: Query<&mut bevy::core_pipeline::tonemapping::Tonemapping, Added<Camera3d>>) {
    for mut t in &mut cams {
        *t = bevy::core_pipeline::tonemapping::Tonemapping::None;
    }
}

/// Terrain layer: standard PBR material whose alpha is multiplied by a per-pixel alpha map.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TerrainSplat {
    #[texture(100)]
    #[sampler(101)]
    pub alpha: Handle<Image>,
}

impl MaterialExtension for TerrainSplat {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(SPLAT_SHADER)
    }
}

pub type SplatMaterial = ExtendedMaterial<StandardMaterial, TerrainSplat>;

// ---------------------------------------------------------------------------------------------
// Decoration layers
// ---------------------------------------------------------------------------------------------

/// Small deterministic generator (SplitMix64) so decoration placement is stable between runs.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Samples a weight map (nearest texel) at heightmap-normalised coordinates.
fn sample(map: &(u32, u32, Vec<u8>), u: f32, v: f32) -> f32 {
    let (w, h, d) = map;
    let x = ((u * *w as f32) as u32).min(w - 1);
    let y = ((v * *h as f32) as u32).min(h - 1);
    d[(y * w + x) as usize] as f32 / 255.0
}

/// Scatters each DecoLayer's static mesh over the terrain. Per heightmap quad, up to MaxPerQuad
/// candidates are placed with probability density_map * DensityMultiplier; scale comes from
/// ScaleMultiplier (lerped by the scale map when present), orientation from the terrain normal
/// (AlignToTerrain) and RandomYaw. Placement is an approximation of the original scatter.
pub(crate) fn build_deco(b: &mut Builder, map: &Arc<Package>, t: &Terrain, clip: Option<&crate::terrain_clip::TerrainClip>) -> Vec<DecoInstance> {
    let mut out = Vec::new();
    let (w, h) = (t.width, t.height);
    for (li, d) in t.deco_layers.iter().enumerate() {
        if d.show_on_terrain == 0 || d.max_per_quad <= 0 {
            continue;
        }
        let Some(mesh_obj) = d.static_mesh.and_then(|r| b.lib.resolve(map, r)) else { continue };
        let sections = b.static_mesh(&mesh_obj);
        if sections.is_empty() {
            continue;
        }
        let load = |r: Option<upkg::ObjRef>| r.and_then(|r| b.lib.resolve(map, r)).and_then(|o| upkg::terrain::read_alpha_map(&o).ok());
        let density = load(d.density_map);
        let scale_map = load(d.scale_map);
        let fade = (d.fadeout_radius.0.max(0.0) / UU_PER_M, d.fadeout_radius.1.max(d.fadeout_radius.0 + 1.0) / UU_PER_M);
        let mut rng = Rng::new(((d.seed as u64) << 32) ^ li as u64 ^ 0x5eed);
        for y in 0..h - 1 {
            for x in 0..w - 1 {
                if !t.quad_visible(x, y) {
                    continue;
                }
                for _ in 0..d.max_per_quad {
                    let (fx, fy) = (rng.next(), rng.next());
                    let (u, v) = ((x as f32 + fx) / w as f32, (y as f32 + fy) / h as f32);
                    let dens = density.as_ref().map(|m| sample(m, u, v)).unwrap_or(1.0);
                    let mult = d.density_multiplier.0 + (d.density_multiplier.1 - d.density_multiplier.0) * rng.next();
                    let roll = rng.next();
                    let (yaw_r, sr) = (rng.next(), [rng.next(), rng.next(), rng.next()]);
                    if roll >= dens * mult {
                        continue;
                    }
                    // Bilinear position on the quad (Unreal space).
                    let p = |xx: u32, yy: u32| Vec3::from(t.vertex(xx, yy));
                    let (p00, p10, p01, p11) = (p(x, y), p(x + 1, y), p(x, y + 1), p(x + 1, y + 1));
                    let pos = p00.lerp(p10, fx).lerp(p01.lerp(p11, fx), fy);
                    // Terrain (and its decorations) is only drawn from terrain zones.
                    if clip.is_some_and(|c| !c.keep_point(pos + Vec3::Z * 4.0)) {
                        continue;
                    }
                    let n_ue = (p10 - p00).cross(p01 - p00).normalize_or_zero();
                    let n_ue = if n_ue.z < 0.0 { -n_ue } else { n_ue };
                    let up = Vec3::new(n_ue.x, n_ue.z, n_ue.y).normalize_or(Vec3::Y);
                    let s_t = scale_map.as_ref().map(|m| sample(m, u, v));
                    let sc = |i: usize| {
                        let (lo, hi) = d.scale_multiplier[i];
                        lo + (hi - lo) * s_t.unwrap_or(sr[i])
                    };
                    // Unreal X, Y, Z scale -> Bevy X, Z, Y.
                    let scale = Vec3::new(sc(0), sc(2), sc(1));
                    let mut rot = if d.align_to_terrain { Quat::from_rotation_arc(Vec3::Y, up) } else { Quat::IDENTITY };
                    if d.random_yaw {
                        rot *= Quat::from_rotation_y(yaw_r * std::f32::consts::TAU);
                    }
                    let transform = Mat4::from_scale_rotation_translation(scale, rot, to_bevy(pos.to_array()));
                    for (mesh, material) in &sections {
                        out.push(DecoInstance { mesh: *mesh, material: *material, transform, fade });
                    }
                    if out.len() >= MAX_DECO {
                        return out;
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------------------------

fn alpha_image(a: &(u32, u32, Vec<u8>)) -> Image {
    let (w, h, d) = a;
    let mut img = Image::new(
        Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        d.clone(),
        TextureFormat::R8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}

/// Everything outdoor-related that p2view's main spawn pass hands over.
pub struct OutdoorSpawn {
    pub unlit: bool,
    pub textures: Vec<Handle<Image>>,
    pub materials: Vec<Handle<StandardMaterial>>,
    pub meshes: Vec<Handle<Mesh>>,
    pub data: OutdoorData,
    /// Usable lightmap atlases by index (None = not computed / baked lighting off).
    pub sky_lightmaps: Vec<Option<Handle<Image>>>,
    /// Zone ambient (0-255 scale) for lit sky actors.
    pub sky_ambient: [f32; 3],
    /// Per material id: TexPanner/TexRotator animation (sky clouds).
    pub uv_anims: Vec<Option<crate::uvanim::UvAnim>>,
}

pub struct OutdoorData {
    pub terrain: Vec<crate::scene::TerrainLayerMesh>,
    pub deco: Vec<DecoInstance>,
    pub sky: Option<crate::scene::SkyData>,
}

impl OutdoorData {
    pub fn take(data: &mut SceneData) -> Self {
        OutdoorData { terrain: std::mem::take(&mut data.terrain), deco: std::mem::take(&mut data.deco), sky: data.sky.take() }
    }
}

/// Marker for the camera that renders the sky zone.
#[derive(Component)]
pub struct SkyCamera {
    origin: Vec3,
}

/// Queues spawning of terrain layers, decorations and the sky (needs the splat material assets).
pub fn spawn_outdoor(commands: &mut Commands, s: OutdoorSpawn, build_mesh: fn(&MeshData) -> Mesh) {
    commands.queue(move |world: &mut World| {
        let OutdoorSpawn { unlit, textures, materials, meshes, data, sky_lightmaps, sky_ambient, uv_anims } = s;
        // Terrain: layer 0 opaque, later layers splat with their alpha map.
        let skip_layers: Vec<u32> = std::env::var("P2VIEW_SKIP_LAYERS").map(|v| v.split(',').filter_map(|s| s.parse().ok()).collect()).unwrap_or_default();
        for layer in data.terrain.iter().filter(|l| !skip_layers.contains(&l.order)) {
            let mesh = world.resource_mut::<Assets<Mesh>>().add(build_mesh(&layer.mesh));
            let base = StandardMaterial {
                base_color: if layer.texture.is_some() { Color::WHITE } else { Color::srgb(0.45, 0.5, 0.35) },
                base_color_texture: layer.texture.map(|t| textures[t as usize].clone()),
                alpha_mode: if layer.order == 0 { AlphaMode::Opaque } else { AlphaMode::Blend },
                depth_bias: layer.order as f32,
                perceptual_roughness: 0.95,
                reflectance: 0.05,
                // Saved static terrain lighting (vertex colours) replaces dynamic lighting, as in
                // UE2: the texture modulated by the baked light, fog still applied.
                unlit: unlit || !layer.mesh.colors.is_empty(),
                ..default()
            };
            match &layer.alpha {
                Some(a) if layer.order > 0 => {
                    let img = world.resource_mut::<Assets<Image>>().add(alpha_image(a));
                    let mat = world.resource_mut::<Assets<SplatMaterial>>().add(SplatMaterial { base, extension: TerrainSplat { alpha: img } });
                    let mut e = world.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::IDENTITY, NotShadowCaster));
                    if layer.mesh.zone != 0 {
                        e.insert(crate::zones::ZoneTag(layer.mesh.zone));
                    }
                }
                _ => {
                    let mat = world.resource_mut::<Assets<StandardMaterial>>().add(base);
                    let mut e = world.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::IDENTITY));
                    if layer.mesh.zone != 0 {
                        e.insert(crate::zones::ZoneTag(layer.mesh.zone));
                    }
                    if layer.order > 0 {
                        e.insert(NotShadowCaster);
                    }
                }
            }
        }
        // Decorations: shared mesh/material handles batch into instanced draws.
        // (P2VIEW_NO_DECO / P2VIEW_NO_SKY env vars disable these for profiling.)
        for d in data.deco.iter().filter(|_| std::env::var_os("P2VIEW_NO_DECO").is_none()) {
            world.spawn((
                Mesh3d(meshes[d.mesh as usize].clone()),
                MeshMaterial3d(materials[d.material as usize].clone()),
                Transform::from_matrix(d.transform),
                // Bevy cross-fades a VisibilityRange margin with a screen-door dither, which drew
                // the fading grass as dark checkered blotches (CowPasture). Cut off at the end of
                // the fade radius instead.
                VisibilityRange::abrupt(0.0, d.fade.1),
                NotShadowCaster,
            ));
        }
        // Sky zone: its own render layer, drawn first by a camera that follows the view rotation.
        if let Some(sky) = data.sky.as_ref().filter(|_| std::env::var_os("P2VIEW_NO_SKY").is_none()) {
            let layer = RenderLayers::layer(SKY_LAYER);
            let mut sky_mats: std::collections::HashMap<u32, Handle<StandardMaterial>> = Default::default();
            let mut sky_mat = |world: &mut World, id: u32| {
                sky_mats
                    .entry(id)
                    .or_insert_with(|| {
                        // Sky zones are not lit by the level's lights: draw them unlit.
                        let src = world.resource::<Assets<StandardMaterial>>().get(&materials[id as usize]).cloned().unwrap_or_default();
                        world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { unlit: true, fog_enabled: false, ..src })
                    })
                    .clone()
            };
            let skip: Vec<u32> = std::env::var("P2VIEW_SKY_SKIP").map(|v| v.split(',').filter_map(|s| s.parse().ok()).collect()).unwrap_or_default();
            for m in sky.world.iter().filter(|m| !skip.contains(&m.material)) {
                if std::env::var_os("P2VIEW_SKY_DEBUG").is_some() {
                    eprintln!("sky bsp batch material {} lightmap {:?} verts {}", m.material, m.lightmap, m.positions.len());
                }
                let mesh = world.resource_mut::<Assets<Mesh>>().add(build_mesh(m));
                // Sky polygons that carry a lightmap (night maps light their sky box like normal
                // geometry, ngt-suburbs-3: near black) use it; unlit sky surfaces draw fullbright.
                if let Some(lm) = m.lightmap.and_then(|lm| sky_lightmaps.get(lm as usize).cloned().flatten()) {
                    let src = world.resource::<Assets<StandardMaterial>>().get(&materials[m.material as usize]).cloned().unwrap_or_default();
                    let mat = world
                        .resource_mut::<Assets<crate::baked::BakedMaterial>>()
                        .add(crate::baked::BakedMaterial { base: StandardMaterial { fog_enabled: false, ..src }, extension: crate::baked::BakedLight { lightmap: lm } });
                    let mut e = world.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::IDENTITY, layer.clone(), NotShadowCaster));
                    if let Some(a) = uv_anims.get(m.material as usize).cloned().flatten() {
                        e.insert(a);
                    }
                    continue;
                }
                let mat = sky_mat(world, m.material);
                let mut e = world.spawn((Mesh3d(mesh), MeshMaterial3d(mat), Transform::IDENTITY, layer.clone(), NotShadowCaster));
                if let Some(a) = uv_anims.get(m.material as usize).cloned().flatten() {
                    e.insert(a);
                }
            }
            for inst in sky.instances.iter().filter(|i| !skip.contains(&i.material)) {
                if std::env::var_os("P2VIEW_SKY_DEBUG").is_some() {
                    let l = inst.light.as_ref().map(|c| {
                        let n = c.len().max(1) as f32;
                        let s = c.iter().fold([0f32; 4], |a, x| [a[0] + x[0] as f32, a[1] + x[1] as f32, a[2] + x[2] as f32, a[3] + x[3] as f32]);
                        s.map(|v| v / n)
                    });
                    eprintln!("sky instance export {} material {} glow {} light mean {:?}", inst.export, inst.material, inst.glow, l);
                }
                // bUnlit sky actors (stars, moon, day sky domes) draw fullbright. Lit ones (the
                // night map's red cloud layer) only get ambient light in a sky zone with no
                // lights, which leaves them nearly invisible against the stars as in the original.
                let mut mat = if inst.unlit {
                    sky_mat(world, inst.material)
                } else {
                    let src = world.resource::<Assets<StandardMaterial>>().get(&materials[inst.material as usize]).cloned().unwrap_or_default();
                    // Lit by its own zone (the SkyZoneInfo), not the level's: ngt-suburbs-3's
                    // LevelInfo ambient is brightness 23, hue 0, saturation 0 (pure red), which
                    // tinted the blue-grey zo_red_cloudbowl into red blotches over the stars. The
                    // sky zone there has no ambient, so the cloud layer only darkens the stars.
                    let own = if std::env::var_os("P2VIEW_SKY_LEVEL_AMBIENT").is_some() { sky_ambient } else { inst.zone_ambient.unwrap_or([0.0; 3]) };
                    let a = [own[0] + inst.glow, own[1] + inst.glow, own[2] + inst.glow];
                    let l = crate::baked::vertex_light([0; 4], a);
                    let c = src.base_color.to_linear();
                    let base_color = Color::linear_rgba(c.red * l[0], c.green * l[1], c.blue * l[2], c.alpha);
                    world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { unlit: true, fog_enabled: false, base_color, ..src })
                };
                // Translucent sky actors draw over the sky BSP's translucent cloud planes, as in
                // UE2. Bevy sorts by entity origin, and the suburbs-3 haze dome's origin is ~900 m
                // away although its geometry wraps the sky camera, so it drew first and the cloud
                // planes hid the warm horizon gradient (FIDELITY #10). depth_bias shifts the sort.
                let blended = world.resource::<Assets<StandardMaterial>>().get(&mat).is_some_and(|m| matches!(m.alpha_mode, AlphaMode::Blend));
                if blended && std::env::var_os("P2VIEW_SKY_NOBIAS").is_none() {
                    let src = world.resource::<Assets<StandardMaterial>>().get(&mat).cloned().unwrap_or_default();
                    mat = world.resource_mut::<Assets<StandardMaterial>>().add(StandardMaterial { depth_bias: 100_000.0, ..src });
                }
                let mut e = world.spawn((Mesh3d(meshes[inst.mesh as usize].clone()), MeshMaterial3d(mat), Transform::from_matrix(inst.transform), layer.clone(), NotShadowCaster));
                if let Some(a) = uv_anims.get(inst.material as usize).cloned().flatten() {
                    e.insert(a);
                }
            }
            world.spawn((
                Camera3d::default(),
                Camera { order: -1, ..default() },
                Projection::Perspective(PerspectiveProjection { fov: 75f32.to_radians(), near: 0.1, far: 4000.0, ..default() }),
                Transform::from_translation(sky.origin),
                layer,
                SkyCamera { origin: sky.origin },
            ));
        }
    });
}

/// Keeps the sky camera at the sky zone origin with the main view's rotation and field of view,
/// and stops the main camera from clearing over the sky.
///
/// The main view is the active camera on the default layer with the lowest order. The first-person
/// overlay (order 50) and the canvas-mesh camera (order 110, identity transform while unused) are
/// Camera3d too: taking the rotation from whichever camera the query visited last froze or jerked
/// the sky against the world while turning. The FOV must match too: a fixed 75 deg vertical FOV
/// against the player's ~69 deg (FOVAngle 101 horizontal at 16:9) made the sky zoom and slide
/// relative to the horizon as the view turned. Runs in PostUpdate after the game has placed the
/// camera, before transforms propagate, so the sky never lags a frame behind.
fn sync_sky_camera(
    mut sky: Query<(&mut Transform, &mut Projection, &SkyCamera)>,
    mut main: Query<(&Transform, &Projection, &mut Camera, Option<&RenderLayers>), (With<Camera3d>, Without<SkyCamera>)>,
) {
    let Ok((mut st, mut sp, sc)) = sky.single_mut() else { return };
    let mut best: Option<(isize, Quat, Option<f32>)> = None;
    for (mt, mp, cam, layers) in &mut main {
        let on_world = layers.is_none_or(|l| l.intersects(&RenderLayers::layer(0)));
        if !cam.is_active || !on_world {
            continue;
        }
        if best.as_ref().is_none_or(|b| cam.order < b.0) {
            let fov = match mp {
                Projection::Perspective(p) => Some(p.fov),
                _ => None,
            };
            best = Some((cam.order, mt.rotation, fov));
        }
    }
    for (_, _, mut cam, layers) in &mut main {
        let on_world = layers.is_none_or(|l| l.intersects(&RenderLayers::layer(0)));
        if on_world && !matches!(cam.clear_color, ClearColorConfig::None) {
            cam.clear_color = ClearColorConfig::None;
        }
    }
    let Some((_, rot, fov)) = best else { return };
    if std::env::var_os("P2VIEW_SKY_TRACE").is_some() {
        println!("SKYCAM rot {rot:?} fov {:?}", fov.map(f32::to_degrees));
    }
    if st.rotation != rot {
        st.rotation = rot;
    }
    if st.translation != sc.origin {
        st.translation = sc.origin;
    }
    if let (Some(fov), Projection::Perspective(p)) = (fov, &mut *sp) {
        if p.fov != fov {
            p.fov = fov;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Light effects
// ---------------------------------------------------------------------------------------------

/// Time modulation for a map light (LightType with LightPeriod/LightPhase).
#[derive(Component)]
pub struct LightFx {
    pub base: f32,
    pub kind: LightType,
    pub period: u8,
    pub phase: u8,
}

impl LightFx {
    /// Returns a component when the light type actually varies over time.
    pub fn new(base: f32, kind: LightType, period: u8, phase: u8) -> Option<Self> {
        matches!(kind, LightType::Pulse | LightType::Blink | LightType::Flicker | LightType::Strobe | LightType::SubtlePulse)
            .then_some(LightFx { base, kind, period, phase })
    }

    /// Brightness factor at time `t` seconds.
    pub fn factor(&self, t: f32) -> f32 {
        // LightPeriod is in engine ticks; ~32 per second gives the authored rates a plausible feel.
        let period_s = (self.period.max(1) as f32 / 32.0).max(0.05);
        let cycle = (t / period_s + self.phase as f32 / 256.0).fract();
        let tau = std::f32::consts::TAU;
        match self.kind {
            LightType::Pulse => 0.5 + 0.5 * (cycle * tau).sin(),
            LightType::SubtlePulse => 0.85 + 0.15 * (cycle * tau).sin(),
            LightType::Blink => {
                if cycle < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            LightType::Strobe => {
                if cycle < 0.1 {
                    1.0
                } else {
                    0.0
                }
            }
            LightType::Flicker => {
                // Hash of the 1/20 s time slot and phase: irregular on/dim flicker.
                let slot = (t * 20.0) as u64 ^ ((self.phase as u64) << 32);
                let v = Rng::new(slot).next();
                if v < 0.35 { 0.25 + v } else { 1.0 }
            }
            _ => 1.0,
        }
    }
}

fn animate_lights(time: Res<Time>, mut q: Query<(&LightFx, Option<&mut PointLight>, Option<&mut SpotLight>)>) {
    let t = time.elapsed_secs();
    for (fx, p, s) in &mut q {
        let i = fx.base * fx.factor(t);
        if let Some(mut p) = p {
            p.intensity = i;
        }
        if let Some(mut s) = s {
            s.intensity = i;
        }
    }
}
