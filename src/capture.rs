//! Application-owned boundary. All methods run on the capture/processing worker.
use crate::{config::CaptureSelection, core::Frame};
use anyhow::{Result, bail};
use std::time::{Duration, Instant};

pub const WIDTH: usize = 160;
pub const HEIGHT: usize = 90;

#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    pub name: String,
}

pub fn sources() -> Vec<Source> {
    #[cfg(target_os = "linux")]
    {
        vec![Source {
            id: "portal".into(),
            name: "Desktop / window (choose in portal)".into(),
        }]
    }
    #[cfg(not(target_os = "linux"))]
    {
        scap::get_all_targets()
            .into_iter()
            .map(|t| match t {
                scap::Target::Display(d) => Source {
                    id: format!("display:{}", d.id),
                    name: d.title,
                },
                scap::Target::Window(w) => Source {
                    id: format!("window:{}", w.id),
                    name: w.title,
                },
            })
            .collect()
    }
}

pub trait CaptureSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>>;
    fn stop(&mut self);
    /// True only when the most recent `next_frame` call returned None after an
    /// explicit backend notification that the previous image is unchanged.
    /// Timeouts and errors must clear this signal; silence is not liveness.
    fn confirms_idle(&self) -> bool {
        false
    }
}

pub struct Synthetic {
    started: Instant,
    next: Instant,
    interval: Duration,
}
impl Synthetic {
    pub fn new(fps: u32) -> Self {
        Self {
            started: Instant::now(),
            next: Instant::now(),
            interval: Duration::from_secs_f64(1.0 / fps.max(1) as f64),
        }
    }
}
pub fn synthetic_frame(t: f32) -> Frame {
    Frame {
        width: WIDTH,
        height: HEIGHT,
        pixels: (0..WIDTH * HEIGHT)
            .map(|i| {
                let x = (i % WIDTH) as f32 / (WIDTH - 1) as f32;
                let y = (i / WIDTH) as f32 / (HEIGHT - 1) as f32;
                [
                    (x * 255.0) as u8,
                    (y * 255.0) as u8,
                    ((t.sin() * 0.5 + 0.5) * 255.0) as u8,
                ]
            })
            .collect(),

        taps: None,
    }
}
impl CaptureSource for Synthetic {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>> {
        let wait = self.next.saturating_duration_since(Instant::now());
        if wait > timeout {
            std::thread::sleep(timeout);
            return Ok(None);
        }
        std::thread::sleep(wait);
        self.next = Instant::now() + self.interval;
        Ok(Some(synthetic_frame(self.started.elapsed().as_secs_f32())))
    }
    fn stop(&mut self) {}
}
struct Desktop {
    capturer: scap::capturer::Capturer,
    stopped: bool,
    idle: bool,
}
impl CaptureSource for Desktop {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>> {
        self.idle = false;
        match self.capturer.get_next_frame_timeout(timeout)? {
            Some(v) => {
                let kind = if cfg!(target_os = "linux") {
                    NormalizationKind::TapGrid
                } else {
                    NormalizationKind::Native
                };
                let frame = normalize_capture(v, kind)?;
                self.idle = frame.is_none();
                Ok(frame)
            }
            None => Ok(None),
        }
    }
    fn confirms_idle(&self) -> bool {
        self.idle
    }
    fn stop(&mut self) {
        if !self.stopped {
            self.stopped = true;
            self.capturer.stop_capture();
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn open(selection: &CaptureSelection, fps: u32) -> Result<Box<dyn CaptureSource>> {
    if matches!(selection, CaptureSelection::Synthetic) {
        return Ok(Box::new(Synthetic::new(fps)));
    }
    if !scap::is_supported() {
        bail!("Screen capture is unsupported on this system");
    }
    if !scap::has_permission() && !scap::request_permission() {
        bail!(
            "Screen recording permission denied. Allow recording in system settings, then Start again."
        );
    }
    let CaptureSelection::Desktop { id } = selection else {
        unreachable!()
    };
    #[cfg(target_os = "linux")]
    let target = {
        let _ = id;
        None
    };
    #[cfg(not(target_os = "linux"))]
    let target = if let Some(id) = id {
        Some(
            scap::get_all_targets()
                .into_iter()
                .find(|t| match t {
                    scap::Target::Display(d) => format!("display:{}", d.id) == *id,
                    scap::Target::Window(w) => format!("window:{}", w.id) == *id,
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("Saved capture source is unavailable. Select a source again.")
                })?,
        )
    } else {
        None
    };
    let mut capturer = scap::capturer::Capturer::build(scap::capturer::Options {
        fps,
        target,
        show_cursor: false,
        show_highlight: false,
        output_type: scap::frame::FrameType::BGRAFrame,
        output_resolution: scap::capturer::Resolution::_480p,
        #[cfg(target_os = "linux")]
        mapped_frame_processor: Some(reduce_mapped),
        ..Default::default()
    })?;
    capturer.start_capture();
    Ok(Box::new(Desktop {
        capturer,
        stopped: false,
        idle: false,
    }))
}

/// scap's macOS BGRA path carries ScreenCaptureKit Idle notifications as an
/// empty, zero-sized frame. They contain no image to normalize, and must not
/// turn an ordinary unchanged desktop into a malformed-buffer error. Do not
/// treat other malformed frames as idle. Desktop exposes this explicit signal
/// through `confirms_idle`, separately from a retrieval timeout.
#[derive(Clone, Copy)]
enum NormalizationKind {
    Native,
    TapGrid,
}
fn normalize_capture(frame: scap::frame::Frame, kind: NormalizationKind) -> Result<Option<Frame>> {
    if matches!(&frame, scap::frame::Frame::BGRA(frame)
        if frame.width == 0 && frame.height == 0 && frame.data.is_empty())
    {
        return Ok(None);
    }
    if matches!(kind, NormalizationKind::TapGrid) {
        if let scap::frame::Frame::RGB(rgb) = frame {
            return tap_grid_frame(&rgb.data, rgb.width, rgb.height).map(Some);
        }
        bail!("Mapped capture returned unexpected pixel format");
    }
    normalize(frame).map(Some)
}

/// SDR/sRGB assumption: scap 0.0.8 exposes no color-space metadata. Reject YUV.
fn normalize(v: scap::frame::Frame) -> Result<Frame> {
    use scap::frame::Frame::*;
    match v {
        BGRA(f) => reduce(&f.data, f.width, f.height, 4, [2, 1, 0]),
        BGRx(f) => reduce(&f.data, f.width, f.height, 4, [2, 1, 0]),
        XBGR(f) => reduce(&f.data, f.width, f.height, 4, [3, 2, 1]),
        RGBx(f) => reduce(&f.data, f.width, f.height, 4, [0, 1, 2]),
        RGB(f) => reduce(&f.data, f.width, f.height, 3, [0, 1, 2]),
        BGR0(f) => reduce(&f.data, f.width, f.height, 3, [2, 1, 0]),
        YUVFrame(_) => bail!("Capture returned YUV instead of SDR RGB; choose another source."),
    }
}
fn reduce(bytes: &[u8], w: i32, h: i32, channels: usize, order: [usize; 3]) -> Result<Frame> {
    if w <= 0 || h <= 0 || bytes.len() != w as usize * h as usize * channels {
        bail!("Capture buffer size does not match dimensions");
    }
    reduce_rows(bytes, w, h, channels, order, w as usize * channels)
}

#[cfg(target_os = "linux")]
fn reduce_mapped(
    bytes: &[u8],
    w: i32,
    h: i32,
    channels: usize,
    order: [usize; 3],
    stride: usize,
) -> Option<scap::frame::Frame> {
    let (ow, oh, taps) = extract_taps(bytes, w, h, channels, order, stride).ok()?;
    let mut data = vec![0u8; ow * oh * 16 * 3];
    for y in 0..oh {
        for x in 0..ow {
            for sy in 0..4 {
                for sx in 0..4 {
                    let src = taps[y * ow + x][sy * 4 + sx];
                    let dst = ((y * 4 + sy) * (ow * 4) + x * 4 + sx) * 3;
                    data[dst..dst + 3].copy_from_slice(&src);
                }
            }
        }
    }
    Some(scap::frame::Frame::RGB(scap::frame::RGBFrame {
        display_time: 0,
        width: (ow * 4) as i32,
        height: (oh * 4) as i32,
        data,
    }))
}

fn tap_grid_frame(bytes: &[u8], w: i32, h: i32) -> Result<Frame> {
    if w <= 0
        || h <= 0
        || w % 4 != 0
        || h % 4 != 0
        || w as usize > WIDTH * 4
        || h as usize > HEIGHT * 4
        || bytes.len() != w as usize * h as usize * 3
    {
        bail!("Invalid mapped tap grid");
    }
    let (ow, oh) = (w as usize / 4, h as usize / 4);
    let taps = (0..oh)
        .flat_map(|y| (0..ow).map(move |x| (x, y)))
        .map(|(x, y)| {
            std::array::from_fn(|i| {
                let base = ((y * 4 + i / 4) * (ow * 4) + x * 4 + i % 4) * 3;
                bytes[base..base + 3].try_into().unwrap()
            })
        })
        .collect();
    Ok(frame_from_taps(ow, oh, taps))
}

fn frame_from_taps(width: usize, height: usize, taps: Vec<[[u8; 3]; 16]>) -> Frame {
    let linear = linear_table();
    let pixels = taps
        .iter()
        .map(|group| {
            let mut sum = [0.0; 3];
            for tap in group {
                for c in 0..3 {
                    sum[c] += linear[tap[c] as usize];
                }
            }
            crate::core::encode(sum.map(|v| v / 16.0), 1.0)
        })
        .collect();
    Frame {
        width,
        height,
        pixels,
        taps: Some(taps),
    }
}
fn linear_table() -> &'static [f32; 256] {
    static LINEAR: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LINEAR.get_or_init(|| std::array::from_fn(|i| crate::core::srgb_to_linear(i as u8)))
}
fn reduce_rows(
    bytes: &[u8],
    w: i32,
    h: i32,
    channels: usize,
    order: [usize; 3],
    stride: usize,
) -> Result<Frame> {
    let (ow, oh, taps) = extract_taps(bytes, w, h, channels, order, stride)?;
    Ok(frame_from_taps(ow, oh, taps))
}
type TapGroups = Vec<[[u8; 3]; 16]>;
fn extract_taps(
    bytes: &[u8],
    w: i32,
    h: i32,
    channels: usize,
    order: [usize; 3],
    stride: usize,
) -> Result<(usize, usize, TapGroups)> {
    if w <= 0 || h <= 0 {
        bail!("Invalid capture dimensions");
    }
    let (w, h) = (w as usize, h as usize);
    let span = (h - 1)
        .checked_mul(stride)
        .and_then(|v| v.checked_add(w.checked_mul(channels)?));
    if w > 32768
        || h > 32768
        || channels == 0
        || order.iter().any(|c| *c >= channels)
        || stride < w.saturating_mul(channels)
        || span.is_none_or(|span| span > bytes.len())
    {
        bail!("Capture buffer size does not match dimensions");
    }
    let scale = (WIDTH as f64 / w as f64)
        .min(HEIGHT as f64 / h as f64)
        .min(1.0);
    let (ow, oh) = (
        (w as f64 * scale).round().max(1.0) as usize,
        (h as f64 * scale).round().max(1.0) as usize,
    );
    let mut taps = Vec::with_capacity(ow * oh);
    for y in 0..oh {
        for x in 0..ow {
            taps.push(std::array::from_fn(|i| {
                let (sx, sy) = (i % 4, i / 4);
                let px = if w <= WIDTH && h <= HEIGHT {
                    x
                } else {
                    ((x * 4 + sx) * w / (ow * 4)).min(w - 1)
                };
                let py = if w <= WIDTH && h <= HEIGHT {
                    y
                } else {
                    ((y * 4 + sy) * h / (oh * 4)).min(h - 1)
                };
                let base = py * stride + px * channels;
                [
                    bytes[base + order[0]],
                    bytes[base + order[1]],
                    bytes[base + order[2]],
                ]
            }));
        }
    }
    Ok((ow, oh, taps))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn competing_capture_taps_survive_gray_preview() {
        let mut packed = Vec::new();
        for _y in 0..4 {
            for x in 0..4 {
                packed.extend(if x < 2 { [255, 0, 0] } else { [0, 255, 255] });
            }
        }
        let frame = reduce(&packed, 4, 4, 3, [0, 1, 2]).unwrap();
        assert_eq!((frame.width, frame.height), (4, 4));
        assert_eq!(frame.taps.as_ref().unwrap()[0], [[255, 0, 0]; 16]);
        let mut large = Vec::new();
        for _y in 0..90 {
            for x in 0..640 {
                large.extend(if x % 4 < 2 {
                    [255, 0, 0]
                } else {
                    [0, 255, 255]
                });
            }
        }
        let frame = reduce(&large, 640, 90, 3, [0, 1, 2]).unwrap();
        let taps = &frame.taps.as_ref().unwrap()[0];
        assert!(taps.contains(&[255, 0, 0]));
        assert!(taps.contains(&[0, 255, 255]));
        assert!((frame.pixels[0][0] as i16 - frame.pixels[0][1] as i16).abs() <= 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mapped_reduction_matches_packed_pixels_for_padding_and_channel_orders() {
        for (channels, order) in [
            (3, [0, 1, 2]),
            (4, [2, 1, 0]),
            (4, [3, 2, 1]),
            (4, [0, 1, 2]),
        ] {
            let (w, h) = (321, 183);
            let stride = w * channels + 17;
            let packed: Vec<u8> = (0..w * h * channels).map(|i| (i % 251) as u8).collect();
            let mut padded = vec![255; stride * h];
            for y in 0..h {
                padded[y * stride..y * stride + w * channels]
                    .copy_from_slice(&packed[y * w * channels..(y + 1) * w * channels]);
            }
            let expected = reduce(&packed, w as i32, h as i32, channels, order).unwrap();
            let native =
                reduce_mapped(&padded, w as i32, h as i32, channels, order, stride).unwrap();
            let scap::frame::Frame::RGB(ref output) = native else {
                panic!("expected RGB")
            };
            assert!(output.data.len() <= WIDTH * HEIGHT * 16 * 3);
            let scap::frame::Frame::RGB(rgb) = native else {
                unreachable!()
            };
            let actual = tap_grid_frame(&rgb.data, rgb.width, rgb.height).unwrap();
            assert_eq!(
                (actual.width, actual.height),
                (expected.width, expected.height)
            );
            assert_eq!(actual.pixels, expected.pixels);
        }
        assert!(reduce_mapped(&[0; 15], 2, 2, 4, [2, 1, 0], 8).is_none());
        assert!(reduce_mapped(&[0; 16], 2, 2, 4, [2, 1, 0], 7).is_none());
    }

    #[cfg(target_os = "linux")]
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "release-mode capture CPU benchmark"]
    fn benchmark_mapped_capture_reduction() {
        let (w, h) = (3840, 2160);
        let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let run = || {
            let grid = reduce_mapped(&pixels, w as i32, h as i32, 4, [2, 1, 0], w * 4).unwrap();
            let scap::frame::Frame::RGB(rgb) = grid else {
                unreachable!()
            };
            std::hint::black_box(tap_grid_frame(&rgb.data, rgb.width, rgb.height).unwrap());
        };
        for _ in 0..30 {
            run();
        }
        let mut durations = Vec::with_capacity(300);
        for _ in 0..300 {
            let start = Instant::now();
            run();
            durations.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        durations.sort_by(f64::total_cmp);
        println!(
            "4K source to mapped tap grid to preview: mean {:.3}ms p50 {:.3}ms p95 {:.3}ms p99 {:.3}ms max {:.3}ms; payload {} -> {} bytes; no compositor/readback",
            durations.iter().sum::<f64>() / durations.len() as f64,
            durations[150],
            durations[285],
            durations[297],
            durations[299],
            pixels.len(),
            WIDTH * HEIGHT * 16 * 3
        );
    }
    #[test]
    fn synthetic_pacing_timeout_does_not_claim_backend_idle() {
        let mut source = Synthetic::new(1);
        assert!(source.next_frame(Duration::ZERO).unwrap().is_some());
        assert!(source.next_frame(Duration::ZERO).unwrap().is_none());
        assert!(!source.confirms_idle());
    }

    #[test]
    fn macos_idle_marker_is_not_an_image_or_malformed_capture() {
        let bgra = |width, height, data| {
            scap::frame::Frame::BGRA(scap::frame::BGRAFrame {
                display_time: 123,
                width,
                height,
                data,
            })
        };
        assert!(
            normalize_capture(bgra(0, 0, vec![]), NormalizationKind::Native)
                .unwrap()
                .is_none()
        );
        assert!(normalize_capture(bgra(0, 1, vec![]), NormalizationKind::Native).is_err());
        assert!(normalize_capture(bgra(0, 0, vec![0]), NormalizationKind::Native).is_err());
        let image = normalize_capture(
            bgra(1, 1, vec![10, 20, 200, 255]),
            NormalizationKind::Native,
        )
        .unwrap()
        .unwrap();
        assert_eq!(image.pixels, vec![[200, 20, 10]]);
    }

    #[test]
    fn padded_or_truncated_rejected() {
        assert!(reduce(&[0; 7], 2, 1, 4, [2, 1, 0]).is_err());
    }
    #[test]
    fn bgra_and_aspect() {
        let f = reduce(&[10, 20, 200, 255], 1, 1, 4, [2, 1, 0]).unwrap();
        assert_eq!(f.pixels[0], [200, 20, 10]);
        let f = reduce(&vec![255; 400 * 100 * 4], 400, 100, 4, [2, 1, 0]).unwrap();
        assert_eq!((f.width, f.height), (160, 40));
    }
    #[test]
    fn deterministic_synthetic() {
        assert_eq!(synthetic_frame(0.0).pixels, synthetic_frame(0.0).pixels);
    }
}
