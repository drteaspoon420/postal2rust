//! Baked static lighting as UE2 renders it: BSP surfaces modulated by their lightmap atlas and
//! static mesh actors modulated by their per-instance baked vertex colours.

use crate::scene::LightmapData;
use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;

const BAKED_SHADER: Handle<Shader> = uuid_handle!("2c7d9a41-5b8e-4f13-a6d2-91e0c4b7f358");
const BAKED_PREPASS_SHADER: Handle<Shader> = uuid_handle!("8e1f4c27-3a9d-4b62-b0e5-7d24a6c9f013");

/// Scale applied to baked static mesh vertex colours before the gamma conversion
/// (UE2 modulate-2x: colour 128 = full brightness).
pub const VERTEX_LIGHT_SCALE: f32 = 2.0;

pub struct BakedPlugin;

impl Plugin for BakedPlugin {
    fn build(&self, app: &mut App) {
        app.world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(BAKED_SHADER.id(), Shader::from_wgsl(include_str!("baked.wgsl"), "p2view/baked.wgsl"))
            .expect("register baked lighting shader");
        app.world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(BAKED_PREPASS_SHADER.id(), Shader::from_wgsl(include_str!("baked_prepass.wgsl"), "p2view/baked_prepass.wgsl"))
            .expect("register baked prepass shader");
        app.add_plugins(MaterialPlugin::<BakedMaterial>::default());
    }
}

/// Bindless (shares slabs with StandardMaterial) so the ~900 baked materials of a town map can
/// be drawn as a few multi-draw batches instead of one bind group + draw each. The binding
/// numbers double as the bindless index table slots (50..52, table at binding 100), clear of
/// StandardMaterial's 0..31. `P2VIEW_NO_BINDLESS=1` (p2game: disables texture binding arrays)
/// forces the old per-material bind groups for A/B.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
#[bindless(index_table(range(50..52), binding(100)))]
pub struct BakedLight {
    #[texture(50)]
    #[sampler(51)]
    pub lightmap: Handle<Image>,
}

impl MaterialExtension for BakedLight {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(BAKED_SHADER)
    }
    fn prepass_fragment_shader() -> ShaderRef {
        ShaderRef::Handle(BAKED_PREPASS_SHADER)
    }
    /// Depth-only opaque prepasses normally run without a fragment stage; give them the
    /// cutaway-aware one so cut surfaces write no depth either.
    fn specialize(
        _: &bevy::pbr::MaterialExtensionPipeline,
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        _: &bevy::mesh::MeshVertexBufferLayoutRef,
        _: bevy::pbr::MaterialExtensionKey<Self>,
    ) -> Result<(), bevy::render::render_resource::SpecializedMeshPipelineError> {
        if descriptor.fragment.is_none() && descriptor.label.as_deref().is_some_and(|l| l.contains("prepass")) {
            descriptor.fragment = Some(bevy::render::render_resource::FragmentState {
                shader: BAKED_PREPASS_SHADER,
                shader_defs: descriptor.vertex.shader_defs.clone(),
                targets: vec![],
                ..default()
            });
        }
        Ok(())
    }
}

pub type BakedMaterial = ExtendedMaterial<StandardMaterial, BakedLight>;

/// Lightmap atlas as a linear (non-sRGB) texture with bilinear filtering and no mips.
pub fn lightmap_image(l: &LightmapData) -> Image {
    // Corrupt headers (seen: width 0xFEFEFEFE) would fail GPU validation and quit the app.
    let ok = l.width <= 16384 && l.height <= 16384 && l.rgba.len() as u64 == l.width as u64 * l.height as u64 * 4;
    let (width, height, rgba) = if ok { (l.width, l.height, l.rgba.clone()) } else { (1, 1, vec![128, 128, 128, 255]) };
    let mut img = Image::new(
        Extent3d { width, height, depth_or_array_layers: 1 },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8Unorm,
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

/// Converts a baked vertex colour (RGBA8, modulate-2x) plus the zone's ambient light (same 0-255
/// scale; UE2 adds it at runtime) to a linear light factor.
pub fn vertex_light(c: [u8; 4], ambient: [f32; 3]) -> [f32; 4] {
    let f = |v: u8, a: f32| (((v as f32 + a) / 255.0).min(1.0) * VERTEX_LIGHT_SCALE).powf(2.2);
    [f(c[0], ambient[0]), f(c[1], ambient[1]), f(c[2], ambient[2]), 1.0]
}

/// A BSP batch to draw with its lightmap.
pub struct BakedBatch {
    pub mesh: Handle<Mesh>,
    pub base: StandardMaterial,
    /// Scene material index: batches with the same material and lightmap share one asset, so
    /// Bevy can batch their draws (a material per batch made ~1000 unbatchable BSP draws).
    pub material: u32,
    pub lightmap: u32,
    pub uv_anim: Option<crate::uvanim::UvAnim>,
    pub tex_anim: Option<crate::uvanim::TexAnim>,
    pub zone: u8,
}

/// Queues spawning of lightmapped BSP batches (needs the baked material asset type).
pub fn spawn_baked_world(commands: &mut Commands, batches: Vec<BakedBatch>, lightmaps: Vec<Handle<Image>>) {
    commands.queue(move |world: &mut World| {
        let mut mats: std::collections::HashMap<(u32, u32), Handle<BakedMaterial>> = Default::default();
        for b in batches {
            let lm = lightmaps[b.lightmap as usize].clone();
            let mat = mats.entry((b.material, b.lightmap)).or_insert_with(|| world.resource_mut::<Assets<BakedMaterial>>().add(BakedMaterial { base: b.base, extension: BakedLight { lightmap: lm } })).clone();
            let mut e = world.spawn((Mesh3d(b.mesh), MeshMaterial3d(mat), Transform::IDENTITY));
            if let Some(a) = b.uv_anim {
                e.insert(a);
            }
            if let Some(a) = b.tex_anim {
                e.insert(a);
            }
            if b.zone != 0 {
                e.insert(crate::zones::ZoneTag(b.zone));
            }
        }
    });
}

/// Queues attaching vertex-lit baked materials to already spawned static mesh entities. Each
/// entry is (entity, material key, base material); entries sharing a key share one material.
pub fn attach_vertex_lit(commands: &mut Commands, entries: Vec<(Entity, u32, StandardMaterial)>) {
    if entries.is_empty() {
        return;
    }
    commands.queue(move |world: &mut World| {
        let white = world.resource_mut::<Assets<Image>>().add(lightmap_image(&LightmapData { width: 1, height: 1, rgba: vec![128, 128, 128, 255] }));
        let mut mats: std::collections::HashMap<u32, Handle<BakedMaterial>> = Default::default();
        for (e, key, base) in entries {
            let mat = mats
                .entry(key)
                .or_insert_with(|| world.resource_mut::<Assets<BakedMaterial>>().add(BakedMaterial { base, extension: BakedLight { lightmap: white.clone() } }))
                .clone();
            if let Ok(mut ent) = world.get_entity_mut(e) {
                ent.insert(MeshMaterial3d(mat));
            }
        }
    });
}
