//! Multiband attacks, independent sparkle accents, and confidence-gated rhythm.
use super::{
    MusicSettings, RATE, Telemetry,
    beat::Tracker,
    spectral::{HOP, Spectral},
};
use std::time::Duration;

pub(super) struct Advanced {
    spectrum: Spectral,
    tracker: Tracker,
    clock: f64,
    samples: u64,
    baseline: [f32; 4],
    main_wait: f32,
    hat_wait: f32,
    hold: f32,
    held_target: f32,
    sparkle_hold: f32,
    following: bool,
    pub telemetry: Telemetry,
}
impl Default for Advanced {
    fn default() -> Self {
        Self {
            spectrum: Spectral::default(),
            tracker: Tracker::default(),
            clock: 0.0,
            samples: 0,
            baseline: [0.0; 4],
            main_wait: 0.0,
            hat_wait: 0.0,
            hold: 0.0,
            held_target: 0.0,
            sparkle_hold: 0.0,
            following: true,
            telemetry: Telemetry::default(),
        }
    }
}
impl Advanced {
    pub fn feed(&mut self, samples: &[f32], settings: &MusicSettings) {
        if self.following != settings.follow_beat {
            self.tracker.reset();
            self.following = settings.follow_beat;
        }
        for frame in samples
            .as_chunks::<2>()
            .0
            .iter()
            .take(super::MAX_SAMPLES / 2)
        {
            self.samples += 1;
            if let Some(features) = self.spectrum.push(*frame) {
                self.clock = self.samples as f64 / RATE as f64;
                let dt = HOP as f32 / RATE as f32;
                self.main_wait = (self.main_wait - dt).max(0.0);
                self.hat_wait = (self.hat_wait - dt).max(0.0);
                self.hold = (self.hold - dt).max(0.0);
                self.sparkle_hold = (self.sparkle_hold - dt).max(0.0);
                let quiet_gate = 0.002 - 0.0008 * settings.sensitivity;
                let window_rms = features.energy.iter().map(|v| v * v).sum::<f32>().sqrt();
                let audible =
                    features.rms > quiet_gate || window_rms > (quiet_gate * 0.5).max(0.0008);
                let mut scores = [0.0; 4];
                for (band, score) in scores.iter_mut().enumerate() {
                    let floor = 0.18 * (0.01f32 / 0.18).powf(settings.sensitivity);
                    let threshold = floor + self.baseline[band] * 1.8;
                    let substantial = if band == 3 { 0.03 } else { 0.12 };
                    if audible
                        && features.energy[band] > quiet_gate * 0.35
                        && features.energy[band] > window_rms * substantial
                    {
                        *score = (features.novelty[band] / threshold).min(6.0);
                    }
                    self.baseline[band] += (features.novelty[band] - self.baseline[band]) * 0.02;
                }
                let main_score =
                    scores[1].max(scores[2] * 0.8) + scores[0] * settings.subbass_weight * 0.35;
                let attack = audible && self.main_wait == 0.0 && main_score >= 1.0;
                let sparkle = audible && self.hat_wait == 0.0 && scores[3] >= 1.0;
                if attack {
                    self.main_wait = 0.12;
                    self.hold = 0.06;
                    self.held_target = 1.0;
                    self.telemetry.onsets += 1;
                }
                if sparkle {
                    self.hat_wait = 0.06;
                    self.sparkle_hold = 0.05;
                    self.telemetry.sparkle_onsets += 1;
                }
                let predicted = if settings.follow_beat {
                    self.tracker.step(
                        self.clock,
                        if attack {
                            (main_score / 2.0).min(1.0)
                        } else {
                            0.0
                        },
                        sparkle,
                        audible,
                    )
                } else {
                    false
                };
                if predicted && self.main_wait == 0.0 {
                    self.hold = 0.06;
                    self.held_target = 0.65;
                    self.telemetry.predicted_beats += 1;
                }
                let target = if !audible {
                    0.0
                } else if self.hold > 0.0 {
                    self.held_target
                } else {
                    (features.rms * (1.0 + settings.sensitivity * 7.0))
                        .sqrt()
                        .min(1.0)
                        * 0.25
                };
                if !audible {
                    self.hold = 0.0;
                    self.sparkle_hold = 0.0;
                }
                envelope(
                    &mut self.telemetry.pulse,
                    target,
                    dt,
                    settings.attack_ms,
                    settings.decay_ms,
                );
                let accent = if audible && self.sparkle_hold > 0.0 {
                    1.0
                } else {
                    0.0
                };
                envelope(&mut self.telemetry.sparkle, accent, dt, 5.0, 75.0);
                self.telemetry.rms = features.rms;
                self.telemetry.peak = features.peak;
                self.telemetry.bass = features.energy[0] + features.energy[1];
                self.telemetry.bands = features.energy;
                self.telemetry.bpm = self.tracker.bpm();
                self.telemetry.confidence = self.tracker.confidence();
                self.telemetry.tracking = self.tracker.state();
            }
        }
    }
    pub fn idle(&mut self, dt: Duration, settings: &MusicSettings) {
        // An explicit idle or absent data must never generate predicted beats.
        self.discontinuity();
        self.samples += (dt.as_secs_f64() * RATE as f64) as u64;
        envelope(
            &mut self.telemetry.pulse,
            0.0,
            dt.as_secs_f32(),
            settings.attack_ms,
            settings.decay_ms,
        );
        envelope(
            &mut self.telemetry.sparkle,
            0.0,
            dt.as_secs_f32(),
            5.0,
            75.0,
        );
    }
    pub fn discontinuity(&mut self) {
        self.spectrum.reset();
        self.tracker.reset();
        self.baseline = [0.0; 4];
        self.main_wait = 0.0;
        self.hat_wait = 0.0;
        self.hold = 0.0;
        self.sparkle_hold = 0.0;
        self.telemetry.rms = 0.0;
        self.telemetry.bass = 0.0;
        self.telemetry.peak = 0.0;
        self.telemetry.bands = [0.0; 4];
        self.telemetry.bpm = None;
        self.telemetry.confidence = 0.0;
        self.telemetry.tracking = self.tracker.state();
    }
    pub fn colors(&self, zones: usize, settings: &MusicSettings) -> Vec<[u8; 3]> {
        let palette = settings.color.map(crate::core::srgb_to_linear);
        let center = self.telemetry.sparkle_onsets.wrapping_mul(7) as usize % zones.max(1);
        (0..zones)
            .map(|zone| {
                let distance = zone.abs_diff(center);
                let distance = distance.min(zones.saturating_sub(distance));
                let accent = if zones == 1 {
                    0.3
                } else if distance == 0 {
                    1.0
                } else if distance == 1 {
                    0.35
                } else {
                    0.0
                };
                let intensity = (self.telemetry.pulse
                    + (1.0 - self.telemetry.pulse)
                        * self.telemetry.sparkle
                        * settings.sparkle_amount
                        * accent)
                    .clamp(0.0, 1.0);
                crate::core::encode(palette, intensity * settings.brightness)
            })
            .collect()
    }
}
fn envelope(value: &mut f32, target: f32, dt: f32, attack_ms: f32, decay_ms: f32) {
    let tau = if target > *value { attack_ms } else { decay_ms } / 1000.0;
    let alpha = if tau <= 0.0 {
        1.0
    } else {
        1.0 - (-dt / tau).exp()
    };
    *value += (target - *value) * alpha;
    if target == 0.0 && *value < 0.0001 {
        *value = 0.0;
    }
}
