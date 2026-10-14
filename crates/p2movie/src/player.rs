//! Movie playback state matching the UE2 `Movie` natives (Play, Pause, IsPaused, StopNow,
//! StopAtEnd, IsPlaying, GetWidth, GetHeight) plus the HUD helpers scripts call
//! (`PlayMovieDirect`, `PlayMovieScaled`, `IsMoviePlaying`, `PauseMovie`, `StopMovie`).
//! Bevy-independent: the host advances it with [`MoviePlayer::tick`] and uploads
//! [`MoviePlayer::rgba`] when [`MoviePlayer::tick`] reports a new frame.

use crate::roq::{AUDIO_RATE, Decoder, Event, RoqError};
use std::path::{Path, PathBuf};

/// Where the movie is drawn (the two HUD paths).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// `PlayMovieDirect`: unscaled at a pixel position.
    Direct { x: i32, y: i32 },
    /// `PlayMovieScaled` / `MovieTexture`: fills a normalized canvas rectangle, letterboxed to
    /// the movie aspect (the HUD uses 4:3 for pigeons.roq and 16:9 otherwise).
    Scaled { left: f32, top: f32, right: f32, bottom: f32, translucent: bool },
}

pub struct MoviePlayer {
    decoder: Option<Decoder>,
    pub filename: String,
    pub placement: Placement,
    looping: bool,
    use_sound: bool,
    paused: bool,
    stop_at_end: bool,
    playing: bool,
    time: f64,
    frames_shown: u32,
    rgba: Vec<u8>,
    /// Audio decoded from the stream but not yet consumed (interleaved stereo i16 at 22050 Hz).
    pub audio: Vec<i16>,
}

impl Default for MoviePlayer {
    fn default() -> Self {
        Self {
            decoder: None,
            filename: String::new(),
            placement: Placement::Scaled { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0, translucent: false },
            looping: false,
            use_sound: false,
            paused: false,
            stop_at_end: false,
            playing: false,
            time: 0.0,
            frames_shown: 0,
            rgba: Vec::new(),
            audio: Vec::new(),
        }
    }
}

/// Finds a movie file by script name: absolute paths, `Movies\<name>` with a case-insensitive
/// match (scripts write e.g. "pigeons.roq" for "pigeons.RoQ").
pub fn find_movie(game: &Path, name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_file() {
        return Some(p.to_path_buf());
    }
    let dir = upkg::ci::join(game, "Movies");
    let want = Path::new(name).file_name()?.to_string_lossy().to_ascii_lowercase();
    let want_ext = if Path::new(&want).extension().is_none() { format!("{want}.roq") } else { want.clone() };
    std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|f| f.file_name().is_some_and(|n| {
        let n = n.to_string_lossy().to_ascii_lowercase();
        n == want || n == want_ext
    }))
}

impl MoviePlayer {
    /// `Movie.Play(MovieFilename, UseSound, LoopMovie)` with the file already located.
    pub fn play_file(&mut self, path: &Path, use_sound: bool, looping: bool) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let d = Decoder::new(data).map_err(|e: RoqError| format!("{}: {e}", path.display()))?;
        self.rgba = vec![0; (d.width * d.height * 4) as usize];
        for px in self.rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        self.decoder = Some(d);
        self.filename = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.use_sound = use_sound;
        self.looping = looping;
        self.paused = false;
        self.stop_at_end = false;
        self.playing = true;
        self.time = 0.0;
        self.frames_shown = 0;
        self.audio.clear();
        Ok(())
    }

    /// `Movie.Play` resolving a script file name under the install's `Movies` folder.
    pub fn play(&mut self, game: &Path, name: &str, use_sound: bool, looping: bool) -> Result<(), String> {
        let path = find_movie(game, name).ok_or_else(|| format!("movie '{name}' not found"))?;
        self.play_file(&path, use_sound, looping)
    }

    /// `HUD.PlayMovieDirect(MovieFilename, XPos, YPos, UseSound, LoopMovie)`.
    pub fn play_direct(&mut self, game: &Path, name: &str, x: i32, y: i32, use_sound: bool, looping: bool) -> Result<(), String> {
        self.stop_now();
        self.placement = Placement::Direct { x, y };
        self.play(game, name, use_sound, looping)
    }

    /// `HUD.PlayMovieScaled(InMovie, MovieFileName, Left, Top, Right, Bottom, UseSound, LoopMovie, Translucent)`.
    #[allow(clippy::too_many_arguments)]
    pub fn play_scaled(&mut self, game: &Path, name: &str, left: f32, top: f32, right: f32, bottom: f32, use_sound: bool, looping: bool, translucent: bool) -> Result<(), String> {
        self.stop_now();
        let c = |v: f32| v.clamp(0.0, 1.0);
        self.placement = Placement::Scaled { left: c(left), top: c(top), right: c(right), bottom: c(bottom), translucent };
        self.play(game, name, use_sound, looping)
    }

    pub fn pause(&mut self, pause: bool) {
        self.paused = pause;
    }

    pub fn is_paused(&self) -> bool {
        self.playing && self.paused
    }

    pub fn stop_now(&mut self) {
        self.playing = false;
        self.decoder = None;
        self.audio.clear();
    }

    /// Lets a looping movie finish its current pass and stop.
    pub fn stop_at_end(&mut self) {
        self.stop_at_end = true;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn width(&self) -> u32 {
        self.decoder.as_ref().map_or(0, |d| d.width)
    }

    pub fn height(&self) -> u32 {
        self.decoder.as_ref().map_or(0, |d| d.height)
    }

    pub fn fps(&self) -> u32 {
        self.decoder.as_ref().map_or(30, |d| d.fps)
    }

    /// Playback time in seconds.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// The most recent frame (RGBA8, `width * height * 4`).
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Display aspect used by the HUD (it ignores the 512x256 coded size).
    pub fn display_aspect(&self) -> f32 {
        if self.filename.eq_ignore_ascii_case("pigeons.roq") { 4.0 / 3.0 } else { 16.0 / 9.0 }
    }

    /// Letterboxed screen rectangle (x, y, w, h) in pixels for a canvas of `sw` x `sh`.
    pub fn screen_rect(&self, sw: f32, sh: f32) -> (f32, f32, f32, f32) {
        match self.placement {
            Placement::Direct { x, y } => (x as f32, y as f32, self.width() as f32, self.height() as f32),
            Placement::Scaled { left, top, right, bottom, .. } => {
                let (rx, ry, rw, rh) = (left * sw, top * sh, (right - left) * sw, (bottom - top) * sh);
                let a = self.display_aspect();
                if rw / rh.max(1.0) > a {
                    let w = rh * a;
                    (rx + (rw - w) / 2.0, ry, w, rh)
                } else {
                    let h = rw / a;
                    (rx, ry + (rh - h) / 2.0, rw, h)
                }
            }
        }
    }

    /// Advances playback by `dt` seconds, decoding as many frames as are due. Returns true when
    /// the displayed frame changed. Audio packets go to [`MoviePlayer::audio`] when sound is on.
    pub fn tick(&mut self, dt: f64) -> bool {
        if !self.playing || self.paused {
            return false;
        }
        self.time += dt;
        let Some(d) = self.decoder.as_mut() else { return false };
        let due = (self.time * d.fps as f64).floor() as u32 + 1;
        let mut changed = false;
        while self.frames_shown < due {
            match d.next_event() {
                Ok(Some(Event::Frame)) => {
                    self.frames_shown += 1;
                    changed = true;
                }
                Ok(Some(Event::Audio(a))) => {
                    if self.use_sound {
                        if a.channels == 2 {
                            self.audio.extend_from_slice(&a.samples);
                        } else {
                            self.audio.extend(a.samples.iter().flat_map(|&s| [s, s]));
                        }
                    }
                }
                Ok(None) | Err(_) => {
                    if self.looping && !self.stop_at_end {
                        d.rewind();
                        self.time = 0.0;
                        self.frames_shown = 0;
                        break;
                    }
                    self.playing = false;
                    break;
                }
            }
        }
        if changed {
            d.write_rgba(&mut self.rgba);
        }
        changed
    }

    /// Takes the pending audio (interleaved stereo i16 at [`AUDIO_RATE`]).
    pub fn take_audio(&mut self) -> (u32, Vec<i16>) {
        (AUDIO_RATE, std::mem::take(&mut self.audio))
    }

    /// The whole embedded soundtrack (stereo i16 at [`AUDIO_RATE`]), empty when the file has no
    /// audio chunks. Hosts can play it as one voice and let it drive the clock.
    pub fn embedded_audio(&self) -> Vec<i16> {
        self.decoder.as_ref().map(|d| d.all_audio()).unwrap_or_default()
    }
}
