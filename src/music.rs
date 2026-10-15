//! Audio-reactive Pulse workspace. Analysis runs outside the PipeWire callback.
use serde::Serialize;
use std::time::Duration;

mod advanced;
#[cfg(test)]
#[path = "music/tests.rs"]
mod advanced_tests;
mod beat;
mod demo;
mod effects;
mod events;
mod onset;
mod settings;
mod source;
pub(crate) use source::{AudioBlock, AudioSource, open};
#[cfg(target_os = "linux")]
mod linux;
mod spectral;

pub const RATE: usize = 48_000;
pub const CHANNELS: usize = 2;
pub(crate) const MAX_SAMPLES: usize = 16_384;
pub use demo::fixture as demo_fixture;

#[cfg(target_os = "linux")]
pub(crate) fn playback_devices() -> anyhow::Result<Vec<(String, String)>> {
    linux::playback_sinks()
}

pub use events::{AnalysisEvent, AnalysisFrame, BeatEvent, BeatState, OnsetEvent, OnsetKind};
pub use settings::{
    AudioInput, DemoScenario, LightMusicOverride, MusicEffect, MusicSettings, RhythmMode, SyncMode,
    TrackingState,
};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Telemetry {
    pub bands: [f32; 4],
    pub sparkle: f32,
    pub sparkle_onsets: u64,
    pub predicted_beats: u64,
    pub visual_beats: u64,
    pub bpm: Option<f32>,
    pub confidence: f32,
    pub tracking: TrackingState,
    pub discontinuities: u64,
    pub rms: f32,
    pub peak: f32,
    pub bass: f32,
    pub pulse: f32,
    pub onsets: u64,
    pub dropped_events: u64,
    pub capture_epoch: u64,
    pub sample_cursor: u64,
    pub estimated_queue_age_ms: Option<f32>,
}

/// Multiband analysis state, independent of the audio source.
#[derive(Default)]
pub struct Analyzer {
    advanced: Box<advanced::Advanced>,
    discontinuities: u64,
    capture_epoch: u64,
    sample_cursor: u64,
    in_idle: bool,
    estimated_queue_age_ms: Option<f32>,
}
impl Analyzer {
    pub fn feed(&mut self, samples: &[f32], settings: &MusicSettings) {
        if !samples.is_empty() {
            self.in_idle = false;
            self.sample_cursor += (samples.len() / CHANNELS).min(MAX_SAMPLES / CHANNELS) as u64;
        }
        self.advanced.feed(samples, settings);
    }
    pub fn idle(&mut self, dt: Duration, settings: &MusicSettings) {
        if !self.in_idle {
            self.discontinuity();
            self.in_idle = true;
        }
        self.advanced.idle(dt, settings);
    }
    pub fn discontinuity(&mut self) {
        self.discontinuities += 1;
        self.capture_epoch += 1;
        self.sample_cursor = 0;
        self.in_idle = true;
        self.advanced.discontinuity();
    }
    pub fn telemetry(&self) -> Telemetry {
        let mut telemetry = self.advanced.telemetry;
        telemetry.discontinuities = self.discontinuities;
        telemetry.capture_epoch = self.capture_epoch;
        telemetry.sample_cursor = self.sample_cursor;
        telemetry.estimated_queue_age_ms = self.estimated_queue_age_ms;
        telemetry
    }
    pub fn set_estimated_queue_age(&mut self, age: Option<Duration>) {
        self.estimated_queue_age_ms = age.map(|value| value.as_secs_f32() * 1000.0);
    }
    pub fn latest_frame(&self) -> Option<AnalysisFrame> {
        self.advanced.latest_frame
    }
    pub fn take_events(&mut self) -> Vec<AnalysisEvent> {
        self.advanced.events.drain(..).collect()
    }
    pub fn beat_state(&self) -> BeatState {
        let telemetry = self.telemetry();
        BeatState {
            bpm: telemetry.bpm,
            confidence: telemetry.confidence,
            tracking: telemetry.tracking,
        }
    }
    pub fn colors(&self, zones: usize, settings: &MusicSettings) -> Vec<[u8; 3]> {
        self.advanced.colors(zones, settings)
    }
    pub fn light_colors(
        &self,
        light: &crate::core::Light,
        settings: &MusicSettings,
    ) -> Vec<[u8; 3]> {
        self.light_colors_indexed(light, settings, 0, 1)
    }
    pub fn light_colors_indexed(
        &self,
        light: &crate::core::Light,
        settings: &MusicSettings,
        rank: usize,
        total: usize,
    ) -> Vec<[u8; 3]> {
        let ambient = light.route.is_ambient();
        let zones = if matches!(light.shape, crate::core::Shape::Bulb { .. }) {
            1
        } else {
            light.zones
        };
        let colors = self.advanced.colors_for_light(
            advanced::LightRenderRequest {
                zones,
                light_id: &light.id,
                rank,
                total,
                force_ambient: ambient,
            },
            settings,
        );
        if matches!(light.shape, crate::core::Shape::Bulb { .. }) {
            vec![colors[0]; light.zones]
        } else {
            colors
        }
    }
}
