//! Bounded logical-zone effects. Physical LED mapping remains in the output path.
use super::{MusicEffect, MusicSettings, Telemetry};
pub(super) struct EffectContext {
    pub telemetry: Telemetry,
    pub ambient_level: f32,
}

pub(super) fn envelope(value: &mut f32, target: f32, dt: f32, attack_ms: f32, decay_ms: f32) {
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

fn palette(settings: &MusicSettings, mix: f32) -> [f32; 3] {
    let first = settings.color.map(crate::core::srgb_to_linear);
    let second = settings.secondary_color.map(crate::core::srgb_to_linear);
    std::array::from_fn(|i| first[i] + (second[i] - first[i]) * mix.clamp(0.0, 1.0))
}

pub(super) fn render(
    ctx: &EffectContext,
    settings: &MusicSettings,
    zones: usize,
    light_id: &str,
    rank: usize,
    total: usize,
    force_ambient: bool,
) -> Vec<[u8; 3]> {
    let overrides = settings.light_overrides.get(light_id);
    if overrides.is_some_and(|item| !item.enabled) {
        return vec![[0; 3]; zones];
    }
    let gain = overrides.map_or(1.0, |item| item.gain);
    let effect = overrides
        .and_then(|item| item.effect)
        .unwrap_or(settings.effect);
    let primary = settings.color.map(crate::core::srgb_to_linear);
    let sparkle_palette = settings
        .sparkle_color
        .unwrap_or(settings.color)
        .map(crate::core::srgb_to_linear);
    let center = sparkle_center(ctx.telemetry.sparkle_onsets, light_id, zones);
    (0..zones)
        .map(|zone| {
            let x = if zones <= 1 {
                0.5
            } else {
                zone as f32 / (zones - 1) as f32
            };
            let (base_color, base_intensity) = if force_ambient {
                let treble = ctx.telemetry.bands[2] + ctx.telemetry.bands[3];
                let bass = ctx.telemetry.bands[0] + ctx.telemetry.bands[1];
                let mix = if treble + bass <= 0.0 {
                    0.0
                } else {
                    treble / (treble + bass)
                };
                (
                    palette(settings, (mix + x * 0.25).min(1.0)),
                    ctx.ambient_level,
                )
            } else {
                match effect {
                    MusicEffect::Pulse => (primary, ctx.telemetry.pulse),
                    MusicEffect::Alternating => {
                        let active = total <= 1
                            || rank
                                == ctx.telemetry.visual_beats.saturating_sub(1) as usize % total;
                        (
                            palette(settings, rank as f32 / total.max(1) as f32),
                            if active { ctx.telemetry.pulse } else { 0.0 },
                        )
                    }
                }
            };
            let distance = zone.abs_diff(center);
            // Pulse's original neighbor wrap is retained for saved layouts.
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
            let sparkle = if force_ambient {
                0.0
            } else {
                (1.0 - base_intensity).max(0.0)
                    * ctx.telemetry.sparkle
                    * settings.sparkle_amount
                    * accent
            };
            let brightness = (settings.brightness * gain).clamp(0.0, 1.0);
            if settings.sparkle_color.is_none() {
                let intensity = (base_intensity + sparkle).clamp(0.0, 1.0);
                crate::core::encode(base_color, brightness * intensity)
            } else {
                let combined = std::array::from_fn(|i| {
                    (base_color[i] * base_intensity + sparkle_palette[i] * sparkle).clamp(0.0, 1.0)
                });
                crate::core::encode(combined, brightness)
            }
        })
        .collect()
}

/// A coprime stride traverses every zone, including strips of seven or fourteen.
pub(super) fn sparkle_center(event: u64, light_id: &str, zones: usize) -> usize {
    if zones <= 1 {
        return 0;
    }
    let hash = light_id.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    let mut stride = (hash.rotate_left(17) as usize % zones).max(1);
    while gcd(stride, zones) != 1 {
        stride = stride % zones + 1;
    }
    ((hash as u128 + event as u128 * stride as u128) % zones as u128) as usize
}
fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sparkle_traverses_every_zone() {
        for zones in [1, 2, 7, 14, 16, 256] {
            let mut visited = vec![false; zones];
            for n in 0..zones {
                visited[sparkle_center(n as u64, "strip-a", zones)] = true;
            }
            assert!(visited.into_iter().all(|v| v), "zones {zones}");
        }
        assert_ne!(
            sparkle_center(0, "strip-a", 256),
            sparkle_center(0, "strip-b", 256)
        );
    }
    #[test]
    fn saved_pulse_color_matches_legacy_linear_encoding() {
        let settings = MusicSettings::default();
        let context = EffectContext {
            telemetry: Telemetry {
                pulse: 0.5,
                sparkle: 0.0,
                ..Default::default()
            },
            ambient_level: 0.0,
        };
        let expected = crate::core::encode(
            settings.color.map(crate::core::srgb_to_linear),
            settings.brightness * 0.5,
        );
        assert_eq!(
            render(&context, &settings, 7, "old-strip", 0, 1, false),
            vec![expected; 7]
        );
    }
    #[test]
    fn effects_are_bounded_and_ambient_ignores_fast_sparkles() {
        let mut settings = MusicSettings::default();
        let context = EffectContext {
            telemetry: Telemetry {
                pulse: 0.8,
                sparkle: 1.0,
                visual_beats: 1,
                bands: [0.2, 0.15, 0.1, 0.05],
                bpm: Some(120.0),
                ..Default::default()
            },
            ambient_level: 0.3,
        };
        for effect in [MusicEffect::Pulse, MusicEffect::Alternating] {
            settings.effect = effect;
            settings.brightness = 0.0;
            assert!(
                render(&context, &settings, 16, "light", 0, 2, false)
                    .iter()
                    .all(|color| *color == [0; 3])
            );
            settings.brightness = 0.7;
            assert_ne!(
                render(&context, &settings, 1, "bulb", 0, 1, true)[0],
                [0; 3]
            );
        }
        settings.effect = MusicEffect::Pulse;
        let ha = render(&context, &settings, 4, "ha", 0, 1, true);
        let mut no_sparkle = context.telemetry;
        no_sparkle.sparkle = 0.0;
        let no_sparkle_context = EffectContext {
            telemetry: no_sparkle,
            ..context
        };
        assert_eq!(
            ha,
            render(&no_sparkle_context, &settings, 4, "ha", 0, 1, true)
        );
    }
    #[test]
    fn alternating_uses_rank_and_advances_with_beats() {
        let settings = MusicSettings {
            sparkle_amount: 0.0,
            effect: MusicEffect::Alternating,
            ..Default::default()
        };
        let mut context = EffectContext {
            telemetry: Telemetry {
                pulse: 1.0,
                visual_beats: 1,
                bpm: Some(120.0),
                ..Default::default()
            },
            ambient_level: 0.0,
        };
        assert!(
            render(&context, &settings, 8, "a", 0, 2, false)
                .iter()
                .any(|c| *c != [0; 3])
        );
        assert!(
            render(&context, &settings, 8, "b", 1, 2, false)
                .iter()
                .all(|c| *c == [0; 3])
        );
        context.telemetry.visual_beats = 2;
        assert!(
            render(&context, &settings, 8, "a", 0, 2, false)
                .iter()
                .all(|c| *c == [0; 3])
        );
        assert!(
            render(&context, &settings, 8, "b", 1, 2, false)
                .iter()
                .any(|c| *c != [0; 3])
        );
    }
}
