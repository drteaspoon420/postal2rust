//! Postal owns which people are enemies. Magicka allies use that disposition
//! instead of treating every mirrored pedestrian as an enemy faction.
use super::Mirror;
use p2magicka::WizardWorld;
use std::collections::{HashMap, HashSet};
use uscript::vm::{ObjId, Value, Vm};

pub(super) struct NativeDisposition {
    friend: bool,
    enemy: bool,
}
fn flag(vm: &Vm, actor: ObjId, name: &str) -> bool {
    vm.get_prop(actor, name).is_some_and(Value::as_bool)
}
fn controller(vm: &Vm, actor: ObjId) -> Option<ObjId> {
    vm.get_prop(actor, "Controller").and_then(Value::as_obj)
}
fn target(vm: &Vm, actor: ObjId) -> Option<ObjId> {
    let c = controller(vm, actor)?;
    vm.get_prop(c, "Enemy").and_then(Value::as_obj)
        .or_else(|| vm.get_prop(c, "Attacker").and_then(Value::as_obj))
}
pub(super) fn restore(vm: &mut Vm, actor: ObjId, saved: NativeDisposition) {
    vm.set_prop(actor, "bPlayerIsFriend", Value::Bool(saved.friend));
    vm.set_prop(actor, "bPlayerIsEnemy", Value::Bool(saved.enemy));
    if let Some(c) = controller(vm, actor) {
        let class = vm.objects[c].class;
        let _ = vm.call(class, Some(c), "FullClearAttacker", vec![]);
        vm.set_prop(c, "Enemy", Value::Object(None));
        let _ = vm.goto_state(c, "Thinking", "Begin");
    }
}
pub(super) fn sync(vm: &mut Vm, player: ObjId, mirrors: &mut HashMap<ObjId, Mirror>, wizard: &mut WizardWorld) {
    let player_handle = wizard.avatar().handle;
    let charmed: HashSet<_> = mirrors.iter().filter_map(|(&actor, m)| {
        wizard.session.world.npcs.get(&m.handle)
            .filter(|n| n.character.charm.is_some_and(|c| c.owner == player_handle && c.timer > 0.))
            .map(|_| actor)
    }).collect();
    let mut allies = charmed.clone();
    allies.insert(player);
    allies.extend(mirrors.keys().filter(|&&a| flag(vm, a, "bPlayerIsFriend")).copied());
    let hostiles: HashSet<_> = mirrors.keys().copied().filter(|a| {
        !charmed.contains(a) && (target(vm, *a).is_some_and(|t| allies.contains(&t))
            || (flag(vm, *a, "bPlayerIsEnemy") && !flag(vm, *a, "bPlayerIsFriend")))
    }).collect();
    for (&actor, mirror) in mirrors.iter_mut() {
        wizard.set_pawn_hostile(mirror.handle, hostiles.contains(&actor));
        if charmed.contains(&actor) {
            if mirror.charm.is_none() {
                mirror.charm = Some(NativeDisposition { friend: flag(vm, actor, "bPlayerIsFriend"), enemy: flag(vm, actor, "bPlayerIsEnemy") });
                vm.set_prop(actor, "bPlayerIsFriend", Value::Bool(true));
                vm.set_prop(actor, "bPlayerIsEnemy", Value::Bool(false));
                if let Some(c) = controller(vm, actor) {
                    let class = vm.objects[c].class;
                    let _ = vm.call(class, Some(c), "FullClearAttacker", vec![]);
                    vm.set_prop(c, "Enemy", Value::Object(None));
                    let _ = vm.goto_state(c, "Thinking", "Begin");
                }
            }
            let nearest = hostiles.iter().copied().filter(|&a| vm.get_prop(a, "Health").map(Value::as_float).unwrap_or(0.) > 0.)
                .min_by(|&a, &b| {
                    let distance = |a| glam::Vec3::from(vm.actor_loc(a)).distance_squared(glam::Vec3::from(vm.actor_loc(actor)));
                    distance(a).total_cmp(&distance(b))
                });
            if target(vm, actor) != nearest {
                if let Some(c) = controller(vm, actor) {
                    let class = vm.objects[c].class;
                    let _ = vm.call(class, Some(c), "SetAttacker", vec![Value::Object(nearest)]);
                    vm.set_prop(c, "Enemy", Value::Object(nearest));
                    let _ = vm.goto_state(c, if nearest.is_some() { "AssessAttacker" } else { "Thinking" }, "Begin");
                }
            }
        } else if let Some(saved) = mirror.charm.take() {
            restore(vm, actor, saved);
        }
    }
    // Drop already-selected neutral targets immediately when Postal's
    // relationship changes (including charm/expiry), not just at AI retarget.
    let neutral: HashSet<_> = mirrors.iter().filter(|(a, _)| !hostiles.contains(a)).map(|(_, m)| m.handle).collect();
    for npc in wizard.session.world.npcs.values_mut() {
        if npc.character.is_charmed() || npc.summon.is_some_and(|(owner, _)| owner == player_handle) {
            while npc.combat.current_target().is_some_and(|h| neutral.contains(&h)) {
                npc.combat.release_target();
            }
        }
    }
}
