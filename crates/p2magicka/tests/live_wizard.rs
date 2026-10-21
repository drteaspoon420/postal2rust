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
fn world() -> WizardWorld {
    let content = std::env::var_os("MAGICKA_CONTENT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../re-lab-starter/target/Magicka/Content")
        });
    let floor = postal_level([
        [
            [-2000., -2000., 0.],
            [2000., -2000., 0.],
            [2000., 2000., 0.],
        ],
        [
            [-2000., -2000., 0.],
            [2000., 2000., 0.],
            [-2000., 2000., 0.],
        ],
    ]);
    let mut w = WizardWorld::new(&content, floor, Loadout::default())
        .expect("load original Wizard and items");
    w.place([0., 0., 60.], [1., 0., 0.]);
    for _ in 0..90 {
        w.step(DT).unwrap();
    }
    w
}
fn frames(w: &mut WizardWorld, input: PlayerInput, n: usize) {
    for _ in 0..n {
        w.input(input, [500., 0., 60.], DT);
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
    w.world.cast_magick(0, MagickType::Haste);
    assert!(w.world.ability_effects.hasted(handle));
    for _ in 0..700 {
        w.step(DT).unwrap();
    }
    assert!(!w.world.ability_effects.hasted(handle), "haste must expire");
}
#[test]
fn arcane_beam_hits_a_hosted_postal_pawn() {
    let mut w = world();
    let handle = w.attach_pawn([200., 0., 60.], 1000., 24., 40.).unwrap();
    // Leave the hosted pawn standing on Postal's floor before casting.
    frames(&mut w, PlayerInput::default(), 30);
    let before = w.world.npcs[&handle].character.combat.health.hit_points;
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
        w.world.npcs[&handle].character.combat.health.hit_points < before,
        "real Magicka beam must damage the mirrored Postal character"
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
    w.receive_damage(physical, None, [3., 1., 0.]);
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
    w.receive_damage(physical, None, [3., 1., 0.]);
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
