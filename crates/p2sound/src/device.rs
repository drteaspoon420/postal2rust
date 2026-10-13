//! Real-time output: the shared [`AudioEngine`] is pulled by a rodio `Source` that renders
//! small stereo blocks on the audio thread.

use crate::SharedEngine;
use crate::engine::AudioEngine;
use std::num::NonZero;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Frames rendered per lock of the engine (about 5 ms at 48 kHz).
const BLOCK_FRAMES: usize = 256;

struct EngineSource {
    engine: SharedEngine,
    buf: Vec<f32>,
    pos: usize,
    rate: u32,
}

impl Iterator for EngineSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.buf.len() {
            self.buf.resize(BLOCK_FRAMES * 2, 0.0);
            match self.engine.lock() {
                Ok(mut e) => e.render(&mut self.buf),
                Err(_) => self.buf.fill(0.0),
            }
            self.pos = 0;
        }
        let s = self.buf[self.pos];
        self.pos += 1;
        Some(s)
    }
}

impl rodio::Source for EngineSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        NonZero::new(2).unwrap()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        NonZero::new(self.rate).unwrap()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// Keeps the OS audio stream alive. Dropping it stops output.
pub struct AudioOutput {
    _sink: rodio::MixerDeviceSink,
    pub engine: SharedEngine,
}

impl AudioOutput {
    /// Opens the default output device and starts pulling from a new engine running at the
    /// device's sample rate.
    pub fn open() -> Result<Self, String> {
        let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|e| format!("audio device: {e}"))?;
        sink.log_on_drop(false);
        let rate = sink.config().sample_rate().get();
        let engine = Arc::new(Mutex::new(AudioEngine::new(rate)));
        sink.mixer().add(EngineSource { engine: engine.clone(), buf: Vec::new(), pos: 0, rate });
        Ok(Self { _sink: sink, engine })
    }
}
