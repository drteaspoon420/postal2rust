//! Severed limbs and popped heads spin about the axis the hit gives them.
//!
//! The parts' script (`Limb` / `Head` / `PeoplePart.GiveMomentum`) launches them in
//! PHYS_Projectile with `bFixedRotationDir` and a random per-component `RotationRate`. Applied as
//! independent pitch/yaw/roll rates that tumble has no relation to the shot, which reads wrong
//! (playtest: "the gib rotation should match the way the limb gets hit"). Here the last hit on the
//! victim (`TakeDamage` HitLocation and Momentum) gives a torque `(hit - centre) x momentum`; the
//! part keeps the script's spin speed (|RotationRate|, still damped by its HitWall) but turns
//! about that world axis (`p2physics::Actor::spin_axis`), from the bone orientation CutThisLimb /
//! DetachFromBone left it in. A hit from the opposite side spins it the opposite way.
//!
//! `P2GAME_GIB_TRACE=1` prints each launch.

use glam::Vec3;
use std::cell::RefCell;
use std::collections::HashMap;
use uscript::vm::{ObjId, Value, Vm};

thread_local! {
    /// Last damage per actor: (HitLocation, Momentum).
    static HITS: RefCell<HashMap<ObjId, (Vec3, Vec3)>> = RefCell::new(HashMap::new());
}

/// Installs the call watches for this VM (idempotent) and forgets the previous level's hits.
pub fn install(vm: &mut Vm) {
    crate::damage_bridge::reset();
    HITS.with(|h| h.borrow_mut().clear());
    vm.watch_calls("TakeDamage", take_damage);
    vm.watch_calls("GiveMomentum", give_momentum);
}

fn vec_arg(a: &[Value], k: usize) -> Option<Vec3> {
    match a.get(k) {
        Some(Value::Vector(v)) => Some(Vec3::from(*v)),
        _ => None,
    }
}

fn take_damage(vm: &mut Vm, me: Option<ObjId>, a: &mut [Value], after: bool) {
    // TakeDamage(Damage, InstigatedBy, HitLocation, Momentum, DamageType), recorded on entry
    // (script rescales Momentum by Mass inside).
    let (Some(me), false) = (me, after) else { return };
    if crate::damage_bridge::intercept(me, a) { return; }
    let (Some(hit), Some(mom)) = (vec_arg(a, 2), vec_arg(a, 3)) else { return };
    if mom.length_squared() < 1e-6 || !vm.class_is_a(vm.objects[me].class, "Pawn") && !vm.class_is_a(vm.objects[me].class, "Head") {
        return;
    }
    HITS.with(|h| h.borrow_mut().insert(me, (hit, mom)));
}

/// Unit spin axis for a part at `centre` hit at `hit` with momentum `mom`; when the hit is at
/// the centre, the lever is from the body centre `body` instead.
pub fn torque_axis(centre: Vec3, hit: Vec3, mom: Vec3, body: Option<Vec3>) -> Option<Vec3> {
    let dir = mom.normalize_or_zero();
    let lever = |p: Vec3| {
        let r = p - centre;
        // Only the lever part perpendicular to the push makes torque.
        r - dir * r.dot(dir)
    };
    let mut r = lever(hit);
    if r.length() < 2.0 {
        if let Some(b) = body {
            r = -lever(b);
        }
    }
    let t = r.cross(dir);
    (t.length() > 1e-3).then(|| t.normalize())
}

fn give_momentum(vm: &mut Vm, me: Option<ObjId>, _a: &mut [Value], after: bool) {
    let (Some(me), true) = (me, after) else { return };
    if !vm.get_prop(me, "bFixedRotationDir").is_some_and(|v| v.as_bool()) {
        return;
    }
    let owner = vm.get_prop(me, "Owner").and_then(|v| v.as_obj());
    let body = vm.get_prop(me, "MyBody").and_then(|v| v.as_obj()).or(owner);
    let hit = HITS.with(|h| {
        let h = h.borrow();
        body.and_then(|b| h.get(&b).copied()).or_else(|| h.get(&me).copied())
    });
    let Some((hit, mom)) = hit else { return };
    let centre = Vec3::from(vm.actor_loc(me));
    let body_loc = body.map(|b| Vec3::from(vm.actor_loc(b)));
    let Some(axis) = torque_axis(centre, hit, mom, body_loc) else { return };
    let Some(ph) = crate::physics::current() else { return };
    ph.register(vm, me);
    let Some(id) = ph.bridge.borrow().phys(me) else { return };
    if let Some(a) = ph.world.borrow_mut().get_mut(id) {
        a.spin_axis = Some(axis);
    }
    if std::env::var_os("P2GAME_GIB_TRACE").is_some() {
        println!(
            "GIB {} rot {:?} rate {:?} hit {hit:.0?} mom {:.2?} centre {centre:.0?} axis {axis:.2?}",
            vm.object_name(me),
            vm.actor_rot(me),
            vm.get_prop(me, "RotationRate").map(|v| v.as_rot()),
            mom.normalize_or_zero()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::torque_axis;
    use glam::Vec3;

    #[test]
    fn opposite_hits_spin_opposite_ways() {
        let c = Vec3::ZERO;
        let hit = Vec3::new(0.0, 0.0, 10.0);
        let a = torque_axis(c, hit, Vec3::X, None).unwrap();
        let b = torque_axis(c, hit, -Vec3::X, None).unwrap();
        assert!(a.dot(b) < -0.99, "{a} {b}");
        // Pushed +X above the centre: the top goes +X, i.e. a rotation about +Y.
        assert!(a.dot(Vec3::Y) > 0.99, "{a}");
        // Hit at the centre: lever from the body centre (below), the part's far end leads.
        let c2 = torque_axis(c, c, Vec3::X, Some(Vec3::new(0.0, 0.0, -30.0))).unwrap();
        assert!(c2.dot(Vec3::Y) > 0.99, "{c2}");
    }
}
