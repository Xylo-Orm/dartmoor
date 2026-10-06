use lumen_desktop::{
    app, capture,
    config::{CaptureSelection, Config},
    core::{self, SamplingPlan},
    engine::{Command, Engine, SessionState},
};
use std::time::{Duration, Instant};
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lumen_desktop=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help") {
        println!(
            "Lumen Desktop\n  --demo-seconds N       synthetic capture + mock output (no UI)\n  --capture-probe N      real screen capture + mock output (portal permission)\n  --music-demo-seconds N synthetic music pulses + mock output\n  --music-probe N        Linux playback audio + mock output\n  --benchmark           deterministic sampling workload\n  --ui-smoke-seconds N   synthetic native UI, minimize/restore/close\n  --real-capture        with UI smoke: real portal source, mock lights\nWithout flags: native editor. Close stops synchronization."
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--benchmark") {
        return benchmark();
    }
    for (flag, real, music) in [
        ("--demo-seconds", false, false),
        ("--capture-probe", true, false),
        ("--music-demo-seconds", false, true),
        ("--music-probe", true, true),
    ] {
        if let Some(i) = args.iter().position(|a| a == flag) {
            let seconds = args
                .get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("Missing seconds"))?
                .parse::<u64>()?;
            return probe(seconds, real, music);
        }
    }
    let smoke = args
        .iter()
        .position(|a| a == "--ui-smoke-seconds")
        .map(|i| {
            args.get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("Missing seconds"))?
                .parse::<u64>()
                .map(Duration::from_secs)
                .map_err(anyhow::Error::from)
        })
        .transpose()?;
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 700.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Lumen Desktop",
        options,
        Box::new(move |_| {
            Ok(Box::new(
                app::App::new(smoke, args.iter().any(|arg| arg == "--real-capture"))
                    .map_err(|error| error.into_boxed_dyn_error())?,
            ))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Could not start desktop editor: {e}"))
}
fn probe(seconds: u64, real: bool, music: bool) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let engine = Engine::new(&runtime);
        let mut config = Config::default();
        if music {
            config.mode = lumen_desktop::music::SyncMode::Music;
            config.music.input = if real {
                lumen_desktop::music::AudioInput::Playback
            } else {
                lumen_desktop::music::AudioInput::Demo
            };
        } else if real {
            config.source = CaptureSelection::Desktop { id: None };
        }
        engine.send(Command::Start(config))?;
        let requested = Instant::now();
        let mut start = None;
        let mut error = None;
        while start.is_none_or(|start: Instant| start.elapsed() < Duration::from_secs(seconds)) {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let s = engine.snapshots.borrow().clone();
            if s.frames > 0 {
                start.get_or_insert_with(Instant::now);
            } else if requested.elapsed() >= Duration::from_secs(120) {
                error = Some(
                    "Capture did not start within 120 seconds; answer or cancel the portal chooser"
                        .into(),
                );
                break;
            }
            println!(
                "{:?}: {} frames; sample {:.3} ms; {}",
                s.state, s.frames, s.processing_ms, s.message
            );
            if let Some(audio) = s.music {
                println!(
                    "audio rms {:.3}; bands {:?}; pulse {:.3}; {} attacks; {} sparkles; tempo {:?}; confidence {:.2}; {} predictions; {} resets",
                    audio.rms, audio.bands, audio.pulse, audio.onsets, audio.sparkle_onsets,
                    audio.bpm, audio.confidence, audio.predicted_beats, audio.discontinuities
                );
            }
            if s.state == SessionState::Error {
                error = Some(s.message);
                break;
            }
        }
        let s = engine.snapshots.borrow().clone();
        println!("Final frames: {}", s.frames);
        engine.shutdown().await;
        if let Some(e) = error {
            anyhow::bail!(e);
        }
        if s.frames == 0 {
            anyhow::bail!("No frames captured; permission or platform path remains unverified");
        }
        Ok(())
    })
}
fn benchmark() -> anyhow::Result<()> {
    let mut c = Config::default();
    for _ in 0..15 {
        let mut l = c.lights[0].clone();
        l.id = format!("bench-{}", c.lights.len());
        l.zones = 64;
        c.lights.push(l);
    }
    let frame = capture::synthetic_frame(0.0);
    let plan = SamplingPlan::compile(&c.lights, frame.width, frame.height)?;
    let mut previous = vec![];
    let start = Instant::now();
    let iterations = 3000;
    for _ in 0..iterations {
        let colors = plan.sample(&frame);
        core::smooth(&mut previous, &colors, Duration::from_millis(33), 200.0);
        std::hint::black_box(&previous);
    }
    println!(
        "SIMULATED CPU workload: 160x90 SDR,16 strips,968 zones,{iterations} iterations: {:.4} ms/iteration",
        start.elapsed().as_secs_f64() * 1000.0 / iterations as f64
    );
    Ok(())
}
