use super::*;
#[test]
fn mode_switch_preserves_displayed_linear_color() {
    let original = [0.8, 0.2, 0.4];
    let mut colors = vec![vec![original]];
    carry_video_colors(&mut colors, true, 0.5);
    assert_eq!(colors[0][0], [0.4, 0.1, 0.2]);
    carry_video_colors(&mut colors, false, 0.5);
    for (actual, expected) in colors[0][0].into_iter().zip(original) {
        assert!((actual - expected).abs() < 1e-6);
    }
    carry_video_colors(&mut colors, true, 0.0);
    carry_video_colors(&mut colors, false, 0.0);
    assert_eq!(colors[0][0], [0.0; 3]);
}
#[test]
fn soft_fade_advances_during_capture_gaps_without_refreshing_source_time() {
    struct OneFrame {
        sent: bool,
    }
    impl capture::CaptureSource for OneFrame {
        fn next_frame(&mut self, timeout: Duration) -> anyhow::Result<Option<Frame>> {
            if !self.sent {
                self.sent = true;
                Ok(Some(solid_frame()))
            } else {
                std::thread::sleep(timeout);
                Ok(None)
            }
        }
        fn stop(&mut self) {}
    }
    let (_, config) = watch::channel(Arc::new(Config {
        soft_ambience: core::SoftAmbience {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    }));
    let (frames, received) = watch::channel(None);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        process_capture(
            config,
            &worker_stop,
            frames,
            1,
            Arc::new(|_, _| Ok(Box::new(OneFrame { sent: false }))),
        )
    });
    std::thread::sleep(Duration::from_millis(260));
    let output = received.borrow().clone().expect("soft output");
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap().unwrap();
    assert!(
        output.frames > 1,
        "fade should render between source frames"
    );
    assert!(output.colors[0][0][0] > 0);
    assert!(output.produced.elapsed() >= Duration::from_millis(180));
}
struct SplitDarkPicture;
impl capture::CaptureSource for SplitDarkPicture {
    fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
        Ok(Some(Frame {
            width: 160,
            height: 90,
            pixels: (0..160 * 90)
                .map(|i| {
                    if !(10..80).contains(&(i / 160)) {
                        [0; 3]
                    } else if i % 160 < 80 {
                        [10; 3]
                    } else {
                        [0, 0, 255]
                    }
                })
                .collect(),

            taps: None,
        }))
    }
    fn stop(&mut self) {}
}

#[tokio::test]
async fn live_dark_settings_crop_and_smoothing_keep_independent_zones_off() {
    let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = opens.clone();
    let factory: CaptureFactory = Arc::new(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(SplitDarkPicture))
    });
    let (commands, rx) = mpsc::channel(8);
    let (snapshots, mut state) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run_with_factory(rx, snapshots, factory));
    let mut config = Config {
        black_bar_detection: true,
        brightness: 1.0,
        smoothing_ms: 5000.0,
        ..Default::default()
    };
    config.soft_ambience.enabled = true;
    config.lights[0].zones = 1;
    config.lights[0].shape = core::Shape::Bulb {
        center: core::Point { x: 0.25, y: 0.5 },
        radius: 0.02,
    };
    let mut second = config.lights[0].clone();
    second.id = "blue-zone".into();
    second.shape = core::Shape::Bulb {
        center: core::Point { x: 0.75, y: 0.5 },
        radius: 0.02,
    };
    config.lights.push(second);
    commands.send(Command::Start(config.clone())).await.unwrap();
    for (enabled, threshold, expected_off) in [
        (false, 8, false),
        (true, 8, false),
        (true, 12, true),
        (false, 12, false),
        (true, 12, true),
        (true, 0, false),
    ] {
        config.dark_zones = crate::dark_zones::DarkZones { enabled, threshold };
        commands.send(Command::Apply(config.clone())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                state.changed().await.unwrap();
                let snapshot = state.borrow_and_update();
                if snapshot
                    .preview
                    .as_ref()
                    .is_none_or(|frame| frame.height != 70)
                {
                    continue;
                }
                if snapshot.colors.len() != 2
                    || snapshot.dark_zones != usize::from(expected_off)
                    || enabled != !snapshot.sampled_colors.is_empty()
                {
                    continue;
                }
                let dark = snapshot.colors[0][0];
                if (dark == [0; 3]) != expected_off {
                    continue;
                }
                let blue = snapshot.colors[1][0];
                assert!(blue[2] > blue[0] && blue[2] > blue[1], "{blue:?}");
                if enabled {
                    assert_eq!(snapshot.sampled_colors[0][0], [10; 3]);
                }
                break;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    commands.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn music_demo_updates_live_and_explicit_workspace_handover_closes_video() {
    struct Tracked {
        stops: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl capture::CaptureSource for Tracked {
        fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
            Ok(Some(solid_frame()))
        }
        fn stop(&mut self) {
            self.stops.fetch_add(1, Ordering::Relaxed);
        }
    }
    let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = stops.clone();
    let factory: CaptureFactory = Arc::new(move |_, _| {
        Ok(Box::new(Tracked {
            stops: count.clone(),
        }))
    });
    let (commands, rx) = mpsc::channel(8);
    let (snapshots, mut state) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run_with_factory(rx, snapshots, factory));
    let mut config = Config::default();
    commands.send(Command::Start(config.clone())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            state.changed().await.unwrap();
            if state.borrow().state == SessionState::Running {
                break;
            }
        }
    })
    .await
    .unwrap();
    config.mode = crate::music::SyncMode::Music;
    config.music.input = crate::music::AudioInput::Demo;
    commands.send(Command::Start(config.clone())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            state.changed().await.unwrap();
            let snapshot = state.borrow_and_update();
            if snapshot.mode == Some(crate::music::SyncMode::Music)
                && snapshot.music.is_some()
                && snapshot
                    .colors
                    .iter()
                    .flatten()
                    .any(|color| *color != [0; 3])
            {
                assert!(snapshot.preview.is_none());
                assert_eq!(snapshot.colors[0].len(), config.lights[0].zones);
                assert_eq!(stops.load(Ordering::Relaxed), 1);
                break;
            }
        }
    })
    .await
    .unwrap();
    for brightness in [0.0, 1.0] {
        config.music.brightness = brightness;
        commands.send(Command::Apply(config.clone())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                state.changed().await.unwrap();
                let snapshot = state.borrow_and_update();
                if !snapshot.colors.is_empty()
                    && snapshot.colors.iter().flatten().any(|c| *c != [0; 3]) == (brightness > 0.0)
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(stops.load(Ordering::Relaxed), 1);
    let frames_before = state.borrow().frames;
    config.music.sparkle_amount = 0.0;
    config.music.follow_beat = false;
    commands.send(Command::Apply(config.clone())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            state.changed().await.unwrap();
            let snapshot = state.borrow_and_update();
            if snapshot.frames > frames_before + 2 {
                let audio = snapshot.music.unwrap();
                assert_eq!(audio.sparkle_onsets, 0);
                assert!(audio.bpm.is_none());
                assert!(
                    snapshot.colors[0]
                        .iter()
                        .all(|c| *c == snapshot.colors[0][0])
                );
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(stops.load(Ordering::Relaxed), 1);
    config.mode = crate::music::SyncMode::Video;
    commands.send(Command::Start(config)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            state.changed().await.unwrap();
            if state.borrow().preview.is_some() {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(state.borrow().music.is_none());
    commands.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while stops.load(Ordering::Relaxed) != 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn music_source_changes_require_restart_but_effect_controls_do_not() {
    let mut a = Config {
        mode: crate::music::SyncMode::Music,
        ..Default::default()
    };
    let mut b = a.clone();
    b.music.sensitivity = 0.8;
    b.music.attack_ms = 50.0;
    b.music.color = [255, 10, 20];
    b.music.subbass_weight = 0.8;
    b.music.sparkle_amount = 0.9;
    b.music.follow_beat = false;
    b.music.effect = crate::music::MusicEffect::Alternating;
    assert!(same_routes(&a, &b));
    b.music.output_fps = 60;
    assert!(!same_routes(&a, &b));
    b.music.output_fps = 30;
    b.music.input = crate::music::AudioInput::Demo;
    assert!(!same_routes(&a, &b));
    a.music.input = b.music.input;
    b.music.demo_scenario = crate::music::DemoScenario::Hats;
    assert!(!same_routes(&a, &b));
    b.music.demo_scenario = a.music.demo_scenario;
    b.music.device = "other-sink".into();
    assert!(!same_routes(&a, &b));
}

#[test]
fn music_worker_rejects_stale_audio_and_drops_its_source() {
    struct SilentTimeouts(Arc<AtomicBool>);
    impl crate::music::AudioSource for SilentTimeouts {
        fn next_samples(&mut self, _: Duration) -> anyhow::Result<Option<Vec<f32>>> {
            Ok(None)
        }
    }
    impl Drop for SilentTimeouts {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let (_, config) = watch::channel(Arc::new(Config {
        mode: crate::music::SyncMode::Music,
        ..Default::default()
    }));
    let (frames, output) = watch::channel(None);
    let result = process_music(
        config,
        &AtomicBool::new(false),
        frames,
        77,
        Box::new(SilentTimeouts(dropped.clone())),
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("stopped delivering audio")
    );
    assert!(dropped.load(Ordering::Relaxed));
    let output = output.borrow().clone().unwrap();
    assert_eq!(output.session, 77);
    assert!(
        output.produced.elapsed() >= Duration::from_secs(2),
        "timeouts must not refresh output freshness"
    );
    assert!(output.colors.iter().flatten().all(|color| *color == [0; 3]));
}
fn solid_frame() -> Frame {
    Frame {
        width: 2,
        height: 2,
        pixels: vec![[255, 0, 0]; 4],

        taps: None,
    }
}

struct Letterboxed;
impl capture::CaptureSource for Letterboxed {
    fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
        Ok(Some(Frame {
            width: 160,
            height: 90,
            pixels: (0..160 * 90)
                .map(|i| {
                    if (10..80).contains(&(i / 160)) {
                        [255, 0, 0]
                    } else {
                        [0; 3]
                    }
                })
                .collect(),

            taps: None,
        }))
    }
    fn stop(&mut self) {}
}

#[tokio::test]
async fn live_black_bar_toggle_remaps_sampling_and_preview_together() {
    let factory: CaptureFactory = Arc::new(|_, _| Ok(Box::new(Letterboxed)));
    let (commands, rx) = mpsc::channel(8);
    let (snapshots, mut state) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run_with_factory(rx, snapshots, factory));
    let mut config = Config {
        black_bar_detection: true,
        brightness: 1.0,
        smoothing_ms: 0.0,
        ..Default::default()
    };
    config.soft_ambience.enabled = false;
    config.lights[0].zones = 1;
    config.lights[0].shape = core::Shape::Bulb {
        center: core::Point { x: 0.5, y: 0.03 },
        radius: 0.01,
    };
    commands.send(Command::Start(config.clone())).await.unwrap();
    for enabled in [true, false, true] {
        config.black_bar_detection = enabled;
        commands.send(Command::Apply(config.clone())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                state.changed().await.unwrap();
                let snapshot = state.borrow_and_update();
                let height = if enabled { 70 } else { 90 };
                let color = if enabled { [255, 0, 0] } else { [0; 3] };
                if snapshot
                    .preview
                    .as_ref()
                    .is_some_and(|frame| frame.height == height)
                    && snapshot.colors == vec![vec![color]]
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    commands.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}

struct AmbiencePicture;
impl capture::CaptureSource for AmbiencePicture {
    fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
        Ok(Some(Frame {
            width: 160,
            height: 90,
            pixels: (0..160 * 90)
                .map(|i| {
                    if !(10..80).contains(&(i / 160)) {
                        [0; 3]
                    } else if i % 160 < 48 {
                        [240, 30, 20]
                    } else {
                        [150; 3]
                    }
                })
                .collect(),

            taps: None,
        }))
    }
    fn stop(&mut self) {}
}

#[tokio::test]
async fn live_ambience_updates_use_cropped_picture_without_reopening_capture() {
    let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = opens.clone();
    let factory: CaptureFactory = Arc::new(move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(AmbiencePicture))
    });
    let (commands, rx) = mpsc::channel(8);
    let (snapshots, mut state) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run_with_factory(rx, snapshots, factory));
    let mut config = Config {
        black_bar_detection: true,
        brightness: 1.0,
        ..Default::default()
    };
    config.soft_ambience.enabled = true;
    config.lights[0].zones = 1;
    config.lights[0].shape = core::Shape::Strip {
        points: vec![
            core::Point { x: 0.0, y: 0.5 },
            core::Point { x: 1.0, y: 0.5 },
        ],
        radius: 1.0,
        reverse: false,
        segment_leds: vec![],
    };
    commands.send(Command::Start(config.clone())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            state.changed().await.unwrap();
            let snapshot = state.borrow_and_update();
            if snapshot
                .preview
                .as_ref()
                .is_some_and(|frame| frame.height == 70)
                && snapshot
                    .colors
                    .first()
                    .and_then(|z| z.first())
                    .is_some_and(|c| c[0] > 170 && c[0] > c[1] * 2)
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    config.soft_ambience.color_strength = 0.0;
    config.soft_ambience.response_ms = 500.0;
    commands.send(Command::Apply(config)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), state.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    commands.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}

#[test]
fn explicit_idle_refreshes_capture_but_silence_does_not_replay() {
    let start = Instant::now();
    let mut capture = CaptureProgress::new(start);
    let image = capture
        .observe(Some(solid_frame()), false, start)
        .unwrap()
        .unwrap();
    // Keep an unchanged desktop healthy well beyond the old 2s stall and
    // 5s resume thresholds, without inventing activity between notifications.
    for second in 1..=12 {
        let unchanged = capture
            .observe(None, true, start + Duration::from_secs(second))
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&image, &unchanged));
    }
    assert!(
        capture
            .observe(None, false, start + Duration::from_secs(13))
            .unwrap()
            .is_none()
    );
    assert!(
        capture
            .observe(None, false, start + Duration::from_secs(14))
            .unwrap()
            .is_none()
    );
    assert!(
        capture
            .observe(None, false, start + Duration::from_millis(14001))
            .unwrap_err()
            .to_string()
            .contains("stopped delivering")
    );
}

#[test]
fn idle_requires_an_image_and_cannot_hide_a_long_interruption() {
    let start = Instant::now();
    let mut capture = CaptureProgress::new(start);
    assert!(
        capture
            .observe(None, true, start + Duration::from_secs(1))
            .unwrap()
            .is_none()
    );
    let image = capture
        .observe(Some(solid_frame()), false, start + Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(image.pixels[0], [255, 0, 0]);
    for idle in [false, true] {
        let mut interrupted = CaptureProgress::new(start);
        interrupted
            .observe(Some(solid_frame()), false, start)
            .unwrap();
        let next = (!idle).then(solid_frame);
        assert!(
            interrupted
                .observe(next, idle, start + Duration::from_secs(6))
                .unwrap_err()
                .to_string()
                .contains("interruption")
        );
    }
}

struct FrameThenIdle {
    calls: usize,
    idle: bool,
    stopped: Arc<AtomicBool>,
}
impl capture::CaptureSource for FrameThenIdle {
    fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
        self.calls += 1;
        self.idle = self.calls == 2;
        match self.calls {
            1 => Ok(Some(solid_frame())),
            2 => Ok(None),
            _ => anyhow::bail!("mock stream disconnected after idle"),
        }
    }
    fn confirms_idle(&self) -> bool {
        self.idle
    }
    fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn idle_publishes_cached_colors_then_capture_failure_cleans_up() {
    let stopped = Arc::new(AtomicBool::new(false));
    let observed = stopped.clone();
    let factory: CaptureFactory = Arc::new(move |_, _| {
        Ok(Box::new(FrameThenIdle {
            calls: 0,
            idle: false,
            stopped: observed.clone(),
        }))
    });
    let mut accurate = Config {
        fps: 120,
        ..Config::default()
    };
    accurate.soft_ambience.enabled = false;
    let (_, config) = watch::channel(Arc::new(accurate));
    let (frames, received) = watch::channel(None);
    let error = process_capture(config, &AtomicBool::new(false), frames, 1, factory).unwrap_err();
    assert!(error.to_string().contains("disconnected after idle"));
    let frame = received.borrow().clone().unwrap();
    assert_eq!(
        frame.frames, 2,
        "explicit idle must refresh output freshness"
    );
    assert_eq!(frame.frame.pixels[0], [255, 0, 0]);
    assert!(
        frame
            .colors
            .iter()
            .flatten()
            .all(|rgb| rgb[0] > 0 && rgb[1] == 0 && rgb[2] == 0)
    );
    assert!(stopped.load(Ordering::SeqCst));
}

async fn await_state(rx: &mut watch::Receiver<Snapshot>, state: SessionState) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if rx.borrow_and_update().state == state {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn worker_spawn_failure_starts_no_outputs_and_coordinator_recovers() {
    let (commands, rx) = mpsc::channel(8);
    let (snapshots, mut state) = watch::channel(Snapshot::default());
    let first = AtomicBool::new(true);
    let spawner: WorkerSpawner = Arc::new(move |task| {
        if first.swap(false, Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "injected thread creation failure",
            ));
        }
        spawn_capture_thread(task)
    });
    let opened = Arc::new(AtomicBool::new(false));
    let observed = opened.clone();
    let factory: CaptureFactory = Arc::new(move |_, fps| {
        observed.store(true, Ordering::Relaxed);
        Ok(Box::new(capture::Synthetic::new(fps)))
    });
    let task = tokio::spawn(run_with_spawner(rx, snapshots, factory, spawner));
    let mut config = Config::default();
    config.lights[0].route = Route::Wled {
        host: "127.0.0.1:9".into(),
        device_id: "aabbccddeeff".into(),
        start: 0,
        count: 1,
    };
    commands.send(Command::Start(config)).await.unwrap();
    await_state(&mut state, SessionState::Error).await;
    assert!(state.borrow().message.contains("thread creation failure"));
    assert!(state.borrow().devices.is_empty());
    assert!(state.borrow().preview.is_none());
    assert!(!opened.load(Ordering::Relaxed));
    // The failed attempt must leave no worker or active configuration that
    // could reject this Start or publish frames from the failed session.
    commands
        .send(Command::Start(Config::default()))
        .await
        .unwrap();
    await_state(&mut state, SessionState::Running).await;
    assert!(opened.load(Ordering::Relaxed));
    assert!(state.borrow().devices.is_empty());
    commands.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn command_errors_distinguish_backpressure_from_stopped_engine() {
    let (commands, receiver) = mpsc::channel(1);
    let (_, snapshots) = watch::channel(Snapshot::default());
    let engine = Engine {
        commands,
        snapshots,
        task: tokio::spawn(async {}),
    };
    engine.send(Command::Stop).unwrap();
    assert!(
        engine
            .send(Command::Stop)
            .unwrap_err()
            .to_string()
            .contains("busy")
    );
    drop(receiver);
    let error = engine.send(Command::Stop).unwrap_err().to_string();
    assert!(error.contains("stopped"));
    assert!(error.contains("Restart"));
    engine.shutdown().await;
}

#[tokio::test]
async fn start_stop_and_latest_preview() {
    let (tx, rx) = mpsc::channel(8);
    let (st, mut sr) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run(rx, st));
    tx.send(Command::Start(Config::default())).await.unwrap();
    await_state(&mut sr, SessionState::Running).await;
    assert!(sr.borrow().frames > 0);
    let c = Config {
        brightness: 0.0,
        ..Default::default()
    };
    tx.send(Command::Apply(c)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            sr.changed().await.unwrap();
            if sr.borrow().colors.iter().flatten().all(|c| *c == [0; 3]) {
                break;
            }
        }
    })
    .await
    .unwrap();
    tx.send(Command::Pause).await.unwrap();
    await_state(&mut sr, SessionState::Paused).await;
    let paused = sr.borrow().frames;
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(sr.borrow().frames, paused);
    tx.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}
#[tokio::test]
async fn permission_failure_releases_session_and_recovers() {
    let (tx, rx) = mpsc::channel(8);
    let (st, mut sr) = watch::channel(Snapshot::default());
    let first = Arc::new(AtomicBool::new(true));
    let factory: CaptureFactory = Arc::new(move |_, fps| {
        if first.swap(false, Ordering::Relaxed) {
            anyhow::bail!("Permission denied (fake capture)");
        }
        Ok(Box::new(capture::Synthetic::new(fps)))
    });
    let task = tokio::spawn(run_with_factory(rx, st, factory));
    tx.send(Command::Start(Config::default())).await.unwrap();
    await_state(&mut sr, SessionState::Error).await;
    assert!(sr.borrow().message.contains("Permission denied"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(Command::Start(Config::default())).await.unwrap();
    await_state(&mut sr, SessionState::Running).await;
    tx.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}
#[test]
fn aliases_combine_into_one_physical_controller_frame() {
    let mut c = Config::default();
    c.lights[0].route = Route::Wled {
        host: "Desk.local".into(),
        device_id: "AA:BB:CC:DD:EE:FF".into(),
        start: 0,
        count: 2,
    };
    c.lights[0].zones = 1;
    let mut second = c.lights[0].clone();
    second.id = "second".into();
    second.route = Route::Wled {
        host: "192.168.1.50".into(),
        device_id: "aabbccddeeff".into(),
        start: 2,
        count: 2,
    };
    c.lights.push(second);
    c.validate().unwrap();
    assert_eq!(wled_routes(&c).len(), 1);
    let processed = Processed {
        session: 1,
        config: Arc::new(c),
        frame: Arc::new(capture::synthetic_frame(0.0)),
        colors: vec![vec![[255, 0, 0]], vec![[0, 0, 255]]],
        frames: 1,
        ms: 0.0,
        produced: Instant::now(),
        dark_zones: 0,
        sampled_colors: vec![],
        music: None,
        music_events: vec![],
        music_frame: None,
    };
    let (frames, _) = route_colors(&processed);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames["aabbccddeeff"],
        vec![[255, 0, 0], [255, 0, 0], [0, 0, 255], [0, 0, 255]]
    );
}

#[test]
fn routing_unequal_segments_keeps_physical_ranges_and_reverses_the_entire_strip() {
    let mut config = Config::default();
    config.lights[0].zones = 6;
    config.lights[0].shape = core::Shape::Strip {
        points: vec![
            core::Point { x: 0.0, y: 0.0 },
            core::Point { x: 1.0, y: 0.0 },
            core::Point { x: 1.0, y: 1.0 },
            core::Point { x: 0.0, y: 1.0 },
        ],
        radius: 0.04,
        segment_leds: vec![2, 5, 3],
        reverse: false,
    };
    config.lights[0].route = Route::Wled {
        host: "desk.local".into(),
        start: 3,
        count: 10,
        device_id: "abcdef".into(),
    };
    let colors: Vec<_> = (1..=6).map(|i| [i, 0, 0]).collect();
    let expected: Vec<_> = [1, 1, 2, 2, 3, 3, 4, 5, 5, 6]
        .into_iter()
        .map(|i| [i, 0, 0])
        .collect();
    for reverse in [false, true] {
        if let core::Shape::Strip {
            reverse: direction, ..
        } = &mut config.lights[0].shape
        {
            *direction = reverse;
        }
        config.validate().unwrap();
        let processed = Processed {
            session: 1,
            config: Arc::new(config.clone()),
            frame: Arc::new(capture::synthetic_frame(0.0)),
            colors: vec![if reverse {
                colors.iter().rev().copied().collect()
            } else {
                colors.clone()
            }],
            frames: 1,
            ms: 0.0,
            produced: Instant::now(),
            dark_zones: 0,
            sampled_colors: vec![],
            music: None,
            music_events: vec![],
            music_frame: None,
        };
        let (frames, _) = route_colors(&processed);
        let mut physical = vec![[0; 3]; 3];
        physical.extend(if reverse {
            expected.iter().rev().copied().collect::<Vec<_>>()
        } else {
            expected.clone()
        });
        assert_eq!(frames["abcdef"], physical);
    }
}
struct TrackedSource {
    stopped: Arc<AtomicBool>,
    fail: bool,
}
impl capture::CaptureSource for TrackedSource {
    fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
        anyhow::ensure!(!self.fail, "injected capture failure");
        Ok(Some(Frame {
            width: 2,
            height: 2,
            pixels: vec![[255, 0, 0]; 4],

            taps: None,
        }))
    }
    fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

#[test]
fn capture_failure_calls_trait_cleanup() {
    let stopped = Arc::new(AtomicBool::new(false));
    let observed = stopped.clone();
    let factory: CaptureFactory = Arc::new(move |_, _| {
        Ok(Box::new(TrackedSource {
            stopped: observed.clone(),
            fail: true,
        }))
    });
    let (_, config) = watch::channel(Arc::new(Config::default()));
    let (frames, _) = watch::channel(None);
    assert!(process_capture(config, &AtomicBool::new(false), frames, 1, factory).is_err());
    assert!(stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn low_fps_worker_stop_wakes_pacing_and_cleans_up() {
    let stopped = Arc::new(AtomicBool::new(false));
    let observed = stopped.clone();
    let factory: CaptureFactory = Arc::new(move |_, _| {
        Ok(Box::new(TrackedSource {
            stopped: observed.clone(),
            fail: false,
        }))
    });
    let (frames, mut received) = watch::channel(None);
    let (events, _) = mpsc::channel(8);
    let config = Config {
        fps: 1,
        ..Default::default()
    };
    let spawner: WorkerSpawner = Arc::new(spawn_capture_thread);
    let worker = worker(Arc::new(config), frames, events, 1, factory, &spawner).unwrap();
    tokio::time::timeout(Duration::from_secs(2), received.changed())
        .await
        .unwrap()
        .unwrap();
    let handle = worker.stop();
    tokio::time::timeout(Duration::from_millis(500), async {
        while !handle.is_finished() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    handle.join().unwrap();
    assert!(stopped.load(Ordering::SeqCst));
}

struct GatedSource {
    entered: std::sync::mpsc::SyncSender<()>,
    permit: std::sync::mpsc::Receiver<()>,
    count: usize,
}
impl capture::CaptureSource for GatedSource {
    fn next_frame(&mut self, timeout: Duration) -> anyhow::Result<Option<Frame>> {
        let _ = self.entered.try_send(());
        if self.permit.recv_timeout(timeout).is_err() {
            return Ok(None);
        }
        self.count += 1;
        let color = if self.count == 1 {
            [255, 0, 0]
        } else {
            [0, 0, 255]
        };
        Ok(Some(Frame {
            width: 2,
            height: 2,
            pixels: vec![color; 4],

            taps: None,
        }))
    }
    fn stop(&mut self) {}
}

#[tokio::test]
async fn apply_during_capture_keeps_frames_and_scalar_smoothing() {
    let (entered, waiting) = std::sync::mpsc::sync_channel(1);
    let (permit, permits) = std::sync::mpsc::channel();
    let source = std::sync::Mutex::new(Some(GatedSource {
        entered,
        permit: permits,
        count: 0,
    }));
    let factory: CaptureFactory =
        Arc::new(move |_, _| Ok(Box::new(source.lock().unwrap().take().unwrap())));
    let (commands, rx) = mpsc::channel(8);
    let (snapshots, mut state) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run_with_factory(rx, snapshots, factory));
    let mut config = Config {
        smoothing_ms: 5000.0,
        brightness: 1.0,
        ..Default::default()
    };
    config.soft_ambience.enabled = false;
    commands.send(Command::Start(config.clone())).await.unwrap();
    permit.send(()).unwrap();
    await_state(&mut state, SessionState::Running).await;
    assert_eq!(state.borrow().colors[0][0], [255, 0, 0]);
    // The next retrieval has already borrowed the old config. Apply new
    // tuning while it waits; its eventual frame must still be accepted.
    while waiting.try_recv().is_ok() {}
    tokio::time::timeout(Duration::from_secs(2), async {
        while waiting.try_recv().is_err() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    config.lights[0].name = "renamed while waiting".into();
    commands.send(Command::Apply(config.clone())).await.unwrap();
    // An observable command barrier avoids relying on scheduling sleeps.
    let mut invalid = config.clone();
    invalid.fps = 0;
    commands.send(Command::Apply(invalid)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            state.changed().await.unwrap();
            if state.borrow().message.contains("fps") || state.borrow().message.contains("FPS") {
                break;
            }
        }
    })
    .await
    .unwrap();
    permit.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.borrow().frames < 2 {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    permit.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.borrow().frames < 3 {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let color = state.borrow().colors[0][0];
    assert!(
        color[0] > color[2],
        "scalar update reset smoothing: {color:?}"
    );
    commands.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}

struct Interrupted;
impl capture::CaptureSource for Interrupted {
    fn next_frame(&mut self, _: Duration) -> anyhow::Result<Option<Frame>> {
        anyhow::bail!("Stream disconnected (fake capture)");
    }
    fn stop(&mut self) {}
}
#[tokio::test]
async fn lost_capture_enters_error_not_idle_or_running() {
    let (tx, rx) = mpsc::channel(8);
    let (st, mut sr) = watch::channel(Snapshot::default());
    let task = tokio::spawn(run_with_factory(
        rx,
        st,
        Arc::new(|_, _| Ok(Box::new(Interrupted))),
    ));
    tx.send(Command::Start(Config::default())).await.unwrap();
    await_state(&mut sr, SessionState::Error).await;
    assert!(sr.borrow().message.contains("disconnected"));
    tx.send(Command::Shutdown).await.unwrap();
    task.await.unwrap();
}
