//! Vortex's native force/core rules applied to movable Postal props. These
//! props retain Postal collision and scripts instead of becoming duplicate bodies.
use p2magicka::{WizardWorld, magicka_sim::shield_entity::ShieldKind};
use uscript::vm::{Value, Vm};

pub(super) fn props(vm: &mut Vm, wizard: &mut WizardWorld) {
    if wizard.session.world.vortices.is_empty() { return; }
    let actors = vm.live_actors();
    for actor in actors {
        let class = vm.objects[actor].class;
        let flag = |name| vm.get_prop(actor, name).is_some_and(Value::as_bool);
        if vm.class_is_a(class, "Pawn") || flag("bHidden") || flag("bStatic") { continue; }
        let physics = vm.get_prop(actor, "Physics").map(Value::as_int).unwrap_or(0);
        let movable = vm.class_is_a(class, "KActor") || vm.class_is_a(class, "Pickup") || matches!(physics, 2 | 13);
        if !movable { continue; }
        // Held inventory and bone attachments belong to their actor, not to
        // the loose-object loop. Projectile motion already has its own path.
        if vm.class_is_a(class, "Projectile") || vm.get_prop(actor, "Base").and_then(Value::as_obj).is_some()
            || vm.get_prop(actor, "Owner").and_then(Value::as_obj).is_some_and(|o| vm.class_is_a(vm.objects[o].class, "Pawn")) { continue; }
        let at = vm.actor_loc(actor);
        let position = p2magicka::to_magicka(at);
        if wizard.session.world.shields.iter().any(|s| !s.shield.dead && s.shield.kind == ShieldKind::Sphere
            && glam::Vec3::from(position).distance_squared(glam::Vec3::from(s.shield.position)) < s.shield.radius.powi(2)) { continue; }
        let radius = vm.get_prop(actor, "CollisionRadius").map(Value::as_float).unwrap_or(12.).max(1.) / p2magicka::UNREAL_UNITS_PER_METRE;
        let half = vm.get_prop(actor, "CollisionHeight").map(Value::as_float).unwrap_or(12.).max(1.) / p2magicka::UNREAL_UNITS_PER_METRE;
        let extent = [radius, half, radius];
        let mut velocity = vm.get_prop(actor, "Velocity").map(Value::as_vec).unwrap_or_default();
        let mut consumed = false;
        let mut pulled = false;
        for live in &mut wizard.session.world.vortices {
            let vortex = &mut live.vortex;
            if vortex.mass < 1e-6 { continue; }
            if glam::Vec3::from(position).distance(glam::Vec3::from(vortex.center)) > vortex.radius + radius.max(half) { continue; }
            if vortex.core_touches(std::array::from_fn(|i| position[i]-extent[i]), std::array::from_fn(|i| position[i]+extent[i])) {
                // Destroy is the native lifecycle path: rendering and physics
                // remove the actor on their next synchronization.
                let mass = vm.get_prop(actor, "KParams").and_then(Value::as_obj)
                    .and_then(|k| vm.get_prop(k, "KMass")).map(Value::as_float)
                    .or_else(|| vm.get_prop(actor, "Mass").map(Value::as_float)).unwrap_or(1.).max(0.01);
                let _ = vm.call(class, Some(actor), "Destroy", vec![]);
                if vm.objects[actor].deleted {
                    vortex.absorb(mass);
                    consumed = true;
                    if std::env::var_os("MAGICKA_TRACE").is_some() { println!("MAGICKA VORTEX consumed {} mass={mass}", vm.object_name(actor)); }
                }
                break;
            }
            if let Some(force) = vortex.pull(position) {
                let dv = p2magicka::to_postal(force);
                for i in 0..3 { velocity[i] += dv[i]; }
                pulled = true;
            }
        }
        if pulled && !consumed {
            if physics == 13 || vm.class_is_a(class, "KActor") {
                let _ = vm.call(class, Some(actor), "KWake", vec![]);
            } else {
                let _ = vm.call(class, Some(actor), "SetPhysics", vec![Value::Byte(2)]);
            }
            // KWake republishes the physics host's old velocity. Set the new
            // velocity afterwards so waking a resting prop cannot erase it.
            vm.set_prop(actor, "Velocity", Value::Vector(velocity));
            // Loose props keep their own owner; the caster is not assigned as
            // Owner because that would make them look like held inventory.
            if std::env::var_os("MAGICKA_TRACE").is_some() { println!("MAGICKA VORTEX pull {} vel={velocity:?}", vm.object_name(actor)); }
        }
    }
}
