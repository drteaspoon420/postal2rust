//! Turns [`SceneData`] into Bevy entities: textures, materials, world BSP, static mesh actors,
//! terrain/decoration/sky (via [`crate::outdoor`]), map lights, ambient light and fog.
//!
//! Shared by the p2view binary and other front ends (e.g. p2game) through the p2view library.

use crate::scene::{self, Blend, PixelFormat, SceneData};
use crate::{baked, outdoor};
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::VisibilityRange;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::collections::HashMap;

pub fn make_image(t: &scene::TexData) -> Image {
    let format = match t.format {
        PixelFormat::Bc1 => TextureFormat::Bc1RgbaUnormSrgb,
        PixelFormat::Bc2 => TextureFormat::Bc2RgbaUnormSrgb,
        PixelFormat::Bc3 => TextureFormat::Bc3RgbaUnormSrgb,
        PixelFormat::Rgba8 => TextureFormat::Rgba8UnormSrgb,
    };
    let size = Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 };
    let mut img = Image::new_uninit(size, TextureDimension::D2, format, RenderAssetUsages::RENDER_WORLD);
    img.data = Some(t.mips.concat());
    img.texture_descriptor.mip_level_count = t.mips.len() as u32;
    let mut sampler = ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    };
    // 16x like the original's D3D renderer (Postal2.ini LevelOfAnisotropy=16).
    sampler.set_anisotropic_filter(16);
    img.sampler = ImageSampler::Descriptor(sampler);
    img
}

pub fn build_mesh(m: &scene::MeshData) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone())
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, m.uvs.clone())
        .with_inserted_indices(Indices::U32(m.indices.clone()));
    if !m.uvs_b.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, m.uvs_b.clone());
    }
    if !m.colors.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, m.colors.clone());
    }
    mesh
}

fn srgb(c: [f32; 3]) -> Color {
    Color::srgb(c[0], c[1], c[2])
}

/// Sun illuminance (lux) per unit of summed LightBrightness.
const SUN_LUX_PER_BRIGHTNESS: f32 = 10.0;
/// Point/spot light illuminance at half radius (lux) per unit of LightBrightness.
const POINT_LUX_PER_BRIGHTNESS: f32 = 2.9;

/// Map lights: Unreal LightBrightness (0-255) and LightRadius become Bevy intensity and range.
/// Sunlight becomes one shadowed DirectionalLight; zone ambient drives the global ambient light.
pub fn spawn_lights(commands: &mut Commands, data: &SceneData) {
    for l in data.lights.iter().filter(|_| std::env::var_os("P2VIEW_NO_LIGHTS").is_none()) {
        // UE2 point light: intensity 2 * (2f^3 - 3f^2 + 1) * cos at f = distance / radius, on the
        // same brightness/255 scale as sunlight (2 * cos). With the sun at SUN_LUX_PER_BRIGHTNESS
        // that is SUN_LUX_PER_BRIGHTNESS * B * 0.5 lux at half radius. Calibrated on ngt-suburbs-3
        // (point lights only) against the original; the old 1500 lux per 128 was ~4x too bright.
        let half = (l.range * 0.5).max(1.0);
        let scale: f32 = std::env::var("P2VIEW_LIGHT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        let lumens = POINT_LUX_PER_BRIGHTNESS * scale * l.brightness * 4.0 * std::f32::consts::PI * half * half;
        let color = srgb(l.color);
        let tf = Transform::from_translation(l.position).looking_to(l.direction, if l.direction.y.abs() > 0.99 { Vec3::X } else { Vec3::Y });
        let mut e = match l.shape {
            scene::LightShape::Point => commands.spawn((PointLight { color, intensity: lumens, range: l.range, shadow_maps_enabled: false, ..default() }, tf)),
            scene::LightShape::Spot => commands.spawn((
                SpotLight { color, intensity: lumens, range: l.range, outer_angle: l.cone, inner_angle: l.cone * 0.7, shadow_maps_enabled: false, ..default() },
                tf,
            )),
        };
        if let Some(fx) = outdoor::LightFx::new(lumens, l.modulation, l.period, l.phase) {
            e.insert(fx);
        }
    }
    match &data.sun {
        Some(s) => {
            // Secondary sun directions (the main direction below carries its own group only).
            let main_dir = s.direction;
            let sun_scale = std::env::var("P2VIEW_SUN_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0f32);
            let sun2_scale = std::env::var("P2VIEW_SUN2_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0f32);
            let mut main = (s.color, s.brightness);
            if std::env::var_os("P2VIEW_SUN_DUMP").is_some() {
                for (d, c, b) in &s.parts {
                    eprintln!("sun part dir {d:?} colour {c:?} brightness {b}");
                }
            }
            if std::env::var_os("P2VIEW_SUN_MERGE").is_none() {
                // The group nearest the brightest sun's direction is the main (shadowed) light.
                let main_idx = s.parts.iter().enumerate().max_by(|a, b| a.1 .0.dot(main_dir).total_cmp(&b.1 .0.dot(main_dir))).map(|(i, _)| i);
                for (i, (d, c, b)) in s.parts.iter().enumerate() {
                    if Some(i) == main_idx {
                        main = (*c, *b);
                        continue;
                    }
                    let dir = if d.y < -0.05 { *d } else { (*d - Vec3::Y * 0.5).normalize() };
                    commands.spawn((
                        DirectionalLight { color: srgb(*c), illuminance: SUN_LUX_PER_BRIGHTNESS * b.max(0.0) * sun_scale * sun2_scale, shadow_maps_enabled: false, ..default() },
                        Transform::IDENTITY.looking_to(dir, Vec3::Y),
                    ));
                }
            }
            let s = scene::SunData { direction: main_dir, color: main.0, brightness: main.1, parts: vec![] };
            let dir = if s.direction.y < -0.05 { s.direction } else { Vec3::new(0.3, -0.8, 0.2).normalize() };
            commands.spawn((
                // Calibrated against the original suburbs-3 start view (out\render\s3_*.png vs
                // out\ref\orig_sub3_t35.png). Linear in the summed sun LightBrightness: the
                // original's night/day terrain ratio (ngt-suburbs-3 68 vs suburbs-3 383) is ~0.19.
                DirectionalLight { color: srgb(s.color), illuminance: SUN_LUX_PER_BRIGHTNESS * s.brightness.max(0.0) * std::env::var("P2VIEW_SUN_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0f32), shadow_maps_enabled: std::env::var_os("P2VIEW_SUN_SHADOWS").is_some(), ..default() },
                // UE2 has no real-time sun shadows (baked lighting only, pawns get blob/projector
                // shadows). Terrain receiving them blacked out sunlit roads (Estates, FIDELITY #32).
                // Two cascades: each cascade re-batches and re-draws every shadow caster, which
                // cost more than the rest of the opaque pass on outdoor maps with four.
                bevy::light::CascadeShadowConfigBuilder { num_cascades: 2, maximum_distance: 150.0, first_cascade_far_bound: 30.0, ..default() }.build(),
                Transform::IDENTITY.looking_to(dir, Vec3::Y),
            ));
        }
        None => {
            // Indoor/night maps without a sun: a weak fill light keeps unlit corners readable.
            let fill: f32 = std::env::var("P2VIEW_FILL_LUX").ok().and_then(|v| v.parse().ok()).unwrap_or(1500.0);
            commands.spawn((
                DirectionalLight { illuminance: fill, shadow_maps_enabled: false, ..default() },
                Transform::from_xyz(0.0, 100.0, 0.0).looking_at(Vec3::new(0.4, 0.0, 0.3), Vec3::Y),
            ));
        }
    }
    // UE2 lights dynamic geometry with the zone ambient in gamma space. With Bevy's default camera
    // exposure (EV100 9.7) and no tonemapping, a gamma-space light factor g is a linear ambient of
    // g^2.2 / exposure. g = 1.5 * AmbientBrightness/255 matches the original's suburbs-3 terrain.
    let exposure = 2f32.powf(-9.7) / 1.2;
    let (amb_color, amb) = match &data.zone {
        Some(z) => (srgb(z.dynamic_ambient), (1.5 * z.dynamic_ambient_brightness / 255.0).clamp(0.05, 1.0).powf(2.2) / exposure),
        None => (Color::WHITE, 700.0),
    };
    commands.insert_resource(GlobalAmbientLight { color: amb_color, brightness: amb, ..default() });
}

/// What [`spawn_scene_data`] created, for front ends that drive actors afterwards.
#[derive(Default)]
pub struct SpawnedScene {
    /// Static mesh entities per map actor export index (one per mesh section).
    pub actors: HashMap<usize, Vec<Entity>>,
    /// Mover actors with their entities (from `actors`) and keyframes, for animation.
    pub movers: Vec<(scene::MoverData, Vec<Entity>)>,
    /// Distance fog the camera should use (colour, falloff), from the level's zone settings.
    pub fog: Option<(Color, FogFalloff)>,
    pub textures: Vec<Handle<Image>>,
    pub materials: Vec<Handle<StandardMaterial>>,
    pub meshes: Vec<Handle<Mesh>>,
}

/// Spawns everything in `data` except the camera, using baked lighting where the map has it.
/// Terrain, decorations, sky and lightmapped BSP are queued (they need the plugins' material
/// types; add [`outdoor::OutdoorPlugin`] and [`baked::BakedPlugin`] to the app).
/// With `unlit`, materials ignore lighting and map lights are not spawned.
pub fn spawn_scene_data(
    commands: &mut Commands,
    data: &mut SceneData,
    unlit: bool,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> SpawnedScene {
    spawn_scene_with(commands, data, SpawnOptions { unlit, baked: true }, images, meshes, materials)
}

#[derive(Clone, Copy, Debug)]
pub struct SpawnOptions {
    /// Plain texture colours, no lighting at all.
    pub unlit: bool,
    /// Use the map's baked lighting (BSP lightmaps, static mesh vertex lighting) where present,
    /// as the original renderer does. When false, everything is lit dynamically.
    pub baked: bool,
}

pub fn spawn_scene_with(
    commands: &mut Commands,
    data: &mut SceneData,
    opts: SpawnOptions,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> SpawnedScene {
    let unlit = opts.unlit;
    let baked = opts.baked && !unlit;
    let t0 = std::time::Instant::now();
    let timing = std::env::var_os("P2VIEW_SPAWN_TIMING").is_some();
    let lap = |what: &str| {
        if timing {
            eprintln!("spawn: {what} at {:.3} s", t0.elapsed().as_secs_f32());
        }
    };
    let tex_handles: Vec<Handle<Image>> = data.textures.iter().map(|t| images.add(make_image(t))).collect();
    lap("textures");
    let mat_handles: Vec<Handle<StandardMaterial>> = data
        .materials
        .iter()
        .map(|m| {
            materials.add(StandardMaterial {
                // Modulate is UE2's modulate-2x (texel 128 leaves the surface unchanged): in linear
                // space that is a factor of 2^2.2 on the sampled texel.
                base_color: match (m.blend, m.texture.is_some()) {
                    (Blend::Modulate, _) => Color::linear_rgb(4.595, 4.595, 4.595),
                    (_, true) => Color::WHITE,
                    _ => Color::srgb(0.6, 0.6, 0.6),
                },
                base_color_texture: m.texture.map(|t| tex_handles[t as usize].clone()),
                alpha_mode: match m.blend {
                    Blend::Opaque => AlphaMode::Opaque,
                    Blend::Masked => AlphaMode::Mask(0.5),
                    Blend::Translucent => AlphaMode::Blend,
                    Blend::Modulate | Blend::Darken => AlphaMode::Multiply,
                    Blend::Additive => AlphaMode::Add,
                },
                double_sided: m.two_sided,
                cull_mode: if m.two_sided { None } else { Some(bevy::render::render_resource::Face::Back) },
                perceptual_roughness: 0.95,
                reflectance: 0.1,
                unlit: unlit || matches!(m.blend, Blend::Modulate | Blend::Additive | Blend::Darken),
                ..default()
            })
        })
        .collect();
    lap("materials");
    // Per material: its TexPanner/TexRotator/TexOscillator animation, if any.
    let uv_anims: Vec<Option<crate::uvanim::UvAnim>> = data.materials.iter().map(|m| crate::uvanim::UvAnim::new(&m.uv_anim, m.size)).collect();
    // Per material: its AnimNext frame chain (animated texture), if any.
    let tex_anims: Vec<Option<crate::uvanim::TexAnim>> = data.materials.iter().map(|m| crate::uvanim::TexAnim::new(&m.frames, m.frame_rate, &tex_handles)).collect();

    // World BSP: one entity per (material, lightmap) batch. Lightmapped batches use the baked
    // material (diffuse * (lightmap + zone ambient)); the rest are lit dynamically. Atlases that
    // are essentially black (not computed for this map's lights) fall back to dynamic lighting.
    let ambient = data.zone.as_ref().map(|z| [z.ambient[0] * z.ambient_brightness, z.ambient[1] * z.ambient_brightness, z.ambient[2] * z.ambient_brightness]).unwrap_or([0.0; 3]);
    let usable: Vec<bool> = data
        .lightmaps
        .iter()
        .map(|l| {
            let n = (l.rgba.len() / 4).max(1) as u64;
            let sum: u64 = l.rgba.chunks_exact(4).map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64).sum();
            sum / (3 * n) >= 8
        })
        .collect();
    lap("lightmap scan");
    if std::env::var_os("P2VIEW_LM_DEBUG").is_some() {
        let bad: Vec<usize> = usable.iter().enumerate().filter(|(_, u)| !**u).map(|(i, _)| i).collect();
        eprintln!("lightmaps: {} atlases, unusable (mean < 8): {bad:?}", usable.len());
    }
    let lightmap_handles: Vec<Handle<Image>> = if baked {
        data.lightmaps
            .iter()
            .map(|l| {
                let mut l2 = scene::LightmapData { width: l.width, height: l.height, rgba: l.rgba.clone() };
                // The editor bakes the zone ambient into the lightmaps already: adding it again
                // made indoor maps ~20% too bright (police: ceiling 162 vs 133 in the original).
                let add = if std::env::var_os("P2VIEW_LM_AMBIENT").is_some() { ambient } else { [0.0; 3] };
                // Debug: P2VIEW_LM_FORCE=v replaces every lightmap texel (isolates lightmap vs other light).
                if let Some(v) = std::env::var("P2VIEW_LM_FORCE").ok().and_then(|v| v.parse::<u8>().ok()) {
                    for p in l2.rgba.chunks_exact_mut(4) {
                        p[..3].fill(v);
                    }
                }
                for p in l2.rgba.chunks_exact_mut(4) {
                    for c in 0..3 {
                        p[c] = (p[c] as f32 + add[c]).min(255.0) as u8;
                    }
                }
                images.add(baked::lightmap_image(&l2))
            })
            .collect()
    } else {
        vec![]
    };
    lap("lightmaps");
    if timing {
        eprintln!("spawn: {} lightmaps ({} MB), {} world batches", data.lightmaps.len(), data.lightmaps.iter().map(|l| l.rgba.len()).sum::<usize>() >> 20, data.world.len());
    }
    let mut baked_batches = Vec::new();
    for m in &data.world {
        let mesh = meshes.add(build_mesh(m));
        match m.lightmap.filter(|&lm| baked && usable.get(lm as usize).copied().unwrap_or(false)) {
            Some(lm) => {
                let base = materials.get(&mat_handles[m.material as usize]).cloned().unwrap_or_default();
                baked_batches.push(baked::BakedBatch { mesh, base, lightmap: lm, uv_anim: uv_anims[m.material as usize].clone(), tex_anim: tex_anims[m.material as usize].clone(), zone: m.zone });
            }
            None => {
                // UE2 static geometry only has baked shadowing: no real-time sun shadow on it.
                let mut e = commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat_handles[m.material as usize].clone()), Transform::IDENTITY, bevy::light::NotShadowReceiver));
                if m.zone != 0 {
                    e.insert(crate::zones::ZoneTag(m.zone));
                }
                if let Some(a) = &uv_anims[m.material as usize] {
                    e.insert(a.clone());
                }
                if let Some(a) = &tex_anims[m.material as usize] {
                    e.insert(a.clone());
                }
            }
        }
    }
    // Sky BSP: a lightmapped sky (night maps) is legitimately near black or fully black, so the
    // "usable" heuristic does not apply. Every atlas is honoured.
    let sky_lightmaps: Vec<Option<Handle<Image>>> = lightmap_handles.iter().map(|h| Some(h.clone())).collect();
    baked::spawn_baked_world(commands, baked_batches, lightmap_handles);
    lap("world bsp");
    // Static meshes: shared mesh + material handles let Bevy batch identical instances into single
    // draws. Actors with baked vertex lighting get their own mesh copy carrying those colours and
    // the baked material (texture * vertex light), attached once that asset type is available.
    let mesh_handles: Vec<Handle<Mesh>> = data.meshes.iter().map(|m| meshes.add(build_mesh(m))).collect();
    let mut vertex_lit = Vec::new();
    let mut actors: HashMap<usize, Vec<Entity>> = HashMap::new();
    for inst in &data.instances {
        let lit = inst.light.as_ref().filter(|_| baked).and_then(|colors| {
            let src = &data.meshes[inst.mesh as usize];
            if src.source.is_empty() || src.source.iter().any(|&v| v as usize >= colors.len()) {
                if std::env::var_os("P2VIEW_BAKE_DEBUG").is_some() {
                    eprintln!("bake fallback: export {} material {} source {} colors {} max {:?}", inst.export, inst.material, src.source.len(), colors.len(), src.source.iter().max());
                }
                return None;
            }
            let mut md = scene::MeshData { positions: src.positions.clone(), normals: src.normals.clone(), uvs: src.uvs.clone(), indices: src.indices.clone(), ..Default::default() };
            // Baked static meshes add the ambient of the zone they were saved in (UE2), not the
            // LevelInfo's: suburbs-3 trees sit in the terrain zone (102, warm), the level is 23.
            // Scaled 0.65 to match the original's leaf brightness on the suburbs-3 start view.
            let k: f32 = std::env::var("P2VIEW_MESH_AMB").ok().and_then(|v| v.parse().ok()).unwrap_or(0.65);
            let za = if std::env::var_os("P2VIEW_MESH_LEVEL_AMB").is_some() { ambient } else { inst.zone_ambient.map(|a| a.map(|v| v * k)).unwrap_or(ambient) };
            let amb = [za[0] + inst.glow, za[1] + inst.glow, za[2] + inst.glow];
            md.colors = src.source.iter().map(|&v| baked::vertex_light(colors[v as usize], amb)).collect();
            Some(meshes.add(build_mesh(&md)))
        });
        let mut e = match lit {
            Some(mesh) => {
                let e = commands.spawn((Mesh3d(mesh), Transform::from_matrix(inst.transform)));
                let base = materials.get(&mat_handles[inst.material as usize]).cloned().unwrap_or_default();
                vertex_lit.push((e.id(), inst.material, base));
                e
            }
            None => commands.spawn((
                Mesh3d(mesh_handles[inst.mesh as usize].clone()),
                MeshMaterial3d(mat_handles[inst.material as usize].clone()),
                Transform::from_matrix(inst.transform),
                bevy::light::NotShadowReceiver,
            )),
        };
        if inst.cull_distance > 0.0 {
            // Mirrors the per-actor CullDistance authored in the map.
            e.insert(VisibilityRange::abrupt(0.0, inst.cull_distance));
        }
        if inst.zone != 0 {
            e.insert(crate::zones::ZoneTag(inst.zone));
        }
        if let Some(a) = &uv_anims[inst.material as usize] {
            e.insert(a.clone());
        }
        if let Some(a) = &tex_anims[inst.material as usize] {
            e.insert(a.clone());
        }
        actors.entry(inst.export).or_default().push(e.id());
    }
    baked::attach_vertex_lit(commands, vertex_lit);
    commands.insert_resource(crate::zones::ZoneGraph::new(&data.portals, data.bsp.clone()));
    lap("static meshes");

    outdoor::spawn_outdoor(
        commands,
        outdoor::OutdoorSpawn {
            unlit,
            textures: tex_handles.clone(),
            materials: mat_handles.clone(),
            meshes: mesh_handles.clone(),
            data: outdoor::OutdoorData::take(data),
            sky_lightmaps,
            sky_ambient: ambient,
            uv_anims,
        },
        build_mesh,
    );
    if !unlit {
        spawn_lights(commands, data);
    }
    let fog = data.zone.as_ref().and_then(|z| z.fog).map(|(c, start, end)| {
        let c = Color::srgb_u8(c[0], c[1], c[2]);
        commands.insert_resource(ClearColor(c));
        (c, FogFalloff::Linear { start, end: end.max(start + 1.0) })
    });
    SpawnedScene { movers: data.movers.iter().map(|m| (m.clone(), actors.get(&m.export).cloned().unwrap_or_default())).collect(), actors, fog, textures: tex_handles, materials: mat_handles, meshes: mesh_handles }
}
