//! Per-zone darkness decisions from accurate linear-light samples.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DarkZones {
    pub enabled: bool,
    /// Threshold displayed in SDR/sRGB code values; compared in linear light.
    pub threshold: u8,
}
impl Default for DarkZones {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 8,
        }
    }
}
impl DarkZones {
    pub fn validate(self) -> Result<()> {
        ensure!(
            self.threshold <= 64,
            "black sensitivity must be between 0 and 64"
        );
        Ok(())
    }
}

#[derive(Default)]
struct Zone {
    off: bool,
    since: Option<Instant>,
}
#[derive(Default)]
pub(crate) struct Detector {
    settings: Option<DarkZones>,
    zones: Vec<Vec<Zone>>,
}
impl Detector {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Force confirmed dark zones and their temporal histories to zero. Never
    /// classify based on enhancement, user brightness or already-smoothed output.
    pub fn apply(
        &mut self,
        accurate: &[Vec<[f32; 3]>],
        smoothed: &mut [Vec<[f32; 3]>],
        settings: DarkZones,
        now: Instant,
    ) -> usize {
        if self.settings != Some(settings)
            || self.zones.len() != accurate.len()
            || self
                .zones
                .iter()
                .zip(accurate)
                .any(|(a, b)| a.len() != b.len())
        {
            self.zones = accurate
                .iter()
                .map(|light| (0..light.len()).map(|_| Zone::default()).collect())
                .collect();
            self.settings = Some(settings);
        }
        if !settings.enabled {
            return 0;
        }
        let enter = crate::core::srgb_to_linear(settings.threshold);
        let leave = crate::core::srgb_to_linear(settings.threshold.saturating_add(4));
        let mut count = 0;
        for ((states, colors), output) in self.zones.iter_mut().zip(accurate).zip(smoothed) {
            for ((state, color), output) in states.iter_mut().zip(colors).zip(output) {
                let level = color.iter().copied().fold(0.0f32, f32::max);
                if state.off {
                    if level > leave {
                        state.off = false;
                        state.since = None;
                    }
                } else if level <= enter {
                    let since = state.since.get_or_insert(now);
                    if now.saturating_duration_since(*since) >= Duration::from_millis(80) {
                        state.off = true;
                    }
                } else {
                    state.since = None;
                }
                if state.off {
                    *output = [0.0; 3];
                    count += 1;
                }
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dark_zones_confirm_hysteresis_and_clear_history_independently() {
        let start = Instant::now();
        let settings = DarkZones {
            enabled: true,
            threshold: 8,
        };
        let mut detector = Detector::default();
        let mut output = vec![vec![[1.0; 3]; 2]];
        let accurate = vec![vec![[0.0; 3], [0.0, 0.0, 0.02]]];
        assert_eq!(detector.apply(&accurate, &mut output, settings, start), 0);
        assert_eq!(
            detector.apply(
                &accurate,
                &mut output,
                settings,
                start + Duration::from_millis(80)
            ),
            1
        );
        assert_eq!(output[0], [[0.0; 3], [1.0; 3]]);
        let near = vec![vec![[crate::core::srgb_to_linear(10); 3], [0.0, 0.0, 0.02]]];
        assert_eq!(
            detector.apply(
                &near,
                &mut output,
                settings,
                start + Duration::from_millis(90)
            ),
            1
        );
        let bright = vec![vec![[crate::core::srgb_to_linear(13); 3], [0.0, 0.0, 0.02]]];
        assert_eq!(
            detector.apply(
                &bright,
                &mut output,
                settings,
                start + Duration::from_millis(100)
            ),
            0
        );
    }
    #[test]
    fn disabled_settings_edits_and_reset_do_not_latch_off() {
        let now = Instant::now();
        let mut detector = Detector::default();
        let accurate = vec![vec![[0.0; 3]]];
        let mut output = vec![vec![[0.5; 3]]];
        assert_eq!(
            detector.apply(&accurate, &mut output, DarkZones::default(), now),
            0
        );
        assert_eq!(output[0][0], [0.5; 3]);
        let enabled = DarkZones {
            enabled: true,
            ..Default::default()
        };
        detector.apply(&accurate, &mut output, enabled, now);
        assert_eq!(
            detector.apply(
                &accurate,
                &mut output,
                enabled,
                now + Duration::from_millis(100)
            ),
            1
        );
        output[0][0] = [0.5; 3];
        assert_eq!(
            detector.apply(
                &accurate,
                &mut output,
                DarkZones {
                    threshold: 9,
                    ..enabled
                },
                now + Duration::from_millis(110)
            ),
            0
        );
        detector.reset();
        assert_eq!(
            detector.apply(
                &accurate,
                &mut output,
                enabled,
                now + Duration::from_secs(1)
            ),
            0
        );
    }
    #[test]
    fn short_black_flashes_do_not_accumulate_confirmation() {
        let now = Instant::now();
        let mut detector = Detector::default();
        let settings = DarkZones {
            enabled: true,
            ..Default::default()
        };
        let mut output = vec![vec![[1.0; 3]]];
        for (ms, value) in [(0, 0.0), (50, 0.1), (90, 0.0), (140, 0.1)] {
            assert_eq!(
                detector.apply(
                    &[vec![[value; 3]]],
                    &mut output,
                    settings,
                    now + Duration::from_millis(ms)
                ),
                0
            );
        }
    }
}
