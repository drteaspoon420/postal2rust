//! Host side of animation notifies: a game/viewer implements [`NotifyHost`] and passes it to
//! [`NotifyDispatcher`], which turns [`AnimEvent::Notify`] events into concrete requests
//! (play a sound at the actor, spawn an effect at a bone with its offset, destroy tagged
//! effects, call a script function by name). Bone placement is computed here from the
//! animation runtime so hosts only deal with actor-space transforms.

use crate::notify::{NotifyKind, ObjHandle};
use crate::runtime::{AnimEvent, AnimEventSink, AnimInstance, Coords, Rotator, mat3_to_rotator, rotator_mat3};
use glam::{Mat3, Vec3};

/// Placement of a spawned effect, in the actor's local space (multiply by the actor transform).
#[derive(Debug, Clone)]
pub struct EffectSpawn {
    pub effect_class: Option<ObjHandle>,
    /// Bone the effect is placed at (and attached to when `attach`).
    pub bone: Option<String>,
    /// Actor-space location (bone origin plus the offset expressed in bone axes).
    pub location: Vec3,
    /// Actor-space rotation (bone orientation composed with OffsetRotation).
    pub rotation: Rotator,
    /// Offsets as authored, relative to the bone (for hosts that attach).
    pub offset_location: Vec3,
    pub offset_rotation: Rotator,
    pub attach: bool,
    pub tag: Option<String>,
    pub scale: f32,
}

/// Engine services a notify can call. Every method has a no-op default so hosts implement only
/// what they support.
pub trait NotifyHost {
    /// `AnimNotify_Sound`: the owner plays `sound` (Radius 0 = default radius).
    fn play_sound(&mut self, _sound: &ObjHandle, _volume: f32, _radius: f32) {}
    /// `AnimNotify_Effect`.
    fn spawn_effect(&mut self, _spawn: &EffectSpawn) {}
    /// `AnimNotify_DestroyEffect`: destroy effects spawned with `tag` (None destroys untagged).
    fn destroy_effect(&mut self, _tag: Option<&str>, _expire_particles: bool) {}
    /// `AnimNotify_Script` and old-style function notifies: call the named function on the
    /// owning actor's script.
    fn call_function(&mut self, _name: &str) {}
    /// `AnimNotify_Scripted`: forwarded to the actor's scripted sequence.
    fn scripted_event(&mut self, _event: &str) {}
    /// `AnimNotify_Trigger`: trigger actors whose Tag matches.
    fn trigger_event(&mut self, _event: &str) {}
    /// End of the sequence on a channel (`AnimEnd(Channel)`).
    fn anim_end(&mut self, _channel: usize) {}
    /// Notify classes not modelled above.
    fn other(&mut self, _class: &str) {}
}

/// Collects events from [`AnimInstance::tick`] and later dispatches them with access to the
/// instance (needed for bone placement). Use as the tick's event sink, then call
/// [`NotifyDispatcher::dispatch`].
#[derive(Default)]
pub struct NotifyDispatcher {
    pub pending: Vec<AnimEvent>,
}

impl AnimEventSink for NotifyDispatcher {
    fn event(&mut self, e: AnimEvent) {
        self.pending.push(e);
    }
}

impl NotifyDispatcher {
    /// Ticks `inst` and dispatches everything it produced.
    pub fn tick(&mut self, inst: &mut AnimInstance, dt: f32, host: &mut dyn NotifyHost) {
        inst.tick(dt, self);
        self.dispatch(inst, host);
    }

    pub fn dispatch(&mut self, inst: &mut AnimInstance, host: &mut dyn NotifyHost) {
        for e in std::mem::take(&mut self.pending) {
            dispatch_event(inst, &e, host);
        }
    }
}

/// Dispatches one event, reading bone placement from `inst`.
pub fn dispatch_event(inst: &mut AnimInstance, e: &AnimEvent, host: &mut dyn NotifyHost) {
    dispatch_event_with(e, &mut |b| inst.get_bone_coords(b), host)
}

/// Dispatches one event with a caller-supplied actor-space bone lookup.
pub fn dispatch_event_with(e: &AnimEvent, bone_coords: &mut dyn FnMut(&str) -> Option<Coords>, host: &mut dyn NotifyHost) {
    match e {
        AnimEvent::AnimEnd { channel } => host.anim_end(*channel),
        AnimEvent::Notify { notify, .. } => match &notify.kind {
            NotifyKind::Sound { sound, volume, radius } => {
                if let Some(s) = sound {
                    host.play_sound(s, *volume, *radius);
                }
            }
            NotifyKind::Effect { effect_class, bone, offset_location, offset_rotation, attach, tag, scale } => {
                let coords = bone.as_deref().and_then(|b| bone_coords(b));
                let spawn = effect_spawn(coords, effect_class.clone(), bone.clone(), Vec3::from(*offset_location), *offset_rotation, *attach, tag.clone(), *scale);
                host.spawn_effect(&spawn);
            }
            NotifyKind::DestroyEffect { tag, expire_particles } => host.destroy_effect(tag.as_deref(), *expire_particles),
            NotifyKind::Script { notify_name } => {
                if !notify_name.is_empty() {
                    host.call_function(notify_name)
                }
            }
            NotifyKind::Function { name } => {
                if !name.is_empty() && !name.eq_ignore_ascii_case("None") {
                    host.call_function(name)
                }
            }
            NotifyKind::Scripted { event_name } => host.scripted_event(event_name),
            NotifyKind::Trigger { event_name } => host.trigger_event(event_name),
            NotifyKind::Other { class, .. } => host.other(class),
        },
    }
}

/// Places an effect: with a bone, the offset is expressed in the bone's axes and the rotation
/// is the bone orientation composed with OffsetRotation; without one, the offset is in actor
/// space.
#[allow(clippy::too_many_arguments)]
pub fn effect_spawn(bone_coords: Option<Coords>, effect_class: Option<ObjHandle>, bone: Option<String>, offset_location: Vec3, offset_rotation: Rotator, attach: bool, tag: Option<String>, scale: f32) -> EffectSpawn {
    let off_m = rotator_mat3(offset_rotation);
    let (location, rot_m) = match bone_coords {
        Some(c) => {
            let axes = Mat3::from_cols(c.x_axis.normalize_or_zero(), c.y_axis.normalize_or_zero(), c.z_axis.normalize_or_zero());
            (c.origin + axes * offset_location, axes * off_m)
        }
        None => (offset_location, off_m),
    };
    EffectSpawn { effect_class, bone, location, rotation: mat3_to_rotator(rot_m), offset_location, offset_rotation, attach, tag, scale }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Notify;

    #[test]
    fn effect_offset_in_bone_axes() {
        // Bone at (10,0,0) turned 90 degrees in yaw: its X axis is +Y, its Y axis is -X.
        let c = Coords { origin: Vec3::new(10.0, 0.0, 0.0), x_axis: Vec3::Y, y_axis: -Vec3::X, z_axis: Vec3::Z };
        let s = effect_spawn(Some(c), None, Some("b".into()), Vec3::new(5.0, 0.0, 1.0), [0, 0, 0], true, None, 1.0);
        assert!((s.location - Vec3::new(10.0, 5.0, 1.0)).length() < 1e-4, "{:?}", s.location);
        assert_eq!(s.rotation, [0, 16384, 0]);
        // Offset rotation composes after the bone: yaw 90 + 90 = 180.
        let s = effect_spawn(Some(c), None, None, Vec3::ZERO, [0, 16384, 0], false, None, 1.0);
        assert_eq!(s.rotation[1].rem_euclid(65536), 32768, "{:?}", s.rotation);
        // No bone: actor space.
        let s = effect_spawn(None, None, None, Vec3::new(1.0, 2.0, 3.0), [0, 0, 0], false, None, 1.0);
        assert_eq!(s.location, Vec3::new(1.0, 2.0, 3.0));
    }

    #[derive(Default)]
    struct Rec(Vec<String>);
    impl NotifyHost for Rec {
        fn call_function(&mut self, n: &str) {
            self.0.push(format!("call {n}"));
        }
        fn destroy_effect(&mut self, t: Option<&str>, _: bool) {
            self.0.push(format!("destroy {t:?}"));
        }
        fn spawn_effect(&mut self, s: &EffectSpawn) {
            self.0.push(format!("effect {:?} {:?}", s.bone, s.location));
        }
        fn trigger_event(&mut self, e: &str) {
            self.0.push(format!("trigger {e}"));
        }
        fn anim_end(&mut self, c: usize) {
            self.0.push(format!("end {c}"));
        }
    }

    #[test]
    fn routes_kinds() {
        let ev = |kind| AnimEvent::Notify { channel: 0, sequence: "s".into(), notify: Notify { time: 0.0, kind } };
        let events = [
            ev(NotifyKind::Script { notify_name: "FireWeapon".into() }),
            ev(NotifyKind::Function { name: "PlayFootstep".into() }),
            ev(NotifyKind::Function { name: "None".into() }),
            ev(NotifyKind::DestroyEffect { tag: Some("muzzle".into()), expire_particles: true }),
            ev(NotifyKind::Trigger { event_name: "door".into() }),
            ev(NotifyKind::Effect { effect_class: None, bone: Some("hand".into()), offset_location: [1.0, 0.0, 0.0], offset_rotation: [0; 3], attach: false, tag: None, scale: 1.0 }),
            AnimEvent::AnimEnd { channel: 2 },
        ];
        let mut h = Rec::default();
        let mut bones = |b: &str| (b == "hand").then_some(Coords { origin: Vec3::new(0.0, 0.0, 50.0), x_axis: Vec3::X, y_axis: Vec3::Y, z_axis: Vec3::Z });
        for e in &events {
            dispatch_event_with(e, &mut bones, &mut h);
        }
        assert_eq!(h.0, ["call FireWeapon", "call PlayFootstep", "destroy Some(\"muzzle\")", "trigger door", "effect Some(\"hand\") Vec3(1.0, 0.0, 50.0)", "end 2"]);
    }
}
