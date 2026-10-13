//! Bevy playback: a fullscreen, letterboxed UI image showing the current movie frame, with an
//! optional soundtrack voice in `p2sound` that also drives the movie clock (audio is the master,
//! so video stays in sync even when frames are dropped).
//!
//! Hosts call [`Movies::play_scaled`] / [`Movies::play_direct`] / [`Movies::stop`] /
//! [`Movies::pause`] (the HUD natives) on the resource.

use crate::player::MoviePlayer;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use p2sound::bevy_plugin::Sound;
use p2sound::{PlayParams, SoundData, VoiceId};
use std::path::PathBuf;
use std::sync::Arc;

pub struct MoviePlugin {
    pub game: PathBuf,
}

/// Movie state plus the soundtrack voice.
#[derive(Resource)]
pub struct Movies {
    pub player: MoviePlayer,
    pub game: PathBuf,
    soundtrack: Option<VoiceId>,
    pending_sound: Option<Arc<SoundData>>,
    pending_pause: Option<bool>,
    stop_sound: bool,
    /// Frames uploaded so far (for tests / HUD).
    pub uploads: u64,
}

impl Movies {
    /// `HUD.PlayMovieScaled` over the whole screen; `soundtrack` is the Sound object the script
    /// plays with it (e.g. `ACTION_PlayMovie.Sound`).
    pub fn play_scaled(&mut self, name: &str, looping: bool, soundtrack: Option<Arc<SoundData>>) -> Result<(), String> {
        self.stop_sound = self.soundtrack.is_some();
        // Embedded RoQ audio is played as one voice, so the clock (audio-driven) stays exact.
        self.player.play_scaled(&self.game, name, 0.0, 0.0, 1.0, 1.0, false, looping, false)?;
        self.pending_sound = soundtrack.or_else(|| self.embedded());
        Ok(())
    }

    /// `HUD.PlayMovieDirect`.
    pub fn play_direct(&mut self, name: &str, x: i32, y: i32, looping: bool) -> Result<(), String> {
        self.stop_sound = self.soundtrack.is_some();
        self.player.play_direct(&self.game, name, x, y, false, looping)?;
        self.pending_sound = self.embedded();
        Ok(())
    }

    fn embedded(&self) -> Option<Arc<SoundData>> {
        let s = self.player.embedded_audio();
        (!s.is_empty()).then(|| {
            Arc::new(SoundData { name: self.player.filename.clone(), sample_rate: crate::roq::AUDIO_RATE, channels: 2, samples: s.iter().map(|&v| v as f32 / 32768.0).collect(), loop_frames: None })
        })
    }

    /// `HUD.StopMovie`.
    pub fn stop(&mut self) {
        self.player.stop_now();
        self.stop_sound = true;
    }

    /// `HUD.PauseMovie`.
    pub fn pause(&mut self, pause: bool) {
        self.player.pause(pause);
        self.pending_pause = Some(pause);
    }

    /// `HUD.IsMoviePlaying`.
    pub fn is_playing(&self) -> bool {
        self.player.is_playing()
    }
}

#[derive(Component)]
struct MovieScreen;

#[derive(Component)]
struct MovieImage(Handle<Image>);

impl Plugin for MoviePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Movies {
            player: MoviePlayer::default(),
            game: self.game.clone(),
            soundtrack: None,
            pending_sound: None,
            pending_pause: None,
            stop_sound: false,
            uploads: 0,
        })
        .add_systems(Startup, setup)
        .add_systems(Update, update);
    }
}

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let img = images.add(blank_image(16, 16));
    commands
        .spawn((
            Node { position_type: PositionType::Absolute, width: Val::Percent(100.0), height: Val::Percent(100.0), ..default() },
            BackgroundColor(Color::BLACK),
            GlobalZIndex(1000),
            Visibility::Hidden,
            MovieScreen,
        ))
        .with_children(|p| {
            p.spawn((Node { position_type: PositionType::Absolute, ..default() }, ImageNode::new(img.clone()), MovieImage(img)));
        });
}

fn blank_image(w: u32, h: u32) -> Image {
    let mut img = Image::new_fill(Extent3d { width: w, height: h, depth_or_array_layers: 1 }, TextureDimension::D2, &[0, 0, 0, 255], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD);
    img.sampler = bevy::image::ImageSampler::linear();
    img
}

fn update(
    time: Res<Time>,
    mut movies: ResMut<Movies>,
    sound: Option<Res<Sound>>,
    mut images: ResMut<Assets<Image>>,
    windows: Query<&Window>,
    mut screen: Query<&mut Visibility, With<MovieScreen>>,
    mut picture: Query<(&mut Node, &MovieImage)>,
) {
    let m = &mut *movies;
    // Soundtrack housekeeping.
    if let Some(s) = sound.as_ref() {
        if m.stop_sound {
            if let Some(v) = m.soundtrack.take() {
                s.with(|e| e.stop_voice(v));
            }
            m.stop_sound = false;
        }
        if let Some(data) = m.pending_sound.take() {
            m.soundtrack = s.with(|e| e.play_sound(None, &data, PlayParams { attenuate: false, max_priority: true, ..Default::default() }));
        }
        if let (Some(p), Some(v)) = (m.pending_pause.take(), m.soundtrack) {
            s.with(|e| e.set_voice_paused(v, p));
        }
    }
    // Clock: follow the soundtrack while it plays on a live device, else wall time.
    let dt = match (sound.as_ref().filter(|s| s.live), m.soundtrack) {
        (Some(s), Some(v)) => match s.with(|e| e.voice_time(v)) {
            Some(t) => (t - m.player.time()).max(0.0),
            None => {
                m.soundtrack = None;
                time.delta_secs_f64()
            }
        },
        _ => time.delta_secs_f64(),
    };
    let changed = m.player.tick(dt);
    let playing = m.player.is_playing();
    if let Ok(mut vis) = screen.single_mut() {
        let want = if playing { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
    if !playing {
        if let Some(v) = m.soundtrack.take() {
            if let Some(s) = sound.as_ref() {
                s.with(|e| e.stop_voice(v));
            }
        }
        return;
    }
    let Ok((mut node, mi)) = picture.single_mut() else { return };
    if changed {
        let (w, h) = (m.player.width(), m.player.height());
        if let Some(mut img) = images.get_mut(&mi.0) {
            if img.width() != w || img.height() != h {
                *img = blank_image(w, h);
            }
            img.data = Some(m.player.rgba().to_vec());
        }
        m.uploads += 1;
    }
    if let Ok(win) = windows.single() {
        let (x, y, w, h) = m.player.screen_rect(win.width(), win.height());
        node.left = Val::Px(x);
        node.top = Val::Px(y);
        node.width = Val::Px(w);
        node.height = Val::Px(h);
    }
}
