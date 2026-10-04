//! Synthetic maximum-zone sampling probe. No capture, UI, networking or device output.
use lumen_desktop::core::{Frame, Light, Point, Route, SamplingPlan, Shape};
use std::{hint::black_box, time::Instant};
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "broad".into());
    assert!(
        matches!(mode.as_str(), "broad" | "narrow" | "bulbs"),
        "Choose broad, narrow or bulbs"
    );
    let lights: Vec<_> = (0..8)
        .map(|i| Light {
            id: format!("light-{i}"),
            name: format!("Light {i}"),
            zones: 256,
            shape: match mode.as_str() {
                "bulbs" => Shape::Bulb {
                    center: Point { x: 0.5, y: 0.5 },
                    radius: 1.0,
                },
                "narrow" => Shape::Strip {
                    points: vec![
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 1.0, y: 1.0 },
                        Point { x: 0.0, y: 1.0 },
                        Point { x: 1.0, y: 0.0 },
                    ],
                    radius: 0.035,
                    reverse: i % 2 == 0,
                },
                _ => Shape::Strip {
                    points: vec![Point { x: 0.1, y: 0.5 }, Point { x: 0.9, y: 0.5 }],
                    radius: 1.0,
                    reverse: false,
                },
            },
            route: Route::Mock,
        })
        .collect();
    let frame = Frame {
        width: 160,
        height: 90,
        pixels: (0..160 * 90)
            .map(|i| [(i * 71 % 256) as u8, (i * 13 % 256) as u8, (i % 7) as u8])
            .collect(),
    };
    let start = Instant::now();
    let plan = SamplingPlan::compile(&lights, 160, 90).unwrap();
    println!(
        "{mode}: compile {:.3}ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    for _ in 0..5 {
        black_box(plan.sample(black_box(&frame)));
    }
    let start = Instant::now();
    let iterations = if mode == "broad" { 120 } else { 1000 };
    for _ in 0..iterations {
        black_box(plan.sample(black_box(&frame)));
    }
    println!(
        "{mode}: sample {:.3}ms/frame ({iterations} iterations)",
        start.elapsed().as_secs_f64() * 1000.0 / iterations as f64
    );
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status
            .lines()
            .filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:"))
        {
            println!("{line}");
        }
    }
    let colors = plan.sample(&frame);
    println!(
        "{mode}: output {} lights / {} zones, checksum {:.6}",
        colors.len(),
        colors.iter().map(Vec::len).sum::<usize>(),
        colors
            .iter()
            .flatten()
            .map(|c| f64::from(c[0]) + f64::from(c[1]) + f64::from(c[2]))
            .sum::<f64>()
    );
}
