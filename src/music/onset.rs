//! Causal per-band novelty thresholds. Histories are fixed-size and allocation-free.
use super::{MusicSettings, spectral::Features};

const HISTORY: usize = 32;

#[derive(Default)]
pub(super) struct OnsetDetector {
    baseline: [f32; 4],
    novelty: [[f32; HISTORY]; 4],
    filled: usize,
    cursor: usize,
}

impl OnsetDetector {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn scores(
        &mut self,
        features: &Features,
        settings: &MusicSettings,
        audible: bool,
        quiet_gate: f32,
    ) -> [f32; 4] {
        let window_rms = features.energy.iter().map(|v| v * v).sum::<f32>().sqrt();
        let mut scores = [0.0; 4];
        for (band, score) in scores.iter_mut().enumerate() {
            let floor = 0.18 * (0.01f32 / 0.18).powf(settings.sensitivity);
            // Sparse quiet hats are short enough that a median of their recent
            // peaks would suppress the next hat; keep the original air gate.
            let robust = if band != 3 && self.filled >= 8 {
                self.robust_floor(band)
            } else {
                0.0
            };
            // EMA preserves the familiar response; the robust floor rejects
            // transient baseline dips from vibrato and repeated soft accents.
            let threshold = floor + (self.baseline[band] * 1.8).max(robust);
            let substantial = if band == 3 { 0.03 } else { 0.12 };
            if audible
                && features.energy[band] > quiet_gate * 0.35
                && features.energy[band] > window_rms * substantial
            {
                *score = (features.novelty[band] / threshold).min(6.0);
            }
            self.baseline[band] += (features.novelty[band] - self.baseline[band]) * 0.02;
            if audible {
                self.novelty[band][self.cursor] = features.novelty[band];
            }
        }
        if audible {
            self.cursor = (self.cursor + 1) % HISTORY;
            self.filled = (self.filled + 1).min(HISTORY);
        }
        scores
    }

    fn robust_floor(&self, band: usize) -> f32 {
        let mut values = self.novelty[band];
        values[..self.filled].sort_by(f32::total_cmp);
        let median = values[self.filled / 2];
        let mut deviations = [0.0; HISTORY];
        for (i, value) in values[..self.filled].iter().enumerate() {
            deviations[i] = (value - median).abs();
        }
        deviations[..self.filled].sort_by(f32::total_cmp);
        (median + deviations[self.filled / 2] * 1.5).min(1.0)
    }
}
