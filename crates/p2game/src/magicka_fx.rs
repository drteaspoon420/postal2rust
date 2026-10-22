//! Magicka spell presentation inside Postal's renderer: the original particle
//! effects (sprays, impacts, statuses, chants), railgun beams and lightning
//! bolts with materials ported from the rewrite viewer, original animated wall
//! and mine models, simple shapes for
//! shields, barriers and mines, and original missile models. Magicka world space (metres, Y up)
//! is Postal's Bevy world space (`p2view::scene::to_bevy`), so positions pass
//! through unchanged.
use crate::magicka::MagicState;
use bevy::{
    asset::{AssetPath, RenderAssetUsages, embedded_asset, embedded_path},
    camera::visibility::NoFrustumCulling,
    image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor},
    light::NotShadowCaster,
    mesh::{Indices, MeshVertexAttribute, MeshVertexBufferLayoutRef, PrimitiveTopology},
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, CompareFunction,
        Extent3d, RenderPipelineDescriptor, SpecializedMeshPipelineError, TextureDimension,
        TextureFormat, VertexFormat,
    },
    shader::ShaderRef,
};
use p2magicka::{
    magicka_runtime::{beam_mesh, bolt_mesh, particles::ParticleGraphics},
    magicka_sim::spells::Elements,
};

fn shader(path: std::path::PathBuf) -> ShaderRef {
    ShaderRef::Path(AssetPath::from_path_buf(path).with_source("embedded"))
}

/// Reverse-Z depth test without depth writes, as the original effects draw
/// after the opaque scene.
pub(crate) fn effect_depth(descriptor: &mut RenderPipelineDescriptor, blend: BlendComponent) {
    if let Some(fragment) = &mut descriptor.fragment {
        for target in fragment.targets.iter_mut().flatten() {
            target.blend = Some(BlendState {
                color: blend,
                alpha: blend,
            });
        }
    }
    if let Some(depth) = &mut descriptor.depth_stencil {
        depth.depth_compare = Some(CompareFunction::GreaterEqual);
        depth.depth_write_enabled = Some(false);
    }
    descriptor.primitive.cull_mode = None;
}

/// The effect meshes are world-space geometry on identity transforms, so
/// their transparent sort distance is meaningless. Like Postal's own particle
/// batches (p2fx `PARTICLE_SORT_BIAS`), they are sorted past every other
/// translucent surface so terrain splat layers do not paint over them.
const SORT_BIAS: f32 = 110_000.0;

const ATTRIBUTE_PARTICLE: MeshVertexAttribute = MeshVertexAttribute::new(
    "ParticleCornerRotationRadiusLife",
    0x6d61_6701,
    VertexFormat::Float32x4,
);
const ATTRIBUTE_FLAGS: MeshVertexAttribute =
    MeshVertexAttribute::new("ParticleFlags", 0x6d61_6702, VertexFormat::Float32x4);
const ATTRIBUTE_VELOCITY: MeshVertexAttribute =
    MeshVertexAttribute::new("ParticleVelocity", 0x6d61_6703, VertexFormat::Float32x3);

/// ParticleEffect over the four sprite sheets (EffectTextures/ParticlesA..D).
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(crate) struct ParticleMaterial {
    #[texture(0, dimension = "3d")]
    #[sampler(1)]
    a: Handle<Image>,
    #[texture(2, dimension = "3d")]
    #[sampler(3)]
    b: Handle<Image>,
    #[texture(4, dimension = "3d")]
    #[sampler(5)]
    c: Handle<Image>,
    #[texture(6, dimension = "3d")]
    #[sampler(7)]
    d: Handle<Image>,
}
impl Material for ParticleMaterial {
    fn depth_bias(&self) -> f32 {
        SORT_BIAS + 3.
    }
    fn vertex_shader() -> ShaderRef {
        shader(embedded_path!("magicka_particles.wgsl"))
    }
    fn fragment_shader() -> ShaderRef {
        shader(embedded_path!("magicka_particles.wgsl"))
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(1),
            ATTRIBUTE_PARTICLE.at_shader_location(2),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(3),
            ATTRIBUTE_FLAGS.at_shader_location(4),
            ATTRIBUTE_VELOCITY.at_shader_location(5),
        ])?];
        // SourceBlend One, DestinationBlend SourceAlpha.
        effect_depth(
            descriptor,
            BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::SrcAlpha,
                operation: BlendOperation::Add,
            },
        );

        Ok(())
    }
}

/// ArcaneEffect Beam technique (railgun beams).
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(crate) struct BeamMaterial {
    #[uniform(0)]
    color_center: Vec4,
    #[uniform(1)]
    color_edge: Vec4,
    #[texture(2)]
    #[sampler(3)]
    texture: Handle<Image>,
}
impl Material for BeamMaterial {
    fn depth_bias(&self) -> f32 {
        SORT_BIAS + 2.
    }
    fn fragment_shader() -> ShaderRef {
        shader(embedded_path!("magicka_beam.wgsl"))
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        _: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        effect_depth(
            descriptor,
            BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
        );
        Ok(())
    }
}

/// Scene.DrawAdditive (lightning bolts, shields, elemental barriers, mines).
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(crate) struct AdditiveMaterial {
    #[uniform(0)]
    color: Vec4,
    #[uniform(1)]
    flags: UVec4,
    #[texture(2)]
    #[sampler(3)]
    texture: Option<Handle<Image>>,
}
impl Material for AdditiveMaterial {
    fn depth_bias(&self) -> f32 {
        SORT_BIAS + 1.
    }
    fn fragment_shader() -> ShaderRef {
        shader(embedded_path!("magicka_additive.wgsl"))
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _: &MeshVertexBufferLayoutRef,
        _: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        effect_depth(
            descriptor,
            BlendComponent {
                src_factor: BlendFactor::OneMinusDst,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
        );
        Ok(())
    }
}

const DECAL_INV_X: MeshVertexAttribute =
    MeshVertexAttribute::new("DecalInverseX", 0x6d64_0001, VertexFormat::Float32x4);
const DECAL_INV_Y: MeshVertexAttribute =
    MeshVertexAttribute::new("DecalInverseY", 0x6d64_0002, VertexFormat::Float32x4);
const DECAL_INV_Z: MeshVertexAttribute =
    MeshVertexAttribute::new("DecalInverseZ", 0x6d64_0003, VertexFormat::Float32x4);
const DECAL_ARGS: MeshVertexAttribute =
    MeshVertexAttribute::new("DecalArgs", 0x6d64_0004, VertexFormat::Float32x3);

/// Magicka's projection effect (viewer `DecalMaterial`) over the decal atlas.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(crate) struct DecalMaterial {
    #[texture(0)]
    #[sampler(1)]
    atlas: Handle<Image>,
}
impl Material for DecalMaterial {
    fn vertex_shader() -> ShaderRef {
        shader(embedded_path!("magicka_decals.wgsl"))
    }
    fn fragment_shader() -> ShaderRef {
        shader(embedded_path!("magicka_decals.wgsl"))
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn depth_bias(&self) -> f32 {
        f32::NEG_INFINITY
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _: &MaterialPipeline,
        d: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        d.vertex.buffers = vec![layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            DECAL_INV_X.at_shader_location(1),
            DECAL_INV_Y.at_shader_location(2),
            DECAL_INV_Z.at_shader_location(3),
            DECAL_ARGS.at_shader_location(4),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(5),
        ])?];
        if let Some(depth) = &mut d.depth_stencil {
            depth.depth_compare = Some(CompareFunction::GreaterEqual);
            depth.depth_write_enabled = Some(false);
        }
        // Exit faces, so projection also works with the camera inside a box.
        d.primitive.cull_mode = Some(bevy::render::render_resource::Face::Front);
        Ok(())
    }
}

/// Frozen bodies: the skinned mesh bloated along its normals as translucent ice.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(crate) struct IceShellMaterial {
    /// rgb ice colour, w bloat in metres.
    #[uniform(0)]
    pub params: Vec4,
}
impl Material for IceShellMaterial {
    fn vertex_shader() -> ShaderRef {
        shader(embedded_path!("magicka_ice.wgsl"))
    }
    fn fragment_shader() -> ShaderRef {
        shader(embedded_path!("magicka_ice.wgsl"))
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
}

/// Magicka's projections read scene depth: give the Postal camera a depth
/// prepass while the wizard is hosted (the camera is respawned per level).
fn depth_prepass(
    mut commands: Commands,
    cameras: Query<Entity, (With<crate::window::FlyCam>, Without<bevy::core_pipeline::prepass::DepthPrepass>)>,
) {
    if std::env::var_os("MAGICKA_NO_PREPASS").is_some() {
        return;
    }
    for camera in &cameras {
        commands.entity(camera).insert((bevy::core_pipeline::prepass::DepthPrepass, bevy::pbr::ScreenSpaceTransmission::default()));
    }
}

pub(crate) struct MagickaFxPlugin;
impl Plugin for MagickaFxPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "magicka_particles.wgsl");
        embedded_asset!(app, "magicka_beam.wgsl");
        embedded_asset!(app, "magicka_additive.wgsl");
        embedded_asset!(app, "magicka_decals.wgsl");
        embedded_asset!(app, "magicka_ice.wgsl");
        app.add_plugins((
            MaterialPlugin::<ParticleMaterial>::default(),
            MaterialPlugin::<BeamMaterial>::default(),
            MaterialPlugin::<AdditiveMaterial>::default(),
            MaterialPlugin::<DecalMaterial>::default(),
            MaterialPlugin::<IceShellMaterial>::default(),
        ))
        .add_systems(Update, depth_prepass)
        .add_systems(
            Update,
            sync.after(crate::magicka::tick)
                .before(crate::ui::follow_player),
        );
    }
}

/// Element tint used for shapes the original draws with dedicated models.
pub(crate) fn element_color(e: Elements) -> [f32; 3] {
    const ORDER: [(Elements, [f32; 3]); 11] = [
        (Elements::ICE, [0.6, 0.85, 1.0]),
        (Elements::STEAM, [0.85, 0.85, 0.9]),
        (Elements::POISON, [0.45, 0.9, 0.2]),
        (Elements::ARCANE, [1.0, 0.15, 0.25]),
        (Elements::LIFE, [0.25, 1.0, 0.35]),
        (Elements::FIRE, [1.0, 0.45, 0.1]),
        (Elements::LIGHTNING, [0.75, 0.45, 1.0]),
        (Elements::COLD, [0.8, 0.95, 1.0]),
        (Elements::WATER, [0.2, 0.5, 1.0]),
        (Elements::SHIELD, [1.0, 0.85, 0.4]),
        (Elements::EARTH, [0.55, 0.4, 0.25]),
    ];
    let mut sum = [0.; 3];
    let mut n = 0.;
    for (element, color) in ORDER {
        if e.0 & element.0 == element.0 && element.0 != 0 {
            for i in 0..3 {
                sum[i] += color[i];
            }
            n += 1.;
            if n >= 2. {
                break;
            }
        }
    }
    if n == 0. {
        return [1.; 3];
    }
    sum.map(|c| c / n)
}

#[derive(Default)]
struct Pool {
    entities: Vec<Entity>,
    used: usize,
}

struct Shared {
    particles: Handle<Mesh>,
    beam_texture: Handle<Image>,
    bolt_strips: Vec<[bolt_mesh::StripVertex; 34]>,
    sphere: Handle<Mesh>,
    cylinder: Handle<Mesh>,
}

#[derive(Default)]
struct FxState {
    shared: Option<Shared>,
    failed: bool,
    beams: Vec<(Entity, Handle<Mesh>, Handle<BeamMaterial>)>,
    bolts: Vec<(u32, Entity, Handle<AdditiveMaterial>)>,
    glows: Pool,
    glow_materials: Vec<Handle<AdditiveMaterial>>,
    missiles: Pool,
    missile_models:
        std::collections::HashMap<String, Vec<(Handle<Mesh>, Handle<StandardMaterial>)>>,
    /// Original animated effect models (walls, mines); None after a load error.
    /// Keyed by model and baked fabric tint.
    effect_models: std::collections::HashMap<(&'static str, [u8; 3]), Option<EffectModel>>,
    animated: Vec<Animated>,
    shields: std::collections::HashMap<u16, (Entity, Handle<AdditiveMaterial>, Handle<Mesh>)>,
    additional_beams: Vec<(Entity, Handle<Mesh>)>,
    additional_material: Option<Handle<AdditiveMaterial>>,
    arcane_blasts: Option<(Entity, Handle<Mesh>)>,
    arcane_disc: Option<Option<Handle<Image>>>,
    /// runesArcane / runesLife meshes and the runes00 texture (None: failed).
    rune_assets: Option<Option<([Handle<Mesh>; 2], Handle<Image>)>>,
    runes: Vec<(Entity, Handle<AdditiveMaterial>)>,
    /// Ground projections: one mesh for all of them (None: atlas failed).
    decals: Option<Option<(Entity, Handle<Mesh>)>>,
}
struct EffectModel {
    effect: p2magicka::visual::SkinnedEffect,
    images: Vec<Option<Handle<Image>>>,
}
/// One drawn instance of an effect model: an entity, mesh and material per part.
struct Animated {
    key: (&'static str, [u8; 3]),
    parts: Vec<(Entity, Handle<Mesh>, Handle<StandardMaterial>)>,
}
/// A requested effect model instance this frame.
struct EffectDraw {
    mesh: &'static str,
    clips: Option<&'static str>,
    time: f32,
    world: [f32; 16],
    tint: [f32; 3],
    emissive: f32,
}
/// Bevy emissive units for EmissiveAmount 1 under Postal's camera exposure.
fn glow_scale() -> f32 {
    std::env::var("MAGICKA_GLOW").ok().and_then(|v| v.parse().ok()).unwrap_or(2.5)
}
/// XNA row-vector world matrix: rows right, up, backward, translation.
fn xna_world(position: Vec3, forward: Vec3, yaw: Option<f32>, scale: f32) -> [f32; 16] {
    let (right, up, back) = match yaw {
        Some(a) => (
            Vec3::new(a.cos(), 0., -a.sin()),
            Vec3::Y,
            Vec3::new(a.sin(), 0., a.cos()),
        ),
        None => {
            let back = -forward.normalize_or(Vec3::NEG_Z);
            let right = Vec3::Y.cross(back).normalize_or(Vec3::X);
            (right, back.cross(right), back)
        }
    };
    let (r, u, b) = (right * scale, up * scale, back * scale);
    [
        r.x, r.y, r.z, 0., u.x, u.y, u.z, 0., b.x, b.y, b.z, 0., position.x, position.y,
        position.z, 1.,
    ]
}

fn volume_image(v: &p2magicka::visual::Volume) -> Image {
    let base = (v.width * v.height * v.depth * 4) as usize;
    let mut image = Image::new(
        Extent3d {
            width: v.width,
            height: v.height,
            depth_or_array_layers: v.depth,
        },
        TextureDimension::D3,
        v.rgba[..base].to_vec(),
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(v.rgba.clone());
    image.texture_descriptor.mip_level_count = v.mip_count;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        address_mode_w: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

/// Four vertices per particle with the instance data repeated per corner.
fn particle_mesh(particles: &[ParticleGraphics]) -> Mesh {
    const CORNERS: [(f32, [f32; 2]); 4] = [
        (0.0, [1.0, 1.0]),
        (1.0, [0.0, 1.0]),
        (2.0, [0.0, 0.0]),
        (3.0, [1.0, 0.0]),
    ];
    let empty = [ParticleGraphics::default()];
    let list = if particles.is_empty() {
        &empty[..]
    } else {
        particles
    };
    let n = list.len() * 4;
    let (mut positions, mut uvs, mut data, mut colors, mut flags, mut velocities) = (
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    );
    let mut indices = Vec::with_capacity(list.len() * 6);
    for (i, p) in list.iter().enumerate() {
        for (corner, uv) in CORNERS {
            positions.push(p.position);
            uvs.push(uv);
            data.push([corner, p.rotation, p.radius, p.normalized_lifetime]);
            colors.push(p.color);
            flags.push([
                f32::from(p.sprite),
                f32::from(p.alpha_blended),
                f32::from(p.hsv_colorize),
                f32::from(p.rotation_aligned),
            ]);
            velocities.push(p.velocity);
        }
        let base = (i * 4) as u32;
        indices.extend([0, 1, 2, 0, 2, 3].map(|k| base + k));
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(ATTRIBUTE_PARTICLE, data);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_attribute(ATTRIBUTE_FLAGS, flags);
    mesh.insert_attribute(ATTRIBUTE_VELOCITY, velocities);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

#[allow(clippy::too_many_arguments)]
fn setup(
    commands: &mut Commands,
    content: &std::path::Path,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    particle_materials: &mut Assets<ParticleMaterial>,
) -> Result<Shared, String> {
    let sheets = p2magicka::visual::particle_sheets(content)?;
    let [a, b, c, d]: [Handle<Image>; 4] = sheets
        .iter()
        .map(|v| images.add(volume_image(v)))
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "four particle sheets")?;
    let particles = meshes.add(particle_mesh(&[]));
    commands.spawn((
        Mesh3d(particles.clone()),
        MeshMaterial3d(particle_materials.add(ParticleMaterial { a, b, c, d })),
        Transform::IDENTITY,
        NoFrustumCulling,
        NotShadowCaster,
    ));
    let beams = p2magicka::visual::texture2d(content, "EffectTextures/Beams")?;
    let mut beam_image = Image::new(
        Extent3d {
            width: beams.width,
            height: beams.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        beams.rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    beam_image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    });
    let mut rng = p2magicka::magicka_runtime::particles::DotNetRandom::new(0);
    Ok(Shared {
        particles,
        beam_texture: images.add(beam_image),
        bolt_strips: (0..16).map(|_| bolt_mesh::strip(&mut rng)).collect(),
        sphere: meshes.add(Sphere::new(1.0).mesh().uv(32, 16)),
        cylinder: meshes.add(Cylinder::new(1.0, 1.0)),
    })
}

fn place(
    commands: &mut Commands,
    pool: &mut Pool,
    transforms: &mut Query<(&mut Transform, &mut Visibility)>,
    transform: Transform,
    spawn: impl FnOnce(&mut Commands) -> Entity,
) -> usize {
    let index = pool.used;
    pool.used += 1;
    if index == pool.entities.len() {
        let entity = spawn(commands);
        commands
            .entity(entity)
            .insert((transform, Visibility::Visible));
        pool.entities.push(entity);
    } else if let Ok((mut t, mut v)) = transforms.get_mut(pool.entities[index]) {
        *t = transform;
        *v = Visibility::Visible;
    }
    index
}

fn finish(pool: &mut Pool, transforms: &mut Query<(&mut Transform, &mut Visibility)>) {
    for &entity in &pool.entities[pool.used..] {
        if let Ok((_, mut v)) = transforms.get_mut(entity) {
            *v = Visibility::Hidden;
        }
    }
    pool.used = 0;
}

#[allow(clippy::too_many_arguments)]
fn sync(
    mut commands: Commands,
    state: NonSend<MagicState>,
    mut fx: Local<FxState>,
    cameras: Query<&GlobalTransform, With<crate::window::FlyCam>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut particle_materials: ResMut<Assets<ParticleMaterial>>,
    mut beam_materials: ResMut<Assets<BeamMaterial>>,
    mut additive: ResMut<Assets<AdditiveMaterial>>,
    mut standard: ResMut<Assets<StandardMaterial>>,
    mut decal_materials: ResMut<Assets<DecalMaterial>>,
    mut transforms: Query<(&mut Transform, &mut Visibility)>,
) {
    let fx = &mut *fx;
    let Some(wizard) = &state.world else { return };
    if fx.shared.is_none() && !fx.failed {
        match setup(
            &mut commands,
            &wizard.content,
            &mut images,
            &mut meshes,
            &mut particle_materials,
        ) {
            Ok(shared) => fx.shared = Some(shared),
            Err(error) => {
                eprintln!("MAGICKA fx: {error}");
                fx.failed = true;
            }
        }
    }
    let Some(shared) = &fx.shared else { return };
    let session = &wizard.session;
    let world = &session.world;
    let eye = cameras
        .iter()
        .next()
        .map_or([0., 0., 0.], |t| t.translation().to_array());

    if std::env::var_os("MAGICKA_TRACE").is_some() {
        println!(
            "MAGICKA FX particles={} beams={} bolts={} shields={} missiles={} barriers={:?} mines={} arcane_blasts={} beam_blasts={}",
            session.effects.particles.active().len(),
            session.beams.len(),
            session.bolts.len(),
            world.shields.len(),
            world.missiles.len(),
            world.barriers.slots.iter().flatten().map(|b| (b.solid(), b.dead, b.position, b.earth_clip.time)).collect::<Vec<_>>(),
            world.mines.slots.iter().flatten().count(),
            session.arcane_blasts.len(),
            session.beam_blasts.len(),
        );
        for m in world.missiles.iter().take(3) {
            println!("MAGICKA MISSILE pos={:?} vel={:?} model={:?}", m.body.position, m.body.velocity, m.body.model);
        }
    }
    // Original particle effects: sprays, impacts, status emitters, chants.
    if let Some(mut mesh) = meshes.get_mut(&shared.particles) {
        *mesh = particle_mesh(session.effects.particles.active());
    }

    // Railgun beams (arcane/life beams and their combinations).
    // Rails carrying additional elements draw only their element billboards
    // (Railgun AdditionalRenderData), below.
    let draws: Vec<_> = session
        .beams
        .iter()
        .filter(|r| r.additional == Elements::NONE)
        .flat_map(beam_mesh::rail_draws)
        .collect();
    for (i, d) in draws.iter().enumerate() {
        let vertices = beam_mesh::strip(d, eye);
        let positions: Vec<[f32; 3]> = vertices.iter().map(|v| v.position).collect();
        let uvs: Vec<[f32; 2]> = vertices.iter().map(|v| v.uv).collect();
        let colors: Vec<[f32; 4]> = vertices
            .iter()
            .map(|v| [v.alpha, v.alpha, v.alpha, 1.0])
            .collect();
        let center = Vec4::new(d.color_center[0], d.color_center[1], d.color_center[2], 1.);
        let edge = Vec4::new(d.color_edge[0], d.color_edge[1], d.color_edge[2], 1.);
        if i == fx.beams.len() {
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
            );
            mesh.insert_indices(Indices::U32(beam_mesh::strip_triangles()));
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_NORMAL,
                vec![[0.0f32, 1.0, 0.0]; beam_mesh::VERTEX_COUNT],
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
            let mesh = meshes.add(mesh);
            let material = beam_materials.add(BeamMaterial {
                color_center: center,
                color_edge: edge,
                texture: shared.beam_texture.clone(),
            });
            let entity = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    Visibility::Visible,
                    NotShadowCaster,
                    NoFrustumCulling,
                ))
                .id();
            fx.beams.push((entity, mesh, material));
            continue;
        }
        let (entity, mesh, material) = fx.beams[i].clone();
        if let Some(mut m) = meshes.get_mut(&mesh) {
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            m.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            m.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
        }
        if let Some(mut m) = beam_materials.get_mut(&material) {
            m.color_center = center;
            m.color_edge = edge;
        }
        if let Ok((_, mut v)) = transforms.get_mut(entity) {
            *v = Visibility::Visible;
        }
    }
    for (entity, _, _) in fx.beams.iter().skip(draws.len()) {
        if let Ok((_, mut v)) = transforms.get_mut(*entity) {
            *v = Visibility::Hidden;
        }
    }

    // Lightning bolts: one ribbon per live bolt, fading with sqrt(10·ttl).
    fx.bolts.retain(|(serial, entity, material)| {
        match session.bolts.iter().find(|b| b.serial == *serial) {
            Some(b) => {
                if let Some(mut m) = additive.get_mut(material) {
                    m.color.w = (b.ttl * 10.0).sqrt();
                }
                true
            }
            None => {
                commands.entity(*entity).despawn();
                false
            }
        }
    });
    for b in &session.bolts {
        if fx.bolts.iter().any(|(s, _, _)| *s == b.serial) {
            continue;
        }
        let v = b.visual;
        let strip = &shared.bolt_strips[b.serial as usize % shared.bolt_strips.len()];
        let tri = bolt_mesh::triangles(strip, v.from, v.to, eye, v.scale);
        if tri.is_empty() {
            continue;
        }
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            tri.iter().map(|t| t.position).collect::<Vec<_>>(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; tri.len()]);
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            tri.iter().map(|t| t.uv).collect::<Vec<_>>(),
        );
        mesh.insert_indices(Indices::U32((0..tri.len() as u32).collect()));
        let material = additive.add(AdditiveMaterial {
            color: Vec4::new(
                v.color[0] * 2.0,
                v.color[1] * 2.0,
                v.color[2] * 2.0,
                (b.ttl * 10.0).sqrt(),
            ),
            flags: UVec4::new(1, 0, 0, 0),
            texture: Some(shared.beam_texture.clone()),
        });
        let entity = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                NotShadowCaster,
                NoFrustumCulling,
            ))
            .id();
        fx.bolts.push((b.serial, entity, material));
    }

    // Glowing shapes: spell shields, elemental barriers, mines and the
    // wizard's self-shield ward.
    let mut glows: Vec<(Transform, [f32; 3], f32, bool)> = Vec::new();
    spell_shields(&mut commands, &mut fx.shields, world, eye, &mut meshes, &mut additive, &mut transforms);
    additional_beams(
        &mut commands,
        (&mut fx.additional_beams, &mut fx.additional_material),
        &shared.beam_texture,
        session,
        &cameras,
        &mut meshes,
        &mut additive,
        &mut transforms,
    );
    arcane_blasts(
        &mut commands,
        (&mut fx.arcane_blasts, &mut fx.arcane_disc),
        wizard,
        session,
        &mut images,
        &mut meshes,
        &mut additive,
        &mut transforms,
    );
    barrier_runes(&mut commands, (&mut fx.rune_assets, &mut fx.runes), wizard, world, &mut images, &mut meshes, &mut additive, &mut transforms);
    ground_decals(&mut commands, &mut fx.decals, wizard, world, &mut images, &mut meshes, &mut decal_materials, &mut transforms);
    // Earth and ice walls and spell mines: the original skinned models with
    // their emerge clips at the simulation's clip times (viewer sync_barriers
    // and sync_mines placement).
    let mut effect_draws: Vec<EffectDraw> = Vec::new();
    for barrier in world.barriers.slots.iter().flatten() {
        if barrier.dead {
            continue;
        }
        if barrier.solid() {
            use p2magicka::magicka_sim::barrier::{EarthModel, IceModel};
            let world_matrix = xna_world(
                Vec3::from(barrier.position) - Vec3::Y * 1.3,
                Vec3::from(barrier.direction),
                None,
                barrier.scale,
            );
            if let Some(model) = barrier.ice_model.filter(|_| barrier.draw_ice) {
                let (mesh, clips) = match model {
                    IceModel::Plain => ("iceBarrier0_mesh", Some("iceBarrier_animation")),
                    IceModel::Geyser => ("icebarrier_geyser", None),
                    IceModel::Tesla => ("icebarrier_tesla", None),
                    IceModel::Earth => ("iceBarrier_earth", None),
                };
                effect_draws.push(EffectDraw {
                    mesh,
                    clips,
                    time: barrier.ice_clip.time,
                    world: world_matrix,
                    tint: [1.; 3],
                    emissive: 0.,
                });
            }
            if let Some(model) = barrier.earth_model {
                let (mesh, clips) = match model {
                    EarthModel::Plain => ("earthBarrier0_mesh", Some("earthBarrier_animation")),
                    EarthModel::Volcano => ("earthBarrier_volcano", None),
                };
                effect_draws.push(EffectDraw {
                    mesh,
                    clips,
                    time: barrier.earth_clip.time,
                    world: world_matrix,
                    tint: [1.; 3],
                    emissive: 0.,
                });
            }
            continue;
        }
        let p = Vec3::from(barrier.position);
        let r = barrier.radius.max(0.3) * barrier.scale.max(0.3);
        glows.push((
            Transform::from_translation(p + Vec3::Y * r).with_scale(Vec3::new(r, r * 2.0, r)),
            element_color(barrier.spell.element),
            0.35,
            true,
        ));
    }
    for mine in world.mines.slots.iter().flatten() {
        if mine.dead {
            continue;
        }
        effect_draws.push(EffectDraw {
            mesh: "mine",
            clips: None,
            time: mine.clip_time,
            world: xna_world(
                Vec3::from(mine.position) - Vec3::Y * 0.1,
                Vec3::NEG_Z,
                Some(mine.model_yaw),
                p2magicka::magicka_sim::spell_mine::MODEL_SCALE,
            ),
            tint: p2magicka::magicka_sim::lightning::spell_color(&mine.spell),
            // SpellMine.RenderData.Draw: EmissiveAmount 2.
            emissive: 2.,
        });
    }
    let avatar = wizard.avatar();
    let ward = &avatar.character.combat.health.shield;
    if ward.health > 0. && state.classic {
        glows.push((
            Transform::from_translation(Vec3::from(wizard.body().position))
                .with_scale(Vec3::splat(1.1)),
            [1.0, 0.85, 0.45],
            0.12,
            false,
        ));
    }
    for (i, (transform, color, intensity, flat)) in glows.into_iter().enumerate() {
        let tint = Vec4::new(
            color[0] * intensity,
            color[1] * intensity,
            color[2] * intensity,
            1.,
        );
        if i == fx.glow_materials.len() {
            fx.glow_materials.push(additive.add(AdditiveMaterial {
                color: tint,
                flags: UVec4::ZERO,
                texture: None,
            }));
        } else if let Some(mut m) = additive.get_mut(&fx.glow_materials[i]) {
            m.color = tint;
        }
        let material = fx.glow_materials[i].clone();
        let mesh = if flat {
            shared.cylinder.clone()
        } else {
            shared.sphere.clone()
        };
        let index = place(
            &mut commands,
            &mut fx.glows,
            &mut transforms,
            transform,
            |c| c.spawn((Mesh3d(mesh.clone()), NotShadowCaster)).id(),
        );
        let entity = fx.glows.entities[index];
        commands
            .entity(entity)
            .insert((Mesh3d(mesh), MeshMaterial3d(material)));
    }
    finish(&mut fx.glows, &mut transforms);

    // The original models encode magnitude/recipe size. Collider radius is
    // not a render scale; model-less missiles are drawn by their native effects.
    for missile in &world.missiles {
        let body = &missile.body;
        if body.dead {
            continue;
        }
        let asset = match (body.model, &body.item_model) {
            (Some(model), _) => format!(
                "Models/Missiles/{}",
                p2magicka::magicka_runtime::missiles::model_name(model)
            ),
            (None, Some(item)) => item.clone(),
            (None, None) => continue,
        };
        if !fx.missile_models.contains_key(&asset) {
            let parts = match p2magicka::visual::model_parts(&wizard.content, &asset) {
                Ok(parts) => parts,
                Err(error) => {
                    eprintln!("MAGICKA missile model: {error}");
                    Vec::new()
                }
            };
            let mut handles = Vec::new();
            for part in parts {
                let texture = part.texture.map(|t| {
                    images.add(Image::new(
                        Extent3d {
                            width: t.width,
                            height: t.height,
                            depth_or_array_layers: 1,
                        },
                        TextureDimension::D2,
                        t.rgba,
                        TextureFormat::Rgba8UnormSrgb,
                        RenderAssetUsages::default(),
                    ))
                });
                let mut mesh = Mesh::new(
                    PrimitiveTopology::TriangleList,
                    RenderAssetUsages::default(),
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.geometry.positions);
                if let Some(uv) = part.geometry.uvs {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
                }
                mesh.insert_indices(Indices::U32(part.geometry.indices));
                mesh.compute_smooth_normals();
                let material = standard.add(StandardMaterial {
                    base_color: Color::srgb(part.color[0], part.color[1], part.color[2]),
                    base_color_texture: texture,
                    alpha_mode: if part.cutout {
                        AlphaMode::Mask(0.5)
                    } else {
                        AlphaMode::Opaque
                    },
                    cull_mode: None,
                    perceptual_roughness: 0.8,
                    ..default()
                });
                handles.push((meshes.add(mesh), material));
            }
            fx.missile_models.insert(asset.clone(), handles);
        }
        let [x, y, z, w] = body.render_orientation().to_quaternion();
        let transform = Transform {
            translation: Vec3::from(body.position),
            rotation: Quat::from_xyzw(x, y, z, w),
            ..default()
        };
        for (mesh, material) in &fx.missile_models[&asset] {
            let index = place(
                &mut commands,
                &mut fx.missiles,
                &mut transforms,
                transform,
                |c| c.spawn(NoFrustumCulling).id(),
            );
            commands
                .entity(fx.missiles.entities[index])
                .insert((Mesh3d(mesh.clone()), MeshMaterial3d(material.clone())));
        }
    }
    finish(&mut fx.missiles, &mut transforms);
    let mut used = 0;
    for draw in effect_draws {
        // Spell colours are HDR; the skinned fabric tint (SkinnedModelDeferred
        // TintColor) keeps their hue at unit peak.
        let peak = draw.tint.iter().copied().fold(1f32, f32::max);
        let tint = draw.tint.map(|c| c / peak);
        let key = (draw.mesh, tint.map(|c| (c * 255.).round() as u8));
        let loaded = fx.effect_models.entry(key).or_insert_with(|| {
            match p2magicka::visual::skinned_effect(&wizard.content, draw.mesh, draw.clips, tint) {
                Ok(mut effect) => {
                    let images = effect
                        .parts
                        .iter_mut()
                        .map(|part| {
                            part.texture.take().map(|t| {
                                images.add(Image::new(
                                    Extent3d {
                                        width: t.width,
                                        height: t.height,
                                        depth_or_array_layers: 1,
                                    },
                                    TextureDimension::D2,
                                    t.rgba,
                                    TextureFormat::Rgba8UnormSrgb,
                                    RenderAssetUsages::default(),
                                ))
                            })
                        })
                        .collect();
                    Some(EffectModel { effect, images })
                }
                Err(error) => {
                    eprintln!("MAGICKA effect model: {error}");
                    None
                }
            }
        });
        let Some(model) = loaded.as_ref() else {
            continue;
        };
        let posed = model.effect.pose(draw.time, &draw.world);
        if used < fx.animated.len() && fx.animated[used].key != key {
            for (entity, ..) in fx.animated.remove(used).parts {
                commands.entity(entity).despawn();
            }
        }
        if used == fx.animated.len() || fx.animated[used].key != key {
            let parts = model
                .effect
                .parts
                .iter()
                .zip(&model.images)
                .map(|(part, image)| {
                    let mut mesh = Mesh::new(
                        PrimitiveTopology::TriangleList,
                        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
                    );
                    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.geometry.positions.clone());
                    if let Some(uv) = &part.geometry.uvs {
                        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv.clone());
                    }
                    mesh.insert_indices(Indices::U32(part.geometry.indices.clone()));
                    let mesh = meshes.add(mesh);
                    let material = standard.add(StandardMaterial {
                        base_color_texture: image.clone(),
                        alpha_mode: if part.cutout {
                            AlphaMode::Mask(0.5)
                        } else {
                            AlphaMode::Opaque
                        },
                        cull_mode: None,
                        perceptual_roughness: 0.8,
                        ..default()
                    });
                    let entity = commands
                        .spawn((
                            Mesh3d(mesh.clone()),
                            MeshMaterial3d(material.clone()),
                            Transform::IDENTITY,
                            NoFrustumCulling,
                        ))
                        .id();
                    (entity, mesh, material)
                })
                .collect();
            fx.animated.insert(
                used,
                Animated {
                    key,
                    parts,
                },
            );
        }
        for ((entity, mesh, material), positions) in fx.animated[used].parts.iter().zip(posed) {
            if let Ok((_, mut v)) = transforms.get_mut(*entity) {
                *v = Visibility::Visible;
            }
            if let Some(mut mesh) = meshes.get_mut(mesh) {
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
                mesh.compute_smooth_normals();
            }
            if let Some(mut m) = standard.get_mut(material) {
                // EmissiveAmount scales the material's own colour.
                m.emissive = LinearRgba::WHITE * draw.emissive * glow_scale();
                m.emissive_texture = m.base_color_texture.clone();
            }
        }
        used += 1;
    }
    for animated in fx.animated.drain(used..) {
        for (entity, ..) in animated.parts {
            commands.entity(entity).despawn();
        }
    }
}

/// Additive effect geometry on an identity transform, as the original draws it.
fn additive_mesh(
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
) -> Mesh {
    let normals = vec![[0.0f32, 1.0, 0.0]; positions.len()];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_indices(Indices::U32(indices));
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh
}

/// Spherical shell up to `max_angle` from -Z (viewer `shield_mesh`).
fn shield_mesh(max_angle: f32) -> Mesh {
    let (rings, segments) = (16u32, 48u32);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    for r in 0..=rings {
        let theta = max_angle * r as f32 / rings as f32;
        for k in 0..=segments {
            let phi = std::f32::consts::TAU * k as f32 / segments as f32;
            let p = [theta.sin() * phi.cos(), theta.sin() * phi.sin(), -theta.cos()];
            positions.push(p);
            normals.push(p);
            uvs.push([k as f32 / segments as f32, r as f32 / rings as f32]);
        }
    }
    let mut indices = Vec::new();
    let row = segments + 1;
    for r in 0..rings {
        for k in 0..segments {
            let (a, b, c, d) = (r * row + k, r * row + k + 1, (r + 1) * row + k, (r + 1) * row + k + 1);
            indices.extend([a, c, b, b, c, d, a, b, c, b, d, c]);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Spell shields: the sphere, the forward disc (a quarter-angle cap) and the
/// Shield wall's own mesh, as additive shells tinted by the shield colour and
/// fade with a view-dependent rim (viewer `sync_shields`; ShieldEffect's
/// noise/refraction is not ported there either).
#[allow(clippy::too_many_arguments)]
fn spell_shields(
    commands: &mut Commands,
    draws: &mut std::collections::HashMap<u16, (Entity, Handle<AdditiveMaterial>, Handle<Mesh>)>,
    world: &p2magicka::magicka_runtime::scene_world::SceneWorld<
        p2magicka::magicka_runtime::play_content::FsSceneContent,
    >,
    eye: [f32; 3],
    meshes: &mut Assets<Mesh>,
    additive: &mut Assets<AdditiveMaterial>,
    transforms: &mut Query<(&mut Transform, &mut Visibility)>,
) {
    use p2magicka::magicka_sim::shield_entity::ShieldKind;
    let live: std::collections::HashSet<u16> = world.shields.iter().map(|s| s.handle).collect();
    draws.retain(|handle, (entity, ..)| {
        let keep = live.contains(handle);
        if !keep {
            commands.entity(*entity).despawn();
        }
        keep
    });
    for s in &world.shields {
        let shield = &s.shield;
        let alpha = shield.tint[3].clamp(0.0, 1.0) * std::env::var("SHIELD_A").ok().and_then(|v| v.parse().ok()).unwrap_or(0.35f32);
        let color = Vec4::new(shield.tint[0] * alpha, shield.tint[1] * alpha, shield.tint[2] * alpha, 1.0);
        let forward = Vec3::from(shield.forward).normalize_or(Vec3::NEG_Z);
        let transform = Transform::from_translation(Vec3::from(shield.position))
            .looking_to(forward, Vec3::Y)
            .with_scale(Vec3::splat(if shield.kind == ShieldKind::Wall { 1.0 } else { shield.radius }));
        if let std::collections::hash_map::Entry::Vacant(slot) = draws.entry(s.handle) {
            let material = additive.add(AdditiveMaterial {
                color,
                flags: UVec4::new(0, 1, 0, 0),
                texture: None,
            });
            let mesh = if shield.kind == ShieldKind::Wall {
                let wall = p2magicka::magicka_sim::shield_wall::mesh();
                let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, wall.positions);
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, wall.normals);
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, wall.uv);
                mesh.insert_indices(Indices::U32(wall.indices));
                meshes.add(mesh)
            } else {
                meshes.add(shield_mesh(if shield.kind == ShieldKind::Disc {
                    std::f32::consts::FRAC_PI_4
                } else {
                    std::f32::consts::PI
                }))
            };
            let entity = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    transform,
                    Visibility::Visible,
                    NotShadowCaster,
                    NoFrustumCulling,
                ))
                .id();
            slot.insert((entity, material, mesh));
        }
        let (entity, material, mesh) = draws[&s.handle].clone();
        if let Some(mut m) = additive.get_mut(&material) {
            m.color = color;
        }
        if let Ok((mut t, mut v)) = transforms.get_mut(entity) {
            *t = transform;
            *v = Visibility::Visible;
        }
        // Rim: brighter where the shell is seen edge-on.
        let view = (transform.translation - Vec3::from(eye)).normalize_or(Vec3::NEG_Z);
        if let Some(mut m) = meshes.get_mut(&mesh) {
            let rim: Option<Vec<[f32; 4]>> = m
                .attribute(Mesh::ATTRIBUTE_NORMAL)
                .and_then(|a| a.as_float3())
                .map(|normals| {
                    normals
                        .iter()
                        .map(|n| {
                            let f = 1.0 - (transform.rotation * Vec3::from(*n)).dot(view).abs();
                            let k = 0.3 + 2.5 * f * f * f;
                            [k, k, k, 1.0]
                        })
                        .collect()
                });
            if let Some(rim) = rim {
                m.insert_attribute(Mesh::ATTRIBUTE_COLOR, rim);
            }
        }
    }
}

/// Railgun AdditionalRenderData: per additional element two textured
/// billboards over EffectTextures/Beams tinted by the element (viewer
/// `sync_additional_beams`).
#[allow(clippy::too_many_arguments)]
fn additional_beams(
    commands: &mut Commands,
    (pool, material): (&mut Vec<(Entity, Handle<Mesh>)>, &mut Option<Handle<AdditiveMaterial>>),
    beam_texture: &Handle<Image>,
    session: &p2magicka::magicka_runtime::play_session::PlaySession,
    cameras: &Query<&GlobalTransform, With<crate::window::FlyCam>>,
    meshes: &mut Assets<Mesh>,
    additive: &mut Assets<AdditiveMaterial>,
    transforms: &mut Query<(&mut Transform, &mut Visibility)>,
) {
    let (eye, forward) = cameras.iter().next().map_or(([0.0; 3], [0.0, 0.0, -1.0]), |t| {
        (t.translation().to_array(), t.forward().to_array())
    });
    let material = material
        .get_or_insert_with(|| {
            additive.add(AdditiveMaterial {
                color: Vec4::ONE,
                flags: UVec4::new(1, 1, 0, 0),
                texture: Some(beam_texture.clone()),
            })
        })
        .clone();
    let rails: Vec<_> = session
        .beams
        .iter()
        .filter(|r| r.additional != Elements::NONE)
        .map(|r| beam_mesh::additional_quads(r, eye, forward))
        .collect();
    for (i, quads) in rails.iter().enumerate() {
        let (mut positions, mut uvs, mut colors, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for q in quads {
            let base = positions.len() as u32;
            positions.extend(q.positions);
            uvs.extend(q.uvs);
            colors.extend([[q.tint[0], q.tint[1], q.tint[2], 1.0]; 4]);
            indices.extend([0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2].map(|k| base + k));
        }
        let mesh = additive_mesh(positions, uvs, colors, indices);
        if i == pool.len() {
            let handle = meshes.add(mesh);
            let entity = commands
                .spawn((
                    Mesh3d(handle.clone()),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    Visibility::Visible,
                    NotShadowCaster,
                    NoFrustumCulling,
                ))
                .id();
            pool.push((entity, handle));
            continue;
        }
        let (entity, handle) = pool[i].clone();
        if let Some(mut m) = meshes.get_mut(&handle) {
            *m = mesh;
        }
        if let Ok((_, mut v)) = transforms.get_mut(entity) {
            *v = Visibility::Visible;
        }
    }
    for (entity, _) in pool.iter().skip(rails.len()) {
        if let Ok((_, mut v)) = transforms.get_mut(*entity) {
            *v = Visibility::Hidden;
        }
    }
}

/// ArcaneBlast RenderData: every live blast's EffectTextures/ArcaneDisc discs
/// in one additive mesh (viewer `sync_arcane_blasts`).
#[allow(clippy::too_many_arguments)]
fn arcane_blasts(
    commands: &mut Commands,
    (draw, disc): (&mut Option<(Entity, Handle<Mesh>)>, &mut Option<Option<Handle<Image>>>),
    wizard: &p2magicka::WizardWorld,
    session: &p2magicka::magicka_runtime::play_session::PlaySession,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    additive: &mut Assets<AdditiveMaterial>,
    transforms: &mut Query<(&mut Transform, &mut Visibility)>,
) {
    let (mut positions, mut uvs, mut colors, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for blast in &session.arcane_blasts {
        for disc in blast.draws() {
            let base = positions.len() as u32;
            positions.extend(disc.positions);
            uvs.extend(disc.uvs);
            colors.extend([[1.0, 1.0, 1.0, disc.alpha]; 4]);
            indices.extend([0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2].map(|k| base + k));
        }
    }
    if positions.is_empty() {
        if let Some((entity, _)) = &*draw {
            if let Ok((_, mut v)) = transforms.get_mut(*entity) {
                *v = Visibility::Hidden;
            }
        }
        return;
    }
    let mesh = additive_mesh(positions, uvs, colors, indices);
    if let Some((entity, handle)) = &*draw {
        if let Some(mut m) = meshes.get_mut(handle) {
            *m = mesh;
        }
        if let Ok((_, mut v)) = transforms.get_mut(*entity) {
            *v = Visibility::Visible;
        }
        return;
    }
    let texture = disc
        .get_or_insert_with(|| {
            match p2magicka::visual::texture2d(&wizard.content, "EffectTextures/ArcaneDisc") {
                Ok(t) => Some(images.add(Image::new(
                    Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
                    TextureDimension::D2,
                    t.rgba,
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::default(),
                ))),
                Err(error) => {
                    eprintln!("MAGICKA arcane disc: {error}");
                    None
                }
            }
        })
        .clone();
    let handle = meshes.add(mesh);
    let material = additive.add(AdditiveMaterial {
        color: Vec4::ONE,
        flags: UVec4::new(texture.is_some() as u32, 1, 0, 0),
        texture,
    });
    let entity = commands
        .spawn((
            Mesh3d(handle.clone()),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            Visibility::Visible,
            NotShadowCaster,
            NoFrustumCulling,
        ))
        .id();
    *draw = Some((entity, handle));
}

/// Solid Arcane/Life barriers' rune discs: runesArcane/runesLife tinted by
/// the spell colour, spinning with the rune rotation below the wall (viewer
/// `sync_barrier_runes`).
#[allow(clippy::too_many_arguments)]
fn barrier_runes(
    commands: &mut Commands,
    (assets, pool): (
        &mut Option<Option<([Handle<Mesh>; 2], Handle<Image>)>>,
        &mut Vec<(Entity, Handle<AdditiveMaterial>)>,
    ),
    wizard: &p2magicka::WizardWorld,
    world: &p2magicka::magicka_runtime::scene_world::SceneWorld<
        p2magicka::magicka_runtime::play_content::FsSceneContent,
    >,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    additive: &mut Assets<AdditiveMaterial>,
    transforms: &mut Query<(&mut Transform, &mut Visibility)>,
) {
    use p2magicka::magicka_sim::barrier::Rune;
    let draws: Vec<_> = world
        .barriers
        .slots
        .iter()
        .flatten()
        .filter(|b| b.solid() && !b.dead)
        .filter_map(|b| {
            let (rune, color) = b.rune()?;
            let (yaw, at) = b.rune_placement();
            Some((rune, color, Transform::from_translation(Vec3::from(at)).with_rotation(Quat::from_rotation_y(yaw))))
        })
        .collect();
    if draws.is_empty() && pool.is_empty() {
        return;
    }
    let loaded = assets.get_or_insert_with(|| {
        let content = &wizard.content;
        let mesh = |name: &str| -> Result<Mesh, String> {
            let parts = p2magicka::visual::model_parts(content, &format!("Models/Effects/{name}"))?;
            let part = parts.into_iter().next().ok_or(format!("{name}: empty"))?;
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.geometry.positions);
            if let Some(uv) = part.geometry.uvs {
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
            }
            mesh.insert_indices(Indices::U32(part.geometry.indices));
            Ok(mesh)
        };
        let result = (|| {
            let texture = p2magicka::visual::texture2d(content, "Models/Effects/runes00_0")?;
            let image = images.add(Image::new(
                Extent3d { width: texture.width, height: texture.height, depth_or_array_layers: 1 },
                TextureDimension::D2,
                texture.rgba,
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            ));
            Ok::<_, String>(([meshes.add(mesh("runesArcane")?), meshes.add(mesh("runesLife")?)], image))
        })();
        result.map_err(|e| eprintln!("MAGICKA runes: {e}")).ok()
    });
    let Some((rune_meshes, image)) = loaded.as_ref() else { return };
    for (i, (rune, color, transform)) in draws.iter().enumerate() {
        let mesh = rune_meshes[if *rune == Rune::Arcane { 0 } else { 1 }].clone();
        let tint = Vec4::new(color[0], color[1], color[2], 1.0);
        if i == pool.len() {
            let material = additive.add(AdditiveMaterial { color: tint, flags: UVec4::new(1, 0, 0, 0), texture: Some(image.clone()) });
            let entity = commands
                .spawn((Mesh3d(mesh), MeshMaterial3d(material.clone()), *transform, Visibility::Visible, NotShadowCaster, NoFrustumCulling))
                .id();
            pool.push((entity, material));
            continue;
        }
        let (entity, material) = pool[i].clone();
        if let Some(mut m) = additive.get_mut(&material) {
            m.color = tint;
        }
        commands.entity(entity).insert(Mesh3d(mesh));
        if let Ok((mut t, mut v)) = transforms.get_mut(entity) {
            *t = *transform;
            *v = Visibility::Visible;
        }
    }
    for (entity, _) in pool.iter().skip(draws.len()) {
        if let Ok((_, mut v)) = transforms.get_mut(*entity) {
            *v = Visibility::Hidden;
        }
    }
}

/// Magicka ground projections (spell scorch marks, shield/element runes,
/// staff effects): the sim's decal frame projected onto scene depth with the
/// original `EffectTextures/decals` atlas (viewer `decals.rs`).
#[allow(clippy::too_many_arguments)]
fn ground_decals(
    commands: &mut Commands,
    state: &mut Option<Option<(Entity, Handle<Mesh>)>>,
    wizard: &p2magicka::WizardWorld,
    world: &p2magicka::magicka_runtime::scene_world::SceneWorld<
        p2magicka::magicka_runtime::play_content::FsSceneContent,
    >,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    decal_materials: &mut Assets<DecalMaterial>,
    transforms: &mut Query<(&mut Transform, &mut Visibility)>,
) {
    let list = &world.decals.frame;
    if list.is_empty() && state.as_ref().is_none_or(|s| s.is_none()) {
        return;
    }
    let (mut positions, mut ix, mut iy, mut iz, mut args, mut colors, mut indices) =
        (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
    for p in list.iter().filter(|p| p.args[2] > 0.0 && p.inverse.iter().all(|x| x.is_finite())) {
        let base = positions.len() as u32;
        let transform = Mat4::from_cols_array(&p.transform);
        let column = |c: usize| [p.inverse[c], p.inverse[c + 4], p.inverse[c + 8], p.inverse[c + 12]];
        for j in 0..8 {
            let v = Vec3::new(
                if j % 2 < 1 { -0.5 } else { 0.5 },
                if j % 4 < 2 { -0.5 } else { 0.5 },
                if j % 8 < 4 { -0.5 } else { 0.5 },
            );
            positions.push(transform.transform_point3(v).to_array());
            ix.push(column(0));
            iy.push(column(1));
            iz.push(column(2));
            args.push(p.args);
            colors.push(p.color);
        }
        indices.extend(
            [0, 2, 1, 1, 2, 3, 1, 0, 4, 4, 5, 1, 0, 2, 4, 4, 2, 6, 4, 6, 5, 5, 6, 7, 2, 3, 6, 6, 3, 7, 3, 1, 5, 5, 7, 3]
                .map(|i| base + i),
        );
    }
    if positions.is_empty() {
        positions.push([0.0; 3]);
        ix.push([0.0; 4]);
        iy.push([0.0; 4]);
        iz.push([0.0; 4]);
        args.push([0.0; 3]);
        colors.push([0.0; 4]);
        indices.extend([0, 0, 0]);
    }
    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(DECAL_INV_X, ix)
        .with_inserted_attribute(DECAL_INV_Y, iy)
        .with_inserted_attribute(DECAL_INV_Z, iz)
        .with_inserted_attribute(DECAL_ARGS, args)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices));
    let entry = state.get_or_insert_with(|| {
        let texture = match p2magicka::visual::texture2d(&wizard.content, "EffectTextures/decals") {
            Ok(t) => t,
            Err(error) => {
                eprintln!("MAGICKA decals: {error}");
                return None;
            }
        };
        // Sampled raw and converted in the shader, as the viewer does.
        let image = images.add(Image::new(
            Extent3d { width: texture.width, height: texture.height, depth_or_array_layers: 1 },
            TextureDimension::D2,
            texture.rgba,
            TextureFormat::Rgba8Unorm,
            RenderAssetUsages::default(),
        ));
        let material = decal_materials.add(DecalMaterial { atlas: image });
        let handle = meshes.add(Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default()));
        let entity = commands
            .spawn((Mesh3d(handle.clone()), MeshMaterial3d(material), Transform::IDENTITY, Visibility::Visible, NotShadowCaster, NoFrustumCulling))
            .id();
        Some((entity, handle))
    });
    let Some((entity, handle)) = entry.as_ref() else { return };
    if let Some(mut m) = meshes.get_mut(handle) {
        *m = mesh;
    }
    if let Ok((_, mut v)) = transforms.get_mut(*entity) {
        *v = if list.is_empty() { Visibility::Hidden } else { Visibility::Visible };
    }
}
