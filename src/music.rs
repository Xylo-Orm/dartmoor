//! Audio-reactive Pulse workspace. Analysis runs outside the PipeWire callback.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

mod advanced;
#[cfg(test)]
#[path = "music/tests.rs"]
mod advanced_tests;
mod beat;
#[cfg(target_os = "linux")]
mod linux;
mod spectral;

pub const RATE: usize = 48_000;
pub const CHANNELS: usize = 2;
pub(crate) const MAX_SAMPLES: usize = 16_384;

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
pub enum DetectorMode {
    Classic,
    #[default]
    Advanced,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TrackingState {
    #[default]
    Listening,
    Following,
    Reacquiring,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MusicSettings {
    pub detector: DetectorMode,
    pub subbass_weight: f32,
    pub sparkle_amount: f32,
    pub follow_beat: bool,
    pub input: AudioInput,
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
            detector: DetectorMode::Advanced,
            subbass_weight: 0.2,
            sparkle_amount: 0.35,
            follow_beat: true,
            input: AudioInput::Playback,
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
        ] {
            ensure!(
                value.is_finite() && (min..=max).contains(&value),
                "{name} must be between {min} and {max}"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Telemetry {
    pub bands: [f32; 4],
    pub sparkle: f32,
    pub sparkle_onsets: u64,
    pub predicted_beats: u64,
    pub bpm: Option<f32>,
    pub confidence: f32,
    pub tracking: TrackingState,
    pub discontinuities: u64,
    pub rms: f32,
    pub peak: f32,
    pub bass: f32,
    pub pulse: f32,
    pub onsets: u64,
}

pub(crate) trait AudioSource {
    /// Interleaved stereo F32 at 48 kHz, bounded to MAX_SAMPLES per block.
    fn next_samples(&mut self, timeout: Duration) -> Result<Option<Vec<f32>>>;
    /// A dropped or renegotiated block invalidates spectral/tempo continuity.
    fn take_discontinuity(&mut self) -> bool {
        false
    }
}
pub(crate) fn open(settings: &MusicSettings) -> Result<Box<dyn AudioSource>> {
    if settings.input == AudioInput::Demo {
        return Ok(Box::new(Demo::default()));
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::Playback::open(&settings.device)?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!(
            "System playback capture is currently available on Linux. Choose Demo pulses to try music mode."
        )
    }
}

#[derive(Default)]
struct Demo {
    sample: usize,
}
impl AudioSource for Demo {
    fn next_samples(&mut self, _: Duration) -> Result<Option<Vec<f32>>> {
        let samples = (0..RATE / 100)
            .flat_map(|_| {
                let t = self.sample as f32 / RATE as f32;
                self.sample = (self.sample + 1) % (RATE / 2);
                let envelope = (-t.rem_euclid(0.5) * 24.0).exp();
                let v = (t * std::f32::consts::TAU * 80.0).sin() * envelope * 0.7;
                [v, v]
            })
            .collect();
        Ok(Some(samples))
    }
}

/// Fixed 10ms energy windows, stereo low-pass bass energy, adaptive onset
/// threshold, a refractory period, and a separate attack/release envelope.
#[derive(Default)]
struct ClassicAnalyzer {
    low: [f32; CHANNELS],
    energy: f64,
    bass_energy: f64,
    peak: f32,
    frames: usize,
    baseline: f32,
    bass_level: f32,
    previous_bass: f32,
    refractory: f32,
    pulse_hold: f32,
    telemetry: Telemetry,
}
impl ClassicAnalyzer {
    pub fn feed(&mut self, samples: &[f32], settings: &MusicSettings) {
        let coefficient = 1.0 - (-std::f32::consts::TAU * 180.0 / RATE as f32).exp();
        for frame in samples
            .as_chunks::<CHANNELS>()
            .0
            .iter()
            .take(MAX_SAMPLES / CHANNELS)
        {
            for (channel, value) in frame.iter().enumerate() {
                let value = if value.is_finite() {
                    value.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                self.low[channel] += coefficient * (value - self.low[channel]);
                self.energy += f64::from(value).powi(2);
                self.bass_energy += f64::from(self.low[channel]).powi(2);
                self.peak = self.peak.max(value.abs());
            }
            self.frames += 1;
            if self.frames == RATE / 100 {
                let n = (self.frames * CHANNELS) as f64;
                let rms = (self.energy / n).sqrt() as f32;
                let bass = (self.bass_energy / n).sqrt() as f32;
                self.refractory = (self.refractory - 0.01).max(0.0);
                self.pulse_hold = (self.pulse_hold - 0.01).max(0.0);
                // Include modest attacks in compressed music and quiet playback.
                // Sensitivity changes both relative contrast and the absolute gate.
                // Short energy smoothing rejects fluctuations from individual
                // bass cycles without masking ordinary kick attacks.
                self.bass_level += (bass - self.bass_level) * (1.0 - (-1.0f32).exp());
                let level = self.bass_level;
                let ratio = 2.1 - 0.98 * settings.sensitivity.sqrt();
                let bass_gate = 0.003 - 0.0022 * settings.sensitivity;
                let quiet_gate = 0.002 - 0.0008 * settings.sensitivity;
                let onset = self.refractory == 0.0
                    && level > bass_gate
                    && level > self.baseline.max(bass_gate * 0.5) * ratio
                    && level - self.previous_bass > self.baseline * 0.03
                    && bass > rms * 0.25
                    && rms > quiet_gate;
                self.baseline += (level - self.baseline) * (1.0 - (-0.01f32 / 0.35).exp());
                self.previous_bass = level;
                if onset {
                    self.refractory = 0.12;
                    self.pulse_hold = 0.06;
                    self.telemetry.onsets += 1;
                }
                let target = if rms < quiet_gate {
                    0.0
                } else if self.pulse_hold > 0.0 {
                    1.0
                } else {
                    (rms * (1.0 + settings.sensitivity * 7.0)).sqrt().min(1.0) * 0.35
                };
                self.envelope(target, 0.01, settings);
                self.telemetry.rms = rms;
                self.telemetry.bass = bass;
                self.telemetry.peak = self.peak;
                self.frames = 0;
                self.energy = 0.0;
                self.bass_energy = 0.0;
                self.peak = 0.0;
            }
        }
    }
    fn envelope(&mut self, target: f32, seconds: f32, settings: &MusicSettings) {
        let tau = if target > self.telemetry.pulse {
            settings.attack_ms
        } else {
            settings.decay_ms
        } / 1000.0;
        let alpha = if tau <= 0.0 {
            1.0
        } else {
            1.0 - (-seconds / tau).exp()
        };
        self.telemetry.pulse += (target - self.telemetry.pulse) * alpha;
        if target == 0.0 && self.telemetry.pulse < 0.0001 {
            self.telemetry.pulse = 0.0;
        }
    }
    pub fn idle(&mut self, dt: Duration, settings: &MusicSettings) {
        self.envelope(0.0, dt.as_secs_f32(), settings);
        self.telemetry.rms = 0.0;
        self.telemetry.peak = 0.0;
        self.telemetry.bass = 0.0;
        self.refractory = (self.refractory - dt.as_secs_f32()).max(0.0);
        self.pulse_hold = (self.pulse_hold - dt.as_secs_f32()).max(0.0);
        self.previous_bass = 0.0;
        self.bass_level *= (-dt.as_secs_f32() / 0.01).exp();
    }
    pub fn telemetry(&self) -> Telemetry {
        self.telemetry
    }
    pub fn color(&self, settings: &MusicSettings) -> [u8; 3] {
        crate::core::encode(
            settings.color.map(crate::core::srgb_to_linear),
            settings.brightness * self.telemetry.pulse,
        )
    }
}

/// A mode change replaces analysis state without reopening the audio source.
#[derive(Default)]
pub(crate) struct Analyzer {
    mode: Option<DetectorMode>,
    classic: ClassicAnalyzer,
    advanced: Option<Box<advanced::Advanced>>,
    discontinuities: u64,
}
impl Analyzer {
    fn configure(&mut self, settings: &MusicSettings) {
        if self.mode != Some(settings.detector) {
            self.classic = ClassicAnalyzer::default();
            self.advanced = (settings.detector == DetectorMode::Advanced)
                .then(|| Box::new(advanced::Advanced::default()));
            self.mode = Some(settings.detector);
        }
    }
    pub fn feed(&mut self, samples: &[f32], settings: &MusicSettings) {
        self.configure(settings);
        if let Some(advanced) = &mut self.advanced {
            advanced.feed(samples, settings);
        } else {
            self.classic.feed(samples, settings);
        }
    }
    pub fn idle(&mut self, dt: Duration, settings: &MusicSettings) {
        self.configure(settings);
        if let Some(advanced) = &mut self.advanced {
            advanced.idle(dt, settings);
        } else {
            self.classic.idle(dt, settings);
        }
    }
    pub fn discontinuity(&mut self) {
        self.discontinuities += 1;
        if let Some(advanced) = &mut self.advanced {
            advanced.discontinuity();
        }
        self.classic = ClassicAnalyzer::default();
    }
    pub fn telemetry(&self) -> Telemetry {
        let mut telemetry = self
            .advanced
            .as_ref()
            .map_or_else(|| self.classic.telemetry(), |a| a.telemetry);
        telemetry.discontinuities = self.discontinuities;
        telemetry
    }
    pub fn colors(&self, zones: usize, settings: &MusicSettings) -> Vec<[u8; 3]> {
        self.advanced.as_ref().map_or_else(
            || vec![self.classic.color(settings); zones],
            |a| a.colors(zones, settings),
        )
    }
    pub fn light_colors(
        &self,
        light: &crate::core::Light,
        settings: &MusicSettings,
    ) -> Vec<[u8; 3]> {
        if matches!(light.shape, crate::core::Shape::Bulb { .. }) {
            vec![self.colors(1, settings)[0]; light.zones]
        } else {
            self.colors(light.zones, settings)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sine(frequency: f32, amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|n| {
                let v =
                    (n as f32 * frequency * std::f32::consts::TAU / RATE as f32).sin() * amplitude;
                [v, -v] // Opposite-phase stereo must not cancel energy.
            })
            .collect()
    }
    #[test]
    fn bass_attacks_pulse_and_silence_fades_completely_off() {
        let settings = MusicSettings::default();
        let mut analyzer = ClassicAnalyzer::default();
        analyzer.feed(&vec![0.0; 960], &settings);
        assert_eq!(analyzer.color(&settings), [0; 3]);
        analyzer.feed(&sine(80.0, 0.7, 480), &settings);
        assert_eq!(analyzer.telemetry.onsets, 1);
        assert!(analyzer.telemetry.rms > 0.3 && analyzer.telemetry.pulse > 0.1);
        for _ in 0..500 {
            analyzer.feed(&vec![0.0; 960], &settings);
        }
        assert_eq!(analyzer.color(&settings), [0; 3]);
        assert_eq!(analyzer.telemetry.onsets, 1);
    }
    #[test]
    fn high_frequency_and_quiet_noise_do_not_trigger_bass_pulses() {
        for (frequency, amplitude) in [(4000.0, 0.5), (80.0, 0.001)] {
            let mut analyzer = ClassicAnalyzer::default();
            analyzer.feed(&sine(frequency, amplitude, 2400), &MusicSettings::default());
            assert_eq!(analyzer.telemetry.onsets, 0);
        }
    }
    #[test]
    fn quiet_bass_is_visible_but_sub_gate_noise_stays_off() {
        let settings = MusicSettings::default();
        let mut quiet = ClassicAnalyzer::default();
        quiet.feed(&sine(100.0, 0.004, 3840), &settings);
        assert_eq!(quiet.telemetry.onsets, 1);
        assert!(quiet.telemetry.pulse > 0.85);
        assert!(quiet.color(&settings)[2] > 180);
        for sensitivity in [0.0, 0.5, 1.0] {
            let mut noise = ClassicAnalyzer::default();
            let settings = MusicSettings {
                sensitivity,
                ..Default::default()
            };
            for _ in 0..20 {
                noise.feed(&sine(100.0, 0.001, 2400), &settings);
            }
            assert_eq!(noise.telemetry.onsets, 0);
            assert_eq!(noise.color(&settings), [0; 3]);
        }
    }
    #[test]
    fn high_sensitivity_recognizes_repeated_modest_beats_above_sustained_bass() {
        for (sensitivity, expected_beats) in [(0.0, 0), (1.0, 10)] {
            let settings = MusicSettings {
                sensitivity,
                ..Default::default()
            };
            let mut analyzer = ClassicAnalyzer::default();
            let bed = sine(100.0, 0.08, 2400);
            for _ in 0..40 {
                analyzer.feed(&bed, &settings);
            }
            let before = analyzer.telemetry.onsets;
            for _ in 0..10 {
                // A 30% bass rise, rather than the old 1.5–3.5x requirement.
                analyzer.feed(&sine(100.0, 0.104, 1440), &settings);
                for _ in 0..9 {
                    analyzer.feed(&bed, &settings);
                }
            }
            assert_eq!(analyzer.telemetry.onsets - before, expected_beats);
            let before = analyzer.telemetry.onsets;
            for _ in 0..40 {
                analyzer.feed(&bed, &settings);
            }
            assert_eq!(
                analyzer.telemetry.onsets, before,
                "steady bass is not a beat"
            );
        }
    }
    #[test]
    fn default_attack_reaches_a_visible_peak_then_decay_releases_it() {
        let settings = MusicSettings::default();
        let mut analyzer = ClassicAnalyzer::default();
        let bed = sine(100.0, 0.08, 2400);
        for _ in 0..40 {
            analyzer.feed(&bed, &settings);
        }
        let before = analyzer.telemetry.onsets;
        analyzer.feed(&sine(100.0, 0.15, 480), &settings);
        for _ in 0..5 {
            analyzer.feed(&sine(100.0, 0.08, 480), &settings);
        }
        assert_eq!(analyzer.telemetry.onsets, before + 1);
        assert!(
            analyzer.telemetry.pulse > 0.9,
            "brief beats must survive attack smoothing"
        );
        for _ in 0..40 {
            analyzer.feed(&bed, &settings);
        }
        assert!(analyzer.telemetry.pulse < 0.3);
    }
    #[test]
    fn sustained_bass_cycles_do_not_retrigger_at_maximum_sensitivity() {
        let settings = MusicSettings {
            sensitivity: 1.0,
            ..Default::default()
        };
        for frequency in [40.0, 60.0, 80.0, 100.0, 120.0, 160.0] {
            let mut analyzer = ClassicAnalyzer::default();
            let tone = sine(frequency, 0.1, 2400);
            for _ in 0..40 {
                analyzer.feed(&tone, &settings);
            }
            let before = analyzer.telemetry.onsets;
            for _ in 0..100 {
                analyzer.feed(&tone, &settings);
            }
            assert_eq!(
                analyzer.telemetry.onsets, before,
                "steady {frequency} Hz bass"
            );
        }
    }
    #[test]
    fn analysis_is_finite_bounded_and_independent_of_block_boundaries() {
        let settings = MusicSettings::default();
        let samples = sine(80.0, 0.5, 4000);
        let mut whole = ClassicAnalyzer::default();
        whole.feed(&samples, &settings);
        let mut split = ClassicAnalyzer::default();
        for chunk in samples.chunks(202) {
            split.feed(chunk, &settings);
        }
        assert!((whole.telemetry.pulse - split.telemetry.pulse).abs() < 1e-6);
        split.feed(&vec![f32::NAN; 960], &settings);
        split.feed(&vec![f32::INFINITY; 960], &settings);
        assert!(split.telemetry.pulse.is_finite() && (0.0..=1.0).contains(&split.telemetry.pulse));
    }
    #[test]
    fn sensitivity_and_envelopes_change_response_without_amplifying_silence() {
        let mut low = ClassicAnalyzer::default();
        let mut high = ClassicAnalyzer::default();
        let low_settings = MusicSettings {
            sensitivity: 0.0,
            ..Default::default()
        };
        let high_settings = MusicSettings {
            sensitivity: 1.0,
            ..Default::default()
        };
        let baseline = sine(100.0, 0.1, 4800);
        for _ in 0..10 {
            low.feed(&baseline, &low_settings);
            high.feed(&baseline, &high_settings);
        }
        let low_before = low.telemetry.onsets;
        let high_before = high.telemetry.onsets;
        let attack = sine(100.0, 0.16, 480);
        low.feed(&attack, &low_settings);
        high.feed(&attack, &high_settings);
        assert_eq!(low.telemetry.onsets, low_before);
        assert!(high.telemetry.onsets > high_before);
        let mut slow = ClassicAnalyzer::default();
        let mut fast = ClassicAnalyzer::default();
        let settings = MusicSettings {
            attack_ms: 0.0,
            ..Default::default()
        };
        fast.feed(&attack, &settings);
        slow.feed(
            &attack,
            &MusicSettings {
                attack_ms: 500.0,
                ..settings.clone()
            },
        );
        assert!(fast.telemetry.pulse > slow.telemetry.pulse);
        fast.idle(Duration::from_secs(10), &settings);
        assert_eq!(fast.color(&settings), [0; 3]);
    }
}
