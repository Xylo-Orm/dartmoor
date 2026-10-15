//! Stateful video ambience. Capture and geometry remain owned by their existing modules.
use crate::core::{self, Frame, Route, SamplingPlan, SoftAmbience};
use anyhow::{Result, ensure};
use std::{
    f32::consts::TAU,
    sync::LazyLock,
    time::{Duration, Instant},
};

const BINS: usize = 12;
const CHANNELS: usize = core::VIDEO_FEATURE_CHANNELS;
const SCORE_EPS: f32 = 1e-6;

#[derive(Clone, Copy, Debug, Default)]
struct Candidate {
    hue: f32,
    saturation: f32,
    support: f32,
    score: f32,
    bin: usize,
}
#[derive(Clone, Copy, Debug, Default)]
struct Pending {
    hue: f32,
    since: Option<Instant>,
}
#[derive(Clone, Debug, Default)]
struct RegionState {
    anchor: Option<f32>,
    pending: Pending,
    neutral_since: Option<Instant>,
    target_h: f32,
    target_s: f32,
    h: f32,
    s: f32,
    v: f32,
    target_v: f32,
    rgb: [f32; 3],
    rgb_velocity: [f32; 3],
    last: Option<Instant>,
}
impl RegionState {
    fn reset_dark(&mut self) {
        *self = Self::default();
    }
    fn observe(
        &mut self,
        features: &[f32; CHANNELS],
        settings: SoftAmbience,
        brightness: f32,
        route: &Route,
        fps: u32,
        now: Instant,
    ) -> [f32; 3] {
        let gap_limit = Duration::from_secs_f32((2.5 / fps.max(1) as f32).max(0.250));
        let dt = self.last.map_or(0.0, |last| {
            let gap = now.saturating_duration_since(last);
            if gap > gap_limit {
                self.pending = Pending::default();
                self.neutral_since = None;
                0.0
            } else {
                gap.as_secs_f32()
            }
        });
        self.last = Some(now);
        let mut storage = [Candidate::default(); BINS];
        let mut count = 0;
        for k in 0..BINS {
            if let Some(value) = candidate(features, k) {
                storage[count] = value;
                count += 1;
            }
        }
        let candidates = &mut storage[..count];
        candidates.sort_by(|a, b| {
            if (a.score - b.score).abs() > SCORE_EPS {
                b.score.total_cmp(&a.score)
            } else {
                let da = self.anchor.map_or(0.0, |h| distance(h, a.hue));
                let db = self.anchor.map_or(0.0, |h| distance(h, b.hue));
                da.total_cmp(&db).then(a.bin.cmp(&b.bin))
            }
        });
        let mut neutral = false;
        let selected = if candidates.is_empty() {
            self.pending = Pending::default();
            let since = *self.neutral_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Duration::from_millis(120) {
                self.target_s = 0.0;
                neutral = true;
            }
            None
        } else {
            self.neutral_since = None;
            let best = candidates[0];
            if self.anchor.is_none() || self.s < 0.02 && self.target_s < 0.02 {
                self.pending = Pending::default();
                self.anchor = Some(best.hue);
                if self.s < 0.02 {
                    self.h = best.hue;
                }
                Some(best)
            } else {
                let anchor = self.anchor.unwrap();
                let local = candidates
                    .iter()
                    .copied()
                    .find(|c| distance(anchor, c.hue) <= 60.0);
                if distance(anchor, best.hue) <= 60.0 {
                    self.pending = Pending::default();
                    self.anchor = Some(best.hue);
                    Some(best)
                } else {
                    let qualified = local.is_none_or(|c| best.score > 1.25 * c.score);
                    if !qualified {
                        self.pending = Pending::default();
                        local
                    } else {
                        if self.pending.since.is_none()
                            || distance(self.pending.hue, best.hue) > 30.0
                        {
                            self.pending = Pending {
                                hue: best.hue,
                                since: Some(now),
                            };
                        }
                        let wait = if local.is_some() { 180 } else { 80 };
                        if now.saturating_duration_since(self.pending.since.unwrap())
                            >= Duration::from_millis(wait)
                        {
                            self.pending = Pending::default();
                            self.anchor = Some(best.hue);
                            Some(best)
                        } else {
                            local
                        }
                    }
                }
            }
        };
        if let Some(candidate) = selected {
            self.target_h = red_hue_correction(candidate.hue, features[61] / candidate.score);
            self.target_s = smoothstep(0.08, 0.20, candidate.support)
                * ((1.0 + 0.5 * settings.color_strength) * candidate.saturation).min(1.0);
        }
        self.target_v = (brightness * features[60]).clamp(0.0, 1.0);
        if settings.response_ms == 0.0 {
            if self.target_s > 0.0 {
                self.h = self.target_h;
            }
            self.s = self.target_s;
            self.v = self.target_v;
            self.rgb = hsv_rgb(self.h, self.s, self.v);
            self.rgb_velocity = [0.0; 3];
            return self.rgb;
        }
        let ha = route.is_ambient();
        let hue_tau = if ha {
            settings.response_ms.max(750.0)
        } else {
            settings.response_ms
        };
        let sat_tau = if neutral { hue_tau.max(600.0) } else { hue_tau };
        let rise = if ha {
            (2.0 * settings.response_ms).max(1200.0)
        } else {
            (1.5 * settings.response_ms).max(650.0)
        };
        let fall = if ha {
            (2.0 * settings.response_ms).max(1500.0)
        } else {
            (2.0 * settings.response_ms).max(900.0)
        };
        // A direct RGB crossfade between selected families avoids sweeping
        // through unrelated hues on a large color change. The critically
        // damped output retains velocity when a new frame retargets it.
        if self.target_s > 0.0 {
            self.h = self.target_h;
        }
        self.s += (self.target_s - self.s) * alpha(dt, sat_tau);
        self.v +=
            (self.target_v - self.v) * alpha(dt, if self.target_v > self.v { rise } else { fall });
        let target_rgb = hsv_rgb(self.h, self.s.clamp(0.0, 1.0), self.v.clamp(0.0, 1.0));
        let output_tau = hue_tau * 0.6;
        for (channel, target) in target_rgb.into_iter().enumerate() {
            (self.rgb[channel], self.rgb_velocity[channel]) = damped_step(
                self.rgb[channel],
                self.rgb_velocity[channel],
                target,
                dt,
                output_tau,
            );
        }
        self.rgb.map(|channel| channel.clamp(0.0, 1.0))
    }
}
// Exact solution of a critically damped spring with a fixed target. This is
// independent of capture FPS and starts a new transition with the old velocity.
fn damped_step(value: f32, velocity: f32, target: f32, dt: f32, tau_ms: f32) -> (f32, f32) {
    let omega = 1000.0 / tau_ms;
    let displacement = value - target;
    let impulse = velocity + omega * displacement;
    let decay = (-omega * dt).exp();
    (
        target + (displacement + impulse * dt) * decay,
        (velocity - omega * impulse * dt) * decay,
    )
}
fn alpha(dt: f32, tau_ms: f32) -> f32 {
    1.0 - (-dt * 1000.0 / tau_ms).exp()
}
fn wrap(degrees: f32) -> f32 {
    let d = degrees.rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}
fn distance(a: f32, b: f32) -> f32 {
    wrap(a - b).abs()
}
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
// A mixed red/pink region can acquire a blue channel from the pink samples,
// even though it contains substantial pure-red evidence. Pull only that
// redward hue toward 0°. Uniform pink has no pure-red evidence and stays pink.
fn red_hue_correction(hue: f32, red_fraction: f32) -> f32 {
    if !(330.0..360.0).contains(&hue) {
        return hue;
    }
    hue + (360.0 - hue) * 0.75 * smoothstep(330.0, 345.0, hue) * smoothstep(0.15, 0.5, red_fraction)
}
fn candidate(features: &[f32; CHANNELS], k: usize) -> Option<Candidate> {
    let mut sums = [0.0; 5];
    for index in [(k + BINS - 1) % BINS, k, (k + 1) % BINS] {
        for channel in 0..5 {
            sums[channel] += features[index * 5 + channel];
        }
    }
    let [support, score, x, y, sat] = sums;
    if support < 0.08 || score <= 1e-8 || x.hypot(y) / score < 0.50 {
        return None;
    }
    Some(Candidate {
        hue: y.atan2(x).to_degrees().rem_euclid(360.0),
        saturation: (sat / score).clamp(0.0, 1.0),
        support,
        score,
        bin: k,
    })
}
fn hsv_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let base = match (h.rem_euclid(360.0) / 60.0) as usize {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    base.map(|n| (n + m).clamp(0.0, 1.0))
}
static TRIG: LazyLock<[(f32, f32); 4096]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let radians = TAU * i as f32 / 4096.0;
        radians.sin_cos()
    })
});
static LINEAR: LazyLock<[f32; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| core::srgb_to_linear(i as u8)));
fn trig(h: f32) -> (f32, f32) {
    let pos = h * 4096.0 / 360.0;
    let i = pos.floor() as usize % 4096;
    let f = pos.fract();
    let (s0, c0) = TRIG[i];
    let (s1, c1) = TRIG[(i + 1) % 4096];
    (s0 + (s1 - s0) * f, c0 + (c1 - c0) * f)
}
fn add_tap(cell: &mut [f32; CHANNELS], rgb: [u8; 3], weight: f32) {
    let [r, g, b] = rgb.map(|v| LINEAR[v as usize]);
    let v = r.max(g).max(b);
    let m = r.min(g).min(b);
    let d = v - m;
    let s = if v > 0.0 { d / v } else { 0.0 };
    cell[60] += weight * v;
    if d == 0.0 {
        return;
    }
    let h = if r == v {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if g == v {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let q = smoothstep(0.10, 0.30, s) * smoothstep(0.002, 0.020, v);
    if q == 0.0 {
        return;
    }
    cell[61] += weight * q * s * (1.0 - smoothstep(3.0, 12.0, distance(h, 0.0)));
    let (sin, cos) = trig(h);
    let pos = h / 30.0;
    let index = pos.floor() as usize % BINS;
    for (bin, fraction) in [
        (index, 1.0 - pos.fract()),
        ((index + 1) % BINS, pos.fract()),
    ] {
        let w = weight * fraction * q;
        let base = bin * 5;
        cell[base] += w;
        cell[base + 1] += w * s;
        cell[base + 2] += w * s * cos;
        cell[base + 3] += w * s * sin;
        cell[base + 4] += w * s * s;
    }
}

pub struct ProcessInput<'a> {
    pub plan: &'a SamplingPlan,
    pub frame: &'a Frame,
    pub routes: &'a [crate::core::Light],
    pub settings: SoftAmbience,
    pub brightness: f32,
    pub fps: u32,
    pub now: Instant,
}

#[derive(Default)]
pub struct Processor {
    cells: Vec<[f32; CHANNELS]>,
    regions: Vec<Vec<[f32; CHANNELS]>>,
    states: Vec<Vec<RegionState>>,
}
impl Processor {
    pub(crate) fn is_animating(&self) -> bool {
        self.states.iter().flatten().any(|state| {
            (state.target_s - state.s).abs() > 0.002
                || (state.target_v - state.v).abs() > 0.002
                || state.rgb_velocity.iter().any(|v| v.abs() > 0.01)
                || state
                    .rgb
                    .iter()
                    .zip(hsv_rgb(state.h, state.s, state.v))
                    .any(|(a, b)| (a - b).abs() > 0.002)
        })
    }
    pub fn retained_bytes(&self) -> usize {
        self.cells.capacity() * std::mem::size_of::<[f32; CHANNELS]>()
            + self
                .regions
                .iter()
                .map(|v| v.capacity() * std::mem::size_of::<[f32; CHANNELS]>())
                .sum::<usize>()
            + self
                .states
                .iter()
                .map(|v| v.capacity() * std::mem::size_of::<RegionState>())
                .sum::<usize>()
    }
    pub(crate) fn reset(&mut self) {
        self.states.clear();
    }
    pub(crate) fn reset_evidence(&mut self) {
        for region in self.states.iter_mut().flatten() {
            region.pending = Pending::default();
            region.neutral_since = None;
        }
    }
    pub(crate) fn force_dark(&mut self, light: usize, zone: usize) {
        if let Some(state) = self.states.get_mut(light).and_then(|zones| {
            let index = if zones.len() == 1 { 0 } else { zone };
            zones.get_mut(index)
        }) {
            state.reset_dark();
        }
    }
    pub fn process(
        &mut self,
        input: ProcessInput<'_>,
        output: &mut Vec<Vec<[f32; 3]>>,
    ) -> Result<()> {
        let ProcessInput {
            plan,
            frame,
            routes,
            settings,
            brightness,
            fps,
            now,
        } = input;
        ensure!(
            frame.valid() && frame.width * frame.height <= core::SAMPLE_WIDTH * core::SAMPLE_HEIGHT,
            "invalid video capture frame"
        );
        self.cells.resize(frame.pixels.len(), [0.0; CHANNELS]);
        for (index, cell) in self.cells.iter_mut().enumerate() {
            cell.fill(0.0);
            if let Some(taps) = &frame.taps {
                for &rgb in &taps[index] {
                    add_tap(cell, rgb, 1.0 / 16.0);
                }
            } else {
                add_tap(cell, frame.pixels[index], 1.0);
            }
        }
        plan.integrate_features(&self.cells, &mut self.regions);
        if self.states.len() != self.regions.len()
            || self.states.iter().enumerate().any(|(light, a)| {
                a.len()
                    != if plan.uniform_light(light) {
                        1
                    } else {
                        self.regions[light].len()
                    }
            })
        {
            self.states = self
                .regions
                .iter()
                .enumerate()
                .map(|(light, zones)| {
                    vec![
                        RegionState::default();
                        if plan.uniform_light(light) {
                            1
                        } else {
                            zones.len()
                        }
                    ]
                })
                .collect();
            if output.len() == self.regions.len()
                && output
                    .iter()
                    .zip(&self.regions)
                    .all(|(colors, regions)| colors.len() == regions.len())
            {
                for (light, colors) in output.iter().enumerate() {
                    if plan.uniform_light(light) {
                        self.states[light][0].rgb = colors[0];
                    } else {
                        for (state, color) in self.states[light].iter_mut().zip(colors) {
                            state.rgb = *color;
                        }
                    }
                }
            }
        }
        output.resize_with(routes.len(), Vec::new);
        for (light, features) in self.regions.iter().enumerate() {
            let mut settings = settings;
            if matches!(routes[light].shape, core::Shape::Bulb { .. })
                && let Some(response) = routes[light].video_response_ms
            {
                settings.response_ms = response;
            }
            output[light].resize(features.len(), [0.0; 3]);
            if plan.uniform_light(light) {
                let color = self.states[light][0].observe(
                    &features[0],
                    settings,
                    brightness,
                    &routes[light].route,
                    fps,
                    now,
                );
                output[light].fill(color);
                continue;
            }
            for (zone, feature) in features.iter().enumerate() {
                output[light][zone] = self.states[light][zone].observe(
                    feature,
                    settings,
                    brightness,
                    &routes[light].route,
                    fps,
                    now,
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bulb_response_overrides_are_per_light_and_apply_live() {
        let bulb = core::Light {
            id: "fast".into(),
            name: "Fast".into(),
            shape: core::Shape::Bulb {
                center: core::Point { x: 0.5, y: 0.5 },
                radius: 0.1,
            },
            zones: 1,
            route: Route::Mock,
            video_response_ms: Some(0.0),
        };
        let mut lights = vec![bulb.clone(); 4];
        lights[1].id = "slow".into();
        lights[1].video_response_ms = Some(500.0);
        lights[2].id = "inherited".into();
        lights[2].video_response_ms = None;
        lights[3].id = "strip".into();
        lights[3].shape = core::Shape::Strip {
            points: vec![
                core::Point { x: 0.1, y: 0.5 },
                core::Point { x: 0.9, y: 0.5 },
            ],
            radius: 0.1,
            segment_leds: vec![],
            reverse: false,
        };
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![[255, 0, 0]],
            taps: None,
        };
        let plan = SamplingPlan::compile(&lights, 1, 1).unwrap();
        let mut processor = Processor::default();
        let mut output = Vec::new();
        let start = Instant::now();
        let settings = SoftAmbience {
            response_ms: 100.0,
            ..Default::default()
        };
        for frame_index in 0..=15 {
            processor
                .process(
                    ProcessInput {
                        plan: &plan,
                        frame: &frame,
                        routes: &lights,
                        settings,
                        brightness: 1.0,
                        fps: 30,
                        now: start + Duration::from_millis(frame_index * 33),
                    },
                    &mut output,
                )
                .unwrap();
            assert_eq!(output[0][0], [1.0, 0.0, 0.0]);
            assert!(
                (output[2][0][0] - output[3][0][0]).abs() < 1e-6,
                "strip retains global response"
            );
        }
        assert!(
            output[1][0][0] < output[2][0][0],
            "the slow bulb should lag the inherited bulb"
        );
        lights[0].video_response_ms = None;
        lights[1].video_response_ms = Some(0.0);
        processor
            .process(
                ProcessInput {
                    plan: &plan,
                    frame: &frame,
                    routes: &lights,
                    settings,
                    brightness: 1.0,
                    fps: 30,
                    now: start + Duration::from_millis(528),
                },
                &mut output,
            )
            .unwrap();
        assert_eq!(
            output[0][0],
            [1.0, 0.0, 0.0],
            "live edit preserves settled state"
        );
        assert_eq!(
            output[1][0],
            [1.0, 0.0, 0.0],
            "zero response applies immediately"
        );
        assert!(output[2][0][0] < 0.9);
    }

    #[test]
    fn hsv_gamut_and_neutral() {
        for h in 0..360 {
            for s in [0.0, 0.5, 1.0] {
                for v in [0.0, 0.5, 1.0] {
                    let rgb = hsv_rgb(h as f32, s, v);
                    assert!(rgb.iter().all(|c| (0.0..=1.0).contains(c)));
                    assert!((rgb.into_iter().fold(0.0f32, f32::max) - v).abs() < 1e-5);
                }
            }
        }
        let mut f = [0.0; CHANNELS];
        add_tap(&mut f, [128; 3], 1.0);
        assert!((0..BINS).all(|k| candidate(&f, k).is_none()));
    }
    #[test]
    fn red_correction_reduces_pink_cast_without_moving_magenta() {
        assert_eq!(red_hue_correction(0.0, 1.0), 0.0);
        assert_eq!(red_hue_correction(300.0, 1.0), 300.0);
        assert_eq!(red_hue_correction(330.0, 1.0), 330.0);
        assert_eq!(red_hue_correction(347.0, 0.0), 347.0);
        assert!(red_hue_correction(347.0, 0.6) > 355.0);
        assert!(red_hue_correction(359.0, 1.0) < 360.0);
        let start = Instant::now();
        let settings = SoftAmbience {
            response_ms: 0.0,
            ..Default::default()
        };
        let mut red = RegionState::default();
        let near_red = mix(&[(0.6, [255, 0, 0]), (0.4, [255, 0, 128])]);
        let output = red.observe(&near_red, settings, 1.0, &Route::Mock, 30, start);
        let bytes = core::encode(output, 1.0);
        assert!(
            bytes[0] > 250 && bytes[1] == 0 && bytes[2] < 55,
            "{bytes:?}"
        );
        let mut pink = RegionState::default();
        let pink_source = mix(&[(1.0, [255, 0, 128])]);
        let pink_bytes = core::encode(
            pink.observe(&pink_source, settings, 1.0, &Route::Mock, 30, start),
            1.0,
        );
        assert!((pink_bytes[2] as i16 - 128).abs() <= 2, "{pink_bytes:?}");
        let mut magenta = RegionState::default();
        let source = mix(&[(1.0, [255, 0, 255])]);
        assert_eq!(
            core::encode(
                magenta.observe(&source, settings, 1.0, &Route::Mock, 30, start),
                1.0
            ),
            [255, 0, 255]
        );
    }
    #[test]
    fn complements_and_neutrals() {
        let mut f = [0.0; CHANNELS];
        for _ in 0..50 {
            add_tap(&mut f, [255, 0, 0], 0.01);
            add_tap(&mut f, [0, 255, 255], 0.01);
        }
        let now = Instant::now();
        let mut state = RegionState::default();
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        let mock = Route::Mock;
        for i in 0..200 {
            state.observe(
                &f,
                settings,
                1.0,
                &mock,
                30,
                now + Duration::from_millis(i * 33),
            );
        }
        assert!(state.s > 0.99 && state.v > 0.99 && distance(state.h, 0.0) < 2.0);
        let mut neutral = [0.0; CHANNELS];
        add_tap(&mut neutral, [255; 3], 1.0);
        state.observe(
            &neutral,
            settings,
            1.0,
            &mock,
            30,
            now + Duration::from_millis(200 * 33),
        );
        assert!(state.target_s > 0.99);
    }
    fn mix(parts: &[(f32, [u8; 3])]) -> [f32; CHANNELS] {
        let mut f = [0.0; CHANNELS];
        for &(area, rgb) in parts {
            add_tap(&mut f, rgb, area);
        }
        f
    }
    #[test]
    fn output_inertia_is_time_based_and_retargets_without_a_velocity_jump() {
        let mut samples = Vec::new();
        for fps in [15, 30, 60, 120] {
            let dt = 1.0 / fps as f32;
            let (mut value, mut velocity) = (0.0, 0.0);
            for _ in 0..fps {
                (value, velocity) = damped_step(value, velocity, 1.0, dt, 180.0);
            }
            samples.push((value, velocity));
        }
        for (value, velocity) in samples {
            assert!((value - 0.9747).abs() < 0.001);
            assert!(velocity > 0.0);
        }
        let (value, velocity) = damped_step(0.5, 1.0, 0.0, 0.001, 180.0);
        assert!(value > 0.5 && velocity > 0.0, "retargeting retains motion");
    }
    #[test]
    fn zero_response_applies_envelopes_immediately_without_nonfinite_values() {
        let red = mix(&[(1.0, [255, 0, 0])]);
        let blue = mix(&[(1.0, [0, 0, 255])]);
        let settings = SoftAmbience {
            response_ms: 0.0,
            ..Default::default()
        };
        let start = Instant::now();
        for route in [
            Route::Mock,
            Route::HomeAssistant {
                entity_id: "light.test".into(),
            },
        ] {
            let mut state = RegionState::default();
            assert_eq!(
                state.observe(&red, settings, 1.0, &route, 30, start),
                [1.0, 0.0, 0.0]
            );
            assert_eq!(
                state.observe(&red, settings, 0.25, &route, 30, start),
                [0.25, 0.0, 0.0]
            );
            state.observe(
                &blue,
                settings,
                1.0,
                &route,
                30,
                start + Duration::from_millis(1),
            );
            let changed = state.observe(
                &blue,
                settings,
                1.0,
                &route,
                30,
                start + Duration::from_millis(101),
            );
            assert_eq!(changed, [0.0, 0.0, 1.0]);
            assert!(state.rgb_velocity.iter().all(|velocity| *velocity == 0.0));
        }
    }
    #[test]
    fn color_cut_crossfades_without_a_third_hue() {
        let red = mix(&[(1.0, [255, 0, 0])]);
        let blue = mix(&[(1.0, [0, 0, 255])]);
        let start = Instant::now();
        let mut state = RegionState::default();
        run(&mut state, &red, start, 0, 5000, &Route::Mock);
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        let mut crossed = false;
        for ms in (5025..=6000).step_by(25) {
            let rgb = state.observe(
                &blue,
                settings,
                1.0,
                &Route::Mock,
                30,
                start + Duration::from_millis(ms),
            );
            assert!(rgb[1] < 0.01, "red to blue must not pass through green");
            if rgb[0] > 0.1 && rgb[2] > 0.1 {
                crossed = true;
            }
        }
        assert!(crossed);
    }
    fn run(
        state: &mut RegionState,
        f: &[f32; CHANNELS],
        start: Instant,
        from: u64,
        to: u64,
        route: &Route,
    ) {
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        for ms in (from..=to).step_by(25) {
            state.observe(
                f,
                settings,
                1.0,
                route,
                30,
                start + Duration::from_millis(ms),
            );
        }
    }
    #[test]
    fn neutral_area_and_subtitles_do_not_dilute_supported_color() {
        let red = mix(&[(0.30, [255, 0, 0]), (0.70, [255; 3])]);
        let red_with_text = mix(&[(0.27, [255, 0, 0]), (0.73, [255; 3])]);
        let gray = mix(&[(1.0, [128; 3])]);
        let mut state = RegionState::default();
        let start = Instant::now();
        run(&mut state, &red, start, 0, 5000, &Route::Mock);
        assert!(distance(state.h, 0.0) < 2.0 && state.s > 0.99 && state.v > 0.99);
        run(&mut state, &red_with_text, start, 5025, 7000, &Route::Mock);
        assert!(distance(state.h, 0.0) < 2.0 && state.s > 0.99 && state.v > 0.99);
        run(&mut state, &gray, start, 7025, 9500, &Route::Mock);
        assert!(state.s < 0.05 && state.target_s == 0.0);
    }
    #[test]
    fn competing_families_hold_then_confirm_and_flash_does_not_accumulate() {
        let even = mix(&[(0.5, [255, 0, 0]), (0.5, [0, 255, 255])]);
        let swing = mix(&[(0.48, [255, 0, 0]), (0.52, [0, 255, 255])]);
        let cut = mix(&[(0.35, [255, 0, 0]), (0.65, [0, 255, 255])]);
        let mut state = RegionState::default();
        let start = Instant::now();
        run(&mut state, &even, start, 0, 5000, &Route::Mock);
        assert!(distance(state.h, 0.0) < 2.0 && state.s > 0.99);
        for cycle in 0..20 {
            let t = 5025 + cycle * 150;
            state.observe(
                &swing,
                SoftAmbience {
                    enabled: true,
                    ..Default::default()
                },
                1.0,
                &Route::Mock,
                30,
                start + Duration::from_millis(t),
            );
            state.observe(
                &even,
                SoftAmbience {
                    enabled: true,
                    ..Default::default()
                },
                1.0,
                &Route::Mock,
                30,
                start + Duration::from_millis(t + 75),
            );
        }
        assert!(distance(state.anchor.unwrap(), 0.0) < 2.0);
        let t = 8100;
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        for offset in [0, 50, 100, 150] {
            state.observe(
                &cut,
                settings,
                1.0,
                &Route::Mock,
                30,
                start + Duration::from_millis(t + offset),
            );
        }
        assert!(distance(state.anchor.unwrap(), 0.0) < 2.0);
        state.observe(
            &cut,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(t + 180),
        );
        assert!(distance(state.anchor.unwrap(), 180.0) < 2.0);
        assert!(state.s > 0.99);
    }
    #[test]
    fn brief_cut_and_neutral_hold_use_elapsed_time() {
        let red = mix(&[(1.0, [255, 0, 0])]);
        let cyan = mix(&[(1.0, [0, 255, 255])]);
        let white = mix(&[(1.0, [255; 3])]);
        let start = Instant::now();
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        let mut state = RegionState::default();
        run(&mut state, &red, start, 0, 5000, &Route::Mock);
        state.observe(
            &cyan,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5025),
        );
        state.observe(
            &red,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5075),
        );
        assert!(distance(state.anchor.unwrap(), 0.0) < 2.0);
        state.observe(
            &white,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5100),
        );
        state.observe(
            &red,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5150),
        );
        assert!(state.s > 0.99 && state.target_s > 0.99);
        state.observe(
            &cyan,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5200),
        );
        state.observe(
            &cyan,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5280),
        );
        assert!(distance(state.anchor.unwrap(), 180.0) < 2.0 && state.s > 0.99);
    }
    #[test]
    fn fixed_target_envelope_is_frame_rate_independent_and_ha_slower() {
        let red = mix(&[(1.0, [255, 0, 0])]);
        let start = Instant::now();
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        let mut values = Vec::new();
        for fps in [15, 30, 60, 120] {
            let mut state = RegionState::default();
            state.observe(&red, settings, 1.0, &Route::Mock, fps, start);
            for n in 1..=fps {
                state.observe(
                    &red,
                    settings,
                    1.0,
                    &Route::Mock,
                    fps,
                    start + Duration::from_secs_f64(n as f64 / fps as f64),
                );
            }
            values.push((state.s, state.v));
        }
        for value in &values {
            assert!((value.0 - (1.0 - (-1.0f32 / 0.05).exp())).abs() < 1e-4);
            assert!((value.1 - (1.0 - (-1.0f32 / 0.65).exp())).abs() < 1e-4);
        }
        let mut ha = RegionState::default();
        let route = Route::HomeAssistant {
            entity_id: "light.test".into(),
        };
        ha.observe(&red, settings, 1.0, &route, 30, start);
        for n in 1..=30 {
            ha.observe(
                &red,
                settings,
                1.0,
                &route,
                30,
                start + Duration::from_secs_f64(n as f64 / 30.0),
            );
        }
        assert!(ha.s < values[0].0 && ha.v < values[0].1);
    }
    #[test]
    fn short_interruption_rebases_timers_without_brightness_jump() {
        let red = mix(&[(1.0, [255, 0, 0])]);
        let cyan = mix(&[(1.0, [0, 255, 255])]);
        let start = Instant::now();
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        let mut state = RegionState::default();
        run(&mut state, &red, start, 0, 5000, &Route::Mock);
        state.observe(
            &cyan,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5025),
        );
        let before = state.v;
        state.observe(
            &cyan,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5625),
        );
        assert!(distance(state.anchor.unwrap(), 0.0) < 2.0 && (state.v - before).abs() < 1e-6);
        state.observe(
            &cyan,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5700),
        );
        assert!(distance(state.anchor.unwrap(), 0.0) < 2.0);
    }
    #[test]
    fn brightness_flash_is_restrained_independently_of_color() {
        let dim = mix(&[(1.0, [89; 3])]); // roughly 0.1 linear peak
        let white = mix(&[(1.0, [255; 3])]);
        let start = Instant::now();
        let settings = SoftAmbience {
            enabled: true,
            ..Default::default()
        };
        let mut state = RegionState::default();
        run(&mut state, &dim, start, 0, 5000, &Route::Mock);
        let before = state.v;
        state.observe(
            &white,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5050),
        );
        assert!(state.v - before <= 0.075 && state.s == 0.0);
        let peak = state.v;
        state.observe(
            &dim,
            settings,
            1.0,
            &Route::Mock,
            30,
            start + Duration::from_millis(5100),
        );
        assert!(state.v < peak);
    }
    #[test]
    fn unaveraged_complements_are_selected_from_gray_preview() {
        let light = crate::core::Light {
            video_response_ms: None,
            id: "micro".into(),
            name: "Micro".into(),
            zones: 1,
            route: Route::Mock,
            shape: crate::core::Shape::Bulb {
                center: crate::core::Point { x: 0.5, y: 0.5 },
                radius: 0.1,
            },
        };
        let plan = SamplingPlan::compile(std::slice::from_ref(&light), 1, 1).unwrap();
        let taps = std::array::from_fn(|i| if i < 8 { [255, 0, 0] } else { [0, 255, 255] });
        let frame = Frame {
            width: 1,
            height: 1,
            pixels: vec![[188; 3]],
            taps: Some(vec![taps]),
        };
        assert_eq!(plan.sample(&frame)[0][0], [core::srgb_to_linear(188); 3]);
        let mut processor = Processor::default();
        let mut output = Vec::new();
        let start = Instant::now();
        for n in 0..=180 {
            processor
                .process(
                    ProcessInput {
                        plan: &plan,
                        frame: &frame,
                        routes: std::slice::from_ref(&light),
                        settings: SoftAmbience {
                            enabled: true,
                            ..Default::default()
                        },
                        brightness: 1.0,
                        fps: 30,
                        now: start + Duration::from_secs_f64(n as f64 / 30.0),
                    },
                    &mut output,
                )
                .unwrap();
        }
        let color = output[0][0];
        assert!(
            color[0] > 0.99 && color[1] < 0.01 && color[2] < 0.01,
            "{color:?}"
        );
    }
}
