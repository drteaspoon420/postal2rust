//! Positional ambient sound: every placed actor with an `AmbientSound` (AmbientSound actors and
//! any other actor whose properties or class defaults set one) loops through `p2sound` with its
//! SoundRadius / SoundVolume / SoundPitch, heard from the main camera.
//!
//! `--mute` disables device output. With `--sound-wav <file>` the plugin also renders a few
//! seconds of what the camera hears to a WAV file (works headless, e.g. with `--screenshot`).

use bevy::prelude::*;
use p2anim::character::{ClassDb, PropStack};
use p2sound::bevy_plugin::{Sound, SoundEmitter, SoundListener, SoundPlugin};
use p2sound::{AmbientDesc, SoundCache};
use std::path::PathBuf;
use std::sync::Arc;
use upkg::library::Library;
use upkg::{ObjRef, begin_object};

pub struct AmbientPlugin {
    pub game: PathBuf,
    pub map: String,
}

#[derive(Resource, Clone)]
struct AmbientCfg {
    game: PathBuf,
    map: String,
    wav: Option<PathBuf>,
}

impl Plugin for AmbientPlugin {
    fn build(&self, app: &mut App) {
        let a: Vec<String> = std::env::args().collect();
        let has = |k: &str| a.iter().any(|x| x == k);
        let wav = a.iter().position(|x| x == "--sound-wav").and_then(|i| a.get(i + 1)).map(PathBuf::from);
        let headless = has("--screenshot") || has("--bench");
        app.add_plugins(SoundPlugin { uu_per_unit: p2view::scene::UU_PER_M, output: !has("--mute") && !headless })
            .insert_resource(AmbientCfg { game: self.game.clone(), map: self.map.clone(), wav })
            .add_systems(Startup, spawn_ambients)
            .add_systems(Update, (tag_listener, capture_wav));
    }
}

/// Actor class defaults (Actor.uc): SoundRadius 64, SoundVolume 128, SoundPitch 64.
const DEFAULT_RADIUS: f32 = 64.0;

fn spawn_ambients(mut commands: Commands, cfg: Res<AmbientCfg>) {
    let t0 = std::time::Instant::now();
    let Ok(lib) = Library::new(&cfg.game) else { return };
    let Some(path) = lib.map_path(&cfg.map) else { return };
    let Ok(map) = upkg::Package::open(&path).map(Arc::new) else { return };
    let mut db = ClassDb::default();
    let mut cache = SoundCache::new();
    let (mut n, mut failed) = (0, 0);
    for i in 0..map.exports.len() {
        let class_ref = map.exports[i].class;
        if class_ref == ObjRef::Null {
            continue;
        }
        let Ok((_, props)) = begin_object(&map, i) else { continue };
        let Some(loc) = props.vector("Location") else { continue };
        // Placed properties first, then the class defaults.
        let mut stack = PropStack { layers: vec![(map.clone(), props.clone())], chain: vec![] };
        if let Some(class) = lib.resolve(&map, class_ref).filter(|c| c.class().eq_ignore_ascii_case("Class")) {
            if let Some(d) = db.defaults(&lib, &class) {
                stack.layers.extend(d.layers.iter().cloned());
                stack.chain = d.chain.clone();
            }
        }
        if !stack.object_is_set("AmbientSound") {
            continue;
        }
        let Some(sound_obj) = stack.object(&lib, "AmbientSound") else {
            failed += 1;
            continue;
        };
        let Some(res) = cache.resolve(&lib, &sound_obj) else {
            failed += 1;
            continue;
        };
        let radius = stack.float("SoundRadius").unwrap_or(DEFAULT_RADIUS);
        let volume = stack.byte("SoundVolume").unwrap_or(128);
        let pitch = stack.byte("SoundPitch").unwrap_or(64);
        let desc = AmbientDesc {
            sound: res.data,
            location: Vec3::from(loc),
            radius,
            volume: ((volume as f32 * res.volume).round() as i32).clamp(0, 255) as u8,
            pitch: ((pitch as f32 * res.pitch).round() as i32).clamp(1, 255) as u8,
        };
        if std::env::var_os("P2VIEW_LIST_AMBIENT").is_some() {
            println!("AMBIENT {} {} r={radius} vol={volume} pitch={pitch} at {loc:?}", map.name(map.exports[i].name), sound_obj.name());
        }
        commands.spawn((Transform::from_translation(p2view::scene::to_bevy(loc)), SoundEmitter { actor: i as u64, ambient: Some(desc) }));
        n += 1;
    }
    println!("AMBIENT sounds={n} unresolved={failed} in {} ms", t0.elapsed().as_millis());
}

/// The main camera is the listener.
fn tag_listener(mut commands: Commands, q: Query<Entity, (With<Camera3d>, Without<SoundListener>)>) {
    for e in &q {
        commands.entity(e).insert(SoundListener);
    }
}

/// `--sound-wav`: after the scene settles, renders 5 s of the camera's mix to a file.
fn capture_wav(cfg: Res<AmbientCfg>, sound: Res<Sound>, time: Res<Time>, mut done: Local<bool>) {
    let Some(path) = &cfg.wav else { return };
    if *done || time.elapsed_secs() < 1.0 {
        return;
    }
    *done = true;
    let (wav, stats, peak) = sound.with(|e| {
        let rate = e.rate;
        let mut all = vec![0.0f32; rate as usize * 2 * 5];
        for chunk in all.chunks_mut(1024) {
            e.render(chunk);
        }
        let peak = all.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        (p2sound::engine::stereo_to_wav(&all, rate), e.stats.clone(), peak)
    });
    match std::fs::write(path, wav) {
        Ok(()) => println!("SOUND_WAV {} voices={} audible={} ambients={} peak={peak:.3}", path.display(), stats.voices, stats.audible, stats.ambients),
        Err(e) => eprintln!("cannot write {}: {e}", path.display()),
    }
}
