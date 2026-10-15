//! Multiband attacks, independent sparkle accents, and confidence-gated rhythm.
use super::{
    AnalysisEvent, AnalysisFrame, BeatEvent, MusicSettings, OnsetEvent, OnsetKind, RATE,
    RhythmMode, Telemetry, TrackingState,
    beat::Tracker,
    effects::{self, EffectContext, envelope},
    onset::OnsetDetector,
    spectral::{HOP, Spectral},
};
use std::collections::VecDeque;
use std::time::Duration;

pub(super) struct LightRenderRequest<'a> {
    pub zones: usize,
    pub light_id: &'a str,
    pub rank: usize,
    pub total: usize,
    pub force_ambient: bool,
}

pub(super) struct Advanced {
    spectrum: Spectral,
    tracker: Tracker,
    clock: f64,
    samples: u64,
    onset: OnsetDetector,
    main_wait: f32,
    hat_wait: f32,
    hold: f32,
    held_target: f32,
    sparkle_hold: f32,
    following: bool,
    recent_prediction: f64,
    rhythm_mode: RhythmMode,
    manual_bpm: f32,
    manual_next: f64,
    manual_misses: u8,
    phase_offset: f32,
    resync_revision: u64,
    pub latest_frame: Option<AnalysisFrame>,
    pub events: VecDeque<AnalysisEvent>,
    ambient_level: f32,
    pub telemetry: Telemetry,
}
impl Default for Advanced {
    fn default() -> Self {
        Self {
            spectrum: Spectral::default(),
            tracker: Tracker::default(),
            clock: 0.0,
            samples: 0,
            onset: OnsetDetector::default(),
            main_wait: 0.0,
            hat_wait: 0.0,
            hold: 0.0,
            held_target: 0.0,
            sparkle_hold: 0.0,
            following: true,
            recent_prediction: f64::NEG_INFINITY,
            rhythm_mode: RhythmMode::Auto,
            manual_bpm: 120.0,
            manual_next: f64::INFINITY,
            manual_misses: 0,
            phase_offset: 0.0,
            resync_revision: 0,
            latest_frame: None,
            events: VecDeque::with_capacity(128),
            ambient_level: 0.0,
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
        self.tracker
            .set_range(settings.tempo_min_bpm, settings.tempo_max_bpm);
        if self.rhythm_mode != settings.rhythm_mode
            || (self.manual_bpm - settings.manual_bpm).abs() > f32::EPSILON
            || self.resync_revision != settings.resync_revision
        {
            self.tracker.reset();
            self.manual_next = f64::INFINITY;
            self.manual_misses = 0;
            self.rhythm_mode = settings.rhythm_mode;
            self.manual_bpm = settings.manual_bpm;
            self.resync_revision = settings.resync_revision;
        }
        if (self.phase_offset - settings.phase_offset_beats).abs() > f32::EPSILON {
            let delta = settings.phase_offset_beats - self.phase_offset;
            self.tracker.nudge(delta);
            if self.manual_next.is_finite() {
                self.manual_next += f64::from(delta) * 60.0 / f64::from(settings.manual_bpm);
            }
            self.phase_offset = settings.phase_offset_beats;
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
                self.latest_frame = Some(AnalysisFrame {
                    time_seconds: self.clock,
                    bands: features.energy,
                    novelty: features.novelty,
                    rms: features.rms,
                    peak: features.peak,
                    audible,
                });
                let scores = self.onset.scores(&features, settings, audible, quiet_gate);
                let main_score =
                    scores[1].max(scores[2] * 0.8) + scores[0] * settings.subbass_weight * 0.35;
                let attack = audible && self.main_wait == 0.0 && main_score >= 1.0;
                let sparkle = audible && self.hat_wait == 0.0 && scores[3] >= 1.0;
                if attack {
                    if settings.rhythm_mode == RhythmMode::Manual {
                        let period = 60.0 / f64::from(settings.manual_bpm);
                        if !self.manual_next.is_finite() {
                            self.manual_next = self.clock + period;
                        } else if (self.clock - self.manual_next).abs() < period * 0.18 {
                            self.manual_next += (self.clock - self.manual_next) * 0.25;
                        }
                        self.manual_misses = 0;
                    }
                    self.main_wait = 0.12;
                    self.telemetry.onsets += 1;
                    self.push_event(AnalysisEvent::Onset(OnsetEvent {
                        sequence: self.telemetry.onsets,
                        time_seconds: self.clock,
                        kind: OnsetKind::Main,
                        strength: (main_score / 2.0).clamp(0.0, 1.0),
                    }));
                    // A late observed attack confirms a recent scheduled beat.
                    // Preserve the visual event's origin and envelope.
                    if self.clock - self.recent_prediction >= 0.12 {
                        self.hold = 0.06;
                        self.held_target = 1.0;
                        self.telemetry.visual_beats += 1;
                    }
                    self.push_event(AnalysisEvent::Beat(BeatEvent {
                        identity: self.telemetry.visual_beats,
                        time_seconds: self.clock,
                        observed: true,
                        starts_visual: self.clock - self.recent_prediction >= 0.12,
                        strength: (main_score / 2.0).clamp(0.0, 1.0),
                        confidence: self.tracker.confidence(),
                    }));
                }
                if sparkle {
                    self.hat_wait = 0.06;
                    self.sparkle_hold = 0.05;
                    self.telemetry.sparkle_onsets += 1;
                    self.push_event(AnalysisEvent::Onset(OnsetEvent {
                        sequence: self.telemetry.sparkle_onsets,
                        time_seconds: self.clock,
                        kind: OnsetKind::Sparkle,
                        strength: (scores[3] / 2.0).clamp(0.0, 1.0),
                    }));
                }
                let predicted = if settings.rhythm_mode == RhythmMode::Manual {
                    let period = 60.0 / f64::from(settings.manual_bpm);
                    if audible && self.manual_next.is_finite() && self.clock >= self.manual_next {
                        self.manual_next += period;
                        if self.manual_next <= self.clock {
                            self.manual_next = self.clock + period;
                        }
                        if self.manual_misses < 2 && !attack {
                            self.manual_misses += 1;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else if settings.follow_beat {
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
                    self.telemetry.visual_beats += 1;
                    self.recent_prediction = self.clock;
                    self.push_event(AnalysisEvent::Beat(BeatEvent {
                        identity: self.telemetry.visual_beats,
                        time_seconds: self.clock,
                        observed: false,
                        starts_visual: true,
                        strength: 0.65,
                        confidence: self.tracker.confidence(),
                    }));
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
                let ambient_target = if audible {
                    (features.rms * (2.0 + settings.sensitivity * 8.0))
                        .sqrt()
                        .min(1.0)
                } else {
                    0.0
                };
                envelope(&mut self.ambient_level, ambient_target, dt, 800.0, 800.0);
                self.telemetry.rms = features.rms;
                self.telemetry.peak = features.peak;
                self.telemetry.bass = features.energy[0] + features.energy[1];
                self.telemetry.bands = features.energy;
                if settings.rhythm_mode == RhythmMode::Manual {
                    self.telemetry.bpm =
                        self.manual_next.is_finite().then_some(settings.manual_bpm);
                    self.telemetry.confidence = if self.manual_next.is_finite() {
                        1.0
                    } else {
                        0.0
                    };
                    self.telemetry.tracking = if self.manual_next.is_finite() {
                        TrackingState::Following
                    } else {
                        TrackingState::Listening
                    };
                } else {
                    self.telemetry.bpm = self.tracker.bpm();
                    self.telemetry.confidence = self.tracker.confidence();
                    self.telemetry.tracking = self.tracker.state();
                }
            }
        }
    }
    fn push_event(&mut self, event: AnalysisEvent) {
        if self.events.len() == 128 {
            self.events.pop_front();
            self.telemetry.dropped_events += 1;
        }
        self.events.push_back(event);
    }
    pub fn idle(&mut self, dt: Duration, settings: &MusicSettings) {
        // An explicit idle or absent data must never generate predicted beats.
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
        envelope(&mut self.ambient_level, 0.0, dt.as_secs_f32(), 800.0, 800.0);
    }
    pub fn discontinuity(&mut self) {
        self.spectrum.reset();
        self.tracker.reset();
        self.onset.reset();
        self.main_wait = 0.0;
        self.hat_wait = 0.0;
        self.hold = 0.0;
        self.sparkle_hold = 0.0;
        self.recent_prediction = f64::NEG_INFINITY;
        self.manual_next = f64::INFINITY;
        self.manual_misses = 0;
        self.latest_frame = None;
        self.events.clear();
        self.telemetry.rms = 0.0;
        self.telemetry.bass = 0.0;
        self.telemetry.peak = 0.0;
        self.telemetry.bands = [0.0; 4];
        self.telemetry.bpm = None;
        self.telemetry.confidence = 0.0;
        self.telemetry.tracking = self.tracker.state();
    }
    pub fn colors(&self, zones: usize, settings: &MusicSettings) -> Vec<[u8; 3]> {
        self.colors_for_light(
            LightRenderRequest {
                zones,
                light_id: "",
                rank: 0,
                total: 1,
                force_ambient: false,
            },
            settings,
        )
    }
    pub fn colors_for_light(
        &self,
        request: LightRenderRequest<'_>,
        settings: &MusicSettings,
    ) -> Vec<[u8; 3]> {
        let LightRenderRequest {
            zones,
            light_id,
            rank,
            total,
            force_ambient,
        } = request;
        effects::render(
            &EffectContext {
                telemetry: self.telemetry,
                ambient_level: self.ambient_level,
            },
            settings,
            zones,
            light_id,
            rank,
            total,
            force_ambient,
        )
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[test]
    fn predicted_beat_is_confirmed_without_a_second_visual_event() {
        let settings = MusicSettings {
            follow_beat: false,
            ..Default::default()
        };
        let mut advanced = Advanced {
            recent_prediction: 0.01,
            telemetry: Telemetry {
                visual_beats: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let kick: Vec<_> = (0..4800)
            .flat_map(|n| {
                let t = n as f32 / RATE as f32;
                let value = (t * std::f32::consts::TAU * 180.0).sin() * (-t * 45.0).exp() * 0.2;
                [value, -value]
            })
            .collect();
        advanced.feed(&kick, &settings);
        assert_eq!(advanced.telemetry.onsets, 1);
        assert_eq!(advanced.telemetry.visual_beats, 1);
        assert!(advanced.events.iter().any(|event| matches!(event,
            AnalysisEvent::Beat(beat) if beat.identity == 1 && beat.observed && !beat.starts_visual)));
    }
}
