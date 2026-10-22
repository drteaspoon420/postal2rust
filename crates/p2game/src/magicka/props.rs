//! Spell damage to native cars, barrels, windows and other destructible props.
//! Their original TakeDamage script decides damage acceptance and explosions.
use super::{HEALTH_SCALE, postal_damage_class, status_elements};
use p2magicka::WizardWorld;
use std::collections::HashMap;
use uscript::vm::{ObjId, Value, Vm};

pub(super) struct Prop {
    handle: u16,
    remainder: f32,
}
fn health(vm: &Vm, actor: ObjId) -> f32 { vm.get_prop(actor, "Health").map(Value::as_float).unwrap_or(0.) }
fn eligible(vm: &Vm, actor: ObjId) -> bool {
    let c = vm.objects[actor].class;
    !vm.objects[actor].deleted && !vm.get_prop(actor, "bHidden").is_some_and(Value::as_bool)
        && health(vm, actor) > 0. && !vm.class_is_a(c, "Pawn") && !vm.class_is_a(c, "Projectile")
        && !vm.class_is_a(c, "Inventory")
        && (vm.class_is_a(c, "KActor") || vm.get_prop(actor, "StaticMesh").and_then(Value::as_resource).is_some())
}
pub(super) fn before(vm: &Vm, wizard: &mut WizardWorld, props: &mut HashMap<ObjId, Prop>) {
    props.retain(|&actor, prop| {
        if eligible(vm, actor) { true } else { wizard.detach_prop(prop.handle); false }
    });
    for actor in vm.live_actors() {
        if !eligible(vm, actor) { continue; }
        let hp = health(vm, actor) * HEALTH_SCALE;
        if let Some(prop) = props.get(&actor) {
            wizard.sync_prop(prop.handle, vm.actor_loc(actor), hp);
        } else {
            let r = vm.get_prop(actor, "CollisionRadius").map(Value::as_float).unwrap_or(12.).max(1.);
            let h = vm.get_prop(actor, "CollisionHeight").map(Value::as_float).unwrap_or(12.).max(1.);
            let mass = vm.get_prop(actor, "Mass").map(Value::as_float).unwrap_or(100.);
            match wizard.attach_prop(vm.actor_loc(actor), [r, r, h], hp, mass) {
                Ok(handle) => { props.insert(actor, Prop { handle, remainder: 0. }); }
                Err(e) => eprintln!("MAGICKA prop: {e}"),
            }
        }
    }
}
pub(super) fn after(vm: &mut Vm, player: ObjId, wizard: &mut WizardWorld, props: &mut HashMap<ObjId, Prop>, elements: u32) {
    for (&actor, prop) in props {
        if vm.objects[actor].deleted { continue; }
        let Some(state) = wizard.session.world.physics_damage.get(&prop.handle) else { continue };
        let delta = health(vm, actor) - state.health / HEALTH_SCALE + prop.remainder;
        let statuses = state.status_mask;
        if delta >= 1. {
            let amount = delta.floor() as i32;
            let elements = if elements != 0 { elements } else { status_elements(statuses) };
            let class = postal_damage_class(vm, elements, false, amount);
            vm.fire_event(actor, "TakeDamage", vec![Value::Int(amount), Value::Object(Some(player)),
                Value::Vector(vm.actor_loc(actor)), Value::Vector([0.; 3]), Value::Class(class)]);
            prop.remainder = delta - amount as f32;
            if std::env::var_os("MAGICKA_TRACE").is_some() {
                println!("MAGICKA PROP HIT {} damage={amount} health={} burning={:?} blown={:?}", vm.object_name(actor), health(vm, actor), vm.get_prop(actor, "BurningEmitter"), vm.get_prop(actor, "bBlownUp"));
            }
        } else { prop.remainder = delta.max(0.); }
        // Let water/steam put out the native flame too, not only its mirror.
        if statuses & p2magicka::magicka_sim::status::Status::Wet as u16 != 0
            && vm.get_prop(actor, "BurningEmitter").and_then(Value::as_obj).is_some() {
            let class = vm.find_class_by_name("ExtinguishDamage").ok();
            vm.fire_event(actor, "TakeDamage", vec![Value::Int(1000), Value::Object(Some(player)),
                Value::Vector(vm.actor_loc(actor)), Value::Vector([0.; 3]), Value::Class(class)]);
        }
        wizard.sync_prop(prop.handle, vm.actor_loc(actor), health(vm, actor) * HEALTH_SCALE);
    }
}
