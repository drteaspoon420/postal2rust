//! Bevy integration: a resource owning the audio engine (and the device stream when available),
//! a listener component (usually on the camera) and emitter components for actors.
//!
//! Bevy space is the p2view convention: `bevy = (ux, uz, uy) / uu_per_unit` (Unreal is
//! left-handed Z-up, Bevy right-handed Y-up; the axis swap converts handedness).

use crate::SharedEngine;
use crate::engine::{ActorId, AmbientDesc, AudioEngine};
use crate::spatial::Listener;
use bevy::prelude::*;
use std::sync::{Arc, Mutex};

pub struct SoundPlugin {
    /// Unreal units per Bevy unit (52.5 in p2view).
    pub uu_per_unit: f32,
    /// Open the default output device (false: engine only, nothing audible).
    pub output: bool,
}

impl Default for SoundPlugin {
    fn default() -> Self {
        Self { uu_per_unit: 52.5, output: true }
    }
}

/// The audio engine shared with the audio thread.
#[derive(Resource, Clone)]
pub struct Sound {
    pub engine: SharedEngine,
    pub uu_per_unit: f32,
    /// True when a device stream is running.
    pub live: bool,
}

impl Sound {
    pub fn with<R>(&self, f: impl FnOnce(&mut AudioEngine) -> R) -> R {
        let mut e = self.engine.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut e)
    }

    /// Bevy position to Unreal units.
    pub fn to_uu(&self, p: Vec3) -> glam::Vec3 {
        glam::Vec3::new(p.x, p.z, p.y) * self.uu_per_unit
    }
}

/// Keeps the OS stream alive (not Send on every platform, so stored as a non-send resource).
struct OutputHolder(#[allow(dead_code)] Option<crate::device::AudioOutput>);

/// Marks the entity whose transform is the listener (one per world).
#[derive(Component, Default)]
pub struct SoundListener;

/// An actor that emits sound. Position follows the entity's `GlobalTransform`; an ambient sound
/// (Actor.AmbientSound) loops while the listener is in range.
#[derive(Component, Clone)]
pub struct SoundEmitter {
    pub actor: ActorId,
    pub ambient: Option<AmbientDesc>,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        let mut output = None;
        if self.output {
            match crate::device::AudioOutput::open() {
                Ok(o) => output = Some(o),
                Err(e) => warn!("p2sound: {e}; continuing silently"),
            }
        }
        let live = output.is_some();
        let engine = output.as_ref().map(|o| o.engine.clone()).unwrap_or_else(|| Arc::new(Mutex::new(AudioEngine::new(48000))));
        app.insert_resource(Sound { engine, uu_per_unit: self.uu_per_unit, live })
            .insert_non_send(OutputHolder(output))
            .add_systems(PostUpdate, (sync_emitters, sync_listener).after(TransformSystems::Propagate));
        app.add_observer(on_emitter_removed);
    }
}

fn sync_listener(sound: Res<Sound>, q: Query<&GlobalTransform, With<SoundListener>>) {
    let Some(t) = q.iter().next() else { return };
    let pos = sound.to_uu(t.translation());
    let f = t.forward();
    let u = t.up();
    let l = Listener { position: pos, forward: glam::Vec3::new(f.x, f.z, f.y), up: glam::Vec3::new(u.x, u.z, u.y) };
    sound.with(|e| e.set_listener(l));
}

fn sync_emitters(sound: Res<Sound>, added: Query<(&SoundEmitter, &GlobalTransform), Changed<SoundEmitter>>, moved: Query<(&SoundEmitter, &GlobalTransform), Changed<GlobalTransform>>) {
    if added.is_empty() && moved.is_empty() {
        return;
    }
    sound.with(|e| {
        for (em, t) in &added {
            let p = sound.to_uu(t.translation());
            e.set_ambient(em.actor, em.ambient.clone().map(|mut a| {
                a.location = p;
                a
            }));
            e.set_actor_location(em.actor, p);
        }
        for (em, t) in &moved {
            e.set_actor_location(em.actor, sound.to_uu(t.translation()));
        }
    });
}

fn on_emitter_removed(ev: On<Remove, SoundEmitter>, q: Query<&SoundEmitter>, sound: Res<Sound>) {
    if let Ok(em) = q.get(ev.entity) {
        sound.with(|e| e.remove_actor(em.actor));
    }
}
