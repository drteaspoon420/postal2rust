//! Magicka-only characters (summoned undead, Death, the phoenix, elementals,
//! slimes, polymorph results) drawn in Postal with their original skinned
//! models. Mirrored Postal pawns keep their own Postal bodies.
use super::MagicState;
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};

struct Model {
    parts: Vec<p2magicka::visual::Part>,
    materials: Vec<Handle<StandardMaterial>>,
}

#[derive(Default)]
pub(super) struct Creatures {
    /// Per template; None after a load error.
    models: HashMap<String, Option<Model>>,
    /// Per NPC handle: its template and one entity/mesh per part.
    drawn: HashMap<u16, (String, Vec<(Entity, Handle<Mesh>)>)>,
}

/// Pseudo-handles for the summoned Death's body and scythe in `drawn`.
const DEATH_BODY: u16 = u16::MAX;
const DEATH_SCYTHE: u16 = u16::MAX - 1;

fn load_model(
    parts: Result<Vec<p2magicka::visual::Part>, String>,
    name: &str,
    images: &mut Assets<Image>,
    materials: &mut Assets<StandardMaterial>,
) -> Option<Model> {
    match parts {
        Ok(mut parts) => {
            let materials = parts
                .iter_mut()
                .map(|part| {
                    let texture = part.texture.take().map(|t| {
                        images.add(Image::new(
                            bevy::render::render_resource::Extent3d {
                                width: t.width,
                                height: t.height,
                                depth_or_array_layers: 1,
                            },
                            bevy::render::render_resource::TextureDimension::D2,
                            t.rgba,
                            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                            bevy::asset::RenderAssetUsages::default(),
                        ))
                    });
                    materials.add(StandardMaterial {
                        base_color: Color::srgb(part.color[0], part.color[1], part.color[2]),
                        base_color_texture: texture,
                        perceptual_roughness: 0.85,
                        cull_mode: None,
                        alpha_mode: if part.cutout { AlphaMode::Mask(0.5) } else { AlphaMode::Opaque },
                        ..default()
                    })
                })
                .collect();
            println!("MAGICKA creature model {name}: {} parts", parts.len());
            Some(Model { parts, materials })
        }
        Err(error) => {
            eprintln!("MAGICKA creature {name}: {error}");
            None
        }
    }
}

/// Spawn (once) and update one drawn character's part meshes.
#[allow(clippy::too_many_arguments)]
fn place(
    commands: &mut Commands,
    drawn: &mut HashMap<u16, (String, Vec<(Entity, Handle<Mesh>)>)>,
    meshes: &mut Assets<Mesh>,
    handle: u16,
    key: &str,
    model: &Model,
    appearance: super::appearance::CharacterPart,
    positions: impl Fn(&p2magicka::visual::Part) -> Vec<[f32; 3]>,
) {
    if drawn.get(&handle).is_some_and(|(t, _)| t != key) {
        for (entity, _) in drawn.remove(&handle).unwrap().1 {
            commands.entity(entity).despawn();
        }
    }
    let parts = drawn.entry(handle).or_insert_with(|| {
        let parts = model
            .parts
            .iter()
            .zip(&model.materials)
            .map(|(part, material)| {
                let mut mesh = Mesh::new(
                    bevy::mesh::PrimitiveTopology::TriangleList,
                    bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.geometry.positions.clone());
                if let Some(uv) = &part.geometry.uvs {
                    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv.clone());
                }
                mesh.insert_indices(bevy::mesh::Indices::U32(part.geometry.indices.clone()));
                let mesh = meshes.add(mesh);
                let entity = commands
                    .spawn((
                        appearance,
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        Transform::IDENTITY,
                        bevy::camera::visibility::NoFrustumCulling,
                    ))
                    .id();
                (entity, mesh)
            })
            .collect();
        (key.to_string(), parts)
    });
    for ((_, mesh), part) in parts.1.iter().zip(&model.parts) {
        if let Some(mut mesh) = meshes.get_mut(mesh) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions(part));
            mesh.compute_smooth_normals();
        }
    }
}

pub(super) fn draw(
    mut commands: Commands,
    state: NonSend<MagicState>,
    mut c: Local<Creatures>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let c = &mut *c;
    let Some(wizard) = &state.world else {
        for (_, (_, parts)) in c.drawn.drain() {
            for (e, _) in parts {
                commands.entity(e).despawn();
            }
        }
        return;
    };
    let world = &wizard.session.world;
    let mirrors: HashSet<u16> = state.mirrors.values().map(|m| m.handle).collect();
    let mut live = HashSet::new();
    for e in &world.entities {
        use p2magicka::magicka_runtime::scene_world::EntityKind;
        if !e.added || e.removed || !matches!(e.kind, EntityKind::Npc { .. }) || mirrors.contains(&e.handle) {
            continue;
        }
        if world.overkilled.contains(&e.handle) {
            continue;
        }
        let Some(template) = e.template_name.clone() else { continue };
        let Some(skin) = p2magicka::visual::npc_pose(wizard, e.handle) else { continue };
        let model = c.models.entry(template.to_ascii_lowercase()).or_insert_with(|| {
            load_model(
                p2magicka::visual::character_parts(&wizard.content, &template, [1.; 3]),
                &template,
                &mut images,
                &mut materials,
            )
        });
        let Some(model) = model.as_ref() else { continue };
        live.insert(e.handle);
        place(&mut commands, &mut c.drawn, &mut meshes, e.handle, &template.to_ascii_lowercase(), model, super::appearance::CharacterPart::Body(e.handle), |part| {
            p2magicka::visual::positions(part, &skin, &[])
        });
    }
    // Summon Elemental: eggs waiting for an element to hatch them.
    if let Some(eggs) = world.elemental_eggs.as_ref() {
        for slot in eggs.slots.iter().filter(|s| s.actor.registered) {
            let Some(frame) = slot.frame.as_ref() else { continue };
            let content = wizard.content.clone();
            let model = c.models.entry("elemental_egg".into()).or_insert_with(|| {
                load_model(
                    p2magicka::visual::skinned_model_parts(&content, "Models/Characters/Elemental/elemental_mesh"),
                    "elemental_egg",
                    &mut images,
                    &mut materials,
                )
            });
            let Some(model) = model.as_ref() else { continue };
            live.insert(slot.handle);
            place(&mut commands, &mut c.drawn, &mut meshes, slot.handle, "elemental_egg", model, super::appearance::CharacterPart::Body(slot.handle), |part| {
                p2magicka::visual::positions(part, &frame.bones, &[])
            });
        }
    }
    // Summon Death: MagickDeath's own actor, its skinned body and the scythe.
    if let Some(frame) = world.death.frame.as_ref().filter(|_| world.death.actor.is_some()) {
        let content = wizard.content.clone();
        for (handle, key, asset) in [
            (DEATH_BODY, "death", "Models/Bosses/death/death"),
            (DEATH_SCYTHE, "death_scythe", "Models/Bosses/death/death_scythe"),
        ] {
            let model = c.models.entry(key.into()).or_insert_with(|| {
                let parts = if handle == DEATH_BODY {
                    p2magicka::visual::skinned_model_parts(&content, asset)
                } else {
                    p2magicka::visual::model_parts(&content, asset)
                };
                load_model(parts, key, &mut images, &mut materials)
            });
            let Some(model) = model.as_ref() else { continue };
            live.insert(handle);
            let appearance = if handle == DEATH_BODY { super::appearance::CharacterPart::DeathBody } else { super::appearance::CharacterPart::DeathScythe };
            place(&mut commands, &mut c.drawn, &mut meshes, handle, key, model, appearance, |part| {
                if handle == DEATH_BODY {
                    p2magicka::visual::positions(part, &frame.bones, &[])
                } else {
                    part.geometry.positions.iter().map(|p| p2magicka::visual::transform_point(*p, &frame.scythe_world)).collect()
                }
            });
        }
    }
    c.drawn.retain(|handle, (_, parts)| {
        let keep = live.contains(handle);
        if !keep {
            for (entity, _) in parts.drain(..) {
                commands.entity(entity).despawn();
            }
        }
        keep
    });
}
