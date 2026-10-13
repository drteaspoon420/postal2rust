//! Animation notifies decoded from the `AnimNotify_*` objects referenced by MeshAnimation
//! sequences. Times are normalized (0..1 of the sequence), as stored in the .ukx files.

use upkg::library::{Library, Obj};
use upkg::skeletal::MeshAnimation;
use upkg::{ObjRef, Package, Properties, begin_object};
use std::sync::Arc;

/// A resolved object reference (package + export) with a readable Debug form.
#[derive(Clone)]
pub struct ObjHandle(pub Obj);

impl std::fmt::Debug for ObjHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.pkg.full_path(ObjRef::Export(self.0.export)))
    }
}

impl ObjHandle {
    /// Dotted path, e.g. "WeaponSounds.shotgun_ejectshell".
    pub fn path(&self) -> String {
        self.0.pkg.full_path(ObjRef::Export(self.0.export))
    }
}

#[derive(Debug, Clone)]
pub enum NotifyKind {
    /// Plays a sound (`AnimNotify_Sound`).
    Sound { sound: Option<ObjHandle>, volume: f32, radius: f32 },
    /// Spawns an effect actor (`AnimNotify_Effect`), optionally attached to a bone.
    Effect { effect_class: Option<ObjHandle>, bone: Option<String>, offset_location: [f32; 3], offset_rotation: [i32; 3], attach: bool, tag: Option<String>, scale: f32 },
    /// Destroys effects spawned with a tag (`AnimNotify_DestroyEffect`).
    DestroyEffect { tag: Option<String>, expire_particles: bool },
    /// Calls a script function on the actor (`AnimNotify_Script`).
    Script { notify_name: String },
    /// Fires a scripted-sequence event (`AnimNotify_Scripted`).
    Scripted { event_name: String },
    /// Triggers an event (`AnimNotify_Trigger`).
    Trigger { event_name: String },
    /// Old-style notify: a function name stored directly on the sequence.
    Function { name: String },
    /// Any other notify class: class name and its properties.
    Other { class: String, props: Properties },
}

#[derive(Debug, Clone)]
pub struct Notify {
    /// Normalized time in the sequence (0..1).
    pub time: f32,
    pub kind: NotifyKind,
}

fn name_prop(p: &Properties, k: &str) -> Option<String> {
    match p.get(k)? {
        upkg::Value::Name(n) if !n.eq_ignore_ascii_case("None") => Some(n.clone()),
        _ => None,
    }
}

/// Decodes one notify object referenced from `pkg`.
pub fn decode_notify(lib: Option<&Library>, pkg: &Arc<Package>, r: ObjRef, function: &str, time: f32) -> Notify {
    let resolve = |p: &Properties, k: &str| -> Option<ObjHandle> {
        let o = p.object(k).filter(|o| *o != ObjRef::Null)?;
        lib.and_then(|l| l.resolve(pkg, o)).map(ObjHandle)
    };
    let kind = match r {
        ObjRef::Export(e) => {
            let class = pkg.export_class(e).to_string();
            let props = begin_object(pkg, e).map(|(_, p)| p).unwrap_or_default();
            match class.to_ascii_lowercase().as_str() {
                "animnotify_sound" => NotifyKind::Sound { sound: resolve(&props, "Sound"), volume: props.float("Volume").unwrap_or(1.0), radius: props.float("Radius").unwrap_or(0.0) },
                "animnotify_effect" => NotifyKind::Effect {
                    effect_class: resolve(&props, "EffectClass"),
                    bone: name_prop(&props, "Bone"),
                    offset_location: props.vector("OffsetLocation").unwrap_or([0.0; 3]),
                    offset_rotation: props.rotator("OffsetRotation").unwrap_or([0; 3]),
                    attach: props.bool("Attach").unwrap_or(false),
                    tag: name_prop(&props, "Tag"),
                    scale: props.float("DrawScale").or_else(|| props.float("Scale")).unwrap_or(1.0),
                },
                "animnotify_destroyeffect" => NotifyKind::DestroyEffect { tag: name_prop(&props, "DestroyTag"), expire_particles: props.bool("bExpireParticles").unwrap_or(true) },
                "animnotify_script" => NotifyKind::Script { notify_name: name_prop(&props, "NotifyName").unwrap_or_default() },
                "animnotify_scripted" => NotifyKind::Scripted { event_name: name_prop(&props, "EventName").unwrap_or_default() },
                "animnotify_trigger" => NotifyKind::Trigger { event_name: name_prop(&props, "EventName").unwrap_or_default() },
                _ => NotifyKind::Other { class, props },
            }
        }
        // Notify objects living in another package (animation sets shared between packages):
        // resolve the import and decode the target export.
        ObjRef::Import(_) => match lib.and_then(|l| l.resolve(pkg, r)) {
            Some(o) if !Arc::ptr_eq(&o.pkg, pkg) => {
                return decode_notify(lib, &o.pkg.clone(), ObjRef::Export(o.export), function, time);
            }
            _ => NotifyKind::Other { class: pkg.class_of(r).to_string(), props: Properties::default() },
        },
        ObjRef::Null => NotifyKind::Function { name: function.to_string() },
    };
    Notify { time, kind }
}

/// Decodes all notifies of every sequence of an animation set (sorted by time per sequence).
pub fn decode_anim_notifies(lib: Option<&Library>, pkg: &Arc<Package>, anim: &MeshAnimation) -> Vec<Vec<Notify>> {
    anim.sequences
        .iter()
        .map(|s| {
            let mut v: Vec<Notify> = s.notifies.iter().map(|n| decode_notify(lib, pkg, n.notify, &n.function, n.time)).collect();
            v.sort_by(|a, b| a.time.total_cmp(&b.time));
            v
        })
        .collect()
}
