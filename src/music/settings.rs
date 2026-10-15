//! Persisted music controls and validation.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    #[default]
    Video,
    Music,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioInput {
    #[default]
    Playback,
    Demo,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoScenario {
    #[default]
    Straight,
    Hats,
    Swing,
    TempoStep,
    Quiet,
    SilenceGap,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RhythmMode {
    #[default]
    Auto,
    Manual,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MusicEffect {
    #[default]
    #[serde(alias = "traveling_wave", alias = "spectrum", alias = "ambient")]
    Pulse,
    Alternating,
}
impl MusicEffect {
    pub const ALL: [Self; 2] = [Self::Pulse, Self::Alternating];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pulse => "Pulse",
            Self::Alternating => "Alternating",
        }
    }
}

// Unit fields accept retired keys without retaining or re-saving their values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LightMusicOverride {
    pub enabled: bool,
    pub gain: f32,
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub phase_offset: (),
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub timing_ms: (),
    pub effect: Option<MusicEffect>,
}
impl Default for LightMusicOverride {
    fn default() -> Self {
        Self {
            enabled: true,
            gain: 1.0,
            phase_offset: (),
            timing_ms: (),
            effect: None,
        }
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingState {
    #[default]
    Listening,
    Following,
    Reacquiring,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MusicSettings {
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub detector: (),
    pub subbass_weight: f32,
    pub sparkle_amount: f32,
    pub sparkle_color: Option<[u8; 3]>,
    pub effect: MusicEffect,
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub preset: (),
    pub secondary_color: [u8; 3],
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub wave_width: (),
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub wave_travel_beats: (),
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub spectrum_contrast: (),
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub ambient_response_ms: (),
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub wled_timing_ms: (),
    #[serde(skip_serializing, deserialize_with = "ignore_retired_setting")]
    pub ha_timing_ms: (),
    pub output_fps: u16,
    pub light_overrides: BTreeMap<String, LightMusicOverride>,
    pub alternating_order: Vec<String>,
    pub follow_beat: bool,
    pub rhythm_mode: RhythmMode,
    pub manual_bpm: f32,
    pub tempo_min_bpm: u16,
    pub tempo_max_bpm: u16,
    pub phase_offset_beats: f32,
    #[serde(skip)]
    pub resync_revision: u64,
    pub input: AudioInput,
    pub demo_scenario: DemoScenario,
    /// Empty selects the default playback device. Otherwise a PipeWire sink name.
    pub device: String,
    pub sensitivity: f32,
    pub attack_ms: f32,
    pub decay_ms: f32,
    pub brightness: f32,
    pub color: [u8; 3],
}
impl Default for MusicSettings {
    fn default() -> Self {
        Self {
            detector: (),
            subbass_weight: 0.2,
            sparkle_amount: 0.35,
            sparkle_color: None,
            effect: MusicEffect::Pulse,
            preset: (),
            secondary_color: [255, 90, 180],
            wave_width: (),
            wave_travel_beats: (),
            spectrum_contrast: (),
            ambient_response_ms: (),
            wled_timing_ms: (),
            ha_timing_ms: (),
            output_fps: 30,
            light_overrides: BTreeMap::new(),
            alternating_order: vec![],
            follow_beat: true,
            rhythm_mode: RhythmMode::Auto,
            manual_bpm: 120.0,
            tempo_min_bpm: 60,
            tempo_max_bpm: 200,
            phase_offset_beats: 0.0,
            resync_revision: 0,
            input: AudioInput::Playback,
            demo_scenario: DemoScenario::Straight,
            device: String::new(),
            sensitivity: 0.5,
            attack_ms: 20.0,
            decay_ms: 240.0,
            brightness: 0.7,
            color: [100, 170, 255],
        }
    }
}
impl MusicSettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.device.len() <= 1024 && !self.device.chars().any(char::is_control),
            "invalid playback device name"
        );
        for (name, value, min, max) in [
            ("subbass weight", self.subbass_weight, 0.0, 1.0),
            ("sparkle amount", self.sparkle_amount, 0.0, 1.0),
            ("music sensitivity", self.sensitivity, 0.0, 1.0),
            ("music brightness", self.brightness, 0.0, 1.0),
            ("music attack", self.attack_ms, 0.0, 500.0),
            ("music decay", self.decay_ms, 20.0, 2000.0),
            ("manual BPM", self.manual_bpm, 40.0, 240.0),
            ("phase offset", self.phase_offset_beats, -0.5, 0.5),
        ] {
            ensure!(
                value.is_finite() && (min..=max).contains(&value),
                "{name} must be between {min} and {max}"
            );
        }
        ensure!(
            (40..=240).contains(&self.tempo_min_bpm)
                && (40..=240).contains(&self.tempo_max_bpm)
                && self.tempo_min_bpm < self.tempo_max_bpm,
            "tempo range must be within 40–240 BPM with minimum below maximum"
        );
        ensure!(
            matches!(self.output_fps, 30 | 60),
            "music output FPS must be 30 or 60"
        );
        ensure!(
            self.light_overrides.len() <= 64,
            "too many music light overrides"
        );
        ensure!(
            self.alternating_order.len() <= 64,
            "too many alternating light IDs"
        );
        let mut ordered = std::collections::HashSet::new();
        for id in &self.alternating_order {
            ensure!(
                !id.is_empty() && id.len() <= 128 && ordered.insert(id),
                "invalid or duplicate alternating light ID"
            );
        }
        for (id, value) in &self.light_overrides {
            ensure!(!id.is_empty() && id.len() <= 128, "invalid music light ID");
            ensure!(
                value.gain.is_finite() && (0.0..=2.0).contains(&value.gain),
                "music light gain must be 0–2"
            );
        }
        Ok(())
    }
}

fn ignore_retired_setting<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<(), D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer).map(|_| ())
}
