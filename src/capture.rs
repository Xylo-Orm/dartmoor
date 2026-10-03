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
    /// False means unchanged content cannot be distinguished from stream loss.
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
    fn confirms_idle(&self) -> bool {
        true
    }
}
struct Desktop {
    capturer: scap::capturer::Capturer,
    stopped: bool,
}
impl CaptureSource for Desktop {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>> {
        match self.capturer.get_next_frame_timeout(timeout)? {
            Some(v) => Ok(Some(normalize(v)?)),
            None => Ok(None),
        }
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
        ..Default::default()
    })?;
    capturer.start_capture();
    Ok(Box::new(Desktop {
        capturer,
        stopped: false,
    }))
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
    if w <= 0 || h <= 0 {
        bail!("Invalid capture dimensions");
    }
    let (w, h) = (w as usize, h as usize);
    if w > 32768 || h > 32768 || bytes.len() != w * h * channels {
        bail!("Capture buffer size does not match dimensions");
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
                    let base = (py * w + px) * channels;
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
