//! Pure screen-region sampling and color conversion. Points are normalized screen
//! coordinates; radii are fractions of the shorter frame dimension. Frames must
//! already be downsampled to at most 160 × 90 before compiling a sampling plan.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};

pub const SAMPLE_WIDTH: usize = 160;
pub const SAMPLE_HEIGHT: usize = 90;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    Bulb {
        center: Point,
        radius: f32,
    },
    Strip {
        points: Vec<Point>,
        radius: f32,
        reverse: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Route {
    Wled {
        host: String,
        start: usize,
        count: usize,
        device_id: String,
    },
    HomeAssistant {
        entity_id: String,
    },
    Mock,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Light {
    pub id: String,
    pub name: String,
    pub shape: Shape,
    pub zones: usize,
    pub route: Route,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureSelection {
    Synthetic,
    Desktop { id: Option<String> },
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Row-major SDR sRGB pixels, without alpha.
    pub pixels: Vec<[u8; 3]>,
}

/// WLED reports a MAC address; separators and ASCII case do not change identity.
pub fn canonical_device_id(device_id: &str) -> String {
    device_id
        .chars()
        .filter(|character| !matches!(character, ':' | '-'))
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Hostnames and validated ASCII address spellings compare without case.
/// This deliberately does not resolve DNS or infer hostname/IP equivalence.
pub fn canonical_host(host: &str) -> String {
    host.to_ascii_lowercase()
}

/// Validate geometry and device identities before allocating a sampling plan.
pub fn validate_lights(lights: &[Light]) -> Result<()> {
    ensure!(lights.len() <= 64, "at most 64 lights are supported");
    let mut ids = HashSet::new();
    let mut entities = HashSet::new();
    let mut total_zones = 0usize;
    let mut ranges: Vec<(String, String, usize, usize)> = Vec::new();
    for light in lights {
        ensure!(
            !light.id.trim().is_empty() && light.id.len() <= 128,
            "light ID must contain 1–128 bytes"
        );
        ensure!(
            ids.insert(light.id.as_str()),
            "duplicate light ID: {}",
            light.id
        );
        ensure!(
            !light.name.trim().is_empty() && light.name.len() <= 256,
            "light name must contain 1–256 bytes"
        );
        ensure!(
            (1..=256).contains(&light.zones),
            "{}: zones must be between 1 and 256",
            light.name
        );
        total_zones += light.zones;
        let check_point = |point: &Point| -> Result<()> {
            ensure!(
                point.x.is_finite()
                    && point.y.is_finite()
                    && (0.0..=1.0).contains(&point.x)
                    && (0.0..=1.0).contains(&point.y),
                "{}: points must be finite normalized coordinates",
                light.name
            );
            Ok(())
        };
        let radius = match &light.shape {
            Shape::Bulb { center, radius } => {
                check_point(center)?;
                *radius
            }
            Shape::Strip { points, radius, .. } => {
                ensure!(
                    (2..=128).contains(&points.len()),
                    "{}: strip requires 2–128 points",
                    light.name
                );
                for point in points {
                    check_point(point)?;
                }
                ensure!(
                    points.windows(2).any(|pair| pair[0] != pair[1]),
                    "{}: strip must have nonzero length",
                    light.name
                );
                *radius
            }
        };
        ensure!(
            radius.is_finite() && radius > 0.0 && radius <= 1.0,
            "{}: radius must be greater than zero and at most one",
            light.name
        );
        match &light.route {
            Route::Mock => {}
            Route::HomeAssistant { entity_id } => {
                ensure!(
                    light.zones == 1,
                    "{}: Home Assistant lights require one zone covering the entire geometry",
                    light.name
                );
                ensure!(
                    entity_id.starts_with("light.")
                        && entity_id.len() > 6
                        && entity_id.len() <= 256
                        && entity_id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.'),
                    "invalid Home Assistant light entity"
                );
                ensure!(
                    entities.insert(entity_id.as_str()),
                    "multiple routes for Home Assistant entity {entity_id}"
                );
            }
            Route::Wled {
                host,
                start,
                count,
                device_id,
            } => {
                ensure!(
                    !host.is_empty()
                        && host.len() <= 253
                        && host.bytes().all(|b| b.is_ascii_alphanumeric()
                            || matches!(b, b'.' | b'-' | b':' | b'[' | b']')),
                    "WLED host must be a hostname or IP address, optionally with a port; credentials and URLs are forbidden"
                );
                ensure!(
                    !device_id.trim().is_empty() && device_id.len() <= 128,
                    "WLED device ID must contain 1–128 bytes"
                );
                ensure!(
                    (1..=4096).contains(count) && *start <= 65535,
                    "invalid WLED LED range"
                );
                let end = start
                    .checked_add(*count)
                    .ok_or_else(|| anyhow::anyhow!("WLED range overflow"))?;
                ensure!(end <= 65536, "WLED range exceeds 65536 LEDs");
                let canonical_id = canonical_device_id(device_id);
                let canonical_host = canonical_host(host);
                ensure!(
                    !canonical_id.is_empty(),
                    "WLED device identity is empty after normalization"
                );
                for (other_id, other_host, other_start, other_end) in &ranges {
                    if other_host == &canonical_host {
                        ensure!(
                            other_id == &canonical_id,
                            "WLED host is assigned conflicting device identities"
                        );
                    }
                    if other_id == &canonical_id {
                        ensure!(
                            end <= *other_start || *start >= *other_end,
                            "overlapping WLED ranges for {device_id}"
                        );
                    }
                }
                ranges.push((canonical_id, canonical_host, *start, end));
            }
        }
    }
    ensure!(
        total_zones <= 2048,
        "at most 2048 total zones are supported"
    );
    Ok(())
}

#[derive(Debug)]
struct Region {
    /// Positive weights sum to one.
    weights: Vec<(usize, f32)>,
}

#[derive(Debug)]
pub struct SamplingPlan {
    width: usize,
    height: usize,
    regions: Vec<Vec<Region>>,
}

impl SamplingPlan {
    pub fn compile(lights: &[Light], width: usize, height: usize) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && width <= SAMPLE_WIDTH && height <= SAMPLE_HEIGHT,
            "sampling frames must be downsampled to at most 160 × 90"
        );
        validate_lights(lights)?;
        let scale = width.min(height) as f32;
        let to_pixel = |p: &Point| Point {
            x: p.x * width as f32,
            y: p.y * height as f32,
        };
        let mut regions = Vec::with_capacity(lights.len());
        for light in lights {
            let mut light_regions = match &light.shape {
                Shape::Bulb { center, radius } => {
                    let center = to_pixel(center);
                    let region = make_region(&[center], radius * scale, width, height);
                    (0..light.zones)
                        .map(|_| Region {
                            weights: region.weights.clone(),
                        })
                        .collect::<Vec<_>>()
                }
                Shape::Strip { points, radius, .. } => {
                    let pixels: Vec<_> = points.iter().map(to_pixel).collect();
                    let lengths: Vec<f32> = pixels
                        .windows(2)
                        .map(|pair| distance(pair[0], pair[1]))
                        .collect();
                    let total: f32 = lengths.iter().sum();
                    (0..light.zones)
                        .map(|zone| {
                            let start = total * zone as f32 / light.zones as f32;
                            let end = total * (zone + 1) as f32 / light.zones as f32;
                            let path = cut_path(&pixels, &lengths, start, end);
                            make_region(&path, radius * scale, width, height)
                        })
                        .collect()
                }
            };
            if matches!(light.shape, Shape::Strip { reverse: true, .. }) {
                light_regions.reverse();
            }
            regions.push(light_regions);
        }
        Ok(Self {
            width,
            height,
            regions,
        })
    }

    /// Invalid/mismatched capture frames produce black, preserving zone layout.
    pub fn sample(&self, frame: &Frame) -> Vec<Vec<[f32; 3]>> {
        if frame.width != self.width
            || frame.height != self.height
            || frame.pixels.len() != self.width * self.height
        {
            return self
                .regions
                .iter()
                .map(|light| vec![[0.0; 3]; light.len()])
                .collect();
        }
        let linear: Vec<[f32; 3]> = frame
            .pixels
            .iter()
            .map(|pixel| pixel.map(srgb_to_linear))
            .collect();
        self.regions
            .iter()
            .map(|light| {
                light
                    .iter()
                    .map(|region| {
                        let mut color = [0.0; 3];
                        for &(index, weight) in &region.weights {
                            for channel in 0..3 {
                                color[channel] += linear[index][channel] * weight;
                            }
                        }
                        color
                    })
                    .collect()
            })
            .collect()
    }
}

fn distance(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn cut_path(points: &[Point], lengths: &[f32], start: f32, end: f32) -> Vec<Point> {
    let mut output = Vec::new();
    let mut offset = 0.0;
    for (segment, &length) in points.windows(2).zip(lengths) {
        if length > 0.0 && offset + length >= start && offset <= end {
            let a = ((start - offset) / length).clamp(0.0, 1.0);
            let b = ((end - offset) / length).clamp(0.0, 1.0);
            let interpolate = |t| Point {
                x: segment[0].x + (segment[1].x - segment[0].x) * t,
                y: segment[0].y + (segment[1].y - segment[0].y) * t,
            };
            output.push(interpolate(a));
            output.push(interpolate(b));
        }
        offset += length;
    }
    output
}

fn path_distance(point: Point, path: &[Point]) -> f32 {
    if path.len() == 1 {
        return distance(point, path[0]);
    }
    path.windows(2)
        .map(|pair| {
            let dx = pair[1].x - pair[0].x;
            let dy = pair[1].y - pair[0].y;
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 {
                (((point.x - pair[0].x) * dx + (point.y - pair[0].y) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            distance(
                point,
                Point {
                    x: pair[0].x + dx * t,
                    y: pair[0].y + dy * t,
                },
            )
        })
        .fold(f32::INFINITY, f32::min)
}

fn make_region(path: &[Point], radius: f32, width: usize, height: usize) -> Region {
    let min_x = path.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let max_x = path.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let min_y = path.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_y = path.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    let x0 = (min_x - radius - 1.0).floor().max(0.0) as usize;
    let x1 = (max_x + radius + 1.0).ceil().max(0.0).min(width as f32) as usize;
    let y0 = (min_y - radius - 1.0).floor().max(0.0) as usize;
    let y1 = (max_y + radius + 1.0).ceil().max(0.0).min(height as f32) as usize;
    let mut weights = Vec::new();
    let mut total = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            // Precomputed feathering avoids abrupt pixel boundaries at low resolution.
            let d = path_distance(
                Point {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                },
                path,
            );
            let weight = (radius + 0.5 - d).clamp(0.0, 1.0);
            if weight > 0.0 {
                weights.push((y * width + x, weight));
                total += weight;
            }
        }
    }
    if weights.is_empty() {
        // Tiny regions still sample their nearest on-screen pixel.
        let point = path.first().copied().unwrap_or(Point { x: 0.0, y: 0.0 });
        let x = (point.x.floor().max(0.0) as usize).min(width - 1);
        let y = (point.y.floor().max(0.0) as usize).min(height - 1);
        weights.push((y * width + x, 1.0));
    } else {
        for (_, weight) in &mut weights {
            *weight /= total;
        }
    }
    Region { weights }
}

pub fn srgb_to_linear(value: u8) -> f32 {
    let value = value as f32 / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Brightness is applied in linear light before the sRGB transfer function.
pub fn encode(color: [f32; 3], brightness: f32) -> [u8; 3] {
    let brightness = if brightness.is_finite() {
        brightness.clamp(0.0, 1.0)
    } else {
        0.0
    };
    color.map(|value| {
        let value = if value.is_finite() {
            (value * brightness).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let srgb = if value <= 0.0031308 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0).round().clamp(0.0, 255.0) as u8
    })
}

/// Time-based exponential smoothing. Layout changes initialize to the target.
pub fn smooth(
    previous: &mut Vec<Vec<[f32; 3]>>,
    target: &[Vec<[f32; 3]>],
    dt: Duration,
    tau_ms: f32,
) {
    if previous.len() != target.len()
        || previous.iter().zip(target).any(|(a, b)| a.len() != b.len())
    {
        *previous = target.to_vec();
        return;
    }
    let alpha = if !tau_ms.is_finite() || tau_ms <= 0.0 {
        1.0
    } else {
        1.0 - (-dt.as_secs_f32() * 1000.0 / tau_ms).exp()
    };
    for (old_light, new_light) in previous.iter_mut().zip(target) {
        for (old, new) in old_light.iter_mut().zip(new_light) {
            for channel in 0..3 {
                old[channel] += alpha * (new[channel] - old[channel]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn strip(zones: usize, reverse: bool) -> Light {
        Light {
            id: "strip".into(),
            name: "Strip".into(),
            shape: Shape::Strip {
                points: vec![Point { x: 0.0, y: 0.5 }, Point { x: 1.0, y: 0.5 }],
                radius: 0.2,
                reverse,
            },
            zones,
            route: Route::Mock,
        }
    }
    #[test]
    fn averaging_occurs_in_linear_light_and_covers_whole_strip() {
        let plan = SamplingPlan::compile(&[strip(1, false)], 10, 2).unwrap();
        let frame = Frame {
            width: 10,
            height: 2,
            pixels: (0..20)
                .map(|index| if index % 10 < 5 { [0; 3] } else { [255; 3] })
                .collect(),
        };
        let color = plan.sample(&frame)[0][0];
        assert!((color[0] - 0.5).abs() < 0.001);
        assert_eq!(encode(color, 1.0), [188; 3]);
    }
    #[test]
    fn reversed_strip_reverses_zones() {
        let frame = Frame {
            width: 10,
            height: 2,
            pixels: (0..20)
                .map(|index| {
                    if index % 10 < 5 {
                        [255, 0, 0]
                    } else {
                        [0, 0, 255]
                    }
                })
                .collect(),
        };
        let forward = SamplingPlan::compile(&[strip(2, false)], 10, 2)
            .unwrap()
            .sample(&frame);
        let reverse = SamplingPlan::compile(&[strip(2, true)], 10, 2)
            .unwrap()
            .sample(&frame);
        assert_eq!(forward[0][0], reverse[0][1]);
        assert!(forward[0][0][0] > forward[0][0][2]);
    }
    #[test]
    fn geometry_and_capture_dimensions_are_bounded() {
        let mut light = strip(1, false);
        light.shape = Shape::Bulb {
            center: Point {
                x: f32::NAN,
                y: 0.5,
            },
            radius: 0.2,
        };
        assert!(SamplingPlan::compile(&[light], 160, 90).is_err());
        assert!(SamplingPlan::compile(&[], 1920, 1080).is_err());
        let tiny = Light {
            id: "tiny".into(),
            name: "Tiny".into(),
            shape: Shape::Bulb {
                center: Point { x: 1.0, y: 1.0 },
                radius: 0.00001,
            },
            zones: 1,
            route: Route::Mock,
        };
        let sample = SamplingPlan::compile(&[tiny], 1, 1)
            .unwrap()
            .sample(&Frame {
                width: 1,
                height: 1,
                pixels: vec![[255; 3]],
            });
        assert_eq!(sample, vec![vec![[1.0; 3]]]);
    }
    #[test]
    fn smoothing_depends_on_elapsed_time() {
        let target = vec![vec![[1.0; 3]]];
        let mut once = vec![vec![[0.0; 3]]];
        let mut twice = once.clone();
        smooth(&mut once, &target, Duration::from_millis(100), 100.0);
        smooth(&mut twice, &target, Duration::from_millis(50), 100.0);
        smooth(&mut twice, &target, Duration::from_millis(50), 100.0);
        assert!((once[0][0][0] - twice[0][0][0]).abs() < 0.00001);
        assert!((once[0][0][0] - (1.0 - (-1.0f32).exp())).abs() < 0.00001);
    }
    #[test]
    fn encoding_roundtrips_srgb() {
        for value in 0..=255 {
            assert_eq!(encode([srgb_to_linear(value); 3], 1.0), [value; 3]);
        }
    }
}
