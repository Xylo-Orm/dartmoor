use lumen_desktop::{
    capture,
    config::Config,
    core::{Point, SamplingPlan, Shape},
    editor::{EditHistory, MAX_HISTORY, MAX_HISTORY_BYTES},
};
use std::{collections::VecDeque, time::Instant};

fn main() {
    // Reviewer-created state-machine oracle, independent of internal history helpers.
    let mut history = EditHistory::default();
    let mut actual = Config::default();
    let mut undo = VecDeque::new();
    let mut redo = VecDeque::new();
    let mut rng = 0x9e3779b97f4a7c15u64;
    let mut counts = [0usize; 3];
    for i in 0..10_000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        match rng % 3 {
            0 => {
                let before = actual.clone();
                actual.lights[0].name = format!("Reviewer transaction {i}");
                assert!(history.checkpoint(&before, &actual));
                redo.clear();
                if undo.len() == MAX_HISTORY { undo.pop_front(); }
                undo.push_back(before);
                counts[0] += 1;
            }
            1 => {
                let expected = undo.pop_back();
                let observed = history.undo(&actual);
                assert_eq!(observed, expected);
                if let Some(next) = observed {
                    if redo.len() == MAX_HISTORY { redo.pop_front(); }
                    redo.push_back(actual);
                    actual = next;
                }
                counts[1] += 1;
            }
            _ => {
                let expected = redo.pop_back();
                let observed = history.redo(&actual);
                assert_eq!(observed, expected);
                if let Some(next) = observed {
                    if undo.len() == MAX_HISTORY { undo.pop_front(); }
                    undo.push_back(actual);
                    actual = next;
                }
                counts[2] += 1;
            }
        }
        assert_eq!(history.can_undo(), !undo.is_empty());
        assert_eq!(history.can_redo(), !redo.is_empty());
        assert!(history.retained_bytes() <= MAX_HISTORY_BYTES);
        actual.validate().unwrap();
    }
    println!("Reviewer history oracle: 10,000 operations passed; edits/undo/redo={counts:?}");

    // Valid supported upper-zone layout with deliberately broad sampling regions.
    let mut config = Config::default();
    let mut template = config.lights[0].clone();
    template.shape = Shape::Bulb { center: Point { x: 0.5, y: 0.5 }, radius: 1.0 };
    template.zones = 32;
    config.lights = (0..64).map(|i| {
        let mut light = template.clone();
        light.id = format!("reviewer-{i}");
        light
    }).collect();
    config.validate().unwrap();
    let frame = capture::synthetic_frame(0.0);
    let started = Instant::now();
    let plan = SamplingPlan::compile(&config.lights, frame.width, frame.height).unwrap();
    let compile_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = Instant::now();
    for _ in 0..3 {
        let colors = plan.sample(&frame);
        assert_eq!(colors.len(), 64);
        assert!(colors.iter().all(|zones| zones.len() == 32 && zones.windows(2).all(|p| p[0] == p[1])));
        std::hint::black_box(colors);
    }
    println!("Reviewer synthetic stress: 64 bulbs / 2048 zones / radius 1.0; compile {compile_ms:.2} ms; sample {:.2} ms/frame", started.elapsed().as_secs_f64() * 1000.0 / 3.0);
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")) { println!("{line}"); }
    }
}
