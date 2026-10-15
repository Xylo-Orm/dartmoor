//! Offline music analysis probe; never opens a playback or output device.
use anyhow::{Context, Result, bail, ensure};
use lumen_desktop::music::{
    AnalysisEvent, Analyzer, CHANNELS, DemoScenario, MusicSettings, OnsetKind, RATE,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, time::Instant};

const MAX_AUDIO_BYTES: usize = 128 * 1024 * 1024;
const BLOCK: usize = RATE / 100 * CHANNELS;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Annotations {
    #[serde(default)]
    onsets: Vec<f64>,
    #[serde(default)]
    beats: Vec<f64>,
}

#[derive(Serialize)]
struct Measure {
    tolerance_ms: u32,
    precision: f64,
    recall: f64,
    f1: f64,
    hits: usize,
    extras: usize,
    misses: usize,
    mean_error_ms: Option<f64>,
}

fn measure(reference: &[f64], observed: &[f64], tolerance_ms: u32) -> Measure {
    let mut used = vec![false; reference.len()];
    let mut errors = Vec::new();
    for &event in observed {
        let nearest = reference
            .iter()
            .enumerate()
            .filter(|(i, t)| !used[*i] && (event - **t).abs() * 1000.0 <= tolerance_ms as f64)
            .min_by(|(_, a), (_, b)| (event - **a).abs().total_cmp(&(event - **b).abs()));
        if let Some((i, t)) = nearest {
            used[i] = true;
            errors.push((event - *t) * 1000.0);
        }
    }
    let hits = errors.len();
    let extras = observed.len() - hits;
    let misses = reference.len() - hits;
    let precision = if observed.is_empty() {
        if reference.is_empty() { 1.0 } else { 0.0 }
    } else {
        hits as f64 / observed.len() as f64
    };
    let recall = if reference.is_empty() {
        if observed.is_empty() { 1.0 } else { 0.0 }
    } else {
        hits as f64 / reference.len() as f64
    };
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Measure {
        tolerance_ms,
        precision,
        recall,
        f1,
        hits,
        extras,
        misses,
        mean_error_ms: (!errors.is_empty())
            .then(|| errors.iter().sum::<f64>() / errors.len() as f64),
    }
}

fn synthetic(scenario: &str) -> Result<(Vec<f32>, Annotations)> {
    let seconds = 12;
    let times: Vec<f64> = match scenario {
        "straight" | "quiet" | "compressed" | "stereo" | "sustained" => {
            (0..seconds * 2).map(|n| n as f64 * 0.5).collect()
        }
        "tempo_step" => (0..12)
            .map(|n| n as f64 * 0.5)
            .chain((0..15).map(|n| 6.0 + n as f64 * 0.4))
            .collect(),
        "missing" => (0..seconds * 2)
            .filter(|n| *n != 12)
            .map(|n| n as f64 * 0.5)
            .collect(),
        "swing" => (0..seconds * 2)
            .map(|n| n as f64 * 0.5 + if n % 2 == 1 { 0.08 } else { 0.0 })
            .collect(),
        "silence" => vec![],
        _ => bail!("unknown synthetic scenario: {scenario}"),
    };
    let mut samples = Vec::with_capacity(seconds * RATE * CHANNELS);
    let mut beat_index = 0;
    for n in 0..seconds * RATE {
        let t = n as f32 / RATE as f32;
        while beat_index + 1 < times.len() && t as f64 >= times[beat_index + 1] {
            beat_index += 1;
        }
        let since = if times.is_empty() || (t as f64) < times[0] {
            f32::INFINITY
        } else {
            t - times[beat_index] as f32
        };
        let beat = (-since * 45.0).exp();
        let amplitude = if scenario == "quiet" { 0.012 } else { 0.2 };
        let bed = if matches!(scenario, "compressed" | "sustained") {
            0.06
        } else {
            0.0
        };
        let bass = (t * std::f32::consts::TAU * 180.0).sin() * (beat * amplitude + bed);
        let hat = if scenario == "silence" || scenario == "sustained" {
            0.0
        } else {
            (t * std::f32::consts::TAU * 6500.0).sin() * (-t.rem_euclid(0.125) * 180.0).exp() * 0.1
        };
        let left = bass + hat;
        samples.extend([left, if scenario == "stereo" { left } else { -left }]);
    }
    let beats = if scenario == "missing" {
        (0..seconds * 2).map(|n| n as f64 * 0.5).collect()
    } else {
        times.clone()
    };
    Ok((
        samples,
        Annotations {
            onsets: times,
            beats,
        },
    ))
}

fn demo_scenario(name: &str) -> Result<(Vec<f32>, Annotations)> {
    let scenario = match name {
        "straight" => DemoScenario::Straight,
        "hats" => DemoScenario::Hats,
        "swing" => DemoScenario::Swing,
        "tempo_step" => DemoScenario::TempoStep,
        "quiet" => DemoScenario::Quiet,
        "silence_gap" => DemoScenario::SilenceGap,
        _ => bail!("unknown demo scenario: {name}"),
    };
    let times: Vec<f64> = match scenario {
        DemoScenario::TempoStep => (0..12)
            .map(|n| n as f64 * 0.5)
            .chain((0..15).map(|n| 6.0 + n as f64 * 0.4))
            .collect(),
        DemoScenario::Swing => (0..24)
            .map(|n| n as f64 * 0.5 + if n % 2 == 1 { 0.08 } else { 0.0 })
            .collect(),
        DemoScenario::SilenceGap => (0..24)
            .map(|n| n as f64 * 0.5)
            .filter(|time| !(3.0..5.0).contains(time))
            .collect(),
        _ => (0..24).map(|n| n as f64 * 0.5).collect(),
    };
    Ok((
        lumen_desktop::music::demo_fixture(scenario, 12),
        Annotations {
            onsets: times.clone(),
            beats: times,
        },
    ))
}

fn wav(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        bytes.len() <= MAX_AUDIO_BYTES,
        "WAV exceeds 128 MiB replay limit"
    );
    ensure!(
        bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE",
        "expected RIFF/WAVE"
    );
    let declared = u32::from_le_bytes(bytes[4..8].try_into()?) as usize + 8;
    ensure!(declared <= bytes.len(), "truncated RIFF file");
    let mut format = None;
    let mut data = None;
    let mut at = 12;
    while at + 8 <= declared {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        at += 8;
        let end = at.checked_add(size).context("WAV chunk overflow")?;
        ensure!(end <= declared, "truncated WAV chunk");
        match &bytes[at - 8..at - 4] {
            b"fmt " => format = Some(&bytes[at..end]),
            b"data" => data = Some(&bytes[at..end]),
            _ => {}
        }
        at = end + (size & 1);
    }
    let fmt = format.context("missing WAV fmt chunk")?;
    ensure!(fmt.len() >= 16, "short WAV fmt chunk");
    let codec = u16::from_le_bytes(fmt[..2].try_into()?);
    let channels = u16::from_le_bytes(fmt[2..4].try_into()?) as usize;
    let rate = u32::from_le_bytes(fmt[4..8].try_into()?);
    let bits = u16::from_le_bytes(fmt[14..16].try_into()?);
    ensure!(rate == RATE as u32, "replay requires 48 kHz WAV");
    ensure!(
        (1..=2).contains(&channels),
        "replay requires mono/stereo WAV"
    );
    ensure!(
        matches!((codec, bits), (1, 16) | (3, 32)),
        "replay requires PCM16 or IEEE F32 WAV"
    );
    let data = data.context("missing WAV data chunk")?;
    let width = (bits / 8) as usize;
    ensure!(
        data.len().is_multiple_of(width * channels),
        "incomplete WAV frame"
    );
    let mut out = Vec::with_capacity(data.len() / width * CHANNELS / channels);
    for frame in data.chunks_exact(width * channels) {
        let sample = |channel: usize| -> f32 {
            let start = channel * width;
            if codec == 1 {
                i16::from_le_bytes(frame[start..start + 2].try_into().unwrap()) as f32 / 32768.0
            } else {
                f32::from_le_bytes(frame[start..start + 4].try_into().unwrap())
            }
        };
        let left = sample(0);
        out.extend([left, sample(channels - 1)]);
    }
    Ok(out)
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    sorted[(sorted.len() as f64 * fraction) as usize]
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (audio, annotations) = if args.first().is_some_and(|arg| arg == "--synthetic")
        && args.len() <= 2
    {
        synthetic(args.get(1).map_or("straight", String::as_str))?
    } else if args.first().is_some_and(|arg| arg == "--demo") && args.len() == 2 {
        demo_scenario(&args[1])?
    } else if args.len() == 2 {
        let annotations: Annotations = serde_json::from_slice(&fs::read(&args[1])?)?;
        (wav(Path::new(&args[0]))?, annotations)
    } else {
        bail!(
            "usage: cargo run --bin music_replay -- --synthetic [straight|quiet|compressed|stereo|sustained|tempo_step|missing|swing|silence] | --demo [straight|hats|swing|tempo_step|quiet|silence_gap] | FILE.wav ANNOTATIONS.json"
        )
    };
    ensure!(
        annotations
            .onsets
            .iter()
            .chain(&annotations.beats)
            .all(|v| v.is_finite() && *v >= 0.0),
        "invalid annotation time"
    );
    let settings = MusicSettings::default();
    let mut analyzer = Analyzer::default();
    let mut onset_times = Vec::new();
    let mut prediction_times = Vec::new();
    let mut timings = Vec::new();
    let mut features = Vec::new();
    let mut events = Vec::new();
    for chunk in audio.chunks(BLOCK) {
        let began = Instant::now();
        analyzer.feed(chunk, &settings);
        timings.push(began.elapsed().as_secs_f64() * 1000.0);
        if let Some(frame) = analyzer.latest_frame() {
            features.push(frame);
        }
        for event in analyzer.take_events() {
            match event {
                AnalysisEvent::Onset(onset) if matches!(onset.kind, OnsetKind::Main) => {
                    onset_times.push(onset.time_seconds)
                }
                AnalysisEvent::Beat(beat) if !beat.observed && beat.starts_visual => {
                    prediction_times.push(beat.time_seconds)
                }
                _ => {}
            }
            events.push(event);
        }
    }
    ensure!(!timings.is_empty(), "empty audio");
    timings.sort_by(f64::total_cmp);
    let render = |zones| {
        let start = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(analyzer.colors(zones, &settings));
        }
        start.elapsed().as_secs_f64() * 1000.0 / 1000.0
    };
    let onset_metrics = [25, 50, 70].map(|ms| measure(&annotations.onsets, &onset_times, ms));
    let beat_metrics = [25, 50, 70].map(|ms| {
        let mut beats = onset_times.clone();
        beats.extend(&prediction_times);
        beats.sort_by(f64::total_cmp);
        measure(&annotations.beats, &beats, ms)
    });
    let report = serde_json::json!({
        "source": if args.first().is_some_and(|arg| arg == "--synthetic") { "synthetic" }
            else if args.first().is_some_and(|arg| arg == "--demo") { "demo" } else { "wav" },
        "scenario": args.first().filter(|arg| *arg == "--synthetic" || *arg == "--demo")
            .map(|_| args.get(1).map_or("straight", String::as_str)),
        "duration_seconds": audio.len() as f64 / (RATE * CHANNELS) as f64,
        "onset_times_seconds": onset_times,
        "predicted_times_seconds": prediction_times,
        "features": features,
        "events": events,
        "onset_metrics": onset_metrics,
        "beat_metrics": beat_metrics,
        "final_bpm": analyzer.telemetry().bpm,
        "final_confidence": analyzer.telemetry().confidence,
        "analysis_ms": {"p50": percentile(&timings, 0.5), "p95": percentile(&timings, 0.95), "p99": percentile(&timings, 0.99)},
        "render_ms": {"zones_60": render(60), "zones_2048": render(2048)},
        "timestamp_quality": "hop_end_estimate"
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_to_one_metrics_do_not_count_duplicate_hits() {
        let result = measure(&[0.5], &[0.51, 0.52], 25);
        assert_eq!((result.hits, result.extras, result.misses), (1, 1, 0));
    }
    #[test]
    fn event_timing_is_independent_of_valid_chunk_boundaries() {
        let (audio, _) = synthetic("straight").unwrap();
        let events = |chunk_size| {
            let mut analyzer = Analyzer::default();
            let mut onsets = Vec::new();
            for chunk in audio.chunks(chunk_size) {
                analyzer.feed(chunk, &MusicSettings::default());
                for event in analyzer.take_events() {
                    if let AnalysisEvent::Onset(onset) = event
                        && matches!(onset.kind, OnsetKind::Main)
                    {
                        onsets.push(onset.time_seconds);
                    }
                }
            }
            onsets
        };
        assert_eq!(events(960), events(202));
    }
}
