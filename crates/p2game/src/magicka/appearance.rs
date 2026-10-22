//! Native Magicka ethereal and invisible character passes on hosted meshes.
use super::MagicState;
use bevy::{asset::{embedded_asset, embedded_path, AssetPath}, prelude::*, render::render_resource::AsBindGroup, shader::ShaderRef};
use std::collections::{HashMap, HashSet};

#[derive(Component, Clone, Copy)]
pub(super) enum CharacterPart {
    Body(u16),
    Item(u16),
    DeathBody,
    DeathScythe,
}

#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(super) struct EtherealMaterial {
    #[uniform(0)]
    color: Vec4,
    #[texture(1)]
    #[sampler(2)]
    texture: Option<Handle<Image>>,
}
impl Material for EtherealMaterial {
    fn fragment_shader() -> ShaderRef { shader("magicka_ethereal.wgsl") }
    fn alpha_mode(&self) -> AlphaMode { AlphaMode::Add }
    fn enable_prepass() -> bool { false }
    fn enable_shadows() -> bool { false }
    fn specialize(
        _: &bevy::pbr::MaterialPipeline,
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        _: &bevy::mesh::MeshVertexBufferLayoutRef,
        _: bevy::pbr::MaterialPipelineKey<Self>,
    ) -> Result<(), bevy::render::render_resource::SpecializedMeshPipelineError> {
        use bevy::render::render_resource::{BlendComponent, BlendFactor, BlendOperation};
        crate::magicka_fx::effect_depth(descriptor, BlendComponent {
            src_factor: BlendFactor::OneMinusDst, dst_factor: BlendFactor::One, operation: BlendOperation::Add,
        });
        Ok(())
    }
}
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug)]
pub(super) struct InvisibilityMaterial {
    #[uniform(0)]
    params: Vec4,
}
impl Material for InvisibilityMaterial {
    fn fragment_shader() -> ShaderRef { shader("magicka_invisibility.wgsl") }
    fn reads_view_transmission_texture(&self) -> bool { true }
    fn enable_prepass() -> bool { false }
    fn enable_shadows() -> bool { false }
}
fn shader(name: &str) -> ShaderRef {
    let path = if name == "magicka_ethereal.wgsl" {
        embedded_path!("magicka_ethereal.wgsl")
    } else { embedded_path!("magicka_invisibility.wgsl") };
    ShaderRef::Path(AssetPath::from_path_buf(path).with_source("embedded"))
}
pub(super) fn install(app: &mut App) {
    embedded_asset!(app, "magicka_ethereal.wgsl");
    embedded_asset!(app, "magicka_invisibility.wgsl");
    app.add_plugins((MaterialPlugin::<EtherealMaterial>::default(), MaterialPlugin::<InvisibilityMaterial>::default()))
        .add_systems(Update, sync.after(super::visuals).after(super::creatures::draw).after(super::looks::apply));
}

#[derive(Default)]
pub(super) struct Appearance {
    originals: HashMap<Entity, Handle<StandardMaterial>>,
    ethereal: HashMap<Entity, Handle<EtherealMaterial>>,
    invisible: Option<Handle<InvisibilityMaterial>>,
    hidden_items: HashSet<Entity>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode { Normal, Ethereal(f32), Invisible, Hidden }
fn mode(c: &p2magicka::magicka_runtime::character::RuntimeCharacter, body: bool) -> Mode {
    if c.is_invisible() {
        if body { Mode::Invisible } else { Mode::Hidden }
    } else if body && (c.ethereal || c.definition.looks_ethereal) {
        Mode::Ethereal(c.ethereal_alpha.clamp(0., 1.))
    } else { Mode::Normal }
}

#[allow(clippy::too_many_arguments)]
fn sync(
    mut commands: Commands,
    state: NonSend<MagicState>,
    sim: NonSend<crate::window::Sim>,
    parts: Query<(Entity, &CharacterPart)>,
    rigs: Query<&crate::rigs::VmRig>,
    sections: Query<&MeshMaterial3d<StandardMaterial>>,
    entities: Query<Entity>,
    mut visibility: Query<&mut Visibility>,
    standards: Res<Assets<StandardMaterial>>,
    mut ethereal: ResMut<Assets<EtherealMaterial>>,
    mut invisibility: ResMut<Assets<InvisibilityMaterial>>,
    mut appearance: Local<Appearance>,
) {
    let a = &mut *appearance;
    let mut wanted = HashMap::new();
    if let Some(wizard) = &state.world {
        let world = &wizard.session.world;
        for (entity, part) in &parts {
            let m = match *part {
                CharacterPart::Body(h) | CharacterPart::Item(h) => {
                    let c = world.players.iter().flatten().find(|p| p.handle == h).map(|p| &p.character)
                        .or_else(|| world.npcs.get(&h).map(|n| &n.character));
                    c.map(|c| mode(c, matches!(part, CharacterPart::Body(_)))).unwrap_or(Mode::Normal)
                }
                CharacterPart::DeathBody => world.death.frame.as_ref().zip(world.death.actor.as_ref())
                    .filter(|(f, _)| f.additive)
                    .map_or(Mode::Normal, |(_, actor)| Mode::Ethereal(actor.state.alpha.clamp(0., 1.))),
                CharacterPart::DeathScythe => Mode::Normal,
            };
            wanted.insert(entity, m);
        }
        // Native Postal bodies and their bone attachments share the mirror's
        // appearance, without changing script visibility or collision flags.
        let vm = &sim.game.world.vm;
        for rig in &rigs {
            let owner = vm.get_prop(rig.actor, "Base").and_then(uscript::vm::Value::as_obj)
                .filter(|h| state.mirrors.contains_key(h)).unwrap_or(rig.actor);
            let Some(mirror) = state.mirrors.get(&owner) else { continue };
            let Some(npc) = world.npcs.get(&mirror.handle) else { continue };
            for &section in &rig.sections { wanted.insert(section, mode(&npc.character, true)); }
        }
    }
    // Restore the exact material after effects end, including on map replacement.
    let ended: Vec<_> = a.originals.keys().copied()
        .filter(|e| matches!(wanted.get(e), None | Some(Mode::Normal) | Some(Mode::Hidden))).collect();
    for e in ended {
        let original = a.originals.remove(&e).unwrap();
        if entities.contains(e) {
            commands.entity(e).remove::<MeshMaterial3d<EtherealMaterial>>()
                .remove::<MeshMaterial3d<InvisibilityMaterial>>().insert(MeshMaterial3d(original));
        }
        if let Some(h) = a.ethereal.remove(&e) { ethereal.remove(&h); }
    }
    a.hidden_items.retain(|e| {
        if wanted.get(e) == Some(&Mode::Hidden) { return true; }
        if let Ok(mut v) = visibility.get_mut(*e) { *v = Visibility::Inherited; }
        false
    });
    for (e, m) in wanted {
        if m == Mode::Hidden {
            if let Ok(mut v) = visibility.get_mut(e) { *v = Visibility::Hidden; }
            a.hidden_items.insert(e);
            continue;
        }
        if m == Mode::Normal { continue; }
        if !a.originals.contains_key(&e) {
            let Ok(material) = sections.get(e) else { continue };
            a.originals.insert(e, material.0.clone());
        }
        let original = &a.originals[&e];
        match m {
            Mode::Ethereal(alpha) => {
                let Some(base) = standards.get(original) else { continue };
                let rgb = base.base_color.to_linear();
                let h = a.ethereal.entry(e).or_insert_with(|| ethereal.add(EtherealMaterial {
                    color: Vec4::new(rgb.red, rgb.green, rgb.blue, alpha), texture: base.base_color_texture.clone(),
                })).clone();
                if let Some(mut material) = ethereal.get_mut(&h) { material.color.w = alpha; }
                commands.entity(e).remove::<MeshMaterial3d<StandardMaterial>>()
                    .remove::<MeshMaterial3d<InvisibilityMaterial>>().insert(MeshMaterial3d(h));
            }
            Mode::Invisible => {
                let h = a.invisible.get_or_insert_with(|| invisibility.add(InvisibilityMaterial {
                    params: Vec4::new(-0.4, 0., 0., 0.),
                })).clone();
                commands.entity(e).remove::<MeshMaterial3d<StandardMaterial>>()
                    .remove::<MeshMaterial3d<EtherealMaterial>>().insert(MeshMaterial3d(h));
            }
            _ => {}
        }
    }
}
