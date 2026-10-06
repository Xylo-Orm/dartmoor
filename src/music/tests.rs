use super::*;

fn rhythm(seconds: usize, amplitude: f32, hats: bool) -> Vec<f32> {
    (0..RATE * seconds)
        .flat_map(|n| {
            let t = n as f32 / RATE as f32;
            let beat = (-t.rem_euclid(0.5) * 45.0).exp();
            let bass = (t * std::f32::consts::TAU * 180.0).sin() * beat * amplitude;
            let hat = if hats {
                (t * std::f32::consts::TAU * 6500.0).sin()
                    * (-t.rem_euclid(0.125) * 180.0).exp()
                    * amplitude
                    * 0.5
            } else {
                0.0
            };
            [bass + hat, -bass - hat]
        })
        .collect()
}
fn feed(analyzer: &mut Analyzer, samples: &[f32], settings: &MusicSettings) {
    for block in samples.chunks(960) {
        analyzer.feed(block, settings);
    }
}
#[test]
fn advanced_recognizes_midbass_and_hats_as_separate_events() {
    let settings = MusicSettings::default();
    let mut analyzer = Analyzer::default();
    let audio = rhythm(12, 0.2, true);
    let mut matched = [false; 24];
    let mut main_before = 0;
    for (hop, block) in audio.chunks(960).enumerate() {
        analyzer.feed(block, &settings);
        let count = analyzer.telemetry().onsets;
        if count > main_before {
            let observed = (hop + 1) as f64 * 0.01;
            let beat = (observed / 0.5).floor() as usize;
            let delay = observed - beat as f64 * 0.5;
            assert!(
                delay <= 0.07,
                "attack away from its known onset: {observed:.3}"
            );
            assert!(!matched[beat], "two main attacks for beat {beat}");
            matched[beat] = true;
        }
        main_before = count;
    }
    assert!(matched.iter().filter(|hit| **hit).count() >= 22);
    let t = analyzer.telemetry();
    assert!((22..=26).contains(&t.onsets), "main attacks {}", t.onsets);
    assert!(
        (85..=100).contains(&t.sparkle_onsets),
        "sparkles {}",
        t.sparkle_onsets
    );
    assert!(
        t.bpm.is_some_and(|bpm| (bpm - 120.0).abs() < 3.0),
        "tempo {:?}, confidence {}",
        t.bpm,
        t.confidence
    );
    feed(&mut analyzer, &vec![0.0; RATE * 2], &settings);
    assert!(analyzer.telemetry().bpm.is_none());
    let before = analyzer.telemetry().predicted_beats;
    feed(&mut analyzer, &vec![0.0; RATE * 2 * 5], &settings);
    assert_eq!(analyzer.telemetry().predicted_beats, before);
    assert!(analyzer.colors(16, &settings).iter().all(|c| *c == [0; 3]));
}
#[test]
fn analysis_survives_quiet_playback_and_chunk_boundaries() {
    let settings = MusicSettings {
        follow_beat: false,
        ..Default::default()
    };
    let samples = rhythm(4, 0.01, true);
    let mut regular = Analyzer::default();
    let mut split = Analyzer::default();
    feed(&mut regular, &samples, &settings);
    for chunk in samples.chunks(202) {
        split.feed(chunk, &settings);
    }
    let a = regular.telemetry();
    let b = split.telemetry();
    assert!((6..=9).contains(&a.onsets), "quiet attacks {}", a.onsets);
    assert!(
        a.sparkle_onsets >= 20,
        "quiet sparkles {}",
        a.sparkle_onsets
    );
    assert_eq!(a.onsets, b.onsets);
    assert_eq!(a.sparkle_onsets, b.sparkle_onsets);
    assert!((a.pulse - b.pulse).abs() < 1e-6);
    assert!(a.bpm.is_none() && a.predicted_beats == 0);
}
#[test]
fn compressed_midbass_responds_to_sensitivity_and_subbass_weight_is_effective() {
    let compressed: Vec<_> = (0..RATE * 8)
        .flat_map(|n| {
            let t = n as f32 / RATE as f32;
            let amplitude = 0.08 + 0.045 * (-t.rem_euclid(0.5) * 35.0).exp();
            let v = (t * std::f32::consts::TAU * 180.0).sin() * amplitude;
            [v, -v]
        })
        .collect();
    let mut counts = Vec::new();
    for sensitivity in [0.0, 1.0] {
        let settings = MusicSettings {
            sensitivity,
            follow_beat: false,
            ..Default::default()
        };
        let mut analyzer = Analyzer::default();
        feed(&mut analyzer, &compressed, &settings);
        counts.push(analyzer.telemetry().onsets);
    }
    assert!(
        counts[1] >= 14 && counts[1] <= 18,
        "compressed beats {counts:?}"
    );
    assert!(
        counts[1] > counts[0],
        "sensitivity must affect modest attacks: {counts:?}"
    );
    let sub: Vec<_> = (0..RATE * 4)
        .flat_map(|n| {
            let t = n as f32 / RATE as f32;
            let v =
                (t * std::f32::consts::TAU * 48.0).sin() * (-t.rem_euclid(0.5) * 30.0).exp() * 0.2;
            [v, -v]
        })
        .collect();
    let mut counts = Vec::new();
    for subbass_weight in [0.0, 1.0] {
        let settings = MusicSettings {
            subbass_weight,
            follow_beat: false,
            ..Default::default()
        };
        let mut analyzer = Analyzer::default();
        feed(&mut analyzer, &sub, &settings);
        counts.push(analyzer.telemetry().onsets);
    }
    assert!(
        counts[1] > counts[0],
        "subbass weight must change its influence: {counts:?}"
    );
}
#[test]
fn spectral_bands_retain_stereo_energy_and_sustained_tones_do_not_retrigger() {
    use super::spectral::Spectral;
    for (frequency, band) in [(60.0, 0), (180.0, 1), (1500.0, 2), (6500.0, 3)] {
        let mut spectrum = Spectral::default();
        let mut last = spectral::Features::default();
        for n in 0..RATE {
            let sample = (n as f32 * frequency * std::f32::consts::TAU / RATE as f32).sin() * 0.1;
            if let Some(features) = spectrum.push([sample, -sample]) {
                last = features;
            }
        }
        assert!(
            last.energy[band] > 0.05,
            "{frequency} Hz energy {:?}",
            last.energy
        );
        assert!(
            last.novelty[band] < 0.02,
            "steady {frequency} Hz novelty {:?}",
            last.novelty
        );
    }
    let settings = MusicSettings::default();
    let mut analyzer = Analyzer::default();
    let mut tone = Vec::new();
    for n in 0..RATE * 5 {
        let t = n as f32 / RATE as f32;
        let phase =
            std::f32::consts::TAU * 180.0 * t + 0.4 * (std::f32::consts::TAU * 5.0 * t).sin();
        tone.extend([phase.sin() * 0.1; 2]);
    }
    feed(&mut analyzer, &tone, &settings);
    assert!(analyzer.telemetry().onsets <= 2);
    assert!(analyzer.telemetry().bpm.is_none());
}
#[test]
fn accents_are_bounded_spatial_and_zero_amount_restores_uniform_output() {
    let mut settings = MusicSettings {
        follow_beat: false,
        ..Default::default()
    };
    let mut analyzer = Analyzer::default();
    feed(&mut analyzer, &rhythm(2, 0.2, true), &settings);
    // An upper-band burst while no main hold is active gives a spatial accent.
    let hat: Vec<_> = (0..4800)
        .flat_map(|n| {
            let v = (n as f32 * std::f32::consts::TAU * 6500.0 / RATE as f32).sin() * 0.15;
            [v, -v]
        })
        .collect();
    feed(&mut analyzer, &hat[..3840], &settings);
    let colors = analyzer.colors(16, &settings);
    assert!(colors.windows(2).any(|pair| pair[0] != pair[1]));
    assert!(analyzer.colors(1, &settings).len() == 1);
    let mut bulb = crate::config::Config::default().lights.remove(0);
    bulb.shape = crate::core::Shape::Bulb {
        center: crate::core::Point { x: 0.5, y: 0.5 },
        radius: 0.08,
    };
    let bulb_colors = analyzer.light_colors(&bulb, &settings);
    assert!(bulb_colors.iter().all(|c| *c == bulb_colors[0]));
    settings.sparkle_amount = 0.0;
    let uniform = analyzer.colors(16, &settings);
    assert!(uniform.iter().all(|c| *c == uniform[0]));
    settings.brightness = 0.0;
    assert!(
        analyzer
            .colors(2048, &settings)
            .iter()
            .all(|c| *c == [0; 3])
    );
}
#[test]
fn mode_changes_gaps_and_nonfinite_audio_reset_safely() {
    let mut settings = MusicSettings::default();
    let mut analyzer = Analyzer::default();
    feed(&mut analyzer, &rhythm(6, 0.2, false), &settings);
    analyzer.discontinuity();
    assert_eq!(analyzer.telemetry().discontinuities, 1);
    assert!(analyzer.telemetry().bpm.is_none());
    analyzer.feed(&vec![f32::NAN; 4800], &settings);
    analyzer.feed(&vec![f32::INFINITY; 4800], &settings);
    assert!(analyzer.telemetry().pulse.is_finite());
    settings.detector = DetectorMode::Classic;
    analyzer.feed(&[0.0; 960], &settings);
    assert_eq!(analyzer.telemetry().onsets, 0);
    assert_eq!(analyzer.telemetry().sparkle_onsets, 0);
    settings.detector = DetectorMode::Advanced;
    analyzer.feed(&[0.0; 960], &settings);
    assert_eq!(analyzer.colors(1, &settings), vec![[0; 3]]);
}
#[test]
fn advanced_noise_floor_does_not_amplify_quiet_tones_or_silence() {
    for sensitivity in [0.0, 0.5, 1.0] {
        let settings = MusicSettings {
            sensitivity,
            ..Default::default()
        };
        let mut analyzer = Analyzer::default();
        let samples: Vec<_> = (0..RATE)
            .flat_map(|n| {
                let v = (n as f32 * 180.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.001;
                [v, -v]
            })
            .collect();
        feed(&mut analyzer, &samples, &settings);
        assert_eq!(analyzer.telemetry().onsets, 0);
        assert_eq!(analyzer.telemetry().sparkle_onsets, 0);
        assert_eq!(analyzer.colors(16, &settings), vec![[0; 3]; 16]);
    }
}

#[test]
#[ignore = "release-only analysis/rendering benchmark, no native capture"]
fn music_analysis_benchmark() {
    use std::time::Instant;
    for mode in [DetectorMode::Classic, DetectorMode::Advanced] {
        let settings = MusicSettings {
            detector: mode,
            ..Default::default()
        };
        let mut analyzer = Analyzer::default();
        let audio = rhythm(8, 0.2, true);
        feed(&mut analyzer, &audio, &settings);
        let mut timings = Vec::with_capacity(8000);
        for _ in 0..10 {
            for block in audio.chunks(960) {
                let start = Instant::now();
                analyzer.feed(block, &settings);
                timings.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        timings.sort_by(f64::total_cmp);
        let average = timings.iter().sum::<f64>() / timings.len() as f64;
        let p95 = timings[timings.len() * 95 / 100];
        let p99 = timings[timings.len() * 99 / 100];
        let maximum = timings[timings.len() - 1];
        let start = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(analyzer.colors(60, &settings));
        }
        let typical = start.elapsed().as_secs_f64();
        let start = Instant::now();
        for _ in 0..1000 {
            for _ in 0..8 {
                std::hint::black_box(analyzer.colors(256, &settings));
            }
        }
        println!(
            "{mode:?}: analysis mean {average:.4} ms/hop p95 {p95:.4} p99 {p99:.4} max {maximum:.4}, render60 {typical:.4} ms/frame render2048 {:.4} ms/frame",
            start.elapsed().as_secs_f64()
        );
    }
}
