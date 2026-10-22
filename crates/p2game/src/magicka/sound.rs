//! Magicka sound: the session's XACT cue requests (spell loops, casts,
//! impacts, magicks, characters) resolved to their original waves and played
//! through Postal's own audio engine, positioned in Postal space.
use p2magicka::magicka_runtime::audio::{AudioLibrary, AudioRequest, SoundSource, Voice};
use p2sound::{PlayParams, SoundData, SoundSlot};
use std::collections::HashMap;
use std::sync::Arc;

/// Synthetic engine actor ids for cues that follow an emitter (Move).
const ACTOR_BASE: u64 = 0x4D41_0000_0000;

struct Pending {
    key: u32,
    voice: Voice,
    gain: f32,
    pitch: f32,
    at: Option<[f32; 3]>,
    delay: f32,
}

#[derive(Default)]
pub(super) struct Sounds {
    library: Option<Arc<AudioLibrary>>,
    clips: HashMap<(String, u16), Option<Arc<SoundData>>>,
    /// Cue key -> engine voices started for it.
    voices: HashMap<u32, Vec<u64>>,
    pending: Vec<Pending>,
    rng: u32,
}

impl Sounds {
    fn clip(&mut self, voice: &Voice) -> Option<Arc<SoundData>> {
        let library = self.library.clone()?;
        self.clips
            .entry((voice.wave_bank.clone(), voice.track))
            .or_insert_with(|| match library.try_decode(&voice.wave_bank, voice.track) {
                Ok(pcm) => Some(Arc::new(SoundData {
                    name: format!("magicka:{}:{}", voice.wave_bank, voice.track),
                    sample_rate: pcm.sample_rate,
                    channels: pcm.channels.clamp(1, 2),
                    samples: if pcm.channels <= 2 {
                        pcm.samples.iter().map(|&s| s as f32 / 32768.).collect()
                    } else {
                        pcm.samples
                            .chunks(pcm.channels as usize)
                            .flat_map(|f| [f[0] as f32 / 32768., f[1] as f32 / 32768.])
                            .collect()
                    },
                    loop_frames: None,
                })),
                Err(error) => {
                    eprintln!("MAGICKA sound {}#{}: {error}", voice.wave_bank, voice.track);
                    None
                }
            })
            .clone()
    }

    fn start(&mut self, p: Pending) {
        let Some(audio) = p2game::audio::audio() else { return };
        let Some(clip) = self.clip(&p.voice) else { return };
        let params = PlayParams {
            slot: SoundSlot::None,
            // XACT levels sit well under Postal's full-volume effects.
            volume: (p.gain * 1.8).clamp(0., 1.),
            no_override: false,
            // Magicka hears spells across a battlefield; Postal radii are uu.
            radius: 2500.,
            pitch: p.pitch.clamp(0.5, 2.),
            attenuate: p.at.is_some(),
            max_priority: false,
            looping: p.voice.loop_count == 255,
        };
        let mut engine = audio.engine.lock().unwrap_or_else(|e| e.into_inner());
        let id = match p.at {
            Some(at) => {
                let actor = ACTOR_BASE + p.key as u64;
                engine.set_actor_location(actor, glam::Vec3::from(p2magicka::to_postal(at)));
                engine.play_sound(Some(actor), &clip, params)
            }
            None => engine.play_sound(None, &clip, params),
        };
        if std::env::var_os("MAGICKA_TRACE").is_some() {
            println!("MAGICKA SOUND {} voice={id:?} gain={:.2} at={:?}", clip.name, p.gain, p.at);
        }
        if let Some(id) = id {
            self.voices.entry(p.key).or_default().push(id);
        }
    }

    /// Consume this step's requests (positions in Magicka metres).
    pub fn update(
        &mut self,
        content: &std::path::Path,
        requests: &[(AudioRequest, Option<[f32; 3]>)],
        dt: f32,
    ) {
        if self.library.is_none() {
            let library = AudioLibrary::load(content);
            for e in &library.errors {
                eprintln!("MAGICKA audio: {e}");
            }
            self.library = Some(Arc::new(library));
            self.rng = 0x9e37_79b9;
        }
        let library = self.library.clone().unwrap();
        for (request, position) in requests {
            match request {
                AudioRequest::Start { key, bank, cue, volume_db, source, magnitude } => {
                    let mut rng = self.rng;
                    let mut random = || {
                        rng ^= rng << 13;
                        rng ^= rng >> 17;
                        rng ^= rng << 5;
                        (rng >> 8) as f32 / (1u32 << 24) as f32
                    };
                    let Some(voices) = library.resolve(*bank, *cue, &mut random) else { continue };
                    self.rng = rng;
                    let at = match source {
                        SoundSource::Emitter(p) => Some(*p),
                        SoundSource::Locator { apply_3d: true, .. } => *position,
                        _ => None,
                    };
                    let variable = |name: &str| match name {
                        "Volume" => Some(*volume_db),
                        "Magnitude" => *magnitude,
                        _ => None,
                    };
                    for voice in voices {
                        let db = library.gain_db(&voice, &variable);
                        let semitones = voice.pitch + library.rpc_pitch(&voice, &variable);
                        self.pending.push(Pending {
                            key: *key,
                            gain: 10f32.powf(db / 20.),
                            pitch: 2f32.powf(semitones / 12.),
                            at,
                            delay: voice.delay,
                            voice,
                        });
                    }
                }
                AudioRequest::Move { key, position } => {
                    if let Some(audio) = p2game::audio::audio() {
                        let mut engine = audio.engine.lock().unwrap_or_else(|e| e.into_inner());
                        engine.set_actor_location(ACTOR_BASE + *key as u64, glam::Vec3::from(p2magicka::to_postal(*position)));
                    }
                    for p in self.pending.iter_mut().filter(|p| p.key == *key) {
                        p.at = Some(*position);
                    }
                }
                AudioRequest::Stop { key, .. } => {
                    self.pending.retain(|p| p.key != *key);
                    if let (Some(ids), Some(audio)) = (self.voices.remove(key), p2game::audio::audio()) {
                        let mut engine = audio.engine.lock().unwrap_or_else(|e| e.into_inner());
                        for id in ids {
                            engine.stop_voice(id);
                        }
                    }
                }
                // Postal keeps its own music; replays of global loops are rare.
                AudioRequest::Replay { .. } | AudioRequest::Music { .. } | AudioRequest::StopMusic => {}
            }
        }
        for p in &mut self.pending {
            p.delay -= dt;
        }
        let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|p| p.delay <= 0.);
        self.pending = waiting;
        for p in due {
            self.start(p);
        }
        // Forget finished voices.
        if let Some(audio) = p2game::audio::audio() {
            let engine = audio.engine.lock().unwrap_or_else(|e| e.into_inner());
            self.voices.retain(|_, ids| {
                ids.retain(|id| engine.is_playing(*id));
                !ids.is_empty()
            });
        }
    }
}
