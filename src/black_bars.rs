//! Conservative, temporally stable letterbox/pillarbox detection on small SDR frames.
use crate::core::Frame;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SETTLE: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Bounds {
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
}

#[derive(Default)]
pub(crate) struct Detector {
    size: (usize, usize),
    active: Bounds,
    pending: Option<(Bounds, Instant, u32)>,
}

impl Detector {
    pub(crate) fn crop_bounds(&self) -> (usize, usize, usize, usize) {
        (
            self.active.left,
            self.active.top,
            self.active.right,
            self.active.bottom,
        )
    }

    /// Crop both preview and sampling input to the same active image. Turning
    /// detection off restores the complete source immediately.
    pub fn apply(&mut self, frame: Arc<Frame>, enabled: bool, now: Instant) -> Arc<Frame> {
        let full = Bounds {
            left: 0,
            top: 0,
            right: frame.width,
            bottom: frame.height,
        };
        if self.size != (frame.width, frame.height) || !enabled {
            self.size = (frame.width, frame.height);
            self.active = full;
            self.pending = None;
        }
        if !enabled
            || frame.width == 0
            || frame.height == 0
            || frame.width.checked_mul(frame.height) != Some(frame.pixels.len())
        {
            return frame;
        }
        if let Some(candidate) = detect(&frame) {
            if candidate == self.active {
                self.pending = None;
            } else {
                match &mut self.pending {
                    Some((bounds, since, observations)) if *bounds == candidate => {
                        *observations += 1;
                        if *observations >= 3 && now.duration_since(*since) >= SETTLE {
                            self.active = candidate;
                            self.pending = None;
                        }
                    }
                    _ => self.pending = Some((candidate, now, 1)),
                }
            }
        } else {
            // A fade or entirely dark scene is not evidence of a new aspect
            // ratio. Keep the established crop, but restart confirmation.
            self.pending = None;
        }
        if self.active == full {
            return frame;
        }
        let bounds = self.active;
        let pixels = (bounds.top..bounds.bottom)
            .flat_map(|y| {
                frame.pixels[y * frame.width + bounds.left..y * frame.width + bounds.right]
                    .iter()
                    .copied()
            })
            .collect();
        Arc::new(Frame {
            width: bounds.right - bounds.left,
            height: bounds.bottom - bounds.top,
            pixels,
        })
    }
}

fn dark(pixel: &[u8; 3]) -> bool {
    pixel.iter().all(|channel| *channel <= 12)
}

fn black_line<'a>(pixels: impl Iterator<Item = &'a [u8; 3]>, length: usize) -> bool {
    // A small amount of noise is allowed; subtitles or logos in a bar usually
    // cause it to be retained rather than cropping potentially useful content.
    pixels.filter(|pixel| !dark(pixel)).count() <= length / 50
}

fn paired(a: usize, b: usize, limit: usize) -> (usize, usize) {
    // Only recognize opposing bars. Reject very deep or asymmetric dark edges.
    if a > 0 && b > 0 && a < limit && b < limit && a.abs_diff(b) <= (a.min(b) / 10).max(2) {
        (a, b)
    } else {
        (0, 0)
    }
}

fn detect(frame: &Frame) -> Option<Bounds> {
    // Near-black frames provide no reliable boundary information.
    let bright = frame
        .pixels
        .iter()
        .filter(|pixel| pixel.iter().any(|channel| *channel >= 32))
        .count();
    if bright * 20 < frame.pixels.len() {
        return None;
    }
    let (w, h) = (frame.width, frame.height);
    let row_limit = h * 3 / 10;
    let row_is_black = |y| black_line(frame.pixels[y * w..(y + 1) * w].iter(), w);
    let top = (0..row_limit).take_while(|&y| row_is_black(y)).count();
    let bottom = (0..row_limit)
        .take_while(|&y| row_is_black(h - 1 - y))
        .count();
    let (top, bottom) = paired(top, bottom, row_limit);
    let column_limit = w * 3 / 10;
    let column_is_black = |x| {
        black_line(
            (top..h - bottom).map(|y| &frame.pixels[y * w + x]),
            h - top - bottom,
        )
    };
    let left = (0..column_limit)
        .take_while(|&x| column_is_black(x))
        .count();
    let right = (0..column_limit)
        .take_while(|&x| column_is_black(w - 1 - x))
        .count();
    let (left, right) = paired(left, right, column_limit);
    Some(Bounds {
        left,
        top,
        right: w - right,
        bottom: h - bottom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(left: usize, top: usize, right: usize, bottom: usize) -> Arc<Frame> {
        Arc::new(Frame {
            width: 160,
            height: 90,
            pixels: (0..160 * 90)
                .map(|i| {
                    let (x, y) = (i % 160, i / 160);
                    if x < left || x >= 160 - right || y < top || y >= 90 - bottom {
                        [0; 3]
                    } else {
                        [200, 80, 20]
                    }
                })
                .collect(),
        })
    }
    fn settle(detector: &mut Detector, frame: Arc<Frame>, start: Instant) -> Arc<Frame> {
        detector.apply(frame.clone(), true, start);
        detector.apply(frame.clone(), true, start + Duration::from_millis(200));
        detector.apply(frame, true, start + Duration::from_millis(400))
    }
    #[test]
    fn letterbox_pillarbox_and_combined_bars_preserve_content() {
        for (left, top, right, bottom) in [(0, 10, 0, 10), (20, 0, 20, 0), (20, 10, 20, 10)] {
            let frame = settle(
                &mut Detector::default(),
                image(left, top, right, bottom),
                Instant::now(),
            );
            assert_eq!(
                (frame.width, frame.height),
                (160 - left - right, 90 - top - bottom)
            );
            assert!(frame.pixels.iter().all(|p| *p == [200, 80, 20]));
        }
    }
    #[test]
    fn dark_frames_hold_crop_and_disable_immediately_restores_source() {
        let mut detector = Detector::default();
        let start = Instant::now();
        settle(&mut detector, image(0, 10, 0, 10), start);
        let dark = Arc::new(Frame {
            width: 160,
            height: 90,
            pixels: vec![[5; 3]; 160 * 90],
        });
        assert_eq!(
            detector
                .apply(dark.clone(), true, start + Duration::from_secs(1))
                .height,
            70
        );
        assert!(Arc::ptr_eq(
            &dark,
            &detector.apply(dark.clone(), false, start + Duration::from_secs(2))
        ));
        assert_eq!(
            detector
                .apply(image(0, 10, 0, 10), true, start + Duration::from_secs(3))
                .height,
            90
        );
    }
    #[test]
    fn transient_bars_do_not_crop_and_full_image_restores_after_confirmation() {
        let mut detector = Detector::default();
        let start = Instant::now();
        assert_eq!(detector.apply(image(0, 10, 0, 10), true, start).height, 90);
        assert_eq!(
            detector
                .apply(image(0, 0, 0, 0), true, start + Duration::from_secs(1))
                .height,
            90
        );
        settle(
            &mut detector,
            image(0, 10, 0, 10),
            start + Duration::from_secs(2),
        );
        assert_eq!(
            settle(
                &mut detector,
                image(0, 0, 0, 0),
                start + Duration::from_secs(3)
            )
            .height,
            90
        );
    }
    #[test]
    fn asymmetric_edges_and_excessive_crop_are_rejected_and_resize_resets() {
        let start = Instant::now();
        for frame in [image(0, 10, 0, 0), image(0, 10, 0, 20), image(0, 28, 0, 28)] {
            let output = settle(&mut Detector::default(), frame, start);
            assert_eq!((output.width, output.height), (160, 90));
        }
        let mut detector = Detector::default();
        settle(&mut detector, image(0, 10, 0, 10), start);
        let resized = Arc::new(Frame {
            width: 10,
            height: 10,
            pixels: vec![[200; 3]; 100],
        });
        assert!(Arc::ptr_eq(
            &resized,
            &detector.apply(resized.clone(), true, start + Duration::from_secs(1))
        ));
    }
}
