//! p2fx: Unreal Engine 2 particle systems (Emitter / ParticleEmitter) for Bevy, loading POSTAL 2
//! emitter classes and placed Emitter actors from the user's own install.
//!
//! - [`load`] resolves classes / actors into self-contained [`FxSystem`] definitions.
//! - [`sim`] is the CPU simulation (UE2 rules, Unreal space).
//! - [`geom`] builds per-sub-emitter vertex batches (billboards, strips, sparks, beams, meshes).
//! - [`FxPlugin`] ticks every emitter in parallel, rebuilds the batches in parallel and uploads one
//!   mesh per sub-emitter drawn with [`ParticleMaterial`], which reproduces the UE2 draw styles.
//!
//! Use: add [`FxPlugin`], then `spawn_emitter(&mut commands, system, transform)`.
//!
//! Script-driven effects (Emitter actors living in the uscript VM, fluids, projector decals):
//! [`vm`] (simulation bridge + natives, no Bevy world needed), [`decal`], and [`FxVmPlugin`] /
//! [`render_vm::FxFrameQueue`] to draw the per-frame snapshot.

pub mod collide;
pub mod decal;
pub mod geom;
pub mod load;
pub mod render_vm;
pub mod sim;
pub mod vm;

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, MeshVertexAttribute, MeshVertexBufferLayoutRef, PrimitiveTopology, VertexFormat};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, CompareFunction, Extent3d, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    TextureDimension, TextureFormat,
};
use bevy::shader::ShaderRef;
use geom::{Batch, View, build_emitter, build_mesh_section, rot_to_unreal, to_unreal};
use load::{FxSystem, TexData};
use sim::{ActorSettings, Owner, SystemState};
use std::collections::HashMap;
use std::sync::Arc;
use upkg::emitter::{DrawStyle, EmitterActor, EmitterKind, ParticleEmitter};

pub use load::FxLibrary;
pub use render_vm::{FxFrameQueue, FxVmPlugin};

const PARTICLE_SHADER: Handle<Shader> = uuid_handle!("2d9b6c1e-55f1-4a7e-9e3b-7c2a8f0d4b61");

/// Per-vertex frame blend factor (BlendBetweenSubdivisions).
pub const ATTRIBUTE_BLEND: MeshVertexAttribute = MeshVertexAttribute::new("P2fxBlend", 0x5032_4658_0001, VertexFormat::Float32);

/// How emitters advance in time.
#[derive(Resource, Clone, Copy, Debug)]
pub struct FxClock {
    /// Fixed step per frame (deterministic, e.g. for screenshots); None = real frame time.
    pub fixed_dt: Option<f32>,
    /// Global pause.
    pub paused: bool,
    /// Largest step simulated per frame (UE2 clamps long frames too).
    pub max_dt: f32,
}

impl Default for FxClock {
    fn default() -> Self {
        Self { fixed_dt: None, paused: false, max_dt: 0.1 }
    }
}

pub struct FxPlugin;

impl Plugin for FxPlugin {
    fn build(&self, app: &mut App) {
        app.world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(PARTICLE_SHADER.id(), Shader::from_wgsl(include_str!("particle.wgsl"), "p2fx/particle.wgsl"))
            .expect("register particle shader");
        app.add_plugins(MaterialPlugin::<ParticleMaterial>::default())
            .init_resource::<FxClock>()
            .init_resource::<FxAssets>()
            .add_systems(PostUpdate, (init_instances, tick_instances, build_batches, upload_batches, despawn_finished).chain().after(TransformSystems::Propagate));
    }
}

// -------------------------------------------------------------------------------------------------
// Material
// -------------------------------------------------------------------------------------------------

/// Blend / depth state of a particle batch (the specialization key).
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ParticleKey {
    /// EParticleDrawStyle as a byte.
    pub style: u8,
    pub z_test: bool,
    pub z_write: bool,
}

/// UE2 particle material: texture x vertex colour, frame blending, alpha test and the
/// fixed-function blend modes of the original renderer.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
#[bind_group_data(ParticleKey)]
pub struct ParticleMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub texture: Option<Handle<Image>>,
    /// x: alpha reference (0..1), y: alpha test, z: draw style, w: has texture.
    #[uniform(2)]
    pub params: Vec4,
    pub key: ParticleKey,
    /// Draw order tie-break between sub-emitters of one system (array order).
    pub order: f32,
}

impl From<&ParticleMaterial> for ParticleKey {
    fn from(m: &ParticleMaterial) -> Self {
        m.key
    }
}

/// Transparent-sort bias for particle/decal batches (see `ParticleMaterial::depth_bias`).
const PARTICLE_SORT_BIAS: f32 = 100_000.0;

fn blend_for(style: u8) -> Option<BlendState> {
    let c = |src, dst| BlendComponent { src_factor: src, dst_factor: dst, operation: BlendOperation::Add };
    let alpha_keep = c(BlendFactor::Zero, BlendFactor::One);
    use BlendFactor as F;
    Some(match DrawStyle::from_byte(style) {
        DrawStyle::Regular => return None,
        DrawStyle::AlphaBlend => BlendState { color: c(F::SrcAlpha, F::OneMinusSrcAlpha), alpha: alpha_keep },
        DrawStyle::Modulated => BlendState { color: c(F::Dst, F::Src), alpha: alpha_keep },
        DrawStyle::Translucent => BlendState { color: c(F::One, F::One), alpha: alpha_keep },
        DrawStyle::AlphaModulate => BlendState { color: c(F::One, F::OneMinusSrcAlpha), alpha: alpha_keep },
        DrawStyle::Darken => BlendState { color: c(F::Zero, F::OneMinusSrc), alpha: alpha_keep },
        // Brighten (ONE, INVSRCCOLOR): additive-ish, black adds nothing (opaque coronas need this).
        DrawStyle::Brighten => BlendState { color: c(F::One, F::OneMinusSrc), alpha: alpha_keep },
    })
}

impl Material for ParticleMaterial {
    fn vertex_shader() -> ShaderRef {
        ShaderRef::Handle(PARTICLE_SHADER)
    }
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(PARTICLE_SHADER)
    }
    fn alpha_mode(&self) -> AlphaMode {
        if self.key.style == 0 { AlphaMode::Opaque } else { AlphaMode::Blend }
    }
    fn depth_bias(&self) -> f32 {
        // Batches are world-space meshes on an identity transform, so their transparent sort
        // distance is meaningless (the world origin). Bias them past every other translucent
        // surface (terrain splat layers use their layer order) so blended terrain layers do not
        // repaint over smoke, puddles and decals; `order` keeps emitter/decal order within fx.
        PARTICLE_SORT_BIAS + self.order
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(_: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, key: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        let vl = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_1.at_shader_location(2),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(3),
            ATTRIBUTE_BLEND.at_shader_location(4),
        ])?;
        descriptor.vertex.buffers = vec![vl];
        descriptor.primitive.cull_mode = None;
        let k = key.bind_group_data;
        if let Some(frag) = descriptor.fragment.as_mut() {
            for t in frag.targets.iter_mut().flatten() {
                t.blend = blend_for(k.style);
            }
        }
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = Some(k.z_write || k.style == 0);
            if !k.z_test {
                ds.depth_compare = Some(CompareFunction::Always);
            }
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------
// Instances
// -------------------------------------------------------------------------------------------------

/// A running particle system. Spawn with [`spawn_emitter`]; the entity's transform is the
/// emitter actor's location/rotation.
#[derive(Component)]
pub struct FxInstance {
    pub system: Arc<FxSystem>,
    defs: Arc<Vec<ParticleEmitter>>,
    pub settings: ActorSettings,
    pub state: SystemState,
    /// Multiplies the clock step.
    pub time_scale: f32,
    pub paused: bool,
    /// Despawn the entity when the system finishes (AutoDestroy / Kill).
    pub despawn_when_finished: bool,
    slots: Vec<Slot>,
    initialized: bool,
    owner: Owner,
    last_location: Option<Vec3>,
}

/// One draw batch (a sub-emitter, or one section of a mesh emitter).
struct Slot {
    emitter: usize,
    section: Option<usize>,
    entity: Entity,
    mesh: Handle<Mesh>,
    batch: Batch,
    lines: bool,
}

impl FxInstance {
    pub fn new(system: Arc<FxSystem>, seed: u64) -> Self {
        let defs: Arc<Vec<ParticleEmitter>> = Arc::new(system.emitters.iter().map(|e| e.data.clone()).collect());
        let a = &system.actor;
        let settings = ActorSettings { auto_destroy: a.auto_destroy, auto_reset: a.auto_reset, global_offset_range: a.global_offset_range, time_till_reset_range: a.time_till_reset_range };
        let state = SystemState::new(&defs, &settings, seed);
        Self { system, defs, settings, state, time_scale: 1.0, paused: false, despawn_when_finished: true, slots: vec![], initialized: false, owner: Owner::default(), last_location: None }
    }

    /// Stops emission and lets the live particles finish (`Emitter.Kill`).
    pub fn kill(&mut self) {
        self.state.kill();
    }

    /// Toggles every sub-emitter on/off (`Emitter.Trigger`).
    pub fn trigger(&mut self) {
        self.state.trigger();
    }

    /// Advances the simulation directly (e.g. warm-up before the first frame).
    pub fn advance(&mut self, seconds: f32, step: f32) {
        let mut t = 0.0;
        while t < seconds {
            let dt = step.min(seconds - t).max(1e-4);
            self.state.tick(&self.defs, &self.settings, &self.owner, dt);
            t += dt;
        }
    }

    pub fn live_particles(&self) -> usize {
        self.state.live_particles()
    }

    /// World (Unreal space) locations of the live particles.
    pub fn particle_points(&self) -> Vec<Vec3> {
        let mut out = Vec::new();
        for (i, e) in self.state.emitters.iter().enumerate() {
            let def = &self.defs[i];
            for p in e.particles.iter().take(e.active_particles).filter(|p| p.alive()) {
                out.push(if def.coordinate_system == 1 { self.owner.location + self.owner.rotation * p.location } else { p.location });
            }
        }
        out
    }

    /// Median sprite half-size of the live particles (Unreal units).
    pub fn typical_size(&self) -> f32 {
        let mut v: Vec<f32> = Vec::new();
        for (i, e) in self.state.emitters.iter().enumerate() {
            let def = &self.defs[i];
            for p in e.particles.iter().take(e.active_particles).filter(|p| p.alive()) {
                v.push(if def.kind == EmitterKind::Mesh { p.size.abs().max_element() } else { p.size.x.abs().max(if def.use_direction_as != 0 { p.size.y.abs() } else { 0.0 }) });
            }
        }
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    }

    /// Bounds (Unreal space) of the live particles including their sizes.
    pub fn particle_bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        let mut any = false;
        for (i, e) in self.state.emitters.iter().enumerate() {
            let def = &self.defs[i];
            for p in e.particles.iter().take(e.active_particles).filter(|p| p.alive()) {
                let loc = if def.coordinate_system == 1 { self.owner.location + self.owner.rotation * p.location } else { p.location };
                let s = if def.kind == EmitterKind::Mesh { p.size.abs().max_element() } else if def.use_direction_as != 0 { p.size.x.abs().max(p.size.y.abs()) } else { p.size.x.abs() };
                let r = Vec3::splat(s);
                lo = lo.min(loc - r);
                hi = hi.max(loc + r);
                any = true;
            }
        }
        any.then_some((lo, hi))
    }

    fn set_owner_from(&mut self, tf: &GlobalTransform, dt: f32) {
        let (_, rot, pos) = tf.to_scale_rotation_translation();
        let loc = to_unreal(pos);
        let vel = match self.last_location {
            Some(prev) if dt > 0.0 => (loc - prev) / dt,
            _ => Vec3::ZERO,
        };
        self.last_location = Some(loc);
        self.owner = Owner { location: loc, rotation: rot_to_unreal(Mat3::from_quat(rot)), velocity: vel };
    }
}

/// Bevy transform for an emitter actor's Unreal location and rotation.
pub fn actor_transform(actor: &EmitterActor) -> Transform {
    let loc = geom::to_bevy(Vec3::from(actor.location));
    let m = sim::rotator_matrix(actor.rotation);
    // Same conjugation in both directions (the axis swap is its own inverse).
    let rot = Quat::from_mat3(&rot_to_unreal(m));
    Transform::from_translation(loc).with_rotation(rot)
}

/// Spawns a particle system. Returns the root entity (carrying [`FxInstance`]).
pub fn spawn_emitter(commands: &mut Commands, system: Arc<FxSystem>, transform: Transform) -> Entity {
    let seed = (system.name.len() as u64) << 32 ^ 0x9e37_79b9 ^ transform.translation.x.to_bits() as u64;
    commands.spawn((FxInstance::new(system, seed), transform, Visibility::default())).id()
}

/// Caches GPU images and materials shared by all instances.
#[derive(Resource, Default)]
pub struct FxAssets {
    images: HashMap<(usize, bool), Handle<Image>>,
    materials: HashMap<(usize, ParticleKey, u32, u8, i32), Handle<ParticleMaterial>>,
}

impl FxAssets {
    fn image(&mut self, images: &mut Assets<Image>, tex: &Arc<TexData>, repeat: bool) -> Handle<Image> {
        let key = (Arc::as_ptr(tex) as usize, repeat);
        self.images
            .entry(key)
            .or_insert_with(|| {
                let mut img = Image::new_uninit(Extent3d { width: tex.width, height: tex.height, depth_or_array_layers: 1 }, TextureDimension::D2, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD);
                img.data = Some(tex.rgba.clone());
                img.texture_descriptor.mip_level_count = tex.levels;
                let mode = if repeat { ImageAddressMode::Repeat } else { ImageAddressMode::ClampToEdge };
                img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: mode,
                    address_mode_v: mode,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    mipmap_filter: ImageFilterMode::Linear,
                    ..default()
                });
                images.add(img)
            })
            .clone()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn material(&mut self, images: &mut Assets<Image>, mats: &mut Assets<ParticleMaterial>, tex: Option<&Arc<TexData>>, repeat: bool, key: ParticleKey, alpha_test: bool, alpha_ref: i32, order: i32) -> Handle<ParticleMaterial> {
        let tex_id = tex.map_or(0, |t| Arc::as_ptr(t) as usize);
        let mkey = (tex_id, key, alpha_ref as u32, alpha_test as u8 | (repeat as u8) << 1, order);
        if let Some(h) = self.materials.get(&mkey) {
            return h.clone();
        }
        let texture = tex.map(|t| self.image(images, t, repeat));
        let params = Vec4::new(alpha_ref.clamp(0, 255) as f32 / 255.0, alpha_test as u8 as f32, key.style as f32, texture.is_some() as u8 as f32);
        let h = mats.add(ParticleMaterial { texture, params, key, order: order as f32 * -0.001 });
        self.materials.insert(mkey, h.clone());
        h
    }
}

pub(crate) fn empty_mesh(lines: bool) -> Mesh {
    let topo = if lines { PrimitiveTopology::LineList } else { PrimitiveTopology::TriangleList };
    let mut m = Mesh::new(topo, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD);
    fill_mesh(&mut m, &Batch::default());
    m
}

pub(crate) fn fill_mesh(m: &mut Mesh, b: &Batch) {
    // Bevy's mesh allocator skips meshes with no vertex data but still copies them, logging
    // "Use-after-free: attempted to copy element data for an unallocated key". Upload one
    // degenerate (zero-area) triangle instead; empty batches are hidden anyway.
    if b.positions.is_empty() {
        m.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3]);
        m.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 3]);
        m.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.0f32; 2]; 3]);
        m.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3]);
        m.insert_attribute(ATTRIBUTE_BLEND, vec![0.0f32; 3]);
        m.insert_indices(Indices::U32(vec![0, 1, 2]));
        return;
    }
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, b.positions.clone());
    m.insert_attribute(Mesh::ATTRIBUTE_UV_0, b.uv0.clone());
    m.insert_attribute(Mesh::ATTRIBUTE_UV_1, b.uv1.clone());
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, b.colors.clone());
    m.insert_attribute(ATTRIBUTE_BLEND, b.blend.clone());
    m.insert_indices(Indices::U32(b.indices.clone()));
}

/// Creates the draw entities (one per batch) of newly spawned instances.
fn init_instances(
    mut commands: Commands,
    mut q: Query<(Entity, &mut FxInstance)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<ParticleMaterial>>,
    mut cache: ResMut<FxAssets>,
) {
    for (root, mut inst) in &mut q {
        if inst.initialized {
            continue;
        }
        inst.initialized = true;
        let system = inst.system.clone();
        let mut slots = Vec::new();
        for (i, fx) in system.emitters.iter().enumerate() {
            let d = &fx.data;
            let key = ParticleKey { style: d.draw_style as u8, z_test: d.z_test, z_write: d.z_write };
            let mut add = |section: Option<usize>, tex: Option<&Arc<TexData>>, key: ParticleKey, alpha_test: bool, lines: bool, repeat: bool| {
                let mesh = meshes.add(empty_mesh(lines));
                let mat = cache.material(&mut images, &mut mats, tex, repeat, key, alpha_test, d.alpha_ref, i as i32);
                let entity = commands
                    .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), Transform::IDENTITY, Visibility::Hidden, NoFrustumCulling, NotShadowCaster, NotShadowReceiver, FxBatchOf(root)))
                    .id();
                slots.push(Slot { emitter: i, section, entity, mesh, batch: Batch::default(), lines });
            };
            match d.kind {
                EmitterKind::Mesh => {
                    let Some(mesh) = &fx.mesh else { continue };
                    for (s, sec) in mesh.sections.iter().enumerate() {
                        let k = if d.use_mesh_blend_mode {
                            ParticleKey { style: sec.style as u8, z_test: true, z_write: sec.style == DrawStyle::Regular }
                        } else {
                            key
                        };
                        add(Some(s), sec.texture.as_ref(), k, d.alpha_test || sec.masked, false, true);
                    }
                }
                EmitterKind::Spark => add(None, None, key, false, true, false),
                EmitterKind::Beam => add(None, fx.texture.as_ref(), key, d.alpha_test, false, true),
                _ => add(None, fx.texture.as_ref(), key, d.alpha_test, false, false),
            }
        }
        inst.slots = slots;
    }
}

/// Ticks every instance's simulation in parallel.
fn tick_instances(time: Res<Time>, clock: Res<FxClock>, coll: Option<Res<collide::FxCollision>>, mut q: Query<(&mut FxInstance, &GlobalTransform)>) {
    let world = coll.map(|c| c.0.clone());
    if clock.paused {
        return;
    }
    let dt = clock.fixed_dt.unwrap_or(time.delta_secs()).min(clock.max_dt);
    if dt <= 0.0 {
        return;
    }
    q.par_iter_mut().for_each(|(mut inst, tf)| {
        if inst.paused {
            return;
        }
        let step = dt * inst.time_scale;
        inst.set_owner_from(tf, step);
        let inst = &mut *inst;
        let collider = world.as_deref().map(|w| w as &dyn sim::Collider);
        inst.state.tick_with(&inst.defs, &inst.settings, &inst.owner, step, collider);
    });
}

/// Rebuilds vertex batches in parallel (camera-facing geometry needs the current view).
/// Camera query used to find the main view.
pub type ViewCams<'w, 's> = Query<'w, 's, (Entity, &'static GlobalTransform, &'static Camera, Option<&'static bevy::camera::visibility::RenderLayers>), With<Camera3d>>;

/// The main world view among several 3D cameras: active, rendering the default layer (sky-zone
/// and first-person overlay cameras use their own layers), lowest order first.
pub fn main_view(cams: &ViewCams) -> Option<(Entity, GlobalTransform)> {
    cams.iter()
        .filter(|(_, _, c, l)| c.is_active && l.is_none_or(|l| l.intersects(&bevy::camera::visibility::RenderLayers::layer(0))))
        .min_by_key(|(_, _, c, _)| c.order)
        .map(|(e, t, _, _)| (e, *t))
}

fn build_batches(cams: ViewCams, mut q: Query<&mut FxInstance>) {
    let Some((_, cam)) = main_view(&cams) else { return };
    let (_, rot, pos) = cam.to_scale_rotation_translation();
    let to_u = |v: Vec3| Vec3::new(v.x, v.z, v.y);
    let view = View { location: to_unreal(pos), forward: to_u(rot * Vec3::NEG_Z), up: to_u(rot * Vec3::Y), right: to_u(rot * Vec3::X) };
    q.par_iter_mut().for_each(|mut inst| {
        let inst = &mut *inst;
        let system = &inst.system;
        let owner = inst.owner;
        for slot in &mut inst.slots {
            let st = &inst.state.emitters[slot.emitter];
            let fx = &system.emitters[slot.emitter];
            if st.disabled && st.live == 0 {
                slot.batch.clear();
                continue;
            }
            match (slot.section, &fx.mesh) {
                (Some(s), Some(mesh)) => {
                    slot.batch.clear();
                    build_mesh_section(&fx.data, st, &owner, mesh, s, &mut slot.batch);
                }
                _ => build_emitter(fx, st, &owner, &view, &mut slot.batch),
            }
        }
    });
}

/// Uploads the batches into their meshes.
fn upload_batches(mut q: Query<&mut FxInstance>, mut meshes: ResMut<Assets<Mesh>>, mut vis: Query<&mut Visibility>) {
    let debug = std::env::var_os("P2FX_DEBUG").is_some();
    for mut inst in &mut q {
        for slot in &mut inst.slots {
            let empty = slot.batch.is_empty();
            if debug {
                let p0 = slot.batch.positions.first().copied();
                let c0 = slot.batch.colors.get(slot.batch.colors.len() / 2).copied();
                eprintln!("p2fx batch emitter={} section={:?} verts={} indices={} first={p0:?} mid_color={c0:?}", slot.emitter, slot.section, slot.batch.positions.len(), slot.batch.indices.len());
            }
            if let Ok(mut v) = vis.get_mut(slot.entity) {
                let want = if empty { Visibility::Hidden } else { Visibility::Inherited };
                if *v != want {
                    *v = want;
                }
            }
            if empty {
                continue;
            }
            let topo = if slot.lines { PrimitiveTopology::LineList } else { PrimitiveTopology::TriangleList };
            if let Some(mut m) = meshes.get_mut(slot.mesh.id()) {
                fill_mesh(&mut m, &slot.batch);
            } else {
                let mut m = Mesh::new(topo, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD);
                fill_mesh(&mut m, &slot.batch);
                let _ = meshes.insert(slot.mesh.id(), m);
            }
        }
    }
}

/// Links a batch entity to its emitter. Batch vertices are already in world space, so batches are
/// not transform children of the emitter; they are despawned together with it.
#[derive(Component, Clone, Copy, Debug)]
pub struct FxBatchOf(pub Entity);

fn despawn_finished(mut commands: Commands, q: Query<(Entity, &FxInstance)>, batches: Query<(Entity, &FxBatchOf)>, mut removed: RemovedComponents<FxInstance>) {
    let mut gone: Vec<Entity> = removed.read().collect();
    for (e, inst) in &q {
        if inst.despawn_when_finished && inst.state.finished {
            commands.entity(e).despawn();
            gone.push(e);
        }
    }
    if !gone.is_empty() {
        for (b, of) in &batches {
            if gone.contains(&of.0) {
                commands.entity(b).despawn();
            }
        }
    }
}
