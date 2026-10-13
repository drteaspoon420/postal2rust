//! Sound for the game host (p2sound).
//!
//! [`install`] registers a level hook that, for every level, pushes an [`AudioHost`] into
//! [`GameHost::ext`] and a frame hook into [`GameHost::frame_hooks`]:
//! - `PlaySound` / `PlayOwnedSound` / `DemoPlaySound` / `PlayFlashbangSound` (via
//!   [`Host::play_sound`], which also carries AnimNotify_Sound) resolve the Sound object through
//!   [`SoundCache`] (ProceduralSound / SoundGroup included) and start a voice owned by the actor in
//!   its slot; `GetSoundDuration` returns the decoded length.
//! - Music natives `PlayMusic` / `PlayMusicAttenuate` / `StopMusic` / `StopAllMusic` (Actor) and the
//!   `StopAllMusic` / `ClientHearSound` paths of PlayerController.
//! - Every frame: actor positions of voices and ambient sounds are refreshed, actors whose
//!   `AmbientSound` / `SoundRadius` / `SoundVolume` / `SoundPitch` changed are re-registered, and
//!   the listener follows the player's view (`ViewTarget`, else the pawn, else the controller),
//!   or the Matinee camera while a scene runs ([`set_listener_override`]).
//!
//! The engine lives in a [`SharedEngine`] so a windowed front end can feed it to an output device
//! (`AudioOutput`) while headless runs mix into nothing (or render to a WAV for checks).

use crate::host::GameHost;
use glam::Vec3;
use p2sound::engine::stereo_to_wav;
use p2sound::{AmbientDesc, AudioEngine, Listener, PlayParams, SharedEngine, SoundCache, SoundData, SoundSlot};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use uscript::host::{Host, NativeCall, SoundRequest};
use uscript::vm::{ObjId, Value, Vm};

/// Process-wide audio state shared by every level.
pub struct Audio {
    pub engine: SharedEngine,
    pub cache: RefCell<SoundCache>,
    pub game_dir: PathBuf,
    /// Music handles returned to script (handle -> voice).
    music: RefCell<HashMap<i32, u64>>,
    next_handle: RefCell<i32>,
    /// Listener forced by a running cutscene (location UU, rotation).
    listener_override: RefCell<Option<(Vec3, [i32; 3])>>,
    /// Ambient settings last registered per actor (to notice script changes).
    ambient_seen: RefCell<HashMap<ObjId, (String, u32, u8, u8)>>,
    /// Content library of the current level (set by the level hook).
    lib: RefCell<Option<Arc<upkg::library::Library>>>,
    pub stats: RefCell<AudioStats>,
    /// Frame counter for the throttled ambient/volume refresh.
    frame_no: std::cell::Cell<u32>,
    /// Collision world the occlusion trace currently uses.
    occlusion_world: RefCell<Option<Arc<p2collide::World>>>,
}

#[derive(Default, Debug, Clone)]
pub struct AudioStats {
    pub played: u64,
    pub unresolved: u64,
    pub music_started: u64,
    pub ambients: usize,
    pub voices: usize,
}

thread_local! {
    static AUDIO: RefCell<Option<Rc<Audio>>> = const { RefCell::new(None) };
}

/// The installed audio state (None before [`install`]).
pub fn audio() -> Option<Rc<Audio>> {
    AUDIO.with(|a| a.borrow().clone())
}

/// Installs sound for every following level. `engine`: an engine already attached to an output
/// device (windowed), or None for a silent engine (headless).
pub fn install(game_dir: PathBuf, engine: Option<SharedEngine>) -> Rc<Audio> {
    let engine = engine.unwrap_or_else(|| Arc::new(Mutex::new(AudioEngine::new(44100))));
    let a = Rc::new(Audio {
        engine,
        cache: RefCell::new(SoundCache::new()),
        game_dir,
        music: RefCell::new(HashMap::new()),
        next_handle: RefCell::new(1),
        listener_override: RefCell::new(None),
        ambient_seen: RefCell::new(HashMap::new()),
        lib: RefCell::new(None),
        stats: RefCell::new(AudioStats::default()),
        frame_no: std::cell::Cell::new(0),
        occlusion_world: RefCell::new(None),
    });
    AUDIO.with(|s| *s.borrow_mut() = Some(a.clone()));
    crate::host::register_level_hook(level_hook);
    a
}

/// Lets a cutscene put the listener at the camera (None: back to the player).
pub fn set_listener_override(v: Option<(Vec3, [i32; 3])>) {
    if let Some(a) = audio() {
        *a.listener_override.borrow_mut() = v;
    }
}

/// Game pause (`Level.Pauser`): world sounds playing now hold, ambient loops stop updating;
/// menu sounds started while paused still play (UE2 pauses the world's audio, not the UI's).
pub fn set_world_paused(paused: bool) {
    if let Some(a) = audio() {
        a.with(|e| e.set_world_paused(paused));
    }
}

#[cfg(feature = "window")]
thread_local! {
    /// The open OS output stream (kept alive for the process; dropping it stops sound).
    static OUTPUT: RefCell<Option<p2sound::device::AudioOutput>> = const { RefCell::new(None) };
}

/// Windowed front ends: opens the default audio device and installs sound mixing into it.
/// Falls back to a silent engine (with a message) when no device is available.
#[cfg(feature = "window")]
pub fn install_with_device(game_dir: PathBuf) -> Rc<Audio> {
    match p2sound::device::AudioOutput::open() {
        Ok(out) => {
            let engine = out.engine.clone();
            OUTPUT.with(|o| *o.borrow_mut() = Some(out));
            println!("audio: output device open");
            install(game_dir, Some(engine))
        }
        Err(e) => {
            eprintln!("audio: {e}; continuing silent");
            install(game_dir, None)
        }
    }
}

/// Closes the OS output stream. Call before the process exits: a cpal stream dropped during
/// thread-local teardown joins its (already terminated) audio thread and aborts the process.
#[cfg(feature = "window")]
pub fn shutdown_device() {
    if let Some(out) = OUTPUT.with(|o| o.borrow_mut().take()) {
        drop(out);
    }
}

fn level_hook(host: &GameHost, _vm: &mut Vm) {
    let Some(a) = audio() else { return };
    a.with(|e| e.clear_level());
    // Occlusion: UE2 traces the level (OCCLUSION_Default = BSP) from each sound to the listener
    // and plays blocked sounds with 0.35x their radius (ALAudio PlaySound/Update). The level's
    // collision is picked up on the first frames (`refresh_occlusion`): level hooks may run
    // before physics has built it.
    a.with(|e| e.set_occlusion(None));
    *a.occlusion_world.borrow_mut() = None;
    a.ambient_seen.borrow_mut().clear();
    a.frame_no.set(0);
    *a.lib.borrow_mut() = Some(host.lib.clone());
    host.ext.borrow_mut().push(Rc::new(AudioHost { audio: a.clone(), lib: host.lib.clone() }));
    let a2 = a.clone();
    host.frame_hooks.borrow_mut().push(Box::new(move |vm, _dt| a2.frame(vm)));
}

fn rot_axes(r: [i32; 3]) -> (Vec3, Vec3) {
    let m = p2anim::runtime::rotator_mat3(r);
    (m.x_axis, m.z_axis)
}

impl Audio {
    pub fn with<R>(&self, f: impl FnOnce(&mut AudioEngine) -> R) -> R {
        let mut e = self.engine.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut e)
    }

    /// Decodes a Sound object by path (`"Package.Group.Name"`), with procedural/group modifiers.
    pub fn load(&self, lib: &upkg::library::Library, path: &str) -> Option<p2sound::Resolved> {
        let obj = crate::host::resolve_path(lib, path, None)?;
        self.cache.borrow_mut().resolve(lib, &obj)
    }

    fn frame(&self, vm: &mut Vm) {
        // Listener: cutscene camera, else the local player's view.
        let listener = self.listener_override.borrow().or_else(|| player_view(vm));
        // Perf: the ambient scan walks every actor and the volume read goes through the console;
        // both change rarely, so refresh them every 8th frame (~130 ms at 60 fps).
        let n = self.frame_no.get();
        self.frame_no.set(n.wrapping_add(1));
        let full = n % 8 == 0;
        let ambients = if full { Some(scan_ambients(vm)) } else { None };
        if full {
            self.refresh_occlusion();
        }
        // The audio menu sets `ini:Engine.Engine.AudioDevice SoundVolume/MusicVolume`; the audio
        // device reads them live (ALAudioSubsystem).
        let ini_vol = |vm: &mut Vm, k: &str| vm.console_get_set(&format!("get ini:Engine.Engine.AudioDevice {k}")).and_then(|s| s.trim().parse::<f32>().ok()).map(|v| v.clamp(0.0, 1.0));
        let (sv, mv) = if full { (ini_vol(vm, "SoundVolume"), ini_vol(vm, "MusicVolume")) } else { (None, None) };
        self.with(|e| {
            if std::env::var_os("P2GAME_SOUND_TRACE").is_some() && (sv.is_some_and(|v| v != e.sound_volume) || mv.is_some_and(|v| v != e.music_volume)) {
                println!("AUDIO volume sound {:?} music {:?}", sv, mv);
            }
            if let Some(v) = sv {
                e.sound_volume = v;
            }
            if let Some(v) = mv {
                e.music_volume = v;
            }
            if let Some((p, r)) = listener {
                let (f, u) = rot_axes(r);
                e.set_listener(Listener { position: p, forward: f, up: u });
            }
            for id in e.tracked_actors() {
                let a = id as ObjId;
                match vm.objects.get(a) {
                    Some(o) if !o.deleted => e.set_actor_location(id, Vec3::from(vm.actor_loc(a))),
                    _ => e.remove_actor(id),
                }
            }
        });
        // Ambient sounds: register new/changed ones, drop removed ones.
        let Some(ambients) = ambients else { return };
        let mut seen = self.ambient_seen.borrow_mut();
        let lib = self.lib.borrow().clone();
        for (a, key) in &ambients {
            if seen.get(a) == Some(key) {
                continue;
            }
            let desc = lib.as_ref().and_then(|lib| self.load(lib, &key.0)).map(|res| AmbientDesc {
                sound: res.data,
                location: Vec3::from(vm.actor_loc(*a)),
                radius: f32::from_bits(key.1),
                volume: ((key.2 as f32 * res.volume).round() as i32).clamp(0, 255) as u8,
                pitch: ((key.3 as f32 * res.pitch).round() as i32).clamp(1, 255) as u8,
            });
            if desc.is_none() {
                self.stats.borrow_mut().unresolved += 1;
            }
            self.with(|e| e.set_ambient(*a as u64, desc));
            seen.insert(*a, key.clone());
        }
        let gone: Vec<ObjId> = seen.keys().filter(|a| !ambients.contains_key(a)).copied().collect();
        for a in gone {
            seen.remove(&a);
            self.with(|e| e.set_ambient(a as u64, None));
        }
        let mut st = self.stats.borrow_mut();
        st.ambients = seen.len();
        st.voices = self.with(|e| e.stats.voices);
    }

    /// Points the occlusion trace at the current level's collision once physics has it.
    fn refresh_occlusion(&self) {
        let cur = crate::physics::level_collision();
        let same = match (&*self.occlusion_world.borrow(), &cur) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let occl = cur.clone().map(|w| Box::new(LevelOcclusion(w)) as Box<dyn p2sound::OcclusionTrace>);
        self.with(|e| e.set_occlusion(occl));
        *self.occlusion_world.borrow_mut() = cur;
    }

    fn play(&self, lib: &upkg::library::Library, vm: &Vm, actor: Option<ObjId>, req: &SoundRequest) -> Option<u64> {
        if req.sound.is_empty() || req.sound.eq_ignore_ascii_case("None") {
            return None;
        }
        // Level setup (BeginPlay / PostBeginPlay / login, before the level's first tick): UE2
        // loads the map and runs actor setup before the client viewport and its audio are
        // attached, so these sounds are never heard. Without this every NPC handed a weapon at
        // start played WeaponSounds.weapon_select at load (FIDELITY #25).
        let level_time = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(1.0);
        if level_time <= 0.0 {
            if std::env::var_os("P2GAME_SOUND_TRACE").is_some() {
                println!("SOUND {} actor {:?} vol {:.2} t=0 LEVELSETUP", req.sound, actor.map(|a| vm.object_name(a)), req.volume);
            }
            return None;
        }
        let Some(res) = self.load(lib, &req.sound) else {
            self.stats.borrow_mut().unresolved += 1;
            return None;
        };
        self.stats.borrow_mut().played += 1;
        // An omitted (0) radius falls back to the actor's TransientSoundRadius (P2Pawn/P2Weapon:
        // 100), then to the engine default (GAudioDefaultRadius).
        let radius = if req.radius > 0.0 { req.radius } else { actor.and_then(|a| vm.get_prop(a, "TransientSoundRadius")).map(|v| v.as_float()).unwrap_or(0.0) };
        let p = PlayParams {
            slot: SoundSlot::from_byte(req.slot),
            volume: req.volume * res.volume,
            no_override: req.no_override,
            radius,
            pitch: req.pitch * res.pitch,
            attenuate: req.attenuate,
            ..Default::default()
        };
        let (v, dist) = self.with(|e| {
            if let Some(a) = actor {
                e.set_actor_location(a as u64, Vec3::from(vm.actor_loc(a)));
            }
            let dist = actor.map(|a| Vec3::from(vm.actor_loc(a)).distance(e.listener_position()));
            (e.play_sound(actor.map(|a| a as u64), &res.data, p), dist)
        });
        if std::env::var_os("P2GAME_SOUND_TRACE").is_some() {
            let t = vm.level.and_then(|l| vm.get_prop(l, "TimeSeconds")).map(|t| t.as_float()).unwrap_or(0.0);
            let state = if v.is_some() { "" } else { " CULLED" };
            println!("SOUND {} actor {:?} vol {:.2} radius {:.0} dist {:.0} t={t:.2}{state}", req.sound, actor.map(|a| vm.object_name(a)), req.volume, req.radius, dist.unwrap_or(0.0));
        }
        v
    }

    /// Music file decoded from `Music\` (Ogg).
    fn music_data(&self, song: &str) -> Option<Arc<SoundData>> {
        self.cache.borrow_mut().music(&self.game_dir, song)
    }

    /// `actor`: PlayMusicAttenuate's actor (positional music), None for flat `PlayMusic`.
    #[allow(clippy::too_many_arguments)]
    fn play_music(&self, song: &str, actor: Option<ObjId>, loc: Vec3, fade: f32, volume: f32, radius: f32, pitch: f32, volume_override: f32) -> i32 {
        // Scripts pass "" for "no song" (e.g. a level without music): handle 0, nothing played.
        if song.trim().is_empty() {
            return 0;
        }
        let Some(d) = self.music_data(song) else {
            self.stats.borrow_mut().unresolved += 1;
            if std::env::var_os("P2GAME_SOUND_TRACE").is_some() {
                println!("MUSIC {song} NOT FOUND");
            }
            return 0;
        };
        let v = self.with(|e| {
            if let Some(a) = actor {
                e.set_actor_location(a as u64, loc);
            }
            e.play_music_ext(&d, actor.map(|a| a as u64), fade, volume, radius, pitch, volume_override)
        });
        let mut h = self.next_handle.borrow_mut();
        let handle = *h;
        *h += 1;
        self.music.borrow_mut().insert(handle, v);
        self.stats.borrow_mut().music_started += 1;
        if std::env::var_os("P2GAME_SOUND_TRACE").is_some() {
            println!("MUSIC start {song} handle {handle} actor {actor:?} at {loc:?} fade {fade:.2} vol {volume:.2} radius {radius:.0} override {volume_override:.2}");
        }
        handle
    }

    /// Renders `secs` of the current mix (headless checks).
    pub fn render_wav(&self, secs: f32) -> Vec<u8> {
        self.with(|e| {
            let rate = e.rate;
            let mut buf = vec![0.0f32; (rate as f32 * secs) as usize * 2];
            for c in buf.chunks_mut(1024) {
                e.render(c);
            }
            stereo_to_wav(&buf, rate)
        })
    }
}

/// Line-of-hearing test against the level geometry (BSP and level static meshes, no terrain).
struct LevelOcclusion(Arc<p2collide::World>);

impl p2sound::OcclusionTrace for LevelOcclusion {
    fn blocked(&self, from: Vec3, to: Vec3) -> bool {
        let flags = p2collide::TraceFlags { world: true, actors: false, terrain: false };
        self.0.trace(from, to, &p2collide::Shape::POINT, flags).is_some_and(|h| !h.start_solid && h.t < 0.999)
    }
}

/// Actors with an ambient sound: (Sound path, SoundRadius bits, SoundVolume, SoundPitch).
fn scan_ambients(vm: &Vm) -> HashMap<ObjId, (String, u32, u8, u8)> {
    let mut out = HashMap::new();
    for &a in &vm.actors {
        let o = &vm.objects[a];
        if o.deleted {
            continue;
        }
        let Some(Value::Resource(s)) = vm.get_prop(a, "AmbientSound") else { continue };
        let r = vm.get_prop(a, "SoundRadius").map(|v| v.as_float()).unwrap_or(64.0);
        let v = vm.get_prop(a, "SoundVolume").map(|v| v.as_int()).unwrap_or(128).clamp(0, 255) as u8;
        let p = vm.get_prop(a, "SoundPitch").map(|v| v.as_int()).unwrap_or(64).clamp(0, 255) as u8;
        out.insert(a, (s.clone(), r.to_bits(), v, p));
    }
    out
}

/// The local player's eye: (location, rotation).
pub fn player_view(vm: &Vm) -> Option<(Vec3, [i32; 3])> {
    let pc = crate::ai::local_player_controller(vm)?;
    let target = vm.get_prop(pc, "ViewTarget").and_then(|v| v.as_obj()).or_else(|| vm.get_prop(pc, "Pawn").and_then(|v| v.as_obj())).unwrap_or(pc);
    let mut loc = Vec3::from(vm.actor_loc(target));
    if let Some(eh) = vm.get_prop(target, "EyeHeight") {
        loc.z += eh.as_float();
    }
    let rot = vm.get_prop(pc, "Rotation").map(|v| v.as_rot()).unwrap_or_default();
    Some((loc, rot))
}

/// Per-level host extension.
struct AudioHost {
    audio: Rc<Audio>,
    lib: Arc<upkg::library::Library>,
}

fn arg_f(a: &[Value], k: usize, d: f32) -> f32 {
    match a.get(k) {
        Some(Value::Void) | None => d,
        Some(v) => v.as_float(),
    }
}

impl Host for AudioHost {
    fn play_sound(&self, vm: &mut Vm, actor: Option<ObjId>, req: &SoundRequest) {
        self.audio.play(&self.lib, vm, actor, req);
    }

    fn sound_duration(&self, _vm: &mut Vm, sound: &str) -> f32 {
        self.audio.load(&self.lib, sound).map(|r| r.data.duration() / r.pitch.max(0.01)).unwrap_or(0.0)
    }

    fn actor_destroyed(&self, _vm: &mut Vm, actor: ObjId) {
        self.audio.with(|e| e.remove_actor(actor as u64));
        self.audio.ambient_seen.borrow_mut().remove(&actor);
    }

    fn native(&self, vm: &mut Vm, call: &NativeCall, a: &mut Vec<Value>) -> Option<uscript::Result<Value>> {
        let song = || a.first().map(|v| v.as_str()).unwrap_or_default();
        let trace = std::env::var_os("P2GAME_SOUND_TRACE").is_some();
        Some(Ok(match call.name.to_ascii_lowercase().as_str() {
            // PlayMusic(Song, FadeInTime, optional VolumeOverride, optional bAllowPause)
            "playmusic" => Value::Int(self.audio.play_music(&song(), None, Vec3::ZERO, arg_f(a, 1, 0.0), 1.0, 0.0, 1.0, arg_f(a, 2, 0.0))),
            // Actor.PlayMusicAttenuate(Song, FadeInTime, Volume, Radius, Pitch, VolumeOverride, ...):
            // music heard from this actor (SongThing bAttenuate: club/bar speakers, protest boom
            // boxes, Mike J's disco), positioned and attenuated like a sound of that radius.
            "playmusicattenuate" => {
                let me = vm.native_self();
                let loc = me.map(|m| Vec3::from(vm.actor_loc(m))).unwrap_or_default();
                let vol = arg_f(a, 2, 0.0);
                let pitch = arg_f(a, 4, 0.0);
                Value::Int(self.audio.play_music(&song(), me, loc, arg_f(a, 1, 0.0), if vol > 0.0 { vol } else { 1.0 }, arg_f(a, 3, 0.0), if pitch > 0.0 { pitch } else { 1.0 }, arg_f(a, 5, 0.0)))
            }
            "stopmusic" => {
                let h = a.first().map(|v| v.as_int()).unwrap_or(0);
                if let Some(v) = self.audio.music.borrow_mut().remove(&h) {
                    let fade = arg_f(a, 1, 0.0);
                    if trace {
                        println!("MUSIC stop handle {h} fade {fade:.2}");
                    }
                    self.audio.with(|e| e.stop_music(v, fade));
                }
                Value::Void
            }
            "stopallmusic" => {
                let fade = arg_f(a, 0, 0.0);
                if trace {
                    println!("MUSIC stop all fade {fade:.2}");
                }
                self.audio.music.borrow_mut().clear();
                self.audio.with(|e| e.stop_all_music(fade));
                Value::Void
            }
            _ => return None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_handles_and_listener_axes() {
        let (f, u) = rot_axes([0, 16384, 0]);
        assert!((f - Vec3::Y).length() < 1e-4 && (u - Vec3::Z).length() < 1e-4);
    }
}
