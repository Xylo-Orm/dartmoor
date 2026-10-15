//! Deterministic pulse source for preview and testing.
use super::{AudioSource, DemoScenario, RATE};
use anyhow::Result;
use std::time::Duration;

pub(super) struct Demo {
    sample: usize,
    scenario: DemoScenario,
}
impl Demo {
    pub fn new(scenario: DemoScenario) -> Self {
        Self {
            sample: 0,
            scenario,
        }
    }
}

pub(super) fn frame(scenario: DemoScenario, sample: usize) -> [f32; 2] {
    let t = (sample % (RATE * 12)) as f32 / RATE as f32;
    if scenario == DemoScenario::SilenceGap && (3.0..5.0).contains(&t) {
        return [0.0; 2];
    }
    let since = match scenario {
        DemoScenario::Swing => {
            let index = (t / 0.5).floor() as usize;
            let beat = index as f32 * 0.5 + if index % 2 == 1 { 0.08 } else { 0.0 };
            if t >= beat {
                t - beat
            } else {
                t - ((index - 1) as f32 * 0.5 + if (index - 1) % 2 == 1 { 0.08 } else { 0.0 })
            }
        }
        DemoScenario::TempoStep if t >= 6.0 => (t - 6.0).rem_euclid(0.4),
        _ => t.rem_euclid(0.5),
    };
    let envelope = (-since * 24.0).exp();
    let amplitude = if scenario == DemoScenario::Quiet {
        0.03
    } else {
        0.7
    };
    let bass = (t * std::f32::consts::TAU * 80.0).sin() * envelope * amplitude;
    let hat = if scenario == DemoScenario::Hats {
        (t * std::f32::consts::TAU * 6500.0).sin() * (-t.rem_euclid(0.125) * 180.0).exp() * 0.18
    } else {
        0.0
    };
    [bass + hat, bass + hat]
}

pub fn fixture(scenario: DemoScenario, seconds: usize) -> Vec<f32> {
    (0..seconds.min(60) * RATE)
        .flat_map(|sample| frame(scenario, sample))
        .collect()
}
impl AudioSource for Demo {
    fn next_samples(&mut self, _: Duration) -> Result<Option<Vec<f32>>> {
        let samples = (0..RATE / 100)
            .flat_map(|_| {
                let frame = frame(self.scenario, self.sample);
                self.sample = (self.sample + 1) % (RATE * 12);
                frame
            })
            .collect();
        Ok(Some(samples))
    }
}
