//! Causal tempo/phase estimation from bounded, timestamped attack history.
use super::TrackingState;
use std::collections::VecDeque;

const HISTORY: usize = 96;
#[derive(Clone, Copy)]
struct Event {
    time: f64,
    weight: f32,
}
pub(super) struct Tracker {
    events: VecDeque<Event>,
    hats: VecDeque<f64>,
    estimate_at: f64,
    period: f64,
    candidate: f64,
    votes: u8,
    next: f64,
    last_main: f64,
    last_pulse: f64,
    last_hat: f64,
    confidence: f32,
    locked: bool,
    ever_locked: bool,
    misses: u8,
}
impl Default for Tracker {
    fn default() -> Self {
        Self {
            events: VecDeque::with_capacity(HISTORY),
            hats: VecDeque::with_capacity(HISTORY),
            estimate_at: 0.0,
            period: 0.5,
            candidate: 0.5,
            votes: 0,
            next: f64::INFINITY,
            last_main: f64::NEG_INFINITY,
            last_pulse: f64::NEG_INFINITY,
            last_hat: f64::NEG_INFINITY,
            confidence: 0.0,
            locked: false,
            ever_locked: false,
            misses: 0,
        }
    }
}
impl Tracker {
    pub fn reset(&mut self) {
        self.events.clear();
        self.hats.clear();
        self.confidence = 0.0;
        self.locked = false;
        self.votes = 0;
        self.next = f64::INFINITY;
        self.last_main = f64::NEG_INFINITY;
        self.last_pulse = f64::NEG_INFINITY;
        self.last_hat = f64::NEG_INFINITY;
        self.estimate_at = 0.0;
        self.misses = 0;
    }
    pub fn confidence(&self) -> f32 {
        self.confidence
    }
    pub fn bpm(&self) -> Option<f32> {
        self.locked.then_some((60.0 / self.period) as f32)
    }
    pub fn state(&self) -> TrackingState {
        if self.locked {
            TrackingState::Following
        } else if self.ever_locked {
            TrackingState::Reacquiring
        } else {
            TrackingState::Listening
        }
    }
    pub fn step(&mut self, time: f64, main: f32, sparkle: bool, audible: bool) -> bool {
        while self
            .events
            .front()
            .is_some_and(|event| time - event.time > 8.0)
        {
            self.events.pop_front();
        }
        while self.hats.front().is_some_and(|event| time - *event > 8.0) {
            self.hats.pop_front();
        }
        if main > 0.0 {
            if self.events.len() == HISTORY {
                self.events.pop_front();
            }
            self.events.push_back(Event {
                time,
                weight: main.clamp(0.25, 1.0),
            });
            self.last_main = time;
            self.misses = 0;
            if self.locked {
                let previous = self.next - self.period;
                let nearest = if (time - previous).abs() < (time - self.next).abs() {
                    previous
                } else {
                    self.next
                };
                let error = time - nearest;
                if error.abs() < self.period * 0.18 {
                    self.next += error * 0.25;
                } else {
                    self.confidence *= 0.85;
                }
            }
            self.last_pulse = time;
        }
        if sparkle {
            if self.hats.len() == HISTORY {
                self.hats.pop_front();
            }
            self.hats.push_back(time);
            self.last_hat = time;
        }
        if !audible {
            // Quiet spaces between percussive beats are normal. Preserve the
            // estimate briefly, but never emit predictions into silence.
            if time - self.last_main > self.period * 1.5 {
                self.confidence *= 0.96;
                self.locked = false;
                self.next = f64::INFINITY;
            }
            return false;
        }
        if time - self.last_main > (self.period * 2.0).min(1.5) {
            self.confidence *= 0.96;
            self.locked = false;
            self.next = f64::INFINITY;
        }
        if time >= self.estimate_at {
            self.estimate_at = time + 0.25;
            self.estimate(time);
        }
        if self.locked && time >= self.next {
            self.next += self.period;
            if self.next <= time {
                self.next = time + self.period;
            }
            // Merge a nearby confirmed attack with its scheduled prediction.
            if time - self.last_pulse < 0.12 || self.misses >= 2 {
                return false;
            }
            self.last_pulse = time;
            self.misses += 1;
            return true;
        }
        false
    }
    fn score(&self, period: f64, anchor: f64) -> f32 {
        let mut fit = 0.0;
        let mut weights = 0.0;
        for event in &self.events {
            let phase = ((event.time - anchor) / period).rem_euclid(1.0);
            let distance = phase.min(1.0 - phase);
            fit += event.weight * (1.0 - distance / 0.18).max(0.0) as f32;
            weights += event.weight;
        }
        let span = anchor - self.events.front().unwrap().time;
        let coverage = (f64::from(weights) / (span / period + 1.0)).min(1.0) as f32;
        let mut score = fit / weights.max(0.01) * coverage.sqrt();
        // Hats support a subdivision interpretation, with deliberately limited
        // influence; a dense hat stream cannot choose the main tempo alone.
        if !self.hats.is_empty() {
            let hat_fit: f32 = self
                .hats
                .iter()
                .map(|t| {
                    let phase = ((*t - anchor) / (period / 2.0)).rem_euclid(1.0);
                    (1.0 - phase.min(1.0 - phase) / 0.18).max(0.0) as f32
                })
                .sum();
            score = score * 0.97 + 0.03 * hat_fit / self.hats.len() as f32;
        }
        score
    }
    fn estimate(&mut self, time: f64) {
        if self.events.len() < 5 {
            return;
        }
        let anchor = self.events.back().unwrap().time;
        let span = anchor - self.events.front().unwrap().time;
        if span < 2.0 || time - anchor > self.period * 2.0 {
            return;
        }
        let mut best = (0.0f32, self.period);
        let mut runner = 0.0f32;
        for bpm in 60..=200 {
            let period = 60.0 / f64::from(bpm);
            let raw = self.score(period, anchor);
            let continuity = if self.locked && (period / self.period - 1.0).abs() < 0.04 {
                0.03
            } else {
                0.0
            };
            if raw + continuity > best.0 {
                best = (raw + continuity, period);
            }
        }
        // Nearby BPMs form one candidate; ambiguity means a different tempo.
        for bpm in 60..=200 {
            let period = 60.0 / f64::from(bpm);
            if (period / best.1 - 1.0).abs() > 0.12 {
                runner = runner.max(self.score(period, anchor));
            }
        }
        if (best.1 / self.candidate - 1.0).abs() < 0.04 {
            self.votes = self.votes.saturating_add(1);
        } else {
            self.votes = 1;
            self.candidate = best.1;
        }
        let confidence =
            (best.0.min(1.0) * ((best.0 - runner) / 0.2).clamp(0.0, 1.0)).clamp(0.0, 1.0);
        self.confidence += (confidence - self.confidence) * 0.4;
        if self.votes >= 3 && self.confidence >= 0.65 {
            let changed = !self.locked || (best.1 / self.period - 1.0).abs() > 0.08;
            self.period = if changed {
                best.1
            } else {
                self.period * 0.8 + best.1 * 0.2
            };
            if changed {
                self.next = anchor + self.period;
                while self.next <= time {
                    self.next += self.period;
                }
            }
            self.locked = true;
            self.ever_locked = true;
        } else if self.confidence < 0.45 {
            self.locked = false;
            self.next = f64::INFINITY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tracks_tempos_and_subdivisions_then_stops_without_evidence() {
        for period in [1.0, 2.0 / 3.0, 0.5, 1.0 / 3.0] {
            let mut tracker = Tracker::default();
            let mut next = 0.0;
            let mut next_hat = 0.0;
            for tick in 0..1600 {
                let time = f64::from(tick) * 0.01;
                let main = if time + 0.005 >= next {
                    next += period;
                    1.0
                } else {
                    0.0
                };
                let hat = time + 0.005 >= next_hat;
                if hat {
                    next_hat += period / 2.0;
                }
                tracker.step(time, main, hat, true);
            }
            assert_eq!(
                tracker.state(),
                TrackingState::Following,
                "period {period}, confidence {}",
                tracker.confidence()
            );
            assert!((tracker.bpm().unwrap() - (60.0 / period) as f32).abs() < 3.0);
            let mut predictions = 0;
            for tick in 1600..2000 {
                predictions += usize::from(tracker.step(f64::from(tick) * 0.01, 0.0, false, true));
            }
            assert!(predictions <= 2);
            assert_ne!(tracker.state(), TrackingState::Following);
            assert!(tracker.bpm().is_none());
            assert!(!tracker.step(20.0, 0.0, false, false));
        }
    }
    #[test]
    fn hats_alone_and_irregular_events_do_not_establish_a_grid() {
        let mut tracker = Tracker::default();
        for tick in 0..1200 {
            tracker.step(f64::from(tick) * 0.01, 0.0, tick % 25 == 0, true);
        }
        assert!(tracker.bpm().is_none());
        let events = [
            0, 39, 110, 143, 227, 301, 358, 399, 478, 550, 581, 677, 709, 783, 866, 921, 998, 1047,
            1152,
        ];
        for tick in 0..1200 {
            tracker.step(
                12.0 + f64::from(tick) * 0.01,
                if events.contains(&tick) { 1.0 } else { 0.0 },
                false,
                true,
            );
        }
        assert!(tracker.bpm().is_none());
    }
    #[test]
    fn fills_one_missing_beat_over_audible_background_then_resumes_real_attacks() {
        let mut tracker = Tracker::default();
        let mut missing_predictions = 0;
        for tick in 0..1200 {
            let main = if tick % 50 == 0 && tick != 600 {
                1.0
            } else {
                0.0
            };
            let predicted = tracker.step(f64::from(tick) * 0.01, main, tick % 25 == 0, true);
            if (595..=605).contains(&tick) {
                missing_predictions += usize::from(predicted);
            }
        }
        assert_eq!(missing_predictions, 1);
        assert_eq!(tracker.state(), TrackingState::Following);
        assert!((tracker.bpm().unwrap() - 120.0).abs() < 3.0);
    }
    #[test]
    fn predictions_merge_with_real_events_and_recover_after_a_tempo_change() {
        let mut tracker = Tracker::default();
        let mut predictions = 0;
        for tick in 0..1200 {
            let main = if tick % 50 == 0 { 1.0 } else { 0.0 };
            predictions += usize::from(tracker.step(f64::from(tick) * 0.01, main, false, true));
        }
        assert!(
            predictions <= 1,
            "confirmed beats should replace predictions: {predictions}"
        );
        for tick in 1200..2800 {
            tracker.step(
                f64::from(tick) * 0.01,
                if (tick - 1200) % 40 == 0 { 1.0 } else { 0.0 },
                false,
                true,
            );
        }
        assert!((tracker.bpm().unwrap() - 150.0).abs() < 3.0);
        tracker.reset();
        assert!(tracker.bpm().is_none());
        assert_eq!(tracker.confidence(), 0.0);
    }
}
