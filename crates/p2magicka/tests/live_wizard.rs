//! Integration tests use the user's local Magicka files, as the game does.
//! These deliberately fail if content is unavailable rather than reporting
//! an unexecuted content test as passing.
use p2magicka::{
    Loadout, PlayerInput, WizardWorld,
    magicka_sim::spells::{Elements, MagickType},
    postal_level,
};
use std::path::PathBuf;
const DT: f32 = 1. / 60.;
/// Postal maps sit far below Magicka's default -50 m kill plane (suburbs-1
/// is around Z -4300), so the test floor does too.
const FLOOR: f32 = -4300.;
fn world() -> WizardWorld {
    let content = std::env::var_os("MAGICKA_CONTENT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../re-lab-starter/target/Magicka/Content")
        });
    let floor = postal_level([
        [
            [-2000., -2000., FLOOR],
            [2000., -2000., FLOOR],
            [2000., 2000., FLOOR],
        ],
        [
            [-2000., -2000., FLOOR],
            [2000., 2000., FLOOR],
            [-2000., 2000., FLOOR],
        ],
    ]);
    let mut w = WizardWorld::new(
        &content,
        floor,
        Loadout::default(),
        [0., 0., FLOOR + 60.],
        [1., 0., 0.],
    )
    .expect("load original Wizard and items");
    w.place([0., 0., FLOOR + 60.], [1., 0., 0.]);
    for _ in 0..90 {
        w.step(DT).unwrap();
    }
    assert!(
        !w.avatar().dead(),
        "the wizard must survive standing on a deep Postal floor"
    );
    w
}
fn frames(w: &mut WizardWorld, input: PlayerInput, n: usize) {
    for _ in 0..n {
        w.input(input, [500., 0., FLOOR + 60.], DT);
        w.step(DT).unwrap();
    }
}
fn element(w: &mut WizardWorld, index: usize) {
    let mut input = PlayerInput::default();
    input.elements[index] = true;
    frames(w, input, 1);
    frames(w, PlayerInput::default(), 10);
}
#[test]
fn wizard_keeps_real_robe_equipment_and_queue_rules() {
    let mut w = world();
    assert!(w.avatar().rig.is_some(), "robe skeleton must be loaded");
    let visual = p2magicka::visual::load(&w).unwrap();
    assert!(
        visual
            .iter()
            .any(|p| p.slot.is_none() && p.texture.is_some()),
        "original textured robe"
    );
    assert!(
        visual.iter().any(|p| p.slot == Some(0)),
        "original melee model"
    );
    assert!(
        visual.iter().any(|p| p.slot == Some(1)),
        "original staff model"
    );
    assert!(w.avatar().items.iter().any(|(s, _)| *s == 0));
    assert!(w.avatar().items.iter().any(|(s, _)| *s == 1));
    element(&mut w, 7); // fire
    assert_eq!(w.avatar().conjure.queue.len(), 1);
    element(&mut w, 3); // cold cancels fire
    assert!(w.avatar().conjure.queue.is_empty());
    element(&mut w, 0); // water
    element(&mut w, 7); // fire -> steam
    assert_eq!(w.avatar().conjure.queue[0].element, Elements::STEAM);
}
#[test]
fn self_life_cast_heals_and_haste_uses_rewrite_lifecycle() {
    let mut w = world();
    w.avatar_mut().character.combat.health.hit_points *= 0.5;
    let before = w.avatar().character.combat.health.hit_points;
    element(&mut w, 1);
    frames(
        &mut w,
        PlayerInput {
            cast_self_down: true,
            ..Default::default()
        },
        90,
    );
    assert!(
        w.avatar().character.combat.health.hit_points > before,
        "self cast life must heal"
    );
    let handle = w.avatar().handle;
    w.session.world.cast_magick(0, MagickType::Haste);
    assert!(w.session.world.ability_effects.hasted(handle));
    for _ in 0..700 {
        w.step(DT).unwrap();
    }
    assert!(
        !w.session.world.ability_effects.hasted(handle),
        "haste must expire"
    );
}
#[test]
fn arcane_beam_hits_a_hosted_postal_pawn() {
    let mut w = world();
    let handle = w
        .attach_pawn([200., 0., FLOOR + 60.], 1000., 24., 40.)
        .unwrap();
    // Leave the hosted pawn standing on Postal's floor before casting.
    frames(&mut w, PlayerInput::default(), 30);
    let before = w.session.world.npcs[&handle]
        .character
        .combat
        .health
        .hit_points;
    element(&mut w, 5);
    frames(
        &mut w,
        PlayerInput {
            cast_down: true,
            ..Default::default()
        },
        120,
    );
    assert!(
        w.session.world.npcs[&handle]
            .character
            .combat
            .health
            .hit_points
            < before,
        "real Magicka beam must damage the mirrored Postal character"
    );
}
#[test]
fn hosted_pawn_keeps_its_postal_health() {
    let mut w = world();
    // Postal health above the Wizard template's 1000 must not be clamped,
    // or every hosted Postal character would be hurt on attach.
    let handle = w
        .attach_pawn([200., 0., FLOOR + 60.], 1250., 24., 40.)
        .unwrap();
    frames(&mut w, PlayerInput::default(), 30);
    assert_eq!(
        w.session.world.npcs[&handle]
            .character
            .combat
            .health
            .hit_points,
        1250.
    );
}
#[test]
fn spells_produce_original_beam_and_particle_presentation() {
    let mut w = world();
    element(&mut w, 5); // arcane
    let cast = PlayerInput {
        cast_down: true,
        ..Default::default()
    };
    frames(&mut w, cast, 30);
    assert!(
        !w.session.beams.is_empty(),
        "an arcane beam must yield railgun render data"
    );
    frames(&mut w, PlayerInput::default(), 60);
    element(&mut w, 7); // fire spray
    frames(&mut w, cast, 30);
    assert!(
        !w.session.effects.particles.active().is_empty(),
        "a fire spray must emit original particle effects"
    );
}

#[test]
fn incoming_host_damage_uses_magicka_health_and_self_shield() {
    use p2magicka::magicka_sim::damage::{Damage, attack};
    let mut w = world();
    let physical = Damage {
        attack: attack::DAMAGE,
        element: Elements::EARTH.0,
        amount: 100.,
        magnitude: 1.,
    };
    let before = w.avatar().character.combat.health.hit_points;
    w.receive_damage(physical, None, w.body().position);
    assert!(
        w.avatar().character.combat.health.hit_points < before,
        "incoming gunshot must reach the wizard's health"
    );
    element(&mut w, 2); // Shield
    frames(
        &mut w,
        PlayerInput {
            cast_self_down: true,
            ..Default::default()
        },
        45,
    );
    frames(&mut w, PlayerInput::default(), 30);
    let health = w.avatar().character.combat.health.hit_points;
    let shield = w.avatar().character.combat.health.shield.health;
    assert!(shield > 0., "the real self cast must create the ward");
    w.receive_damage(physical, None, w.body().position);
    assert_eq!(
        w.avatar().character.combat.health.hit_points,
        health,
        "the ward must intercept physical damage before health"
    );
    assert!(
        w.avatar().character.combat.health.shield.health < shield,
        "incoming host damage must wear down the ward"
    );
}

#[test]
fn loadout_catalog_and_equipping_preserve_live_combat_and_existing_items() {
    use p2magicka::{
        catalog::Catalog,
        magicka_sim::status::{Status, StatusEffect},
    };
    let mut w = world();
    let catalog = Catalog::load(&w.content).unwrap();
    assert!(catalog.robes.iter().any(|s| s == "Wizard"));
    assert!(catalog.staffs.iter().any(|s| s == "staff_basic"));
    assert!(catalog.weapons.iter().any(|s| s == "weapon_blunt_blade"));
    let before = w.equipped_loadout();
    let replacement = catalog
        .weapons
        .iter()
        .find(|name| Some(*name) != before.weapon.as_ref())
        .unwrap();
    w.avatar_mut().character.combat.health.hit_points *= 0.4;
    let health = w.avatar().character.combat.health.hit_points;
    w.avatar_mut()
        .character
        .combat
        .statuses
        .set_effect(StatusEffect::new(Status::Wet, 0., 1., false))
        .unwrap();
    w.avatar_mut().character.combat.statuses.current_mask = Status::Wet as u16;
    w.avatar_mut()
        .items
        .iter_mut()
        .find(|(slot, _)| *slot == 1)
        .unwrap()
        .1
        .special_cooldown = 7.;
    w.equip(Loadout {
        weapon: Some(replacement.clone()),
        ..before.clone()
    })
    .unwrap();
    assert_eq!(
        w.avatar().character.combat.health.hit_points,
        health,
        "equipping must not heal"
    );
    assert!(w.avatar().character.combat.statuses.has(Status::Wet));
    assert_eq!(
        w.avatar()
            .items
            .iter()
            .find(|(slot, _)| *slot == 1)
            .unwrap()
            .1
            .special_cooldown,
        7.
    );
    assert_eq!(w.equipped_loadout().weapon.as_ref(), Some(replacement));
    assert_eq!(w.loadout, w.equipped_loadout());
    let parts = p2magicka::visual::load(&w).unwrap();
    assert!(
        parts.iter().any(|p| p.slot == Some(0)),
        "replacement model is renderable"
    );

    let robe = catalog
        .robes
        .iter()
        .find(|name| **name != before.robe)
        .unwrap();
    let selected = Loadout {
        robe: robe.clone(),
        ..w.equipped_loadout()
    };
    w.equip(selected.clone()).unwrap();
    let hp = &w.avatar().character.combat.health;
    assert!((hp.hit_points / hp.max_hit_points - 0.4).abs() < 1e-5);
    let health_before_invalid = hp.hit_points;
    assert_eq!(w.session.player_robe(0), Some(robe.as_str()));
    assert!(w.avatar().rig.is_some());
    assert!(
        p2magicka::visual::load(&w)
            .unwrap()
            .iter()
            .any(|p| p.slot.is_none())
    );
    assert!(w.avatar().character.combat.statuses.has(Status::Wet));
    let mut invalid = selected;
    invalid.staff = Some("not-an-installed-staff".into());
    let expected = w.equipped_loadout();
    assert!(w.equip(invalid).is_err());
    assert_eq!(
        w.equipped_loadout(),
        expected,
        "failed selection must leave gear intact"
    );
    assert_eq!(
        w.avatar().character.combat.health.hit_points,
        health_before_invalid
    );
}
#[test]
fn installed_catalog_loads_original_robes_staffs_and_melee_models() {
    let mut w = world();
    let catalog = p2magicka::catalog::Catalog::load(&w.content).unwrap();
    eprintln!(
        "loadout catalog: {} robes, {} staffs, {} melee weapons",
        catalog.robes.len(),
        catalog.staffs.len(),
        catalog.weapons.len()
    );
    for robe in &catalog.robes {
        w.equip(Loadout {
            robe: robe.clone(),
            ..w.equipped_loadout()
        })
        .unwrap_or_else(|e| panic!("{robe}: {e}"));
        assert!(w.avatar().rig.is_some(), "{robe}: original skeleton");
        let parts = p2magicka::visual::load(&w).unwrap_or_else(|e| panic!("{robe}: {e}"));
        assert!(
            parts.iter().any(|p| p.slot.is_none()),
            "{robe}: original robe geometry"
        );
        assert!(
            p2magicka::visual::pose(&w).is_some(),
            "{robe}: pose sampling"
        );
    }
    w.equip(Loadout {
        robe: "Wizard".into(),
        ..w.equipped_loadout()
    })
    .unwrap();
    for (slot, names) in [(1, &catalog.staffs), (0, &catalog.weapons)] {
        for name in names {
            let mut gear = w.equipped_loadout();
            if slot == 1 {
                gear.staff = Some(name.clone());
            } else {
                gear.weapon = Some(name.clone());
            }
            w.equip(gear).unwrap_or_else(|e| panic!("{name}: {e}"));
            let item = &w.avatar().items.iter().find(|(s, _)| *s == slot).unwrap().1;
            let hidden = item.definition.hide_model;
            let parts = p2magicka::visual::load(&w).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                hidden || parts.iter().any(|p| p.slot == Some(slot)),
                "{name}: original equipment geometry"
            );
        }
    }
}

#[test]
fn saved_wizard_roundtrip_retains_health_shield_status_queue_items_and_haste() {
    use p2magicka::{
        magicka_sim::status::{Status, StatusEffect},
        persistence::SavedWizard,
    };
    let mut source = world();
    element(&mut source, 2);
    frames(
        &mut source,
        PlayerInput {
            cast_self_down: true,
            ..Default::default()
        },
        45,
    );
    frames(&mut source, PlayerInput::default(), 20);
    assert!(source.avatar().character.combat.health.shield.health > 0.);
    source.avatar_mut().character.combat.health.hit_points = 370.25;
    source
        .avatar_mut()
        .character
        .combat
        .statuses
        .set_effect(StatusEffect::new(Status::Wet, 0., 0.7, false))
        .unwrap();
    source.avatar_mut().character.combat.statuses.current_mask = Status::Wet as u16;
    let staff = &mut source
        .avatar_mut()
        .items
        .iter_mut()
        .find(|(s, _)| *s == 1)
        .unwrap()
        .1;
    staff.special_cooldown = 7.25;
    let weapon = &mut source
        .avatar_mut()
        .items
        .iter_mut()
        .find(|(s, _)| *s == 0)
        .unwrap()
        .1;
    weapon.cooldown = 0.75;
    weapon.spells.add(
        p2magicka::magicka_sim::spells::Spell::default_for(Elements::FIRE),
        false,
        &mut vec![],
    );
    source.session.world.cast_magick(0, MagickType::Haste);
    element(&mut source, 0);
    let saved = SavedWizard::capture(&source, true);
    let encoded = saved.encode().unwrap();
    let decoded = SavedWizard::decode(&encoded).unwrap();
    assert_eq!(saved, decoded);
    let mut destination = world();
    decoded.restore(&mut destination).unwrap();
    assert_eq!(
        SavedWizard::capture(&destination, true),
        saved,
        "restoring into a fresh host must retain every serialized player field"
    );
    assert!(
        destination
            .session
            .world
            .ability_effects
            .hasted(destination.avatar().handle)
    );
    let health = destination.avatar().character.combat.health.hit_points;
    destination.receive_damage(
        p2magicka::magicka_sim::damage::Damage {
            attack: p2magicka::magicka_sim::damage::attack::DAMAGE,
            element: Elements::EARTH.0,
            amount: 50.,
            magnitude: 1.,
        },
        None,
        destination.body().position,
    );
    assert_eq!(
        destination.avatar().character.combat.health.hit_points,
        health,
        "restored self-shield must still block incoming host damage"
    );
    assert!(
        destination.avatar().character.combat.health.shield.health
            < source.avatar().character.combat.health.shield.health
    );
    for _ in 0..700 {
        destination.step(DT).unwrap();
    }
    assert!(
        !destination
            .session
            .world
            .ability_effects
            .hasted(destination.avatar().handle)
    );
}

#[test]
fn saved_resistance_ward_keeps_its_damage_rules_and_remaps_buff_ids() {
    use p2magicka::persistence::SavedWizard;
    let mut source = world();
    element(&mut source, 2); // shield
    element(&mut source, 7); // fire immunity aura
    frames(
        &mut source,
        PlayerInput {
            cast_self_down: true,
            ..Default::default()
        },
        45,
    );
    frames(&mut source, PlayerInput::default(), 15);
    assert!(
        !source.avatar().character.auras.is_empty(),
        "original cast must create an elemental ward"
    );
    assert!(!source.avatar().character.buffs.list.is_empty());
    let old_id = source.avatar().character.auras[0].visual.buff_id;
    let saved =
        SavedWizard::decode(&SavedWizard::capture(&source, false).encode().unwrap()).unwrap();
    let mut restored = world();
    saved.restore(&mut restored).unwrap();
    let new_id = restored.avatar().character.auras[0].visual.buff_id;
    assert_ne!(
        old_id, new_id,
        "old effect IDs cannot collide with new-world auras"
    );
    assert!(
        restored
            .avatar()
            .character
            .buffs
            .list
            .iter()
            .any(|b| b.id == new_id)
    );
    let hit = p2magicka::magicka_sim::damage::Damage {
        attack: p2magicka::magicka_sim::damage::attack::DAMAGE,
        element: Elements::FIRE.0,
        amount: 100.,
        magnitude: 1.,
    };
    let before = restored.avatar().character.combat.health.hit_points;
    source.receive_damage(hit, None, source.body().position);
    restored.receive_damage(hit, None, restored.body().position);
    assert_eq!(
        restored.avatar().character.combat.health.hit_points,
        before,
        "the restored fire ward must still block fire damage"
    );
    assert_eq!(
        restored.avatar().character.combat.health.hit_points,
        source.avatar().character.combat.health.hit_points
    );
}

#[test]
fn saved_wizard_rejects_corruption_and_restores_dead_state() {
    use p2magicka::persistence::SavedWizard;
    let mut source = world();
    let valid = SavedWizard::capture(&source, false).encode().unwrap();
    assert!(SavedWizard::decode("not JSON").is_err());
    assert!(SavedWizard::decode(&valid.replace("\"version\":1", "\"version\":999")).is_err());
    assert!(SavedWizard::decode(&valid.replace("\"kind\":0", "\"kind\":99")).is_err());
    assert!(
        SavedWizard::decode(&valid.replace("\"hit_points\":1000.0", "\"hit_points\":null"))
            .is_err()
    );
    source.avatar_mut().character.combat.health.hit_points = -25.;
    source.avatar_mut().character.combat.dead = true;
    source
        .avatar_mut()
        .items
        .iter_mut()
        .for_each(|(_, item)| item.attached = false);
    let dead =
        SavedWizard::decode(&SavedWizard::capture(&source, false).encode().unwrap()).unwrap();
    let mut destination = world();
    dead.restore(&mut destination).unwrap();
    assert!(destination.avatar().dead());
    assert_eq!(
        destination.avatar().character.combat.health.hit_points,
        -25.
    );
    assert!(destination.avatar().items.iter().all(|(_, i)| !i.attached));
}

#[test]
fn particle_sprite_sheets_decode_with_content() {
    let content = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../re-lab-starter/target/Magicka/Content");
    let sheets = p2magicka::visual::particle_sheets(&content).unwrap();
    assert_eq!(sheets.len(), 4);
    for v in sheets {
        let base = (v.width * v.height * v.depth * 4) as usize;
        assert!(v.rgba.len() > base, "every sheet keeps its mip chain");
        assert!(
            v.rgba[..base].chunks_exact(4).any(|c| c[3] == 255),
            "sprites have opaque texels"
        );
    }
}
#[test]
#[ignore]
fn print_item_bounds() {
    let w = world();
    for p in p2magicka::visual::load(&w).unwrap() {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in &p.geometry.positions {
            for i in 0..3 {
                min[i] = min[i].min(v[i]);
                max[i] = max[i].max(v[i]);
            }
        }
        eprintln!(
            "slot {:?} verts {} min {:?} max {:?}",
            p.slot,
            p.geometry.positions.len(),
            min,
            max
        );
    }
}
