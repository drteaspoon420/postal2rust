//! Magicka status looks on Postal bodies: frozen pawns turn to glossy ice,
//! chilled ones blue, wet ones dark and shiny, poisoned ones green, and a
//! lightning hit flickers a shock glow. The pawn's rig sections (and the head
//! and other parts attached to it) get per-pawn tinted copies of their
//! materials while the status lasts.
use super::MagicState;
use crate::{rigs::VmRig, window::Sim};
use bevy::prelude::*;
use p2magicka::magicka_sim::status::Status as S;
use std::collections::HashMap;
use uscript::vm::Value;

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Frozen,
    Cold,
    Wet,
    Poisoned,
    Shock(bool),
}

fn look(statuses: u16, shocked: f32, clock: f32) -> Option<Look> {
    let has = |s: S| statuses & s as u16 != 0;
    if shocked > 0. {
        // Fast flicker, like the electrocution x-ray flashes.
        return Some(Look::Shock((clock * 24.) as i32 % 2 == 0));
    }
    if has(S::Frozen) {
        Some(Look::Frozen)
    } else if has(S::Poisoned) {
        Some(Look::Poisoned)
    } else if has(S::Cold) {
        Some(Look::Cold)
    } else if has(S::Wet) {
        Some(Look::Wet)
    } else {
        None
    }
}

fn tinted(original: &StandardMaterial, look: Look) -> StandardMaterial {
    let mut m = original.clone();
    // Bevy emissive units under Postal's EV100 9.7 camera exposure.
    let glow = 1.2 * 2f32.powf(9.7);
    match look {
        Look::Frozen => {
            m.base_color = Color::srgb(0.72, 0.88, 1.0);
            m.emissive = LinearRgba::rgb(0.10, 0.18, 0.28) * glow;
            m.perceptual_roughness = 0.08;
            m.reflectance = 0.9;
        }
        Look::Cold => {
            m.base_color = Color::srgb(0.75, 0.85, 1.0);
        }
        Look::Wet => {
            m.base_color = Color::srgb(0.6, 0.62, 0.68);
            m.perceptual_roughness = 0.12;
            m.reflectance = 0.7;
        }
        Look::Poisoned => {
            m.base_color = Color::srgb(0.65, 1.0, 0.5);
            m.emissive = LinearRgba::rgb(0.03, 0.12, 0.0) * glow;
        }
        Look::Shock(on) => {
            m.emissive = if on { LinearRgba::rgb(0.6, 0.7, 1.0) * glow } else { LinearRgba::rgb(0.1, 0.12, 0.3) * glow };
        }
    }
    m
}

#[derive(Default)]
pub(super) struct Looks {
    /// Section entity -> (original material, tinted copy, look applied).
    applied: HashMap<Entity, (Handle<StandardMaterial>, Handle<StandardMaterial>, u8)>,
    clock: f32,
    /// Frozen sections -> their bloated ice shell entity.
    shells: HashMap<Entity, Entity>,
    ice: Option<Handle<crate::magicka_fx::IceShellMaterial>>,
}

fn key(look: Look) -> u8 {
    match look {
        Look::Frozen => 1,
        Look::Cold => 2,
        Look::Wet => 3,
        Look::Poisoned => 4,
        Look::Shock(false) => 5,
        Look::Shock(true) => 6,
    }
}

pub(super) fn apply(
    mut commands: Commands,
    state: NonSend<MagicState>,
    sim: NonSend<Sim>,
    time: Res<Time>,
    rigs: Query<&VmRig>,
    sections: Query<&MeshMaterial3d<StandardMaterial>>,
    section_meshes: Query<(&Mesh3d, Option<&bevy::mesh::skinning::SkinnedMesh>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut ice_materials: ResMut<Assets<crate::magicka_fx::IceShellMaterial>>,
    mut looks: Local<Looks>,
) {
    looks.clock += time.delta_secs();
    let clock = looks.clock;
    let vm = &sim.game.world.vm;
    let mut wanted: HashMap<Entity, Look> = HashMap::new();
    if state.world.is_some() {
        for rig in &rigs {
            // Heads and other attachments follow their body's look.
            let owner = vm
                .get_prop(rig.actor, "Base")
                .and_then(Value::as_obj)
                .filter(|b| state.mirrors.contains_key(b))
                .unwrap_or(rig.actor);
            let Some(mirror) = state.mirrors.get(&owner) else { continue };
            if let Some(l) = look(mirror.statuses, mirror.shocked, clock) {
                for &s in &rig.sections {
                    wanted.insert(s, l);
                }
            }
        }
    }
    // Frozen: a bloated translucent ice shell over each section.
    let ice = looks
        .ice
        .get_or_insert_with(|| {
            ice_materials.add(crate::magicka_fx::IceShellMaterial { params: Vec4::new(0.6, 0.85, 1.0, 0.035) })
        })
        .clone();
    looks.shells.retain(|section, shell| {
        let keep = wanted.get(section) == Some(&Look::Frozen) && section_meshes.contains(*section);
        if !keep {
            if let Ok(mut ec) = commands.get_entity(*shell) {
                ec.despawn();
            }
            if std::env::var_os("MAGICKA_TRACE").is_some() {
                println!("MAGICKA ICE removed {section:?}");
            }
        }
        keep
    });
    for (&section, &look) in &wanted {
        if look != Look::Frozen {
            continue;
        }
        if looks.shells.contains_key(&section) {
            continue;
        }
        let Ok((mesh, skinned)) = section_meshes.get(section) else { continue };
        let mut shell = commands.spawn((
            Mesh3d(mesh.0.clone()),
            MeshMaterial3d(ice.clone()),
            // Follow the section's transform and visibility, and disappear
            // with it on map travel or rig replacement. Skinned sections use
            // the original joints; rigid sections inherit their placement.
            Transform::IDENTITY,
            ChildOf(section),
            bevy::camera::visibility::NoFrustumCulling,
            bevy::light::NotShadowCaster,
        ));
        if let Some(skinned) = skinned {
            shell.insert(skinned.clone());
        }
        let id = shell.id();
        looks.shells.insert(section, id);
        if std::env::var_os("MAGICKA_TRACE").is_some() {
            println!("MAGICKA ICE attached {section:?}");
        }
    }
    // Restore sections whose look ended (or whose rig is gone).
    let ended: Vec<Entity> = looks.applied.keys().copied().filter(|e| !wanted.contains_key(e)).collect();
    for e in ended {
        let (original, tinted, _) = looks.applied.remove(&e).unwrap();
        // Postal can replace skins (for example a burned corpse) while a
        // status is active. Never restore an obsolete skin over that change.
        if sections.get(e).is_ok_and(|m| m.0 == tinted) {
            commands.entity(e).insert(MeshMaterial3d(original));
        }
        materials.remove(&tinted);
    }
    for (e, l) in wanted {
        let k = key(l);
        match looks.applied.get_mut(&e) {
            Some((original, tinted, applied)) => {
                let replaced = sections.get(e).is_ok_and(|m| m.0 != *tinted);
                if replaced {
                    *original = sections.get(e).unwrap().0.clone();
                }
                if *applied != k || replaced {
                    if let Some(base) = materials.get(original).cloned() {
                        if let Some(mut m) = materials.get_mut(&*tinted) {
                            *m = self::tinted(&base, l);
                        }
                    }
                    *applied = k;
                    if replaced {
                        commands.entity(e).insert(MeshMaterial3d(tinted.clone()));
                    }
                }
            }
            None => {
                let Ok(current) = sections.get(e) else { continue };
                let original = current.0.clone();
                let Some(base) = materials.get(&original).cloned() else { continue };
                let copy = materials.add(tinted(&base, l));
                commands.entity(e).insert(MeshMaterial3d(copy.clone()));
                looks.applied.insert(e, (original, copy, k));
            }
        }
    }
}
