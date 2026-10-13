//! UE2-style audio for POSTAL 2 content.
//!
//! * [`engine::AudioEngine`]: voices keyed by (actor, slot) with UE2 override rules, a voice
//!   limit with priority stealing, distance attenuation/pan, occlusion, actor ambient sounds,
//!   music with fades, pitch through resampling. Renders stereo f32 with [`engine::AudioEngine::render`]
//!   so it works headless (tests, WAV capture) as well as on a device.
//! * [`load::SoundCache`]: decodes `Sound` objects (resolving `ProceduralSound`/`SoundGroup`) and
//!   loose music files.
//! * `device` (feature "device"): real-time output through rodio/cpal.
//! * `bevy_plugin` (feature "bevy"): listener/emitter components.

pub mod engine;
pub mod load;
pub mod spatial;

#[cfg(feature = "device")]
pub mod device;

#[cfg(feature = "bevy")]
pub mod bevy_plugin;

pub use engine::{ActorId, AmbientDesc, AudioEngine, Emitter, OcclusionTrace, PlayParams, SoundData, SoundSlot, VoiceId, VoiceInfo};
pub use load::{Resolved, SoundCache};
pub use spatial::{AttenuationParams, Listener};

/// The engine shared between the game thread and the audio callback.
pub type SharedEngine = std::sync::Arc<std::sync::Mutex<AudioEngine>>;

/// Converts an Unreal byte volume (`SoundVolume`, 0..255) and the actor's radius into
/// [`AmbientDesc`] form. Unset properties take Actor defaults (radius 64, volume 128, pitch 64);
/// the AmbientSound class overrides SoundVolume to 190 in its defaults.
pub fn ambient_desc(sound: std::sync::Arc<SoundData>, location: glam::Vec3, radius: Option<f32>, volume: Option<u8>, pitch: Option<u8>) -> AmbientDesc {
    AmbientDesc { sound, location, radius: radius.unwrap_or(64.0), volume: volume.unwrap_or(128), pitch: pitch.unwrap_or(64) }
}

#[cfg(test)]
mod tests;
