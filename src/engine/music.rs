//! Audio worker: bounded polling, analysis, and logical light frames.
use super::*;

pub(super) fn process_music(
    mut config: watch::Receiver<Arc<Config>>,
    stop: &AtomicBool,
    frames: watch::Sender<Option<Arc<Processed>>>,
    session: u64,
    mut source: Box<dyn crate::music::AudioSource>,
) -> anyhow::Result<()> {
    let mut analyzer = crate::music::Analyzer::default();
    let mut count = 0;
    let mut last_audio = Instant::now();
    let mut last_tick = last_audio;
    let mut next_output = last_audio;
    let interval = Duration::from_secs_f64(1.0 / config.borrow().music.output_fps as f64);
    let mut analysis_ms = 0.0;
    let empty = Arc::new(Frame {
        width: 1,
        height: 1,
        pixels: vec![[0; 3]],

        taps: None,
    });
    while !stop.load(Ordering::Relaxed) {
        let poll_started = Instant::now();
        let active = config.borrow_and_update().clone();
        let read = source.read(Duration::from_millis(10))?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let now = Instant::now();
        analyzer.set_estimated_queue_age(read.estimated_queue_age);
        if read.discontinuity {
            analyzer.discontinuity();
        }
        analyzer.feed(&[], &active.music);
        match read.block {
            crate::music::AudioBlock::Samples(samples) => {
                anyhow::ensure!(
                    samples.len() <= crate::music::MAX_SAMPLES
                        && samples.len().is_multiple_of(crate::music::CHANNELS),
                    "invalid audio sample block"
                );
                last_audio = now;
                analyzer.feed(&samples, &active.music);
            }
            crate::music::AudioBlock::Idle => {
                last_audio = now;
                analyzer.idle(now.duration_since(last_tick), &active.music);
            }
            crate::music::AudioBlock::Timeout
                if now.duration_since(last_audio) >= Duration::from_millis(100) =>
            {
                // Ordinary delivery jitter must not reset the spectral windows.
                analyzer.idle(now.duration_since(last_tick), &active.music);
            }
            crate::music::AudioBlock::Timeout => {}
        }
        anyhow::ensure!(
            now.duration_since(last_audio) <= Duration::from_secs(2),
            "Playback capture stopped delivering audio. Start again to reconnect."
        );
        last_tick = now;
        analysis_ms += now.elapsed().as_secs_f64() * 1000.0;
        if now >= next_output {
            let rendered = Instant::now();
            let mut ordered_ids: Vec<_> = active
                .lights
                .iter()
                .map(|light| light.id.as_str())
                .collect();
            ordered_ids.sort_unstable();
            ordered_ids.sort_by_key(|id| {
                active
                    .music
                    .alternating_order
                    .iter()
                    .position(|selected| selected == id)
                    .unwrap_or(active.music.alternating_order.len())
            });
            let colors = active
                .lights
                .iter()
                .map(|light| {
                    let rank = ordered_ids
                        .iter()
                        .position(|id| *id == light.id)
                        .unwrap_or(0);
                    analyzer.light_colors_indexed(light, &active.music, rank, ordered_ids.len())
                })
                .collect();
            count += 1;
            let processed = Processed {
                session,
                config: active,
                frame: empty.clone(),
                colors,
                frames: count,
                ms: analysis_ms + rendered.elapsed().as_secs_f64() * 1000.0,
                produced: last_audio,
                dark_zones: 0,
                sampled_colors: vec![],
                music: Some(analyzer.telemetry()),
                music_events: analyzer.take_events(),
                music_frame: analyzer.latest_frame(),
            };
            frames.send_replace(Some(Arc::new(processed)));
            analysis_ms = 0.0;
            next_output += interval;
            if next_output <= now {
                next_output = now + interval;
            }
        }
        let deadline = poll_started + Duration::from_millis(10);
        while !stop.load(Ordering::Relaxed) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::park_timeout(remaining);
        }
    }
    Ok(())
}
