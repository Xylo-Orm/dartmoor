//! Bounded stereo spectral analysis: no allocation after constructing the plan.
use super::{CHANNELS, RATE};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

pub(super) const WINDOW: usize = 2048;
pub(super) const HOP: usize = RATE / 100;
const BINS: usize = WINDOW / 2 + 1;

#[derive(Clone, Copy, Default)]
pub(super) struct Features {
    pub energy: [f32; 4],
    pub novelty: [f32; 4],
    pub rms: f32,
    pub peak: f32,
}

pub(super) struct Spectral {
    ring: Box<[[f32; CHANNELS]; WINDOW]>,
    position: usize,
    filled: usize,
    hop: usize,
    hann: Box<[f32; WINDOW]>,
    fft: Arc<dyn Fft<f32>>,
    work: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    power: Box<[f32; BINS]>,
    previous: Box<[f32; BINS]>,
    weights: Box<[[f32; 4]; BINS]>,
    energy: f64,
    peak: f32,
}
impl Default for Spectral {
    fn default() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(WINDOW);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            ring: Box::new([[0.0; CHANNELS]; WINDOW]),
            position: 0,
            filled: 0,
            hop: 0,
            hann: Box::new(std::array::from_fn(|n| {
                0.5 - 0.5 * (std::f32::consts::TAU * n as f32 / WINDOW as f32).cos()
            })),
            fft,
            work: vec![Complex::default(); WINDOW],
            scratch,
            power: Box::new([0.0; BINS]),
            previous: Box::new([0.0; BINS]),
            weights: Box::new(std::array::from_fn(|k| {
                let frequency = k as f32 * RATE as f32 / WINDOW as f32;
                [
                    (35.0, 90.0),
                    (90.0, 300.0),
                    (300.0, 4000.0),
                    (4000.0, 12000.0),
                ]
                .map(|(low, high)| band_weight(frequency, low, high))
            })),
            energy: 0.0,
            peak: 0.0,
        }
    }
}
fn band_weight(frequency: f32, low: f32, high: f32) -> f32 {
    let rise = ((frequency - low * 0.85) / (low * 0.3)).clamp(0.0, 1.0);
    let fall = ((high * 1.15 - frequency) / (high * 0.3)).clamp(0.0, 1.0);
    rise.min(fall)
}
impl Spectral {
    pub fn reset(&mut self) {
        self.ring.fill([0.0; CHANNELS]);
        self.previous.fill(0.0);
        self.position = 0;
        self.filled = 0;
        self.hop = 0;
        self.energy = 0.0;
        self.peak = 0.0;
    }
    pub fn push(&mut self, frame: [f32; CHANNELS]) -> Option<Features> {
        let frame = frame.map(|value| {
            if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        });
        self.ring[self.position] = frame;
        self.position = (self.position + 1) % WINDOW;
        self.filled = (self.filled + 1).min(WINDOW);
        for value in frame {
            self.energy += f64::from(value).powi(2);
            self.peak = self.peak.max(value.abs());
        }
        self.hop += 1;
        if self.hop < HOP {
            return None;
        }
        self.hop = 0;
        let mut features = Features {
            rms: (self.energy / (HOP * CHANNELS) as f64).sqrt() as f32,
            peak: self.peak,
            ..Default::default()
        };
        self.energy = 0.0;
        self.peak = 0.0;
        if self.filled < WINDOW {
            return Some(features);
        }
        self.power.fill(0.0);
        for channel in 0..CHANNELS {
            for (n, bin) in self.work.iter_mut().enumerate() {
                *bin = Complex::new(
                    self.ring[(self.position + n) % WINDOW][channel] * self.hann[n],
                    0.0,
                );
            }
            self.fft
                .process_with_scratch(&mut self.work, &mut self.scratch);
            for (power, bin) in self.power.iter_mut().zip(&self.work) {
                *power += bin.norm_sqr() / (CHANNELS * WINDOW * WINDOW) as f32;
            }
        }
        let mut sums = [0.0f32; 4];
        for k in 1..BINS - 1 {
            let magnitude = self.power[k].sqrt();
            let compressed = (magnitude * 100.0).ln_1p();
            // Compare to nearby previous bins: small pitch shifts should not
            // become a fresh onset simply by crossing a bin boundary.
            let reference = self.previous[k - 1]
                .max(self.previous[k])
                .max(self.previous[k + 1]);
            let change = (compressed - reference).max(0.0);
            for (band, sum) in sums.iter_mut().enumerate() {
                let weight = self.weights[k][band];
                features.energy[band] += self.power[k] * weight * (2.0 / 0.375);
                features.novelty[band] += change * weight;
                *sum += compressed * weight;
            }
        }
        for k in 0..BINS {
            self.previous[k] = (self.power[k].sqrt() * 100.0).ln_1p();
        }
        for (band, sum) in sums.into_iter().enumerate() {
            features.energy[band] = features.energy[band].sqrt();
            features.novelty[band] = (features.novelty[band] / sum.max(0.05)).clamp(0.0, 1.0);
        }
        Some(features)
    }
}
