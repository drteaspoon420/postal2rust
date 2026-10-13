use crate::engine::*;
use crate::spatial::Listener;
use glam::Vec3;
use std::sync::Arc;

const RATE: u32 = 22050;

/// A constant (DC) mono sound: makes gains directly measurable in the output.
fn dc(frames: usize, level: f32) -> Arc<SoundData> {
    Arc::new(SoundData { name: "dc".into(), sample_rate: RATE, channels: 1, samples: vec![level; frames], loop_frames: None })
}

fn sine(freq: f32, secs: f32) -> Arc<SoundData> {
    let n = (RATE as f32 * secs) as usize;
    let samples = (0..n).map(|i| (i as f32 / RATE as f32 * freq * std::f32::consts::TAU).sin() * 0.5).collect();
    Arc::new(SoundData { name: format!("sine{freq}"), sample_rate: RATE, channels: 1, samples, loop_frames: None })
}

/// Renders `frames` and returns the (left, right) of the last frame.
fn last(e: &mut AudioEngine, frames: usize) -> (f32, f32) {
    let mut b = vec![0.0; frames * 2];
    // Two passes so per-buffer gain ramps settle.
    e.render(&mut b);
    e.render(&mut b);
    (b[b.len() - 2], b[b.len() - 1])
}

#[test]
fn slot_override_rules() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(RATE as usize, 0.1);
    e.set_actor_location(1, Vec3::ZERO);
    let p = PlayParams { slot: SoundSlot::Talk, ..Default::default() };
    let a = e.play_sound(Some(1), &s, p).unwrap();
    // Same slot replaces.
    let b = e.play_sound(Some(1), &s, p).unwrap();
    assert!(!e.is_playing(a) && e.is_playing(b));
    // bNoOverride keeps the old one and drops the new.
    assert!(e.play_sound(Some(1), &s, PlayParams { no_override: true, ..p }).is_none());
    assert!(e.is_playing(b));
    // SLOT_None never overrides: both play.
    let n1 = e.play_sound(Some(1), &s, PlayParams::default()).unwrap();
    let n2 = e.play_sound(Some(1), &s, PlayParams::default()).unwrap();
    assert!(e.is_playing(n1) && e.is_playing(n2) && e.is_playing(b));
    // Other actor, same slot: independent.
    e.set_actor_location(2, Vec3::ZERO);
    let c = e.play_sound(Some(2), &s, p).unwrap();
    assert!(e.is_playing(b) && e.is_playing(c));
    e.stop_slot(1, SoundSlot::Talk);
    assert!(!e.is_playing(b) && e.is_playing(c));
}

#[test]
fn voice_limit_steals_lowest_priority() {
    let mut e = AudioEngine::new(RATE);
    e.max_voices = 4;
    let s = dc(RATE as usize, 0.01);
    let far = PlayParams { radius: 100.0, ..Default::default() };
    let mut ids = Vec::new();
    for i in 0..4 {
        ids.push(e.play_sound_at(&s, Vec3::new(100.0 + i as f32 * 10.0, 0.0, 0.0), far).unwrap());
    }
    // A quieter far sound is rejected.
    assert!(e.play_sound_at(&s, Vec3::new(140.0, 0.0, 0.0), PlayParams { volume: 0.1, ..far }).is_none());
    // A near loud sound steals the farthest (lowest-priority) voice.
    let near = e.play_sound_at(&s, Vec3::new(1.0, 0.0, 0.0), far).unwrap();
    assert!(e.is_playing(near));
    assert!(!e.is_playing(ids[3]));
    assert!(ids[..3].iter().all(|&i| e.is_playing(i)));
    // bMaxPriority beats everything.
    assert!(e.play_sound_at(&s, Vec3::new(149.0, 0.0, 0.0), PlayParams { max_priority: true, ..far }).is_some());
}

#[test]
fn attenuation_and_pan_in_output() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(RATE as usize * 2, 0.5);
    let p = PlayParams { radius: 100.0, ..Default::default() };
    // Inside the radius, straight ahead: unity, centred.
    let v = e.play_sound_at(&s, Vec3::new(50.0, 0.0, 0.0), p).unwrap();
    let (l, r) = last(&mut e, 512);
    assert!((l - 0.5).abs() < 1e-3 && (r - 0.5).abs() < 1e-3, "{l} {r}");
    e.stop_voice(v);
    // To the right (+Y) and far (5x the radius): right louder, both attenuated (inverse distance).
    e.play_sound_at(&s, Vec3::new(0.0, 500.0, 0.0), p).unwrap();
    let (l, r) = last(&mut e, 512);
    let g = 100.0 / (100.0 + 0.9 * 400.0) * std::f32::consts::SQRT_2 * 0.5;
    assert!((r - g).abs() < 0.01 && l < 0.01, "{l} {r} {g}");
    // Out of range (GAudioMaxRadiusMultiplier 100): culled at start.
    assert!(e.play_sound_at(&s, Vec3::new(0.0, 10001.0, 0.0), p).is_none());
    // Non-attenuated sounds are heard anywhere.
    assert!(e.play_sound_at(&s, Vec3::new(0.0, 1e5, 0.0), PlayParams { attenuate: false, ..p }).is_some());
}

#[test]
fn listener_turn_swaps_ears() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(RATE as usize * 2, 0.5);
    e.play_sound_at(&s, Vec3::new(0.0, 300.0, 0.0), PlayParams { radius: 1000.0, ..Default::default() });
    let (l, r) = last(&mut e, 256);
    assert!(r > l * 10.0, "{l} {r}");
    // Face -X: +Y is now on the left.
    e.set_listener(Listener { forward: -Vec3::X, ..Default::default() });
    let (l, r) = last(&mut e, 256);
    assert!(l > r * 10.0, "{l} {r}");
}

#[test]
fn one_shots_end_and_loops_continue() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(1000, 0.2);
    let v = e.play_sound(None, &s, PlayParams::default()).unwrap();
    let mut b = vec![0.0; 2 * 800];
    e.render(&mut b);
    assert!(e.is_playing(v));
    e.render(&mut b);
    assert!(!e.is_playing(v));
    assert_eq!(b[2 * 300], 0.0, "silence after end");
    let l = e.play_sound(None, &s, PlayParams { looping: true, ..Default::default() }).unwrap();
    for _ in 0..10 {
        e.render(&mut b);
    }
    assert!(e.is_playing(l));
    assert!(b.iter().all(|x| (x - 0.2).abs() < 1e-4));
}

#[test]
fn pitch_resamples() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(1000, 0.2);
    // Pitch 2 plays 1000 frames in 500.
    let v = e.play_sound(None, &s, PlayParams { pitch: 2.0, ..Default::default() }).unwrap();
    let mut b = vec![0.0; 2 * 499];
    e.render(&mut b);
    assert!(e.is_playing(v));
    let mut b = vec![0.0; 2 * 4];
    e.render(&mut b);
    assert!(!e.is_playing(v));
    // 44.1k source at 22.05k output advances 2 source frames per output frame.
    let hi = Arc::new(SoundData { name: "hi".into(), sample_rate: RATE * 2, channels: 1, samples: vec![0.1; 1000], loop_frames: None });
    let v = e.play_sound(None, &hi, PlayParams::default()).unwrap();
    let mut b = vec![0.0; 2 * 501];
    e.render(&mut b);
    assert!(!e.is_playing(v));
}

#[test]
fn ambient_sounds_follow_range() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(RATE as usize, 0.5);
    e.set_ambient(7, Some(AmbientDesc { sound: s.clone(), location: Vec3::new(30000.0, 0.0, 0.0), radius: 200.0, volume: 255, pitch: 64 }));
    let (l, _) = last(&mut e, 256);
    assert_eq!(l, 0.0);
    assert_eq!(e.stats.voices, 0, "out of range: not playing");
    e.set_listener(Listener { position: Vec3::new(29900.0, 0.0, 0.0), ..Default::default() });
    let (l, r) = last(&mut e, 256);
    assert_eq!(e.stats.voices, 1);
    assert!((l - 0.5).abs() < 1e-3 && (r - 0.5).abs() < 1e-3, "{l} {r}");
    // Volume byte scales.
    e.set_ambient(7, Some(AmbientDesc { sound: s.clone(), location: Vec3::new(30000.0, 0.0, 0.0), radius: 200.0, volume: 128, pitch: 64 }));
    let (l, _) = last(&mut e, 256);
    assert!((l - 0.5 * 128.0 / 255.0).abs() < 2e-3, "{l}");
    e.set_listener(Listener::default());
    last(&mut e, 256);
    assert_eq!(e.stats.voices, 0, "left range: stopped");
    e.set_listener(Listener { position: Vec3::new(29900.0, 0.0, 0.0), ..Default::default() });
    last(&mut e, 256);
    e.remove_actor(7);
    last(&mut e, 256);
    assert_eq!(e.stats.voices, 0);
}

/// A destroyed actor's one-shot keeps playing at its last position (window glass break:
/// PropBreakable plays the sound and Destroy()s the window in the same tick).
#[test]
fn destroyed_actor_one_shot_keeps_playing() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(RATE as usize, 0.5);
    e.set_actor_location(9, Vec3::new(100.0, 0.0, 0.0));
    e.play_sound(Some(9), &s, PlayParams { radius: 2000.0, ..Default::default() });
    last(&mut e, 256);
    assert_eq!(e.stats.voices, 1);
    e.remove_actor(9);
    let (l, r) = last(&mut e, 256);
    assert_eq!(e.stats.voices, 1, "one-shot survives its actor");
    assert!(l > 0.0 || r > 0.0, "still audible ({l} {r})");
}

/// With every voice busy, a nearer ambient steals a voice; it must be flagged ambient (a stolen
/// slot used to leave it an unmanaged loop that restarted every update: 543 LampBuzz starts in
/// 30 s on suburbs-3).
#[test]
fn ambient_start_with_full_voice_table() {
    let mut e = AudioEngine::new(RATE);
    e.max_voices = 2;
    let s = dc(RATE as usize, 0.1);
    for _ in 0..2 {
        e.play_sound_at(&s, Vec3::new(5000.0, 0.0, 0.0), PlayParams { radius: 100.0, looping: true, volume: 0.1, ..Default::default() });
    }
    e.set_ambient(9, Some(AmbientDesc { sound: s.clone(), location: Vec3::new(10.0, 0.0, 0.0), radius: 200.0, volume: 255, pitch: 64 }));
    for _ in 0..5 {
        last(&mut e, 128);
    }
    let infos = e.voice_infos();
    assert_eq!(infos.iter().filter(|v| v.ambient).count(), 1, "{infos:?}");
    assert_eq!(infos.len(), 2);
}

struct Wall;
impl OcclusionTrace for Wall {
    fn blocked(&self, from: Vec3, to: Vec3) -> bool {
        (from.x > 0.0) != (to.x > 0.0)
    }
}

#[test]
fn occlusion_shrinks_radius() {
    let mut e = AudioEngine::new(RATE);
    let s = dc(RATE as usize * 2, 0.5);
    e.play_sound_at(&s, Vec3::new(-150.0, 0.0, 0.0), PlayParams { radius: 200.0, ..Default::default() });
    let (open, _) = last(&mut e, 256);
    e.set_occlusion(Some(Box::new(Wall)));
    e.set_listener(Listener { position: Vec3::new(1.0, 0.0, 0.0), ..Default::default() });
    let mut b = vec![0.0; 2 * 4096];
    e.render(&mut b);
    let (shut, _) = last(&mut e, 256);
    assert!(open > 0.4 && shut < open * 0.5, "{open} {shut}");
}

#[test]
fn music_fades() {
    let mut e = AudioEngine::new(RATE);
    e.music_volume = 1.0;
    let s = dc(RATE as usize, 0.5);
    let m = e.play_music(&s, 1.0, 1.0);
    let mut b = vec![0.0; 2 * (RATE as usize / 2)];
    e.render(&mut b);
    // Halfway through a 1 s fade-in.
    let mid = b[b.len() - 2];
    assert!((mid - 0.25).abs() < 0.02, "{mid}");
    e.render(&mut b);
    e.render(&mut b);
    assert!((b[b.len() - 2] - 0.5).abs() < 1e-3);
    e.stop_music(m, 0.5);
    e.render(&mut b);
    e.render(&mut b);
    assert!(!e.is_playing(m));
    // StopAllSounds leaves music alone.
    let m = e.play_music(&s, 0.0, 1.0);
    e.stop_all_sounds();
    assert!(e.is_playing(m));
    e.stop_all_music(0.0);
    e.render(&mut b);
    assert!(!e.is_playing(m));
}

/// PlayMusicAttenuate: music from an actor fades with distance; VolumeOverride replaces the user's
/// music volume; level change drops actor music but keeps flat music.
#[test]
fn attenuated_music_and_override() {
    let mut e = AudioEngine::new(RATE);
    e.music_volume = 0.5;
    let s = dc(RATE as usize, 0.5);
    e.set_actor_location(7, Vec3::new(500000.0, 0.0, 0.0));
    let m = e.play_music_ext(&s, Some(7), 0.0, 1.0, 1000.0, 1.0, 0.0);
    let mut b = vec![0.0; 2 * 1024];
    e.render(&mut b);
    assert!(b.iter().all(|x| x.abs() < 1e-6), "far music must be silent");
    e.set_actor_location(7, Vec3::new(100.0, 0.0, 0.0));
    e.render(&mut b);
    e.render(&mut b);
    let near = b[b.len() - 2] + b[b.len() - 1];
    assert!((near - 0.25 * 2.0).abs() < 0.02, "{near}");
    // Flat music with an override ignores the user's music volume.
    let f = e.play_music(&s, 0.0, 1.0);
    e.stop_music(m, 0.0);
    e.render(&mut b);
    e.render(&mut b);
    assert!((b[b.len() - 2] - 0.5).abs() < 1e-3, "{}", b[b.len() - 2]);
    let m2 = e.play_music_ext(&s, Some(7), 0.0, 1.0, 1000.0, 1.0, 0.0);
    e.clear_level();
    assert!(e.is_playing(f) && !e.is_playing(m2));
}

/// Writes a short spatial scene to target/p2sound_test.wav for listening.
#[test]
fn render_scene_to_wav() {
    let mut e = AudioEngine::new(RATE);
    let a = sine(440.0, 3.0);
    let b = sine(660.0, 3.0);
    e.set_actor_location(1, Vec3::new(0.0, -300.0, 0.0));
    e.set_actor_location(2, Vec3::new(0.0, 300.0, 0.0));
    e.play_sound(Some(1), &a, PlayParams { radius: 1000.0, slot: SoundSlot::Misc, ..Default::default() });
    e.play_sound(Some(2), &b, PlayParams { radius: 1000.0, slot: SoundSlot::Misc, ..Default::default() });
    let mut all = Vec::new();
    let mut buf = vec![0.0; 2 * 1024];
    for i in 0..60 {
        // Turn the listener a full circle over the clip.
        let yaw = i as f32 / 60.0 * std::f32::consts::TAU;
        e.set_listener(Listener { forward: Vec3::new(yaw.cos(), yaw.sin(), 0.0), ..Default::default() });
        e.render(&mut buf);
        all.extend_from_slice(&buf);
    }
    assert!(all.iter().any(|s| s.abs() > 0.1));
    assert!(all.iter().all(|s| s.abs() <= 1.0));
    let wav = stereo_to_wav(&all, RATE);
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/p2sound_test.wav");
    let _ = std::fs::write(out, &wav);
    assert_eq!(&wav[0..4], b"RIFF");
}
