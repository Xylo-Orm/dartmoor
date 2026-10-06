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
                let frame = normalize_capture(v)?;
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
fn normalize_capture(frame: scap::frame::Frame) -> Result<Option<Frame>> {
    if matches!(&frame, scap::frame::Frame::BGRA(frame)
        if frame.width == 0 && frame.height == 0 && frame.data.is_empty())
    {
        return Ok(None);
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
    let frame = reduce_rows(bytes, w, h, channels, order, stride).ok()?;
    Some(scap::frame::Frame::RGB(scap::frame::RGBFrame {
        display_time: 0,
        width: frame.width as i32,
        height: frame.height as i32,
        data: frame.pixels.into_iter().flatten().collect(),
    }))
}

fn reduce_rows(
    bytes: &[u8],
    w: i32,
    h: i32,
    channels: usize,
    order: [usize; 3],
    stride: usize,
) -> Result<Frame> {
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
        || stride < w * channels
        || span.is_none_or(|span| span > bytes.len())
    {
        bail!("Capture buffer size does not match dimensions");
    }
    // Linux has already applied this exact kernel while the PipeWire buffer
    // was borrowed. Preserve its small RGB output without filtering twice.
    if w <= WIDTH && h <= HEIGHT && channels == 3 && order == [0, 1, 2] {
        let pixels = (0..h)
            .flat_map(|y| {
                bytes[y * stride..y * stride + w * 3]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .copied()
            })
            .collect();
        return Ok(Frame {
            width: w,
            height: h,
            pixels,
        });
    }
    let scale = (WIDTH as f64 / w as f64)
        .min(HEIGHT as f64 / h as f64)
        .min(1.0);
    let (ow, oh) = (
        (w as f64 * scale).round().max(1.0) as usize,
        (h as f64 * scale).round().max(1.0) as usize,
    );
    // A fixed 4x4 stratified kernel bounds CPU work regardless of source size.
    // Average in linear light; the sampling core decodes this sRGB small image.
    static LINEAR: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    let linear =
        LINEAR.get_or_init(|| std::array::from_fn(|i| crate::core::srgb_to_linear(i as u8)));
    let mut pixels = Vec::with_capacity(ow * oh);
    for y in 0..oh {
        for x in 0..ow {
            let mut sum = [0.0; 3];
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = ((x * 4 + sx) * w / (ow * 4)).min(w - 1);
                    let py = ((y * 4 + sy) * h / (oh * 4)).min(h - 1);
                    let base = py * stride + px * channels;
                    for c in 0..3 {
                        sum[c] += linear[bytes[base + order[c]] as usize];
                    }
                }
            }
            pixels.push(crate::core::encode(sum.map(|v| v / 16.0), 1.0));
        }
    }
    Ok(Frame {
        width: ow,
        height: oh,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
            assert!(output.data.len() <= WIDTH * HEIGHT * 3);
            let actual = normalize(native).unwrap();
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
    #[test]
    #[ignore = "release-mode capture CPU benchmark"]
    fn benchmark_mapped_capture_reduction() {
        let (w, h) = (3840, 2160);
        let pixels: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let iterations = 100;
        let start = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(
                reduce(&pixels.clone(), w as i32, h as i32, 4, [2, 1, 0]).unwrap(),
            );
        }
        let old = start.elapsed();
        let start = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(
                normalize(reduce_mapped(&pixels, w as i32, h as i32, 4, [2, 1, 0], w * 4).unwrap())
                    .unwrap(),
            );
        }
        println!(
            "4K buffer + reduction: old {:.3} ms/frame; mapped {:.3} ms/frame; transferred {} -> {} bytes (excludes compositor readback)",
            old.as_secs_f64() * 1000.0 / iterations as f64,
            start.elapsed().as_secs_f64() * 1000.0 / iterations as f64,
            pixels.len(),
            WIDTH * HEIGHT * 3
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
        assert!(normalize_capture(bgra(0, 0, vec![])).unwrap().is_none());
        assert!(normalize_capture(bgra(0, 1, vec![])).is_err());
        assert!(normalize_capture(bgra(0, 0, vec![0])).is_err());
        let image = normalize_capture(bgra(1, 1, vec![10, 20, 200, 255]))
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
