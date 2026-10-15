//! Deterministic, hardware-free Soft ambience replay. CSV output can be plotted or diffed.
use lumen_desktop::{
    core::{Frame, Light, Point, Route, SamplingPlan, Shape, SoftAmbience, encode},
    video::{ProcessInput, Processor},
};
use std::time::{Duration, Instant};
fn main() {
    let light = Light {
        video_response_ms: None,
        id: "replay".into(),
        name: "Replay".into(),
        zones: 1,
        route: Route::Mock,
        shape: Shape::Bulb {
            center: Point { x: 0.5, y: 0.5 },
            radius: 1.0,
        },
    };
    let plan = SamplingPlan::compile(std::slice::from_ref(&light), 16, 9).unwrap();
    let mut processor = Processor::default();
    let mut output = Vec::new();
    let start = Instant::now();
    println!("frame,ms,scene,r,g,b");
    for n in 0..480 {
        let scene = n / 120;
        let taps: Vec<[[u8; 3]; 16]> = (0..16 * 9)
            .map(|_| {
                std::array::from_fn(|i| match scene {
                    0 => {
                        if i < 5 {
                            [255, 0, 0]
                        } else {
                            [255; 3]
                        }
                    }
                    1 => {
                        if i < 8 {
                            [255, 0, 0]
                        } else {
                            [0, 255, 255]
                        }
                    }
                    2 => [128; 3],
                    _ => [0, 255, 255],
                })
            })
            .collect();
        let pixels = taps
            .iter()
            .map(|group| {
                let mut sum = [0.0; 3];
                for rgb in group {
                    for c in 0..3 {
                        sum[c] += lumen_desktop::core::srgb_to_linear(rgb[c]);
                    }
                }
                encode(sum.map(|v| v / 16.0), 1.0)
            })
            .collect();
        let frame = Frame {
            width: 16,
            height: 9,
            pixels,
            taps: Some(taps),
        };
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
        let [r, g, b] = encode(output[0][0], 1.0);
        println!("{n},{:.3},{scene},{r},{g},{b}", n as f64 * 1000.0 / 30.0);
    }
}
