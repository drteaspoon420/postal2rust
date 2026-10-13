//! Loading sounds from the game install: Sound objects (with ProceduralSound / SoundGroup
//! resolution), loose music files (Music\*.ogg via lewton), and a decode cache.

use crate::engine::SoundData;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use upkg::library::{Library, Obj};
use upkg::sound::{SoundKind, read_sound};
use upkg::{ObjRef, Package};

/// A playable choice after resolving procedural/group sounds: the concrete sound plus the
/// pitch/volume modifiers the procedural layer adds.
pub struct Resolved {
    pub data: Arc<SoundData>,
    pub pitch: f32,
    pub volume: f32,
}

#[derive(Default)]
pub struct SoundCache {
    sounds: HashMap<(String, usize), Option<Arc<SoundData>>>,
    music: HashMap<PathBuf, Option<Arc<SoundData>>>,
    rng: u64,
}

impl SoundCache {
    pub fn new() -> Self {
        Self { rng: 0x9E37_79B9_7F4A_7C15, ..Default::default() }
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Decodes a concrete `Sound` export (cached).
    pub fn sound(&mut self, obj: &Obj) -> Option<Arc<SoundData>> {
        let key = obj.key();
        if let Some(s) = self.sounds.get(&key) {
            return s.clone();
        }
        let d = read_sound(&obj.pkg, obj.export).ok().and_then(|s| {
            let pcm = s.decode_pcm().ok()?;
            Some(Arc::new(SoundData::from_pcm(&obj.pkg.full_path(ObjRef::Export(obj.export)), &pcm)))
        });
        self.sounds.insert(key, d.clone());
        d
    }

    /// Resolves any sound-family object (Sound, ProceduralSound, SoundGroup) to playable data.
    /// Procedural sounds apply their PitchModification/VolumeModification (+ random variance);
    /// groups pick a member, weighted by Probabilities when present.
    pub fn resolve(&mut self, lib: &Library, obj: &Obj) -> Option<Resolved> {
        let mut cur = obj.clone();
        let (mut pitch, mut volume) = (1.0f32, 1.0f32);
        for _ in 0..16 {
            let s = read_sound(&cur.pkg, cur.export).ok()?;
            let next = match s.kind {
                SoundKind::Procedural => {
                    let p = &s.props;
                    let pm = p.float("PitchModification").unwrap_or(0.0);
                    let pv = p.float("PitchVariance").unwrap_or(0.0);
                    let vm = p.float("VolumeModification").unwrap_or(0.0);
                    let vv = p.float("VolumeVariance").unwrap_or(0.0);
                    let r1 = self.rand() * 2.0 - 1.0;
                    let r2 = self.rand() * 2.0 - 1.0;
                    pitch *= 1.0 + (pm + pv * r1) / 100.0;
                    volume *= 1.0 + (vm + vv * r2) / 100.0;
                    s.base_sound()
                }
                SoundKind::Group => {
                    let members: Vec<ObjRef> = s.group_sounds().into_iter().filter(|r| *r != ObjRef::Null).collect();
                    if members.is_empty() {
                        return None;
                    }
                    let probs = s.group_probabilities();
                    let pick = if probs.len() == members.len() && probs.iter().sum::<f32>() > 0.0 {
                        let total: f32 = probs.iter().sum();
                        let mut x = self.rand() * total;
                        let mut k = 0;
                        for (i, p) in probs.iter().enumerate() {
                            if x < *p {
                                k = i;
                                break;
                            }
                            x -= p;
                            k = i;
                        }
                        k
                    } else {
                        ((self.rand() * members.len() as f32) as usize).min(members.len() - 1)
                    };
                    Some(members[pick])
                }
                _ => {
                    let data = self.sound(&cur)?;
                    return Some(Resolved { data, pitch: pitch.clamp(0.25, 4.0), volume: volume.clamp(0.0, 2.0) });
                }
            };
            cur = lib.resolve(&cur.pkg, next?)?;
        }
        None
    }

    /// Resolves an object reference made from `pkg`.
    pub fn resolve_ref(&mut self, lib: &Library, pkg: &Arc<Package>, r: ObjRef) -> Option<Resolved> {
        let o = lib.resolve(pkg, r)?;
        self.resolve(lib, &o)
    }

    /// Decodes a loose music file (Ogg Vorbis or WAV) by name, searching `Music\`.
    pub fn music(&mut self, game: &Path, song: &str) -> Option<Arc<SoundData>> {
        let mut cands = vec![PathBuf::from(song), game.join("Music").join(song)];
        if Path::new(song).extension().is_none() {
            cands.push(game.join("Music").join(format!("{song}.ogg")));
        }
        let path = cands.into_iter().find(|p| p.is_file())?;
        if let Some(s) = self.music.get(&path) {
            return s.clone();
        }
        let d = decode_file(&path).map(Arc::new);
        self.music.insert(path, d.clone());
        d
    }
}

/// Decodes an Ogg Vorbis or WAV file to stereo/mono f32.
pub fn decode_file(path: &Path) -> Option<SoundData> {
    use rodio::Source;
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"RIFF") {
        let pcm = upkg::sound::decode_pcm(&bytes).ok()?;
        return Some(SoundData::from_pcm(&path.display().to_string(), &pcm));
    }
    let dec = rodio::Decoder::try_from(std::io::Cursor::new(bytes)).ok()?;
    let channels = dec.channels().get();
    let rate = dec.sample_rate().get();
    let samples: Vec<f32> = dec.collect();
    let samples = if channels <= 2 { samples } else { samples.chunks(channels as usize).flat_map(|f| [f[0], f[1]]).collect() };
    Some(SoundData { name: path.display().to_string(), sample_rate: rate, channels: channels.clamp(1, 2), samples, loop_frames: None })
}
