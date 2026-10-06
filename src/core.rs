//! Pure screen-region sampling and color conversion. Points are normalized screen
//! coordinates; radii are fractions of the shorter frame dimension. Frames must
//! already be downsampled to at most 160 × 90 before compiling a sampling plan.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, sync::LazyLock, time::Duration};

pub const SAMPLE_WIDTH: usize = 160;
pub const SAMPLE_HEIGHT: usize = 90;

/// Saved live controls; all amounts are normalized to 0..=1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SoftAmbience {
    pub enabled: bool,
    pub strength: f32,
    pub color_emphasis: f32,
    pub vibrancy: f32,
}

impl Default for SoftAmbience {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 0.6,
            color_emphasis: 0.5,
            vibrancy: 0.2,
        }
    }
}

impl SoftAmbience {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("ambience strength", self.strength),
            ("color emphasis", self.color_emphasis),
            ("vibrancy", self.vibrancy),
        ] {
            ensure!(
                value.is_finite() && (0.0..=1.0).contains(&value),
                "{name} must be between 0 and 1"
            );
        }
        Ok(())
    }
}

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
        /// Physical LED counts in path order, one per adjacent point pair.
        /// Empty retains automatic mapping by whole-path arc length.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        segment_leds: Vec<usize>,
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
            Shape::Strip {
                points,
                radius,
                segment_leds,
                ..
            } => {
                ensure!(
                    (2..=128).contains(&points.len()),
                    "{}: strip requires 2–128 points",
                    light.name
                );
                for point in points {
                    check_point(point)?;
                }
                if !segment_leds.is_empty() {
                    ensure!(
                        segment_leds.len() == points.len() - 1,
                        "{}: specify one LED count per path segment",
                        light.name
                    );
                    ensure!(
                        segment_leds.iter().all(|count| (1..=4096).contains(count)),
                        "{}: segment LED counts must be between 1 and 4096",
                        light.name
                    );
                    let total: usize = segment_leds.iter().sum();
                    ensure!(total <= 4096, "{}: at most 4096 LEDs per strip", light.name);
                    ensure!(
                        light.zones <= total,
                        "{}: color zones cannot exceed the segment LED total ({total})",
                        light.name
                    );
                    ensure!(
                        points.windows(2).all(|pair| pair[0] != pair[1]),
                        "{}: segments with LEDs must have nonzero length",
                        light.name
                    );
                    ensure!(
                        light.zones == 1 || light.zones >= segment_leds.len(),
                        "{}: addressable output needs at least one color zone per segment",
                        light.name
                    );
                    if let Route::Wled { count, .. } = &light.route {
                        ensure!(
                            *count == total,
                            "{}: mapped LED count must equal the sum of segment LED counts ({total})",
                            light.name
                        );
                    }
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
    /// Interior pixels have raw weight one, so each horizontal run needs only
    /// two row-prefix indices. Only the feathered boundary stores pixel weights.
    runs: Box<[PixelRun]>,
    feather: Box<[FeatherPixel]>,
    inverse_weight: f64,
}

// A row-prefix table has at most (160 + 1) * 90 entries, which fits in u16.
const _: () = assert!((SAMPLE_WIDTH + 1) * SAMPLE_HEIGHT <= u16::MAX as usize);

#[derive(Debug)]
struct PixelRun {
    start: u16,
    end: u16,
}

#[derive(Debug)]
struct FeatherPixel {
    start: u16,
    weight: f32,
}

#[derive(Debug)]
enum LightRegions {
    // Physical appearance is independent of hardware zone count. All zones
    // drawn as one bulb share a single region and one average per frame.
    Uniform { region: Region, zones: usize },
    Ordered(Vec<Region>),
}
impl LightRegions {
    fn zones(&self) -> usize {
        match self {
            Self::Uniform { zones, .. } => *zones,
            Self::Ordered(regions) => regions.len(),
        }
    }
}

#[derive(Debug)]
pub struct SamplingPlan {
    width: usize,
    height: usize,
    regions: Vec<LightRegions>,
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
            let light_regions = match &light.shape {
                Shape::Bulb { center, radius } => {
                    let center = to_pixel(center);
                    let region = make_region(&[center], radius * scale, width, height);
                    LightRegions::Uniform {
                        region,
                        zones: light.zones,
                    }
                }
                Shape::Strip {
                    points,
                    radius,
                    segment_leds,
                    reverse,
                } => {
                    let pixels: Vec<_> = points.iter().map(to_pixel).collect();
                    let lengths: Vec<f32> = pixels
                        .windows(2)
                        .map(|pair| distance(pair[0], pair[1]))
                        .collect();
                    let total: f32 = lengths.iter().sum();
                    let mut regions: Vec<_> = if !segment_leds.is_empty() && light.zones > 1 {
                        let allocation = segment_zone_counts(segment_leds, light.zones);
                        pixels
                            .windows(2)
                            .zip(allocation)
                            .flat_map(|(pair, zones)| {
                                (0..zones).map(move |zone| {
                                    let a =
                                        interpolate(pair[0], pair[1], zone as f32 / zones as f32);
                                    let b = interpolate(
                                        pair[0],
                                        pair[1],
                                        (zone + 1) as f32 / zones as f32,
                                    );
                                    make_region(&[a, b], radius * scale, width, height)
                                })
                            })
                            .collect()
                    } else {
                        (0..light.zones)
                            .map(|zone| {
                                let start = total * zone as f32 / light.zones as f32;
                                let end = total * (zone + 1) as f32 / light.zones as f32;
                                let path = cut_path(&pixels, &lengths, start, end);
                                make_region(&path, radius * scale, width, height)
                            })
                            .collect()
                    };
                    if *reverse {
                        regions.reverse();
                    }
                    LightRegions::Ordered(regions)
                }
            };
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
                .map(|light| vec![[0.0; 3]; light.zones()])
                .collect();
        }
        static LINEAR: LazyLock<[f64; 256]> =
            LazyLock::new(|| std::array::from_fn(|value| f64::from(srgb_to_linear(value as u8))));
        let linear = &*LINEAR;
        // Build once per frame, shared by every region. f64 keeps subtraction
        // accurate even for a tiny dark region following bright pixels in a row.
        let stride = self.width + 1;
        let mut prefix = vec![[0.0; 3]; stride * self.height];
        for (pixels, sums) in frame
            .pixels
            .chunks_exact(self.width)
            .zip(prefix.chunks_exact_mut(stride))
        {
            let mut sum = [0.0; 3];
            for (pixel, entry) in pixels.iter().zip(&mut sums[1..]) {
                for channel in 0..3 {
                    sum[channel] += linear[pixel[channel] as usize];
                }
                *entry = sum;
            }
        }
        self.regions
            .iter()
            .map(|light| match light {
                LightRegions::Uniform { region, zones } => vec![average(region, &prefix); *zones],
                LightRegions::Ordered(regions) => regions
                    .iter()
                    .map(|region| average(region, &prefix))
                    .collect(),
            })
            .collect()
    }
    /// Bounded color-weighted moments, sharing row sums across all zones.
    /// Disabled/zero-strength/invalid controls use the unchanged accurate path.
    pub fn sample_with_ambience(
        &self,
        frame: &Frame,
        settings: SoftAmbience,
    ) -> Vec<Vec<[f32; 3]>> {
        self.enhance_accurate(frame, settings, self.sample(frame))
    }

    /// Reuse accurate samples for downstream darkness decisions.
    pub(crate) fn enhance_accurate(
        &self,
        frame: &Frame,
        settings: SoftAmbience,
        accurate: Vec<Vec<[f32; 3]>>,
    ) -> Vec<Vec<[f32; 3]>> {
        if !settings.enabled
            || settings.strength == 0.0
            || settings.validate().is_err()
            || frame.width != self.width
            || frame.height != self.height
            || frame.pixels.len() != self.width * self.height
        {
            return accurate;
        }
        static LINEAR: LazyLock<[f64; 256]> =
            LazyLock::new(|| std::array::from_fn(|v| f64::from(srgb_to_linear(v as u8))));
        let stride = self.width + 1;
        let mut prefix = vec![[0.0; 4]; stride * self.height];
        for (pixels, sums) in frame
            .pixels
            .chunks_exact(self.width)
            .zip(prefix.chunks_exact_mut(stride))
        {
            let mut sum = [0.0; 4];
            for (pixel, entry) in pixels.iter().zip(&mut sums[1..]) {
                let rgb = pixel.map(|v| LINEAR[v as usize]);
                let max = rgb.into_iter().fold(0.0f64, f64::max);
                let min = rgb.into_iter().fold(1.0f64, f64::min);
                // Absolute chroma times relative chroma suppresses dark noise.
                // Bounded influence preserves area: one outlier cannot win a broad zone.
                let chroma = if max > 0.0 {
                    (max - min).powi(2) / max
                } else {
                    0.0
                };
                let weight = 1.0 + 4.0 * f64::from(settings.color_emphasis) * chroma;
                for c in 0..3 {
                    sum[c] += rgb[c] * weight;
                }
                sum[3] += weight;
                *entry = sum;
            }
        }
        self.regions
            .iter()
            .zip(accurate)
            .map(|(light, colors)| {
                let enhance = |region: &Region, accurate: [f32; 3]| {
                    let sums = weighted_sum(region, &prefix);
                    let representative = std::array::from_fn(|c| (sums[c] / sums[3]) as f32);
                    let vibrant = vibrancy(representative, settings.vibrancy);
                    std::array::from_fn(|c| {
                        (accurate[c] + settings.strength * (vibrant[c] - accurate[c]))
                            .clamp(0.0, 1.0)
                    })
                };
                match light {
                    LightRegions::Uniform { region, zones } => {
                        vec![enhance(region, colors[0]); *zones]
                    }
                    LightRegions::Ordered(regions) => regions
                        .iter()
                        .zip(colors)
                        .map(|(r, c)| enhance(r, c))
                        .collect(),
                }
            })
            .collect()
    }
}

fn average(region: &Region, prefix: &[[f64; 3]]) -> [f32; 3] {
    let mut color = [0.0; 3];
    for run in &region.runs {
        let start = prefix[run.start as usize];
        let end = prefix[run.end as usize];
        for channel in 0..3 {
            color[channel] += end[channel] - start[channel];
        }
    }
    for pixel in &region.feather {
        let start = prefix[pixel.start as usize];
        let end = prefix[pixel.start as usize + 1];
        for channel in 0..3 {
            color[channel] += (end[channel] - start[channel]) * f64::from(pixel.weight);
        }
    }
    color.map(|value| (value * region.inverse_weight) as f32)
}

fn weighted_sum(region: &Region, prefix: &[[f64; 4]]) -> [f64; 4] {
    let mut sum = [0.0; 4];
    for run in &region.runs {
        for c in 0..4 {
            sum[c] += prefix[run.end as usize][c] - prefix[run.start as usize][c];
        }
    }
    for pixel in &region.feather {
        for c in 0..4 {
            sum[c] += (prefix[pixel.start as usize + 1][c] - prefix[pixel.start as usize][c])
                * f64::from(pixel.weight);
        }
    }
    sum
}

/// Linear-sRGB saturation about Rec.709 luminance. Reduce the requested chroma
/// scale to fit the RGB cube, preserving luminance and hue without channel clipping.
fn vibrancy(rgb: [f32; 3], amount: f32) -> [f32; 3] {
    if amount == 0.0 || (rgb[0] == rgb[1] && rgb[1] == rgb[2]) {
        return rgb;
    }
    let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
    let mut scale = 1.0 + 0.5 * amount;
    for value in rgb {
        let delta = value - luma;
        if delta > 0.0 {
            scale = scale.min((1.0 - luma) / delta);
        }
        if delta < 0.0 {
            scale = scale.min(-luma / delta);
        }
    }
    rgb.map(|v| (luma + (v - luma) * scale).clamp(0.0, 1.0))
}

fn distance(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn interpolate(a: Point, b: Point, t: f32) -> Point {
    Point {
        x: a.x + (b.x - a.x) * t,
        y: a.y + (b.y - a.y) * t,
    }
}

/// Allocate at least one zone per segment, then use highest averages to assign
/// the rest in proportion to physical LEDs. When zones equal LEDs this gives
/// exactly one zone per LED. Ties follow path order, independent of aspect ratio.
pub fn segment_zone_counts(counts: &[usize], zones: usize) -> Vec<usize> {
    if counts.is_empty() || zones < counts.len() {
        return Vec::new();
    }
    let mut allocation = vec![1; counts.len()];
    for _ in counts.len()..zones {
        let mut best = 0;
        for i in 1..counts.len() {
            if counts[i] * (allocation[best] + 1) > counts[best] * (allocation[i] + 1) {
                best = i;
            }
        }
        allocation[best] += 1;
    }
    allocation
}

/// Physical LED → sampled color index, including direction and segment bounds.
/// Callers validate the light and provide its mapped LED count.
pub fn led_zone_indices(light: &Light, count: usize) -> Vec<usize> {
    if light.zones > 1
        && let Shape::Strip {
            segment_leds,
            reverse,
            ..
        } = &light.shape
        && !segment_leds.is_empty()
    {
        let allocation = segment_zone_counts(segment_leds, light.zones);
        let mut indices = Vec::with_capacity(count);
        let mut offset = 0;
        for (&leds, &zones) in segment_leds.iter().zip(&allocation) {
            indices.extend((0..leds).map(|led| offset + led * zones / leds));
            offset += zones;
        }
        if *reverse {
            indices.reverse();
            for index in &mut indices {
                *index = light.zones - 1 - *index;
            }
        }
        indices
    } else {
        (0..count).map(|led| led * light.zones / count).collect()
    }
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
    // Distinct normalized points can round to the same pixel coordinate.
    // Preserve that position instead of sending an empty path to the fallback,
    // which would otherwise sample the source's top-left corner.
    if output.is_empty() {
        output.extend(points.first().copied());
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
    let mut runs = Vec::new();
    let mut feather = Vec::new();
    let mut total = 0.0;
    for y in y0..y1 {
        let row = y * (width + 1);
        let mut run_start = None;
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
            total += f64::from(weight);
            if weight == 1.0 {
                run_start.get_or_insert(x);
            } else {
                if let Some(start) = run_start.take() {
                    runs.push(PixelRun {
                        start: (row + start) as u16,
                        end: (row + x) as u16,
                    });
                }
                if weight > 0.0 {
                    feather.push(FeatherPixel {
                        start: (row + x) as u16,
                        weight,
                    });
                }
            }
        }
        if let Some(start) = run_start {
            runs.push(PixelRun {
                start: (row + start) as u16,
                end: (row + x1) as u16,
            });
        }
    }
    if total == 0.0 {
        // Tiny regions still sample their nearest on-screen pixel.
        let point = path.first().copied().unwrap_or(Point { x: 0.0, y: 0.0 });
        let x = (point.x.floor().max(0.0) as usize).min(width - 1);
        let y = (point.y.floor().max(0.0) as usize).min(height - 1);
        let start = (y * (width + 1) + x) as u16;
        runs.push(PixelRun {
            start,
            end: start + 1,
        });
        total = 1.0;
    }
    Region {
        runs: runs.into_boxed_slice(),
        feather: feather.into_boxed_slice(),
        inverse_weight: 1.0 / total,
    }
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

    fn ambience() -> SoftAmbience {
        SoftAmbience {
            enabled: true,
            strength: 1.0,
            color_emphasis: 1.0,
            vibrancy: 0.2,
        }
    }

    #[test]
    fn ambience_bypass_and_uniform_scenes() {
        let plan = SamplingPlan::compile(&[strip(1, false)], 100, 2).unwrap();
        for pixel in [[0; 3], [128; 3], [255; 3], [80, 150, 210], [255, 0, 0]] {
            let frame = Frame {
                width: 100,
                height: 2,
                pixels: vec![pixel; 200],
            };
            let accurate = plan.sample(&frame);
            for settings in [
                SoftAmbience::default(),
                SoftAmbience {
                    strength: 0.0,
                    ..ambience()
                },
            ] {
                assert_eq!(plan.sample_with_ambience(&frame, settings), accurate);
            }
            let enhanced = plan.sample_with_ambience(&frame, ambience())[0][0];
            assert!(
                enhanced
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            );
            if pixel[0] == pixel[1] && pixel[1] == pixel[2] {
                for c in 0..3 {
                    assert!((enhanced[c] - accurate[0][0][c]).abs() < 1e-6);
                }
            } else {
                let unboosted = plan.sample_with_ambience(
                    &frame,
                    SoftAmbience {
                        vibrancy: 0.0,
                        ..ambience()
                    },
                );
                for c in 0..3 {
                    assert!((unboosted[0][0][c] - accurate[0][0][c]).abs() < 1e-6);
                }
            }
        }
    }

    #[test]
    fn ambience_emphasizes_area_without_outlier_domination_or_hue_switches() {
        let plan = SamplingPlan::compile(&[strip(1, false)], 100, 2).unwrap();
        let mixed = |n| Frame {
            width: 100,
            height: 2,
            pixels: (0..200)
                .map(|i| if i % 100 < n { [240, 30, 20] } else { [150; 3] })
                .collect(),
        };
        let frame = mixed(30);
        let accurate = plan.sample(&frame)[0][0];
        let enhanced = plan.sample_with_ambience(&frame, ambience())[0][0];
        assert!(enhanced[0] - enhanced[1] > 1.5 * (accurate[0] - accurate[1]));
        let tiny = plan.sample_with_ambience(&mixed(1), ambience())[0][0];
        assert!((tiny[0] - tiny[1]).abs() < 0.08);
        let mut previous: Option<[f32; 3]> = None;
        for n in 40..=60 {
            let frame = Frame {
                width: 100,
                height: 2,
                pixels: (0..200)
                    .map(|i| {
                        if i % 100 < n {
                            [255, 0, 0]
                        } else {
                            [0, 255, 255]
                        }
                    })
                    .collect(),
            };
            let color = plan.sample_with_ambience(&frame, ambience())[0][0];
            assert!(
                color
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            );
            if let Some(before) = previous {
                for c in 0..3 {
                    assert!((before[c] - color[c]).abs() < 0.03);
                }
            }
            previous = Some(color);
        }
    }

    #[test]
    fn vibrancy_preserves_luminance_and_fits_gamut() {
        let luminance = |rgb: [f32; 3]| rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        for rgb in [
            [0.2, 0.4, 0.6],
            [0.0, 0.5, 1.0],
            [0.8, 0.7, 0.2],
            [0.0001, 0.0002, 0.0003],
        ] {
            let enhanced = vibrancy(rgb, 1.0);
            assert!((luminance(enhanced) - luminance(rgb)).abs() < 1e-6);
            assert!(
                enhanced
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            );
        }
        assert_eq!(vibrancy([0.2, 0.4, 0.6], 0.0), [0.2, 0.4, 0.6]);
        for value in [0.0, 0.001, 0.5, 1.0] {
            assert_eq!(vibrancy([value; 3], 1.0), [value; 3]);
        }
    }

    #[test]
    fn ambience_matches_spatial_weight_oracle_and_blends_in_linear_light() {
        let light = strip(3, false);
        let frame = Frame {
            width: 37,
            height: 19,
            pixels: (0..37 * 19)
                .map(|i| [(i * 71 % 256) as u8, (i * 13 % 256) as u8, (i % 7) as u8])
                .collect(),
        };
        let plan = SamplingPlan::compile(&[light], 37, 19).unwrap();
        let LightRegions::Ordered(regions) = &plan.regions[0] else {
            unreachable!()
        };
        let accurate = plan.sample(&frame);
        assert_eq!(
            plan.sample_with_ambience(&frame, SoftAmbience::default()),
            accurate
        );
        assert_eq!(
            plan.sample_with_ambience(
                &frame,
                SoftAmbience {
                    strength: 0.0,
                    ..ambience()
                }
            ),
            accurate
        );
        let settings = SoftAmbience {
            vibrancy: 0.0,
            ..ambience()
        };
        let enhanced = plan.sample_with_ambience(&frame, settings);
        let halfway = plan.sample_with_ambience(
            &frame,
            SoftAmbience {
                strength: 0.5,
                ..settings
            },
        );
        for (i, region) in regions.iter().enumerate() {
            let mut sum = [0.0f64; 4];
            let mut accumulate = |index: usize, spatial: f64| {
                let rgb = frame.pixels[index].map(|v| f64::from(srgb_to_linear(v)));
                let max = rgb.into_iter().fold(0.0f64, f64::max);
                let min = rgb.into_iter().fold(1.0f64, f64::min);
                let weight = spatial * (1.0 + 4.0 * (max - min).powi(2) / max.max(1e-20));
                for c in 0..3 {
                    sum[c] += rgb[c] * weight;
                }
                sum[3] += weight;
            };
            for run in &region.runs {
                for index in run.start as usize..run.end as usize {
                    accumulate(index / 38 * 37 + index % 38, 1.0);
                }
            }
            for pixel in &region.feather {
                let index = pixel.start as usize;
                accumulate(index / 38 * 37 + index % 38, f64::from(pixel.weight));
            }
            for c in 0..3 {
                assert!((enhanced[0][i][c] - (sum[c] / sum[3]) as f32).abs() < 1e-6);
                assert!(
                    (halfway[0][i][c] - (accurate[0][i][c] + enhanced[0][i][c]) * 0.5).abs() < 1e-6
                );
            }
        }
    }

    #[test]
    fn ambience_retains_zone_direction_and_bulb_broadcast() {
        let frame = Frame {
            width: 100,
            height: 2,
            pixels: (0..200)
                .map(|i| match i % 100 {
                    0..30 => [230, 20, 10],
                    50..80 => [10, 20, 230],
                    _ => [150; 3],
                })
                .collect(),
        };
        let forward = SamplingPlan::compile(&[strip(2, false)], 100, 2)
            .unwrap()
            .sample_with_ambience(&frame, ambience());
        let reverse = SamplingPlan::compile(&[strip(2, true)], 100, 2)
            .unwrap()
            .sample_with_ambience(&frame, ambience());
        assert!(forward[0][0][0] > forward[0][0][2]);
        assert!(forward[0][1][2] > forward[0][1][0]);
        assert_eq!(
            forward[0],
            reverse[0].iter().rev().copied().collect::<Vec<_>>()
        );
        let mut bulb = strip(4, false);
        bulb.shape = Shape::Bulb {
            center: Point { x: 0.1, y: 0.5 },
            radius: 0.1,
        };
        let colors = SamplingPlan::compile(&[bulb], 100, 2)
            .unwrap()
            .sample_with_ambience(&frame, ambience());
        assert_eq!(colors[0], vec![colors[0][0]; 4]);
    }

    #[test]
    fn explicit_segment_sampling_stays_within_each_segment_and_reverses() {
        let points = vec![
            Point { x: 0.1, y: 0.2 },
            Point { x: 0.8, y: 0.2 },
            Point { x: 0.8, y: 0.9 },
            Point { x: 0.4, y: 0.9 },
        ];
        let mut light = strip(10, false);
        light.shape = Shape::Strip {
            points: points.clone(),
            radius: 0.03,
            segment_leds: vec![2, 5, 3],
            reverse: false,
        };
        let frame = Frame {
            width: 40,
            height: 20,
            pixels: (0..800)
                .map(|i| [(i % 40 * 6) as u8, (i / 40 * 12) as u8, 80])
                .collect(),
        };
        let pixels: Vec<_> = points
            .iter()
            .map(|p| Point {
                x: p.x * 40.0,
                y: p.y * 20.0,
            })
            .collect();
        let mut expected = Vec::new();
        for (pair, leds) in pixels.windows(2).zip([2, 5, 3]) {
            for led in 0..leds {
                let path = [
                    interpolate(pair[0], pair[1], led as f32 / leds as f32),
                    interpolate(pair[0], pair[1], (led + 1) as f32 / leds as f32),
                ];
                expected.push(dense_region(&path, 0.6, &frame));
            }
        }
        for reverse in [false, true] {
            if let Shape::Strip {
                reverse: direction, ..
            } = &mut light.shape
            {
                *direction = reverse;
            }
            let actual = SamplingPlan::compile(&[light.clone()], 40, 20)
                .unwrap()
                .sample(&frame);
            let ordered: Vec<_> = if reverse {
                expected.iter().rev().copied().collect()
            } else {
                expected.clone()
            };
            for (actual, expected) in actual[0].iter().zip(ordered) {
                for channel in 0..3 {
                    assert!((actual[channel] - expected[channel]).abs() < 0.00001);
                }
            }
        }
        light.zones = 1;
        let actual = SamplingPlan::compile(&[light], 40, 20)
            .unwrap()
            .sample(&frame);
        let expected = dense_region(&pixels, 0.6, &frame);
        for (actual, expected) in actual[0][0].iter().zip(expected) {
            assert!((actual - expected).abs() < 0.00001);
        }
    }

    #[test]
    fn segment_allocation_and_physical_indices_respect_unequal_counts() {
        for a in 1..8 {
            for b in 1..8 {
                for c in 1..8 {
                    let counts = [a, b, c];
                    assert_eq!(segment_zone_counts(&counts, a + b + c), counts);
                    for zones in 3..=32 {
                        let allocation = segment_zone_counts(&counts, zones);
                        assert_eq!(allocation.iter().sum::<usize>(), zones);
                        assert!(allocation.iter().all(|n| *n > 0));
                    }
                }
            }
        }
        let mut light = strip(6, false);
        light.shape = Shape::Strip {
            points: vec![
                Point { x: 0.0, y: 0.0 },
                Point { x: 1.0, y: 0.0 },
                Point { x: 1.0, y: 1.0 },
                Point { x: 0.0, y: 1.0 },
            ],
            radius: 0.03,
            segment_leds: vec![2, 5, 3],
            reverse: false,
        };
        assert_eq!(segment_zone_counts(&[2, 5, 3], 6), [1, 3, 2]);
        assert_eq!(led_zone_indices(&light, 10), [0, 0, 1, 1, 2, 2, 3, 4, 4, 5]);
        if let Shape::Strip { reverse, .. } = &mut light.shape {
            *reverse = true;
        }
        assert_eq!(led_zone_indices(&light, 10), [0, 1, 1, 2, 3, 3, 4, 4, 5, 5]);
    }

    #[test]
    fn invalid_segment_counts_and_ranges_are_rejected() {
        let mut light = strip(2, false);
        light.shape = Shape::Strip {
            points: vec![
                Point { x: 0.0, y: 0.0 },
                Point { x: 1.0, y: 0.0 },
                Point { x: 1.0, y: 1.0 },
            ],
            radius: 0.03,
            segment_leds: vec![2, 3],
            reverse: false,
        };
        light.route = Route::Wled {
            host: "desk.local".into(),
            device_id: "abcdef".into(),
            start: 10,
            count: 5,
        };
        validate_lights(&[light.clone()]).unwrap();
        for counts in [
            vec![5],
            vec![2, 3, 1],
            vec![0, 5],
            vec![usize::MAX, 1],
            vec![4096, 1],
        ] {
            let mut invalid = light.clone();
            if let Shape::Strip { segment_leds, .. } = &mut invalid.shape {
                *segment_leds = counts;
            }
            assert!(validate_lights(&[invalid]).is_err());
        }
        if let Route::Wled { count, .. } = &mut light.route {
            *count = 6;
        }
        assert!(
            validate_lights(&[light.clone()])
                .unwrap_err()
                .to_string()
                .contains("sum")
        );
        light.route = Route::Mock;
        if let Shape::Strip { points, .. } = &mut light.shape {
            points[1] = points[0];
        }
        assert!(validate_lights(&[light]).is_err());
    }
    fn strip(zones: usize, reverse: bool) -> Light {
        Light {
            id: "strip".into(),
            name: "Strip".into(),
            shape: Shape::Strip {
                points: vec![Point { x: 0.0, y: 0.5 }, Point { x: 1.0, y: 0.5 }],
                radius: 0.2,
                segment_leds: vec![],
                reverse,
            },
            zones,
            route: Route::Mock,
        }
    }

    /// Deliberately dense oracle: evaluate every pixel, with no runs or prefix
    /// sums, using the same feathering and nearest-pixel fallback as the sampler.
    fn dense_region(path: &[Point], radius: f32, frame: &Frame) -> [f32; 3] {
        let mut color = [0.0f64; 3];
        let mut total = 0.0f64;
        for y in 0..frame.height {
            for x in 0..frame.width {
                let weight = f64::from(
                    (radius + 0.5
                        - path_distance(
                            Point {
                                x: x as f32 + 0.5,
                                y: y as f32 + 0.5,
                            },
                            path,
                        ))
                    .clamp(0.0, 1.0),
                );
                total += weight;
                for (channel, value) in color.iter_mut().enumerate() {
                    *value += f64::from(srgb_to_linear(frame.pixels[y * frame.width + x][channel]))
                        * weight;
                }
            }
        }
        if total == 0.0 {
            let point = path[0];
            let x = (point.x.floor().max(0.0) as usize).min(frame.width - 1);
            let y = (point.y.floor().max(0.0) as usize).min(frame.height - 1);
            frame.pixels[y * frame.width + x].map(srgb_to_linear)
        } else {
            color.map(|value| (value / total) as f32)
        }
    }

    fn dense_sample(light: &Light, frame: &Frame) -> Vec<[f32; 3]> {
        let to_pixel = |p: &Point| Point {
            x: p.x * frame.width as f32,
            y: p.y * frame.height as f32,
        };
        let scale = frame.width.min(frame.height) as f32;
        match &light.shape {
            Shape::Bulb { center, radius } => {
                vec![dense_region(&[to_pixel(center)], radius * scale, frame); light.zones]
            }
            Shape::Strip {
                points,
                radius,
                reverse,
                ..
            } => {
                let points: Vec<_> = points.iter().map(to_pixel).collect();
                let lengths: Vec<_> = points
                    .windows(2)
                    .map(|pair| distance(pair[0], pair[1]))
                    .collect();
                let total: f32 = lengths.iter().sum();
                let mut colors: Vec<_> = (0..light.zones)
                    .map(|zone| {
                        let path = cut_path(
                            &points,
                            &lengths,
                            total * zone as f32 / light.zones as f32,
                            total * (zone + 1) as f32 / light.zones as f32,
                        );
                        dense_region(&path, radius * scale, frame)
                    })
                    .collect();
                if *reverse {
                    colors.reverse();
                }
                colors
            }
        }
    }

    #[test]
    fn compact_regions_match_dense_sampling_across_geometry_and_frame_sizes() {
        let paths = [
            vec![(0.0, 0.5), (1.0, 0.5)],
            // A corner with a repeated point exercises zero-length segments.
            vec![(0.0, 0.0), (0.0, 0.0), (0.0, 1.0), (1.0, 1.0)],
            // A crossing can produce multiple separated runs in one row.
            vec![(0.0, 0.0), (1.0, 1.0), (0.0, 1.0), (1.0, 0.0)],
            vec![(0.001, 0.999), (0.321, 0.782), (0.973, 0.003)],
        ];
        for (width, height) in [(160, 90), (37, 19), (9, 29), (160, 1), (1, 90), (1, 1)] {
            let frame = Frame {
                width,
                height,
                pixels: (0..width * height)
                    .map(|i| [(i * 71 % 256) as u8, (i * 13 % 256) as u8, (i % 7) as u8])
                    .collect(),
            };
            for radius in [0.00001, 0.035, 0.3, 1.0] {
                let mut lights: Vec<_> = [(0.0, 0.0), (1.0, 1.0), (0.413, 0.891)]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (x, y))| Light {
                        id: format!("bulb-{i}"),
                        name: format!("Bulb {i}"),
                        shape: Shape::Bulb {
                            center: Point { x, y },
                            radius,
                        },
                        zones: 7,
                        route: Route::Mock,
                    })
                    .collect();
                for (i, path) in paths.iter().enumerate() {
                    for zones in [1, 9] {
                        for reverse in [false, true] {
                            lights.push(Light {
                                id: format!("strip-{i}-{zones}-{reverse}"),
                                name: "Strip".into(),
                                shape: Shape::Strip {
                                    points: path.iter().map(|&(x, y)| Point { x, y }).collect(),
                                    radius,
                                    segment_leds: vec![],
                                    reverse,
                                },
                                zones,
                                route: Route::Mock,
                            });
                        }
                    }
                }
                let actual = SamplingPlan::compile(&lights, width, height)
                    .unwrap()
                    .sample(&frame);
                for (light, colors) in lights.iter().zip(actual) {
                    let expected = dense_sample(light, &frame);
                    assert_eq!(colors.len(), expected.len());
                    for (zone, (actual, expected)) in colors.iter().zip(expected).enumerate() {
                        for channel in 0..3 {
                            assert!(
                                (actual[channel] - expected[channel]).abs() <= 0.000001,
                                "{} zone {zone}, {width}x{height}, radius {radius}: {actual:?} != {expected:?}",
                                light.id
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn prefix_subtraction_preserves_a_dark_pixel_after_a_bright_row() {
        let light = Light {
            id: "dark-pixel".into(),
            name: "Dark pixel".into(),
            shape: Shape::Bulb {
                center: Point { x: 1.0, y: 0.5 },
                radius: 0.00001,
            },
            zones: 1,
            route: Route::Mock,
        };
        let mut frame = Frame {
            width: 160,
            height: 1,
            pixels: vec![[255; 3]; 160],
        };
        frame.pixels[159] = [1, 2, 3];
        assert_eq!(
            SamplingPlan::compile(&[light], 160, 1)
                .unwrap()
                .sample(&frame),
            vec![vec![[1, 2, 3].map(srgb_to_linear)]]
        );
    }

    #[test]
    fn strip_collapsing_during_pixel_scaling_samples_its_own_position() {
        for (x, y) in [(0.9f32, 0.9), (f32::from_bits(0x3ee66666), 0.5)] {
            let next_x = f32::from_bits(x.to_bits() + 1);
            assert_ne!(x, next_x);
            assert_eq!(x * 160.0, next_x * 160.0);
            let mut frame = Frame {
                width: 160,
                height: 90,
                pixels: vec![[0, 255, 0]; 160 * 90],
            };
            frame.pixels[0] = [255, 0, 0];
            for zones in [1, 8] {
                for reverse in [false, true] {
                    let light = Light {
                        id: "collapsed-strip".into(),
                        name: "Tiny strip".into(),
                        zones,
                        route: Route::Mock,
                        shape: Shape::Strip {
                            points: vec![Point { x, y }, Point { x: next_x, y }],
                            radius: 0.00001,
                            segment_leds: vec![],
                            reverse,
                        },
                    };
                    validate_lights(std::slice::from_ref(&light)).unwrap();
                    let actual = SamplingPlan::compile(std::slice::from_ref(&light), 160, 90)
                        .unwrap()
                        .sample(&frame);
                    // This independent expectation checks declared placement,
                    // rather than reusing the path geometry or storage helpers.
                    assert_eq!(actual, vec![vec![[0.0, 1.0, 0.0]; zones]]);
                    assert_eq!(dense_sample(&light, &frame), actual[0]);
                }
            }
        }
    }

    #[test]
    fn maximum_broad_strips_store_runs_instead_of_millions_of_pixel_weights() {
        let lights: Vec<_> = (0..8)
            .map(|i| Light {
                id: format!("strip-{i}"),
                name: format!("Strip {i}"),
                shape: Shape::Strip {
                    points: vec![Point { x: 0.1, y: 0.5 }, Point { x: 0.9, y: 0.5 }],
                    radius: 1.0,
                    segment_leds: vec![],
                    reverse: i % 2 == 0,
                },
                zones: 256,
                route: Route::Mock,
            })
            .collect();
        let plan = SamplingPlan::compile(&lights, SAMPLE_WIDTH, SAMPLE_HEIGHT).unwrap();
        let mut stored_bytes = 0;
        let mut region_count = 0;
        for light in &plan.regions {
            let LightRegions::Ordered(regions) = light else {
                panic!("addressable strips must retain their regions");
            };
            for region in regions {
                region_count += 1;
                stored_bytes += std::mem::size_of::<Region>()
                    + std::mem::size_of_val(region.runs.as_ref())
                    + std::mem::size_of_val(region.feather.as_ref());
            }
        }
        assert_eq!(region_count, 2048);
        assert!(stored_bytes < 4 * 1024 * 1024, "{stored_bytes} bytes");
        let frame = Frame {
            width: SAMPLE_WIDTH,
            height: SAMPLE_HEIGHT,
            pixels: vec![[12, 127, 240]; SAMPLE_WIDTH * SAMPLE_HEIGHT],
        };
        let expected = [12, 127, 240].map(srgb_to_linear);
        let output = plan.sample(&frame);
        assert_eq!(output.len(), 8);
        for zones in output {
            assert_eq!(zones.len(), 256);
            assert!(zones.iter().all(|color| *color == expected));
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
    fn bulb_average_is_broadcast_without_changing_zone_or_light_order() {
        let bulb = |id: &str, x: f32, zones: usize| Light {
            id: id.into(),
            name: id.into(),
            route: Route::Mock,
            zones,
            shape: Shape::Bulb {
                center: Point { x, y: 0.5 },
                radius: 0.02,
            },
        };
        let frame = Frame {
            width: 10,
            height: 2,
            pixels: (0..20)
                .map(|i| {
                    if i % 10 < 5 {
                        [32, 100, 200]
                    } else {
                        [220, 40, 10]
                    }
                })
                .collect(),
        };
        let lights = [bulb("left", 0.1, 3), strip(2, false), bulb("right", 0.9, 5)];
        let plan = SamplingPlan::compile(&lights, 10, 2).unwrap();
        let colors = plan.sample(&frame);
        assert_eq!(colors.iter().map(Vec::len).collect::<Vec<_>>(), [3, 2, 5]);
        assert_eq!(colors[0], vec![[32, 100, 200].map(srgb_to_linear); 3]);
        assert_eq!(colors[2], vec![[220, 40, 10].map(srgb_to_linear); 5]);
        assert!(colors[1][0][2] > colors[1][1][2]);
        let invalid = Frame {
            width: 10,
            height: 2,
            pixels: vec![],
        };
        assert_eq!(
            plan.sample(&invalid),
            vec![vec![[0.0; 3]; 3], vec![[0.0; 3]; 2], vec![[0.0; 3]; 5]]
        );
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
