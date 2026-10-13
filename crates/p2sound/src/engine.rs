//! Audio engine core: sound cache, voices with UE2 slot/override/priority rules, ambient sounds,
//! music, and a stereo mixer that renders into an f32 buffer. Independent of any output device:
//! [`AudioEngine::render`] is called by the device callback (see `device`) or by tests.

use crate::spatial::{AttenuationParams, Listener, distance_gain, pan, pan_gains, priority};
use glam::Vec3;
use std::collections::HashMap;
use std::sync::Arc;

/// Identifies the actor that plays a sound (any host id, e.g. an object index).
pub type ActorId = u64;

/// `ESoundSlot` from Actor.uc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SoundSlot {
    None = 0,
    Misc = 1,
    Pain = 2,
    Interact = 3,
    Ambient = 4,
    Talk = 5,
    Interface = 6,
}

impl SoundSlot {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Misc,
            2 => Self::Pain,
            3 => Self::Interact,
            4 => Self::Ambient,
            5 => Self::Talk,
            6 => Self::Interface,
            _ => Self::None,
        }
    }
}

/// Decoded audio ready for mixing (mono or stereo f32).
#[derive(Debug)]
pub struct SoundData {
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
    /// Loop region in frames (start, end inclusive) from the file's `smpl` chunk.
    pub loop_frames: Option<(u32, u32)>,
}

impl SoundData {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    /// `GetSoundDuration`.
    pub fn duration(&self) -> f32 {
        self.frames() as f32 / self.sample_rate.max(1) as f32
    }

    pub fn from_pcm(name: &str, pcm: &upkg::sound::Pcm) -> Self {
        Self { name: name.to_string(), sample_rate: pcm.sample_rate, channels: pcm.channels.clamp(1, 2), samples: to_stereo_or_mono(pcm), loop_frames: pcm.loop_frames }
    }

    /// Sample of channel `c` at a fractional frame (linear interpolation).
    #[inline]
    fn sample(&self, frame: f64, c: usize) -> f32 {
        let ch = self.channels as usize;
        let i = frame as usize;
        let f = (frame - i as f64) as f32;
        let n = self.frames();
        if i >= n {
            return 0.0;
        }
        let c = c.min(ch - 1);
        let a = self.samples[i * ch + c];
        let b = if i + 1 < n { self.samples[(i + 1) * ch + c] } else { a };
        a + (b - a) * f
    }
}

fn to_stereo_or_mono(pcm: &upkg::sound::Pcm) -> Vec<f32> {
    let ch = pcm.channels.max(1) as usize;
    if ch <= 2 {
        return pcm.samples.iter().map(|&s| s as f32 / 32768.0).collect();
    }
    // Down-mix >2 channels to stereo (first two channels).
    pcm.samples.chunks(ch).flat_map(|f| [f[0] as f32 / 32768.0, f[1] as f32 / 32768.0]).collect()
}

/// Where a voice is heard from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Emitter {
    /// Non-positional (UI, music-like): full volume, centred.
    Flat,
    /// Fixed position.
    At(Vec3),
    /// Follows an actor; the host updates positions with [`AudioEngine::set_actor_location`].
    Actor(ActorId),
}

/// Parameters of `PlaySound` / `PlayOwnedSound` / `DemoPlaySound`.
#[derive(Debug, Clone, Copy)]
pub struct PlayParams {
    pub slot: SoundSlot,
    /// 0..1 (scripts pass TransientSoundVolume when omitted).
    pub volume: f32,
    /// Do not interrupt a sound already playing in this actor's slot.
    pub no_override: bool,
    /// 0 = default radius.
    pub radius: f32,
    /// 0.5..2 (0 = 1).
    pub pitch: f32,
    /// false: no distance attenuation (heard everywhere, still panned).
    pub attenuate: bool,
    pub max_priority: bool,
    /// Loop (ambient sounds).
    pub looping: bool,
}

impl Default for PlayParams {
    fn default() -> Self {
        Self { slot: SoundSlot::None, volume: 1.0, no_override: false, radius: 0.0, pitch: 1.0, attenuate: true, max_priority: false, looping: false }
    }
}

/// Answers "is the path from the sound to the listener blocked?" (UE2 traces BSP/static meshes
/// and, when blocked, shrinks the effective radius).
pub trait OcclusionTrace: Send {
    fn blocked(&self, from: Vec3, to: Vec3) -> bool;
}

/// Handle of a playing voice.
pub type VoiceId = u64;

/// Occlusion traces per rendered block (a device block is ~5 ms).
const OCCLUSION_TRACES_PER_BLOCK: usize = 8;

struct Voice {
    id: VoiceId,
    sound: Arc<SoundData>,
    actor: Option<ActorId>,
    slot: SoundSlot,
    emitter: Emitter,
    params: PlayParams,
    /// Position in source frames.
    pos: f64,
    /// Current smoothed gains (left, right).
    gain: (f32, f32),
    priority: f32,
    ambient: bool,
    /// Volume fade for music (gain per second, target).
    fade: Option<(f32, f32)>,
    level: f32,
    music: bool,
    /// Music VolumeOverride (> 0: used instead of the user's music volume).
    music_override: f32,
    occluded: bool,
    paused: bool,
    /// Paused by [`AudioEngine::set_world_paused`] (resumed with the game).
    world_held: bool,
}

/// Ambient sound attached to an actor (Actor.AmbientSound / SoundRadius / SoundVolume / SoundPitch).
#[derive(Debug, Clone)]
pub struct AmbientDesc {
    pub sound: Arc<SoundData>,
    pub location: Vec3,
    /// SoundRadius (UU).
    pub radius: f32,
    /// SoundVolume byte (0..255, 128 = normal).
    pub volume: u8,
    /// SoundPitch byte (64 = normal).
    pub pitch: u8,
}

/// One voice as seen by [`AudioEngine::voice_infos`].
#[derive(Debug, Clone)]
pub struct VoiceInfo {
    pub id: VoiceId,
    pub name: String,
    pub actor: Option<ActorId>,
    pub ambient: bool,
    pub music: bool,
    pub looping: bool,
    pub volume: f32,
    pub radius: f32,
    /// Distance to the listener (None: flat).
    pub distance: Option<f32>,
    /// Last applied output gain (max of left/right, after attenuation and master volume).
    pub gain: f32,
    pub level: f32,
    pub paused: bool,
}

/// Summary of the mixer state (for HUD/tests).
#[derive(Debug, Clone, Default)]
pub struct MixStats {
    pub voices: usize,
    pub audible: usize,
    pub ambients: usize,
    pub stolen: usize,
    pub rejected: usize,
}

pub struct AudioEngine {
    /// Output sample rate.
    pub rate: u32,
    pub params: AttenuationParams,
    /// Maximum simultaneous voices (`Channels` in the ini).
    pub max_voices: usize,
    pub sound_volume: f32,
    pub music_volume: f32,
    pub listener: Listener,
    voices: Vec<Voice>,
    next_id: VoiceId,
    actors: HashMap<ActorId, Vec3>,
    ambients: HashMap<ActorId, AmbientDesc>,
    occlusion: Option<Box<dyn OcclusionTrace>>,
    occlusion_timer: f32,
    /// Next voice index to re-trace for occlusion.
    occlusion_cursor: usize,
    pub stats: MixStats,
    /// Distance under which pan is pulled toward the centre (UU).
    pub pan_spread: f32,
    /// The game is paused ([`AudioEngine::set_world_paused`]).
    world_paused: bool,
}

impl AudioEngine {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            params: AttenuationParams::default(),
            max_voices: 64,
            sound_volume: 1.0,
            music_volume: 0.8,
            listener: Listener::default(),
            voices: Vec::new(),
            next_id: 1,
            actors: HashMap::new(),
            ambients: HashMap::new(),
            occlusion: None,
            occlusion_timer: 0.0,
            occlusion_cursor: 0,
            stats: MixStats::default(),
            pan_spread: 128.0,
            world_paused: false,
        }
    }

    pub fn set_occlusion(&mut self, trace: Option<Box<dyn OcclusionTrace>>) {
        self.occlusion = trace;
    }

    pub fn set_listener(&mut self, l: Listener) {
        self.listener = l;
    }

    pub fn listener_position(&self) -> Vec3 {
        self.listener.position
    }

    pub fn set_actor_location(&mut self, actor: ActorId, pos: Vec3) {
        self.actors.insert(actor, pos);
        if let Some(a) = self.ambients.get_mut(&actor) {
            a.location = pos;
        }
    }

    /// Forgets an actor (Actor destroyed): its ambient sound stops, but one-shot voices it
    /// started keep playing where it was, as in UE2. PropBreakable.BlowThisUp plays the glass
    /// break and then Destroy()s a window without a broken mesh in the same tick; stopping its
    /// voices made every window break silent.
    pub fn remove_actor(&mut self, actor: ActorId) {
        let last = self.actors.remove(&actor);
        self.ambients.remove(&actor);
        self.voices.retain(|v| !(v.actor == Some(actor) && (v.ambient || v.params.looping)));
        for v in self.voices.iter_mut().filter(|v| v.actor == Some(actor)) {
            v.actor = None;
            v.emitter = match (v.emitter, last) {
                (Emitter::Actor(_), Some(p)) => Emitter::At(p),
                (e, _) => e,
            };
        }
    }

    fn emitter_pos(&self, e: Emitter) -> Option<Vec3> {
        match e {
            Emitter::Flat => None,
            Emitter::At(p) => Some(p),
            Emitter::Actor(a) => self.actors.get(&a).copied(),
        }
    }

    fn radius(&self, r: f32) -> f32 {
        if r > 0.0 { r } else { self.params.default_radius }
    }

    /// `Actor.PlaySound(Sound, Slot, Volume, bNoOverride, Radius, Pitch, Attenuate, ..., bMaxPriority)`
    /// played by `actor` at its current location. Returns the voice, or None when the sound was
    /// rejected (slot busy with bNoOverride, inaudible, or lower priority than every voice).
    pub fn play_sound(&mut self, actor: Option<ActorId>, sound: &Arc<SoundData>, p: PlayParams) -> Option<VoiceId> {
        let emitter = match actor {
            Some(a) => Emitter::Actor(a),
            None => Emitter::Flat,
        };
        self.start_voice(actor, sound, emitter, p, false)
    }

    /// Plays at a fixed location without an owning actor.
    pub fn play_sound_at(&mut self, sound: &Arc<SoundData>, pos: Vec3, p: PlayParams) -> Option<VoiceId> {
        self.start_voice(None, sound, Emitter::At(pos), p, false)
    }

    fn start_voice(&mut self, actor: Option<ActorId>, sound: &Arc<SoundData>, emitter: Emitter, mut p: PlayParams, music: bool) -> Option<VoiceId> {
        p.volume = p.volume.clamp(0.0, 1.0);
        if p.volume <= 0.0 || sound.frames() == 0 {
            self.stats.rejected += 1;
            return None;
        }
        p.pitch = if p.pitch <= 0.0 { 1.0 } else { p.pitch.clamp(0.5, 2.0) };
        p.radius = self.radius(p.radius);
        // Slot override: a sound in the same actor slot replaces the old one, unless bNoOverride
        // is set and the slot is busy (then the new sound is dropped). SLOT_None never overrides.
        if let (Some(a), slot) = (actor, p.slot) {
            if slot != SoundSlot::None {
                if let Some(i) = self.voices.iter().position(|v| v.actor == Some(a) && v.slot == slot && !v.ambient) {
                    if p.no_override {
                        self.stats.rejected += 1;
                        return None;
                    }
                    self.voices.remove(i);
                }
            }
        }
        let pos = self.emitter_pos(emitter);
        if p.attenuate {
            if let Some(pos) = pos {
                if pos.distance(self.listener.position) >= p.radius * self.params.max_radius_multiplier {
                    self.stats.rejected += 1;
                    return None;
                }
            }
        }
        let prio = priority(&self.listener, if p.attenuate { pos } else { None }, p.volume, p.radius, p.max_priority, &self.params);
        if self.voices.len() >= self.max_voices {
            // Steal the lowest-priority voice if the new one is more important.
            let (i, low) = self.voices.iter().enumerate().filter(|(_, v)| !v.music).min_by(|a, b| a.1.priority.total_cmp(&b.1.priority)).map(|(i, v)| (i, v.priority))?;
            if low >= prio {
                self.stats.rejected += 1;
                return None;
            }
            self.voices.remove(i);
            self.stats.stolen += 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.voices.push(Voice {
            id,
            sound: sound.clone(),
            actor,
            slot: p.slot,
            emitter,
            params: p,
            pos: 0.0,
            gain: (f32::NAN, f32::NAN),
            priority: prio,
            ambient: false,
            fade: None,
            level: 1.0,
            music,
            music_override: 0.0,
            occluded: false,
            paused: false,
            world_held: false,
        });
        Some(id)
    }

    /// Stops a voice (`StopSound` on a handle).
    pub fn stop_voice(&mut self, id: VoiceId) {
        self.voices.retain(|v| v.id != id);
    }

    /// Stops the sound an actor is playing in a slot.
    pub fn stop_slot(&mut self, actor: ActorId, slot: SoundSlot) {
        self.voices.retain(|v| !(v.actor == Some(actor) && v.slot == slot && !v.ambient));
    }

    /// `STOPSOUNDS`: stops every non-music voice.
    pub fn stop_all_sounds(&mut self) {
        self.voices.retain(|v| v.music);
    }

    /// Level change: drops every actor, ambient and non-music voice (flat music keeps playing;
    /// music played from a level actor goes with it).
    pub fn clear_level(&mut self) {
        self.voices.retain(|v| v.music && v.actor.is_none());
        self.ambients.clear();
        self.actors.clear();
    }

    /// Actors that currently own a voice or an ambient sound (their positions need updating).
    pub fn tracked_actors(&self) -> Vec<ActorId> {
        let mut v: Vec<ActorId> = self.voices.iter().filter_map(|v| v.actor).chain(self.ambients.keys().copied()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The ambient sound currently registered for an actor.
    pub fn ambient_of(&self, actor: ActorId) -> Option<&AmbientDesc> {
        self.ambients.get(&actor)
    }

    /// Snapshot of every voice (diagnostics): what is playing, how loud and from where.
    pub fn voice_infos(&self) -> Vec<VoiceInfo> {
        self.voices
            .iter()
            .map(|v| {
                let pos = self.emitter_pos(v.emitter);
                VoiceInfo {
                    id: v.id,
                    name: v.sound.name.clone(),
                    actor: v.actor,
                    ambient: v.ambient,
                    music: v.music,
                    looping: v.params.looping,
                    volume: v.params.volume,
                    radius: v.params.radius,
                    distance: pos.map(|p| p.distance(self.listener.position)),
                    gain: if v.gain.0.is_nan() { 0.0 } else { v.gain.0.max(v.gain.1) },
                    level: v.level,
                    paused: v.paused,
                }
            })
            .collect()
    }

    pub fn is_playing(&self, id: VoiceId) -> bool {
        self.voices.iter().any(|v| v.id == id)
    }

    /// Pauses or resumes a voice (movie soundtracks, menus).
    pub fn set_voice_paused(&mut self, id: VoiceId, paused: bool) {
        if let Some(v) = self.voices.iter_mut().find(|v| v.id == id) {
            v.paused = paused;
        }
    }

    /// Game pause: holds every non-music voice that is playing now and stops ambient sounds from
    /// (re)starting; sounds started while paused (menu clicks) play normally. `false` resumes
    /// the voices this paused.
    pub fn set_world_paused(&mut self, paused: bool) {
        if paused == self.world_paused {
            return;
        }
        self.world_paused = paused;
        for v in &mut self.voices {
            if !v.music {
                if paused && !v.paused {
                    v.paused = true;
                    v.world_held = true;
                } else if !paused && v.world_held {
                    v.paused = false;
                    v.world_held = false;
                }
            }
        }
    }

    pub fn world_paused(&self) -> bool {
        self.world_paused
    }

    /// Playback position of a voice in seconds of its source (None when finished).
    pub fn voice_time(&self, id: VoiceId) -> Option<f64> {
        self.voices.iter().find(|v| v.id == id).map(|v| v.pos / v.sound.sample_rate.max(1) as f64)
    }

    /// Sets (or clears with None) an actor's ambient sound.
    pub fn set_ambient(&mut self, actor: ActorId, desc: Option<AmbientDesc>) {
        match desc {
            Some(d) => {
                // A changed ambient restarts with the new parameters.
                self.voices.retain(|v| !(v.ambient && v.actor == Some(actor)));
                self.actors.insert(actor, d.location);
                self.ambients.insert(actor, d);
            }
            None => {
                self.ambients.remove(&actor);
                self.voices.retain(|v| !(v.ambient && v.actor == Some(actor)));
            }
        }
    }

    pub fn ambient_count(&self) -> usize {
        self.ambients.len()
    }

    /// Registered ambient sounds (diagnostics).
    pub fn ambients(&self) -> impl Iterator<Item = (&ActorId, &AmbientDesc)> {
        self.ambients.iter()
    }

    /// `PlayMusic(Song, FadeInTime, VolumeOverride)`: decoded music data; returns the song handle.
    /// `volume_override` > 0 replaces the user's music volume (the script's VolumeOverride).
    pub fn play_music(&mut self, song: &Arc<SoundData>, fade_in: f32, volume_override: f32) -> VoiceId {
        self.play_music_ext(song, None, fade_in, 1.0, 0.0, 1.0, volume_override)
    }

    /// `Actor.PlayMusicAttenuate(Song, FadeInTime, Volume, Radius, Pitch, VolumeOverride)`: music
    /// heard from `actor` (radios, club speakers, protest boom boxes), attenuated like a sound of
    /// that radius. `actor` None plays it flat (`PlayMusic`).
    #[allow(clippy::too_many_arguments)]
    pub fn play_music_ext(&mut self, song: &Arc<SoundData>, actor: Option<ActorId>, fade_in: f32, volume: f32, radius: f32, pitch: f32, volume_override: f32) -> VoiceId {
        let p = PlayParams {
            volume: volume.clamp(0.0, 1.0),
            attenuate: actor.is_some(),
            radius: self.radius(radius),
            pitch: if pitch <= 0.0 { 1.0 } else { pitch.clamp(0.5, 2.0) },
            looping: true,
            ..Default::default()
        };
        let id = self.next_id;
        self.next_id += 1;
        let level = if fade_in > 0.0 { 0.0 } else { 1.0 };
        let target = 1.0;
        self.voices.push(Voice {
            id,
            sound: song.clone(),
            actor,
            slot: SoundSlot::None,
            emitter: actor.map_or(Emitter::Flat, Emitter::Actor),
            params: p,
            pos: 0.0,
            gain: (f32::NAN, f32::NAN),
            priority: 1e9,
            ambient: false,
            fade: (fade_in > 0.0).then_some((target / fade_in, target)),
            level: if fade_in > 0.0 { level } else { target },
            music: true,
            music_override: if volume_override > 0.0 { volume_override.min(1.0) } else { 0.0 },
            occluded: false,
            paused: false,
            world_held: false,
        });
        id
    }

    /// `StopMusic(SongHandle, FadeOutTime)`.
    pub fn stop_music(&mut self, handle: VoiceId, fade_out: f32) {
        if let Some(v) = self.voices.iter_mut().find(|v| v.id == handle && v.music) {
            if fade_out <= 0.0 {
                v.level = 0.0;
                v.fade = Some((f32::INFINITY, 0.0));
            } else {
                v.fade = Some((v.level / fade_out, 0.0));
            }
        }
    }

    /// `StopAllMusic(FadeOutTime)`.
    pub fn stop_all_music(&mut self, fade_out: f32) {
        let ids: Vec<VoiceId> = self.voices.iter().filter(|v| v.music).map(|v| v.id).collect();
        for id in ids {
            self.stop_music(id, fade_out);
        }
    }

    /// Starts/stops ambient voices according to distance (UE2 re-evaluates every update).
    fn update_ambients(&mut self) {
        let lp = self.listener.position;
        let mult = self.params.max_radius_multiplier;
        // Stop ambients that went out of range or were changed.
        let ambients = &self.ambients;
        self.voices.retain(|v| {
            if !v.ambient {
                return true;
            }
            let Some(a) = v.actor.and_then(|id| ambients.get(&id)) else { return false };
            Arc::ptr_eq(&a.sound, &v.sound) && a.location.distance(lp) < a.radius * mult
        });
        let playing: Vec<ActorId> = self.voices.iter().filter(|v| v.ambient).filter_map(|v| v.actor).collect();
        let mut start = Vec::new();
        for (id, a) in &self.ambients {
            if !playing.contains(id) && a.location.distance(lp) < a.radius * mult {
                start.push((*id, a.clone()));
            }
        }
        for (id, a) in start {
            let p = PlayParams {
                slot: SoundSlot::Ambient,
                volume: (a.volume as f32 / 255.0).clamp(0.0, 1.0),
                radius: a.radius,
                pitch: a.pitch as f32 / 64.0,
                attenuate: true,
                looping: true,
                ..Default::default()
            };
            // With every voice busy, start_voice steals one, so the count does not grow: find the
            // new voice by id (a looping voice left unflagged would never stop and would restart
            // again on every update).
            if let Some(vid) = self.start_voice(Some(id), &a.sound, Emitter::Actor(id), p, false) {
                if let Some(v) = self.voices.iter_mut().find(|v| v.id == vid) {
                    v.ambient = true;
                    // Start ambient loops at a pseudo-random phase so identical sounds do not phase.
                    v.pos = (id.wrapping_mul(2654435761) % a.sound.frames().max(1) as u64) as f64;
                }
            }
        }
    }

    /// Renders `out.len() / 2` stereo frames (interleaved L, R) and advances all voices.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        if frames == 0 {
            return;
        }
        let dt = frames as f32 / self.rate as f32;
        // Paused game: ambient loops hold where they are (no stops or starts).
        if !self.world_paused {
            self.update_ambients();
        }
        // Occlusion is re-traced round-robin, a few voices per block, so a full voice table never
        // costs 64 level traces in one device callback.
        self.occlusion_timer -= dt;
        let budget = if self.occlusion.is_some() { OCCLUSION_TRACES_PER_BLOCK } else { 0 };
        let n_voices = self.voices.len().max(1);
        let start = self.occlusion_cursor % n_voices;
        self.occlusion_cursor = (start + budget) % n_voices;
        let listener = self.listener;
        let params = self.params;
        let mut audible = 0;
        let actors = &self.actors;
        let occl = self.occlusion.as_deref();
        for (vi, v) in self.voices.iter_mut().enumerate() {
            if v.paused {
                continue;
            }
            // Target gains.
            let pos = match v.emitter {
                Emitter::Flat => None,
                Emitter::At(p) => Some(p),
                Emitter::Actor(a) => actors.get(&a).copied(),
            };
            let retrace = (vi + n_voices - start) % n_voices < budget || v.gain.0.is_nan();
            if retrace {
                v.occluded = match (occl, pos) {
                    (Some(t), Some(p)) if v.params.attenuate => t.blocked(p, listener.position),
                    _ => false,
                };
            }
            let mut g = v.params.volume
                * if !v.music {
                    self.sound_volume
                } else if v.music_override > 0.0 {
                    v.music_override
                } else {
                    self.music_volume
                };
            let mut p = 0.0;
            if let Some(pos) = pos {
                if v.params.attenuate {
                    let r = if v.occluded { v.params.radius * params.occluded_radius_scale } else { v.params.radius };
                    g *= distance_gain(pos.distance(listener.position), r, &params);
                }
                p = pan(&listener, pos, self.pan_spread);
            }
            if let Some((rate, target)) = v.fade {
                if v.level < target {
                    v.level = (v.level + rate * dt).min(target);
                } else {
                    v.level = (v.level - rate * dt).max(target);
                }
            }
            g *= v.level;
            let (tl, tr) = if v.sound.channels == 2 && pos.is_none() {
                (g, g)
            } else {
                let (l, r) = pan_gains(p);
                // Equal-power pan normalized so a centred sound keeps unity gain.
                (g * l * std::f32::consts::SQRT_2, g * r * std::f32::consts::SQRT_2)
            };
            if v.gain.0.is_nan() {
                v.gain = (tl, tr);
            }
            if tl > 1e-4 || tr > 1e-4 || v.gain.0 > 1e-4 || v.gain.1 > 1e-4 {
                audible += 1;
            }
            // Mix with per-buffer gain ramp to avoid clicks.
            let step = v.sound.sample_rate as f64 / self.rate as f64 * v.params.pitch as f64;
            let n = v.sound.frames() as f64;
            let (loop_start, loop_end) = match v.sound.loop_frames {
                Some((s, e)) if v.params.looping && (e as f64) < n && s < e => (s as f64, e as f64 + 1.0),
                _ => (0.0, n),
            };
            let (gl0, gr0) = v.gain;
            let stereo = v.sound.channels == 2;
            for f in 0..frames {
                if v.pos >= loop_end {
                    if v.params.looping {
                        v.pos = loop_start + (v.pos - loop_end) % (loop_end - loop_start).max(1.0);
                    } else {
                        v.pos = f64::INFINITY;
                        break;
                    }
                }
                let t = f as f32 / frames as f32;
                let gl = gl0 + (tl - gl0) * t;
                let gr = gr0 + (tr - gr0) * t;
                let (l, r) = if stereo && pos.is_none() {
                    (v.sound.sample(v.pos, 0), v.sound.sample(v.pos, 1))
                } else {
                    let m = if stereo { 0.5 * (v.sound.sample(v.pos, 0) + v.sound.sample(v.pos, 1)) } else { v.sound.sample(v.pos, 0) };
                    (m, m)
                };
                out[f * 2] += l * gl;
                out[f * 2 + 1] += r * gr;
                v.pos += step;
            }
            v.gain = (tl, tr);
        }
        // Drop finished voices and silenced music.
        self.voices.retain(|v| v.pos.is_finite() && !(v.music && v.level <= 0.0 && v.fade.is_some_and(|f| f.1 == 0.0)));
        // Soft limiter: linear below 0.8, smoothly saturating to 1.0 above.
        for s in out.iter_mut() {
            let a = s.abs();
            if a > 0.8 {
                *s = s.signum() * (0.8 + 0.2 * ((a - 0.8) / 0.2).tanh());
            }
        }
        self.stats.voices = self.voices.len();
        self.stats.audible = audible;
        self.stats.ambients = self.ambients.len();
    }
}

/// Encodes interleaved stereo f32 as a 16-bit PCM WAV file.
pub fn stereo_to_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let pcm = upkg::sound::Pcm { sample_rate: rate, channels: 2, samples: samples.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect(), loop_frames: None };
    pcm.to_wav()
}
