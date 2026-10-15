//! Synthetic maximum-zone sampling probe. No capture, UI, networking or device output.
use lumen_desktop::core::{Frame, Light, Point, Route, SamplingPlan, Shape, SoftAmbience};
use lumen_desktop::video::{ProcessInput, Processor};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

struct CountAlloc;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "broad".into());
    assert!(
        matches!(
            mode.as_str(),
            "broad" | "narrow" | "bulbs" | "typical" | "ordinary" | "adversarial"
        ),
        "Choose broad, narrow, bulbs, typical, ordinary or adversarial"
    );
    let lights: Vec<_> = (0..if mode == "adversarial" {
        64
    } else if mode == "typical" {
        16
    } else if mode == "ordinary" {
        1
    } else {
        8
    })
        .map(|i| Light {
            video_response_ms: None,
            id: format!("light-{i}"),
            name: format!("Light {i}"),
            zones: if mode == "adversarial" {
                32
            } else if mode == "typical" {
                if i < 8 { 61 } else { 60 }
            } else if mode == "ordinary" {
                60
            } else {
                256
            },
            shape: match mode.as_str() {
                "bulbs" => Shape::Bulb {
                    center: Point { x: 0.5, y: 0.5 },
                    radius: 1.0,
                },
                "adversarial" => Shape::Strip {
                    points: (0..128)
                        .map(|point| match (point + i) % 4 {
                            0 => Point { x: 0.0, y: 0.0 },
                            1 => Point { x: 1.0, y: 1.0 },
                            2 => Point { x: 0.0, y: 1.0 },
                            _ => Point { x: 1.0, y: 0.0 },
                        })
                        .collect(),
                    radius: 0.35,
                    segment_leds: vec![],
                    reverse: i % 2 == 0,
                },
                "typical" | "ordinary" => Shape::Strip {
                    points: vec![
                        Point { x: 0.0, y: 1.0 },
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 1.0, y: 0.0 },
                        Point { x: 1.0, y: 1.0 },
                    ],
                    radius: 0.04,
                    segment_leds: vec![],
                    reverse: false,
                },
                "narrow" => Shape::Strip {
                    points: vec![
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 1.0, y: 1.0 },
                        Point { x: 0.0, y: 1.0 },
                        Point { x: 1.0, y: 0.0 },
                    ],
                    radius: 0.035,
                    segment_leds: vec![],
                    reverse: i % 2 == 0,
                },
                _ => Shape::Strip {
                    points: vec![Point { x: 0.1, y: 0.5 }, Point { x: 0.9, y: 0.5 }],
                    radius: 1.0,
                    segment_leds: vec![],
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

        taps: Some(
            (0..160 * 90)
                .map(|i| {
                    std::array::from_fn(|tap| {
                        [
                            ((i * 71 + tap * 37) % 256) as u8,
                            ((i * 13 + tap * 59) % 256) as u8,
                            ((i * 7 + tap * 91) % 256) as u8,
                        ]
                    })
                })
                .collect(),
        ),
    };
    let start = Instant::now();
    let plan = SamplingPlan::compile(&lights, 160, 90).unwrap();
    println!(
        "{mode}: compile {:.3}ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
    println!("{mode}: geometry {} bytes", plan.retained_bytes());
    for (label, settings) in [
        ("accurate", SoftAmbience::default()),
        (
            "enhanced",
            SoftAmbience {
                enabled: true,
                ..SoftAmbience::default()
            },
        ),
    ] {
        let mut processor = Processor::default();
        let mut result = Vec::new();
        for _ in 0..300 {
            if settings.enabled {
                processor
                    .process(
                        ProcessInput {
                            plan: &plan,
                            frame: black_box(&frame),
                            routes: &lights,
                            settings,
                            brightness: 1.0,
                            fps: 30,
                            now: Instant::now(),
                        },
                        &mut result,
                    )
                    .unwrap();
            } else {
                result = plan.sample(black_box(&frame));
            }
            black_box(&result);
        }
        let iterations = 3000;
        let mut durations = Vec::with_capacity(iterations);
        let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
        for _ in 0..iterations {
            let started = Instant::now();
            if settings.enabled {
                processor
                    .process(
                        ProcessInput {
                            plan: &plan,
                            frame: black_box(&frame),
                            routes: &lights,
                            settings,
                            brightness: 1.0,
                            fps: 30,
                            now: Instant::now(),
                        },
                        &mut result,
                    )
                    .unwrap();
            } else {
                result = plan.sample(black_box(&frame));
            }
            black_box(&result);
            durations.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before;
        durations.sort_by(f64::total_cmp);
        let mean = durations.iter().sum::<f64>() / iterations as f64;
        println!(
            "{mode} {label}: mean {mean:.3}ms p50 {:.3}ms p95 {:.3}ms p99 {:.3}ms max {:.3}ms ({iterations} iterations); analysis scratch {} bytes; measured allocations {allocations}",
            durations[iterations / 2],
            durations[iterations * 95 / 100],
            durations[iterations * 99 / 100],
            durations[iterations - 1],
            processor.retained_bytes()
        );
    }
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
