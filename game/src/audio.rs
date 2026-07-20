//! Placeholder audio feedback: synthesized tones, not sample playback.
//!
//! No sound assets exist yet — STARTER.md §8 lists "specific CC0/GPL asset
//! packs to be sourced" as an open decision, so nothing gets bundled or
//! fetched here. This delivers the actual `rodio` integration (the spec'd
//! subsystem) using procedurally generated PCM instead, so swapping in real
//! samples later is a content change, not an engineering one.
//!
//! Previously crashed the whole binary on load under this project's GNU/
//! LLVM-MinGW toolchain (see MEMORY.md) — `cpal`'s `windows-rs` bindings on
//! that target. Re-added after switching this project to the MSVC toolchain,
//! which is what `windows-rs` actually targets.

use std::num::NonZero;

use rodio::{buffer::SamplesBuffer, mixer::Mixer, DeviceSinkBuilder, MixerDeviceSink, Player};

const SAMPLE_RATE: u32 = 44_100;

/// Exponentially-decaying sine burst — cheap, doesn't need any audio assets,
/// and two different frequencies are enough to tell "mine" and "place" apart.
fn synth_tone(freq: f32, duration_secs: f32, sample_rate: u32) -> Vec<f32> {
    let sample_count = (duration_secs * sample_rate as f32) as usize;
    (0..sample_count)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            let envelope = (-t * 12.0).exp(); // fast decay so it reads as a "tap," not a tone
            (2.0 * std::f32::consts::PI * freq * t).sin() * envelope
        })
        .collect()
}

pub struct Audio {
    // Must stay alive for the duration of playback; unused otherwise.
    _device: MixerDeviceSink,
    mixer: Mixer,
    mine_tone: Vec<f32>,
    place_tone: Vec<f32>,
}

impl Audio {
    /// `None` if no output device is available (e.g. CI, headless) — sound
    /// is a nice-to-have, not something that should crash the game.
    pub fn new() -> Option<Self> {
        let device = DeviceSinkBuilder::open_default_sink().ok()?;
        let mixer = device.mixer().clone();
        Some(Self {
            _device: device,
            mixer,
            mine_tone: synth_tone(180.0, 0.08, SAMPLE_RATE),
            place_tone: synth_tone(420.0, 0.06, SAMPLE_RATE),
        })
    }

    pub fn play_mine(&self) {
        self.play(&self.mine_tone);
    }

    pub fn play_place(&self) {
        self.play(&self.place_tone);
    }

    fn play(&self, samples: &[f32]) {
        let player = Player::connect_new(&self.mixer);
        let channels = NonZero::new(1u16).expect("1 is nonzero");
        let sample_rate = NonZero::new(SAMPLE_RATE).expect("SAMPLE_RATE is nonzero");
        player.append(SamplesBuffer::new(channels, sample_rate, samples.to_vec()));
        player.detach(); // fire-and-forget: plays out on its own, no handle to hold
    }
}
