//! Magicka spell presentation inside Postal's renderer: the original particle
//! effects (sprays, impacts, statuses, chants), railgun beams and lightning
//! bolts with materials ported from the rewrite viewer, plus simple shapes for
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
fn effect_depth(descriptor: &mut RenderPipelineDescriptor, blend: BlendComponent) {
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

pub(crate) struct MagickaFxPlugin;
impl Plugin for MagickaFxPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "magicka_particles.wgsl");
        embedded_asset!(app, "magicka_beam.wgsl");
        embedded_asset!(app, "magicka_additive.wgsl");
        app.add_plugins((
            MaterialPlugin::<ParticleMaterial>::default(),
            MaterialPlugin::<BeamMaterial>::default(),
            MaterialPlugin::<AdditiveMaterial>::default(),
        ))
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
    rock: Handle<StandardMaterial>,
    ice: Handle<StandardMaterial>,
}

#[derive(Default)]
struct FxState {
    shared: Option<Shared>,
    failed: bool,
    beams: Vec<(Entity, Handle<Mesh>, Handle<BeamMaterial>)>,
    bolts: Vec<(u32, Entity, Handle<AdditiveMaterial>)>,
    glows: Pool,
    glow_materials: Vec<Handle<AdditiveMaterial>>,
    solids: Pool,
    missiles: Pool,
    missile_models:
        std::collections::HashMap<String, Vec<(Handle<Mesh>, Handle<StandardMaterial>)>>,
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
    standard: &mut Assets<StandardMaterial>,
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
        rock: standard.add(StandardMaterial {
            base_color: Color::srgb(0.45, 0.36, 0.27),
            perceptual_roughness: 0.95,
            ..default()
        }),
        ice: standard.add(StandardMaterial {
            base_color: Color::srgba(0.7, 0.9, 1.0, 0.8),
            emissive: LinearRgba::rgb(0.05, 0.12, 0.2),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.15,
            ..default()
        }),
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
            &mut standard,
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
            "MAGICKA FX particles={} beams={} bolts={} shields={} missiles={}",
            session.effects.particles.active().len(),
            session.beams.len(),
            session.bolts.len(),
            world.shields.len(),
            world.missiles.len()
        );
    }
    // Original particle effects: sprays, impacts, status emitters, chants.
    if let Some(mut mesh) = meshes.get_mut(&shared.particles) {
        *mesh = particle_mesh(session.effects.particles.active());
    }

    // Railgun beams (arcane/life beams and their combinations).
    let draws: Vec<_> = session
        .beams
        .iter()
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
    for s in &world.shields {
        let shield = &s.shield;
        let alpha = shield.tint[3].clamp(0.0, 1.0);
        let forward = Vec3::from(shield.forward).normalize_or(Vec3::NEG_Z);
        let scale = match shield.kind {
            p2magicka::magicka_sim::shield_entity::ShieldKind::Wall => {
                Vec3::new(shield.radius, shield.radius * 0.5, 0.15)
            }
            p2magicka::magicka_sim::shield_entity::ShieldKind::Disc => {
                Vec3::new(shield.radius, shield.radius, 0.1)
            }
            _ => Vec3::splat(shield.radius),
        };
        glows.push((
            Transform::from_translation(Vec3::from(shield.position))
                .looking_to(forward, Vec3::Y)
                .with_scale(scale),
            [shield.tint[0], shield.tint[1], shield.tint[2]],
            0.18 * alpha,
            false,
        ));
    }
    let mut solids: Vec<(Transform, bool)> = Vec::new();
    for barrier in world.barriers.slots.iter().flatten() {
        if barrier.dead {
            continue;
        }
        let p = Vec3::from(barrier.position);
        let r = barrier.radius.max(0.3) * barrier.scale.max(0.3);
        let element = barrier.spell.element;
        let earth = element.0 & Elements::EARTH.0 != 0;
        let ice = element.0 & Elements::ICE.0 != 0 || barrier.draw_ice;
        if earth || ice {
            solids.push((
                Transform::from_translation(p + Vec3::Y * r).with_scale(Vec3::new(
                    r * 0.7,
                    r * 2.0,
                    r * 0.7,
                )),
                ice && !earth,
            ));
        }
        if barrier.kind == p2magicka::magicka_sim::barrier::BarrierKind::Elemental || !earth {
            glows.push((
                Transform::from_translation(p + Vec3::Y * r).with_scale(Vec3::new(r, r * 2.0, r)),
                element_color(element),
                0.35,
                true,
            ));
        }
    }
    for mine in world.mines.slots.iter().flatten() {
        glows.push((
            Transform::from_translation(Vec3::from(mine.position) + Vec3::Y * 0.05).with_scale(
                Vec3::new(0.4 * mine.scale.max(0.5), 1., 0.4 * mine.scale.max(0.5)),
            ),
            element_color(mine.spell.element),
            if mine.activated { 0.9 } else { 0.5 },
            true,
        ));
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
    for (transform, ice) in solids {
        let material = if ice {
            shared.ice.clone()
        } else {
            shared.rock.clone()
        };
        let mesh = shared.sphere.clone();
        let index = place(
            &mut commands,
            &mut fx.solids,
            &mut transforms,
            transform,
            |c| c.spawn(Mesh3d(mesh.clone())).id(),
        );
        let entity = fx.solids.entities[index];
        commands.entity(entity).insert(MeshMaterial3d(material));
    }
    finish(&mut fx.solids, &mut transforms);
}
