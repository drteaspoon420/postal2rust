//! Bevy side: turns the frame's queued [`DrawItem`]s into 2D meshes drawn with [`CanvasMaterial`].
//!
//! Immediate mode, like the original: every frame the Canvas queue is rebuilt from scratch. Runs of
//! consecutive items that share a material and style become one mesh (one draw call). Batches are
//! placed at increasing Z in the transparent 2D phase so painter's order matches call order.
//! Mesh entities and material assets are pooled and reused between frames.

use crate::canvas::{Canvas, DrawItem};
use crate::eval::{Blend, Evaluated, MaterialDb, SlotSource, UvXform};
use crate::font::CanvasFont;
use bevy::asset::{AssetPath, RenderAssetUsages, embedded_asset, embedded_path};
use bevy::camera::ScalingMode;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Extent3d, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureDimension,
    TextureFormat,
};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d, Material2dKey, Material2dPlugin, MeshMaterial2d};
use bevy::window::PrimaryWindow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use upkg::library::{Library, Obj};
use upkg::texture::read_texture;

// ------------------------------------------------------------------ material

#[derive(Clone, Default, ShaderType)]
pub struct CanvasUniform {
    pub tint: Vec4,
    pub slot_color0: Vec4,
    pub slot_color1: Vec4,
    pub slot_color2: Vec4,
    pub xu0: Vec4,
    pub xv0: Vec4,
    pub xu1: Vec4,
    pub xv1: Vec4,
    pub xu2: Vec4,
    pub xv2: Vec4,
    pub kinds: UVec4,
    pub ops: UVec4,
    pub params: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
#[bind_group_data(CanvasMaterialKey)]
#[uniform(0, CanvasUniform)]
pub struct CanvasMaterial {
    pub uniform_data: CanvasUniformData,
    #[texture(1)]
    #[sampler(2)]
    pub tex0: Option<Handle<Image>>,
    #[texture(3)]
    #[sampler(4)]
    pub tex1: Option<Handle<Image>>,
    #[texture(5)]
    #[sampler(6)]
    pub tex2: Option<Handle<Image>>,
    pub blend: Blend,
}

/// CPU copy of the uniform (kept separately so it can be compared/updated cheaply).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CanvasUniformData {
    pub tint: [f32; 4],
    pub slot_colors: [[f32; 4]; 3],
    pub xforms: [UvXform; 3],
    pub kinds: [u32; 3],
    pub ops: [u32; 4],
    pub modulate: f32,
    pub alpha_test: f32,
}

impl bevy::render::render_resource::AsBindGroupShaderType<CanvasUniform> for CanvasMaterial {
    fn as_bind_group_shader_type(&self, _images: &bevy::render::render_asset::RenderAssets<bevy::render::texture::GpuImage>) -> CanvasUniform {
        let d = &self.uniform_data;
        let row = |x: &UvXform| (Vec4::new(x.m[0], x.m[1], x.t[0], 0.0), Vec4::new(x.m[2], x.m[3], x.t[1], 0.0));
        let (xu0, xv0) = row(&d.xforms[0]);
        let (xu1, xv1) = row(&d.xforms[1]);
        let (xu2, xv2) = row(&d.xforms[2]);
        CanvasUniform {
            tint: Vec4::from(d.tint),
            slot_color0: Vec4::from(d.slot_colors[0]),
            slot_color1: Vec4::from(d.slot_colors[1]),
            slot_color2: Vec4::from(d.slot_colors[2]),
            xu0,
            xv0,
            xu1,
            xv1,
            xu2,
            xv2,
            kinds: UVec4::new(d.kinds[0], d.kinds[1], d.kinds[2], 0),
            ops: UVec4::from(d.ops),
            params: Vec4::new(d.modulate, d.alpha_test, 0.0, 0.0),
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Hash, Eq, PartialEq)]
pub struct CanvasMaterialKey {
    blend: Blend,
}

impl From<&CanvasMaterial> for CanvasMaterialKey {
    fn from(m: &CanvasMaterial) -> Self {
        Self { blend: m.blend }
    }
}

fn blend_state(b: Blend) -> Option<BlendState> {
    let c = |src, dst| BlendComponent { src_factor: src, dst_factor: dst, operation: BlendOperation::Add };
    let alpha = c(BlendFactor::One, BlendFactor::OneMinusSrcAlpha);
    Some(match b {
        Blend::Opaque => return None,
        Blend::Alpha => BlendState::ALPHA_BLENDING,
        // Src + Dst * (1 - Src)
        Blend::Translucent => BlendState { color: c(BlendFactor::One, BlendFactor::OneMinusSrc), alpha },
        // Src * Dst * 2
        Blend::Modulate => BlendState { color: c(BlendFactor::Dst, BlendFactor::Src), alpha: c(BlendFactor::Zero, BlendFactor::One) },
        Blend::AlphaModulate => BlendState { color: c(BlendFactor::One, BlendFactor::OneMinusSrcAlpha), alpha },
        Blend::Darken => BlendState { color: c(BlendFactor::Zero, BlendFactor::OneMinusSrc), alpha: c(BlendFactor::Zero, BlendFactor::One) },
        Blend::Brighten => BlendState { color: c(BlendFactor::One, BlendFactor::One), alpha: c(BlendFactor::Zero, BlendFactor::One) },
        Blend::Invisible => BlendState { color: c(BlendFactor::Zero, BlendFactor::One), alpha: c(BlendFactor::Zero, BlendFactor::One) },
    })
}

impl Material2d for CanvasMaterial {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Path(AssetPath::from_path_buf(embedded_path!("canvas.wgsl")).with_source("embedded"))
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        // Everything goes through the sorted transparent phase so draw order is call order.
        AlphaMode2d::Blend
    }

    fn specialize(descriptor: &mut RenderPipelineDescriptor, _layout: &MeshVertexBufferLayoutRef, key: Material2dKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(frag) = descriptor.fragment.as_mut() {
            for t in frag.targets.iter_mut().flatten() {
                t.blend = blend_state(key.bind_group_data.blend);
            }
        }
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ resources

/// Game-side access to fonts, materials and their sizes.
#[derive(Resource, Clone)]
pub struct CanvasAssets {
    pub db: Arc<MaterialDb>,
    fonts: Arc<Mutex<HashMap<String, Option<Arc<CanvasFont>>>>>,
}

impl CanvasAssets {
    pub fn new(lib: Arc<Library>) -> Self {
        Self { db: Arc::new(MaterialDb::new(lib)), fonts: Default::default() }
    }

    /// Loads a font by "Package.Name" (e.g. "p2fonts.Plain24").
    pub fn font(&self, name: &str) -> Option<Arc<CanvasFont>> {
        let key = name.to_ascii_lowercase();
        if let Some(f) = self.fonts.lock().unwrap().get(&key) {
            return f.clone();
        }
        let f = self.db.find(name, Some("Font")).and_then(|o| CanvasFont::load(&self.db.lib, &o));
        self.fonts.lock().unwrap().insert(key, f.clone());
        f
    }

    /// Finds a material object by "Package.Group.Name".
    pub fn material(&self, name: &str) -> Option<Obj> {
        self.db.find(name, None)
    }
}

/// The frame's Canvas. Fill it during `Update`; it is flushed to the GPU in `PostUpdate` and reset
/// (via `Canvas::begin_frame`) in `PreUpdate` with the window size and elapsed time.
#[derive(Resource, Default)]
pub struct HudCanvas(pub Canvas);

#[derive(Component)]
pub struct CanvasCamera;

#[derive(Component)]
struct CanvasBatch;

#[derive(Default, Resource)]
struct RenderCache {
    images: HashMap<(String, usize, bool), Option<Handle<Image>>>,
    materials: HashMap<(String, usize, u8, bool), (Handle<CanvasMaterial>, bool)>,
    pool: Vec<(Entity, Handle<Mesh>)>,
    /// Diagnostics: last frame's batch and quad counts.
    pub batches: usize,
    pub quads: usize,
}

/// Last frame's batch statistics.
#[derive(Default, Resource, Clone, Copy, Debug)]
pub struct CanvasStats {
    pub batches: usize,
    pub quads: usize,
    pub materials: usize,
    pub images: usize,
}

pub struct CanvasPlugin {
    pub lib: Arc<Library>,
    /// Clear colour for the canvas camera; `None` draws over whatever earlier cameras rendered.
    pub clear: Option<Color>,
}

#[derive(Resource, Clone, Copy)]
struct CanvasClear(Option<Color>);

impl Plugin for CanvasPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "canvas.wgsl");
        app.add_plugins(Material2dPlugin::<CanvasMaterial>::default())
            .insert_resource(CanvasAssets::new(self.lib.clone()))
            .insert_resource(CanvasClear(self.clear))
            .init_resource::<HudCanvas>()
            .init_resource::<RenderCache>()
            .init_resource::<CanvasStats>()
            .add_systems(Startup, spawn_camera)
            .add_systems(PreUpdate, begin_frame)
            .add_systems(PostUpdate, flush);
    }
}

fn spawn_camera(mut commands: Commands, clear: Res<CanvasClear>, existing: Query<(), With<CanvasCamera>>) {
    if !existing.is_empty() {
        return;
    }
    let clear_color = match clear.0 {
        Some(c) => ClearColorConfig::Custom(c),
        None => ClearColorConfig::None,
    };
    commands.spawn((
        Camera2d,
        Camera { order: 100, clear_color, ..default() },
        Projection::Orthographic(OrthographicProjection { scaling_mode: ScalingMode::Fixed { width: 640.0, height: 480.0 }, ..OrthographicProjection::default_2d() }),
        CanvasCamera,
    ));
}

fn begin_frame(time: Res<Time>, windows: Query<&Window, With<PrimaryWindow>>, mut hud: ResMut<HudCanvas>, mut cams: Query<&mut Projection, With<CanvasCamera>>) {
    let (w, h) = windows.single().map(|w| (w.physical_width().max(1) as i32, w.physical_height().max(1) as i32)).unwrap_or((640, 480));
    hud.0.begin_frame(w, h, time.elapsed_secs());
    for mut p in &mut cams {
        if let Projection::Orthographic(o) = &mut *p {
            o.scaling_mode = ScalingMode::Fixed { width: w as f32, height: h as f32 };
        }
    }
}

fn linear(c: [u8; 4]) -> [f32; 4] {
    Color::srgba_u8(c[0], c[1], c[2], c[3]).to_linear().to_f32_array()
}

fn make_image(lib_tex: &Obj, nearest: bool) -> Option<Image> {
    let tex = read_texture(&lib_tex.pkg, lib_tex.export).ok()?;
    let (w, h) = (tex.mips.first()?.width, tex.mips.first()?.height);
    let mut data = Vec::new();
    let mut levels = 0u32;
    let (mut ew, mut eh) = (w, h);
    for level in 0..tex.mips.len() {
        let m = &tex.mips[level];
        if m.width != ew || m.height != eh {
            break;
        }
        let Ok((_, _, px)) = tex.decode_rgba(level) else { break };
        data.extend_from_slice(&px);
        levels += 1;
        if ew == 1 && eh == 1 {
            break;
        }
        ew = (ew / 2).max(1);
        eh = (eh / 2).max(1);
    }
    if levels == 0 {
        return None;
    }
    let clamp_u = tex.props.byte("UClampMode") == Some(1);
    let clamp_v = tex.props.byte("VClampMode") == Some(1);
    let mut img = Image::new_uninit(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
    img.data = Some(data);
    img.texture_descriptor.mip_level_count = levels;
    let filter = if nearest { ImageFilterMode::Nearest } else { ImageFilterMode::Linear };
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: if clamp_u { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat },
        address_mode_v: if clamp_v { ImageAddressMode::ClampToEdge } else { ImageAddressMode::Repeat },
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    Some(img)
}

impl RenderCache {
    fn image(&mut self, images: &mut Assets<Image>, tex: &Obj, nearest: bool) -> Option<Handle<Image>> {
        let key = (tex.pkg.name.clone(), tex.export, nearest);
        self.images.entry(key).or_insert_with(|| make_image(tex, nearest).map(|i| images.add(i))).clone()
    }

    fn uniform_and_textures(&mut self, images: &mut Assets<Image>, ev: &Evaluated, nearest: bool) -> (CanvasUniformData, [Option<Handle<Image>>; 3]) {
        let mut d = CanvasUniformData { tint: ev.tint, modulate: 1.0, alpha_test: ev.alpha_test.unwrap_or(-1.0), ..default() };
        let mut texs: [Option<Handle<Image>>; 3] = [None, None, None];
        for (i, s) in ev.slots.iter().enumerate() {
            d.xforms[i] = s.xform;
            match &s.source {
                SlotSource::Empty => d.kinds[i] = 0,
                SlotSource::Texture(t) => match self.image(images, t, nearest) {
                    Some(h) => {
                        d.kinds[i] = 1;
                        d.slot_colors[i] = s.color;
                        texs[i] = Some(h);
                    }
                    None => d.kinds[i] = 0,
                },
                SlotSource::Color(c) => {
                    d.kinds[i] = 2;
                    // Constant colours are authored in sRGB.
                    d.slot_colors[i] = Color::srgba(c[0], c[1], c[2], c[3]).to_linear().to_f32_array();
                }
            }
        }
        if let Some(c) = &ev.combine {
            d.ops = [c.color_op as u32, c.alpha_op as u32, c.invert_mask as u32, 1];
            d.modulate = c.modulate;
        } else if ev.opacity_from_mask {
            d.ops = [0, 0, 0, 2];
        }
        (d, texs)
    }
}

#[allow(clippy::too_many_arguments)]
fn flush(
    mut commands: Commands,
    mut hud: ResMut<HudCanvas>,
    assets: Res<CanvasAssets>,
    mut cache: ResMut<RenderCache>,
    mut stats: ResMut<CanvasStats>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<CanvasMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut batches_q: Query<(&mut Transform, &mut Visibility, &mut MeshMaterial2d<CanvasMaterial>), With<CanvasBatch>>,
) {
    let canvas = &mut hud.0;
    let items = std::mem::take(&mut canvas.items);
    let (sw, sh, t) = (canvas.size_x as f32, canvas.size_y as f32, canvas.time);
    let db = assets.db.clone();

    // Group consecutive items with the same material, style and filtering.
    let mut batches: Vec<(Handle<CanvasMaterial>, &[DrawItem])> = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let a = &items[start];
        let mut end = start + 1;
        while end < items.len() {
            let b = &items[end];
            if b.material.export != a.material.export || !Arc::ptr_eq(&b.material.pkg, &a.material.pkg) || b.style != a.style || b.no_smooth != a.no_smooth {
                break;
            }
            end += 1;
        }
        let key = (a.material.pkg.name.clone(), a.material.export, a.style, a.no_smooth);
        let handle = match cache.materials.get(&key).cloned() {
            Some((h, animated)) => {
                if animated {
                    if let Some(ev) = db.evaluate(&a.material, a.style, t) {
                        let (d, texs) = cache.uniform_and_textures(&mut images, &ev, a.no_smooth);
                        if let Some(mut m) = mats.get_mut(&h) {
                            if m.uniform_data != d || m.tex0 != texs[0] || m.tex1 != texs[1] || m.tex2 != texs[2] {
                                m.uniform_data = d;
                                [m.tex0, m.tex1, m.tex2] = texs;
                            }
                        }
                    }
                }
                Some(h)
            }
            None => db.evaluate(&a.material, a.style, t).map(|ev| {
                let (d, texs) = cache.uniform_and_textures(&mut images, &ev, a.no_smooth);
                let [tex0, tex1, tex2] = texs;
                let h = mats.add(CanvasMaterial { uniform_data: d, tex0, tex1, tex2, blend: ev.blend });
                cache.materials.insert(key, (h.clone(), ev.animated));
                h
            }),
        };
        if let Some(h) = handle {
            batches.push((h, &items[start..end]));
        }
        start = end;
    }

    // Build one mesh per batch; reuse pooled entities.
    let mut quads = 0;
    for (bi, (mat, run)) in batches.iter().enumerate() {
        let n = run.len();
        quads += n;
        let mut pos = Vec::with_capacity(n * 4);
        let mut uv = Vec::with_capacity(n * 4);
        let mut col = Vec::with_capacity(n * 4);
        let mut idx = Vec::with_capacity(n * 6);
        for it in run.iter() {
            let base = pos.len() as u32;
            let (x0, y0, x1, y1) = (it.x - sw * 0.5, sh * 0.5 - it.y, it.x + it.w - sw * 0.5, sh * 0.5 - (it.y + it.h));
            pos.extend_from_slice(&[[x0, y0, 0.0], [x1, y0, 0.0], [x1, y1, 0.0], [x0, y1, 0.0]]);
            uv.extend_from_slice(&[[it.u0, it.v0], [it.u1, it.v0], [it.u1, it.v1], [it.u0, it.v1]]);
            let c = linear(it.color.0);
            col.extend_from_slice(&[c; 4]);
            idx.extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
        }
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
        mesh.insert_indices(Indices::U32(idx));
        let z = bi as f32 * 0.01;
        if let Some((e, mh)) = cache.pool.get(bi).cloned() {
            let _ = meshes.insert(&mh, mesh);
            if let Ok((mut tf, mut vis, mut mm)) = batches_q.get_mut(e) {
                tf.translation.z = z;
                *vis = Visibility::Visible;
                if mm.0 != *mat {
                    mm.0 = mat.clone();
                }
            }
        } else {
            let mh = meshes.add(mesh);
            let e = commands.spawn((Mesh2d(mh.clone()), MeshMaterial2d(mat.clone()), Transform::from_xyz(0.0, 0.0, z), NoFrustumCulling, CanvasBatch)).id();
            cache.pool.push((e, mh));
        }
    }
    for (e, _) in cache.pool.iter().skip(batches.len()) {
        if let Ok((_, mut vis, _)) = batches_q.get_mut(*e) {
            *vis = Visibility::Hidden;
        }
    }
    *stats = CanvasStats { batches: batches.len(), quads, materials: cache.materials.len(), images: cache.images.len() };
    cache.batches = batches.len();
    cache.quads = quads;
}
