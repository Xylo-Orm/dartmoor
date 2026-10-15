//! One owner for session configuration and routing; blocking capture never runs on Tokio.
mod music;
use music::process_music;

use crate::{
    capture,
    config::Config,
    core::{self, Frame, Route, SamplingPlan},
    outputs::{self, HaFrame, OutputFrame},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    RequestingPermission,
    Running,
    Paused,
    Error,
}
#[derive(Clone)]
pub struct Snapshot {
    pub state: SessionState,
    pub message: String,
    pub preview: Option<Arc<Frame>>,
    pub colors: Vec<Vec<[u8; 3]>>,
    pub frames: u64,
    pub processing_ms: f64,
    pub devices: Vec<(String, String)>,
    pub mode: Option<crate::music::SyncMode>,
    pub dark_zones: usize,
    pub sampled_colors: Vec<Vec<[u8; 3]>>,
    pub music: Option<crate::music::Telemetry>,
    pub music_events: Vec<crate::music::AnalysisEvent>,
    pub music_frame: Option<crate::music::AnalysisFrame>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            state: SessionState::Idle,
            message: "Ready — select a source and add lights".into(),
            preview: None,
            colors: vec![],
            frames: 0,
            processing_ms: 0.0,
            devices: vec![],
            mode: None,
            dark_zones: 0,
            sampled_colors: vec![],
            music: None,
            music_events: vec![],
            music_frame: None,
        }
    }
}
pub enum Command {
    Start(Config),
    Apply(Config),
    Stop,
    Pause,
    Shutdown,
}
pub struct Engine {
    pub commands: mpsc::Sender<Command>,
    pub snapshots: watch::Receiver<Snapshot>,
    task: tokio::task::JoinHandle<()>,
}
impl Engine {
    pub fn new(runtime: &tokio::runtime::Runtime) -> Self {
        let (commands, rx) = mpsc::channel(8);
        let (tx, snapshots) = watch::channel(Snapshot::default());
        let task = runtime.spawn(run(rx, tx));
        Self {
            commands,
            snapshots,
            task,
        }
    }
    pub fn send(&self, cmd: Command) -> anyhow::Result<()> {
        self.commands.try_send(cmd).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                anyhow::anyhow!("Engine is busy. Retry this action.")
            }
            mpsc::error::TrySendError::Closed(_) => {
                anyhow::anyhow!("Engine has stopped. Restart the application.")
            }
        })
    }
    pub async fn shutdown(mut self) {
        let completed = tokio::time::timeout(Duration::from_secs(10), async {
            let _ = self.commands.send(Command::Shutdown).await;
            let _ = (&mut self.task).await;
        })
        .await;
        if completed.is_err() {
            self.task.abort();
            // Wait for cancellation so worker/output drop guards signal Stop
            // before the caller shuts down the runtime.
            let _ = (&mut self.task).await;
        }
    }
}
struct Processed {
    session: u64,
    config: Arc<Config>,
    frame: Arc<Frame>,
    colors: Vec<Vec<[u8; 3]>>,
    frames: u64,
    ms: f64,
    produced: Instant,
    dark_zones: usize,
    sampled_colors: Vec<Vec<[u8; 3]>>,
    music: Option<crate::music::Telemetry>,
    music_events: Vec<crate::music::AnalysisEvent>,
    music_frame: Option<crate::music::AnalysisFrame>,
}
enum Event {
    Failed(u64, String),
}
struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    config: watch::Sender<Arc<Config>>,
}
impl Worker {
    fn stop(mut self) -> thread::JoinHandle<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.as_ref().unwrap().thread().unpark();
        self.thread.take().unwrap()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
}

/// Every successful open is paired with stop, including processing errors and
/// backend panics. Contain a cleanup panic so unwinding cannot abort the process.
struct CaptureGuard(Box<dyn capture::CaptureSource>);
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.0.stop())).is_err() {
            tracing::warn!("Capture backend panicked during cleanup");
        }
    }
}
struct OutputGroup {
    wled: BTreeMap<String, watch::Sender<Option<OutputFrame>>>,
    ha: Option<watch::Sender<Option<HaFrame>>>,
    zigbee: BTreeMap<String, watch::Sender<Option<OutputFrame>>>,
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    statuses: Vec<(String, watch::Receiver<String>)>,
}
impl Drop for OutputGroup {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
impl OutputGroup {
    fn start(c: &Config) -> Self {
        let (stop, stop_rx) = watch::channel(false);
        let mut tasks = vec![];
        let mut statuses = vec![];
        let mut wled = BTreeMap::new();
        for (id, (host, expected_id)) in wled_routes(c) {
            let (tx, rx) = watch::channel(None);
            let (st, sr) = watch::channel("Connecting".into());
            tasks.push(outputs::spawn_wled_device_at_rate(
                host.clone(),
                rx,
                stop_rx.clone(),
                st,
                c.restore_wled_state,
                expected_id,
                if c.mode == crate::music::SyncMode::Music {
                    c.music.output_fps
                } else {
                    30
                },
            ));
            statuses.push((host, sr));
            wled.insert(id, tx);
        }
        let ha = if c
            .lights
            .iter()
            .any(|l| matches!(l.route, Route::HomeAssistant { .. }))
        {
            let (tx, rx) = watch::channel(None);
            let (st, sr) = watch::channel("Connecting (ambient)".into());
            tasks.push(outputs::spawn_ha(
                c.ha_url.clone(),
                c.ha_interval_ms,
                rx,
                stop_rx.clone(),
                st,
            ));
            statuses.push(("Home Assistant".into(), sr));
            Some(tx)
        } else {
            None
        };
        let mut zigbee = BTreeMap::new();
        for light in &c.lights {
            if let Route::Zigbee { ieee_address, .. } = &light.route {
                let (tx, rx) = watch::channel(None);
                let (st, sr) = watch::channel("Connecting (ambient)".into());
                tasks.push(crate::zigbee::spawn_output(
                    light.route.clone(),
                    rx,
                    stop_rx.clone(),
                    st,
                ));
                statuses.push((format!("Zigbee {ieee_address}"), sr));
                zigbee.insert(light.id.clone(), tx);
            }
        }
        Self {
            wled,
            ha,
            zigbee,
            stop,
            tasks,
            statuses,
        }
    }
    fn publish(&self, p: &Processed) {
        for (light, colors) in p.config.lights.iter().zip(&p.colors) {
            if let Some(tx) = self.zigbee.get(&light.id) {
                tx.send_replace(Some(OutputFrame {
                    colors: vec![colors[0]],
                    produced: p.produced,
                }));
            }
        }
        let (pixels, ha) = route_colors(p);
        for (host, colors) in pixels {
            if let Some(tx) = self.wled.get(&host) {
                tx.send_replace(Some(OutputFrame {
                    colors,
                    produced: p.produced,
                }));
            }
        }
        if let Some(tx) = &self.ha {
            tx.send_replace(Some(HaFrame {
                colors: ha,
                produced: p.produced,
            }));
        }
    }
    async fn shutdown(mut self) -> Vec<(String, String)> {
        self.stop.send_replace(true);
        futures_util::future::join_all(std::mem::take(&mut self.tasks).into_iter().map(
            |mut task| async move {
                if tokio::time::timeout(Duration::from_secs(8), &mut task)
                    .await
                    .is_err()
                {
                    task.abort();
                }
            },
        ))
        .await;
        std::mem::take(&mut self.statuses)
            .into_iter()
            .map(|(name, status)| (name, status.borrow().clone()))
            .collect()
    }
}
type CaptureFactory = Arc<
    dyn Fn(&crate::config::CaptureSelection, u32) -> anyhow::Result<Box<dyn capture::CaptureSource>>
        + Send
        + Sync,
>;
type WorkerTask = Box<dyn FnOnce() + Send>;
type WorkerSpawner =
    Arc<dyn Fn(WorkerTask) -> std::io::Result<thread::JoinHandle<()>> + Send + Sync>;

fn spawn_capture_thread(task: WorkerTask) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("capture-sampling".into())
        .spawn(task)
}
fn wled_routes(c: &Config) -> BTreeMap<String, (String, String)> {
    let mut routes = BTreeMap::new();
    for light in &c.lights {
        if let Route::Wled {
            host, device_id, ..
        } = &light.route
        {
            let identity = core::canonical_device_id(device_id);
            routes
                .entry(identity.clone())
                .or_insert_with(|| (core::canonical_host(host), identity));
        }
    }
    routes
}
type RgbFrame = Vec<[u8; 3]>;
type HaColors = Vec<(String, [u8; 3])>;
fn route_colors(p: &Processed) -> (BTreeMap<String, RgbFrame>, HaColors) {
    let mut pixels: BTreeMap<String, RgbFrame> = BTreeMap::new();
    let mut ha = vec![];
    for (l, colors) in p.config.lights.iter().zip(&p.colors) {
        match &l.route {
            Route::Wled {
                device_id,
                start,
                count,
                ..
            } => {
                let buf = pixels
                    .entry(core::canonical_device_id(device_id))
                    .or_default();
                buf.resize(buf.len().max(start + count), [0; 3]);
                for (i, zone) in core::led_zone_indices(l, *count).into_iter().enumerate() {
                    buf[start + i] = colors[zone];
                }
            }
            Route::HomeAssistant { entity_id } => ha.push((entity_id.clone(), colors[0])),
            Route::Zigbee { .. } | Route::Mock => {}
        }
    }
    (pixels, ha)
}
fn carry_video_colors(previous: &mut [Vec<[f32; 3]>], enabling_soft: bool, brightness: f32) {
    for zone in previous.iter_mut().flatten() {
        for channel in zone {
            *channel = if enabling_soft {
                *channel * brightness
            } else if brightness > 0.0 {
                (*channel / brightness).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
    }
}
/// Only an image or an explicit unchanged-image notification proves capture is
/// alive. A timeout never refreshes this deadline or republishes cached colors.
struct CaptureProgress {
    last_activity: Instant,
    image: Option<Arc<Frame>>,
}
impl CaptureProgress {
    fn new(now: Instant) -> Self {
        Self {
            last_activity: now,
            image: None,
        }
    }

    fn observe(
        &mut self,
        frame: Option<Frame>,
        idle: bool,
        now: Instant,
    ) -> anyhow::Result<Option<Arc<Frame>>> {
        let gap = now.saturating_duration_since(self.last_activity);
        if frame.is_none() && !idle {
            if gap > Duration::from_secs(2) {
                anyhow::bail!(
                    "Capture stopped delivering frames. Synchronization stopped for safety; select the source and Start again."
                );
            }
            return Ok(None);
        }
        if gap > Duration::from_secs(5) {
            anyhow::bail!("Desktop resumed after interruption. Start again to reacquire capture.");
        }
        self.last_activity = now;
        if let Some(frame) = frame {
            self.image = Some(Arc::new(frame));
        }
        // Idle before the first image proves liveness but cannot provide colors.
        Ok(self.image.clone())
    }
}

fn process_capture(
    mut config: watch::Receiver<Arc<Config>>,
    stop: &AtomicBool,
    frames: watch::Sender<Option<Arc<Processed>>>,
    session: u64,
    opener: CaptureFactory,
) -> anyhow::Result<()> {
    let mut active = config.borrow().clone();
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut source = CaptureGuard(opener(&active.source, active.fps)?);
    let mut plan = None;
    let mut dimensions = (0, 0);
    let mut black_bars = crate::black_bars::Detector::default();
    let mut darkness = crate::dark_zones::Detector::default();
    let mut crop_bounds = None;
    let mut previous: Vec<Vec<[f32; 3]>> = vec![];
    let mut ambience = crate::video::Processor::default();
    let mut count = 0;
    let mut last_sample = Instant::now();
    let mut progress = CaptureProgress::new(last_sample);
    let mut cropped_image: Option<Arc<Frame>> = None;
    let interval = Duration::from_secs_f64(1.0 / active.fps as f64);
    let mut next = last_sample;
    while !stop.load(Ordering::Relaxed) {
        let updated = config.borrow_and_update().clone();
        if !Arc::ptr_eq(&active, &updated) {
            if !same_geometry(&active, &updated) {
                plan = None;
                previous.clear();
                ambience.reset();
                darkness.reset();
            }
            if active.soft_ambience.enabled != updated.soft_ambience.enabled {
                ambience.reset();
                // Accurate stores unscaled linear RGB; Soft stores RGB with
                // master brightness applied. Preserve the displayed color
                // across the mode switch in the destination representation.
                carry_video_colors(
                    &mut previous,
                    updated.soft_ambience.enabled,
                    active.brightness,
                );
            }
            if active
                .lights
                .iter()
                .zip(&updated.lights)
                .any(|(a, b)| std::mem::discriminant(&a.route) != std::mem::discriminant(&b.route))
            {
                ambience.reset();
            }
            if active.black_bar_detection != updated.black_bar_detection
                || active.dark_zones != updated.dark_zones
            {
                darkness.reset();
                if active.black_bar_detection != updated.black_bar_detection {
                    cropped_image = None;
                }
                if active.dark_zones != updated.dark_zones {
                    ambience.reset_evidence();
                }
            }
            active = updated;
        }
        let frame = source
            .0
            .next_frame(interval.min(Duration::from_millis(100)))?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let processing_started = Instant::now();
        let received = progress.observe(frame, source.0.confirms_idle(), processing_started)?;
        let frame = if let Some(frame) = received {
            let cropped = black_bars.apply(frame, active.black_bar_detection, processing_started);
            cropped_image = Some(cropped.clone());
            cropped
        } else if active.soft_ambience.enabled && ambience.is_animating() {
            // Continue an in-flight fade at the configured render cadence even
            // when a variable-rate compositor sends no new image. The source
            // timestamp below remains unchanged, so this cannot mask a stall.
            let Some(cropped) = cropped_image.clone() else {
                continue;
            };
            cropped
        } else {
            continue;
        };
        let bounds = black_bars.crop_bounds();
        if crop_bounds != Some(bounds) {
            darkness.reset();
            ambience.reset();
            previous.clear();
            crop_bounds = Some(bounds);
        }
        if dimensions != (frame.width, frame.height) {
            dimensions = (frame.width, frame.height);
            plan = None;
            ambience.reset();
            previous.clear();
            darkness.reset();
        }
        if plan.is_none() {
            let compiled = SamplingPlan::compile_with_cancel(
                &active.lights,
                dimensions.0,
                dimensions.1,
                || stop.load(Ordering::Relaxed),
            );
            if stop.load(Ordering::Relaxed) {
                break;
            }
            plan = Some(compiled?);
        }
        let plan = plan.as_ref().unwrap();
        anyhow::ensure!(frame.valid(), "invalid cropped capture frame");
        let soft = active.soft_ambience.enabled;
        let accurate = if active.dark_zones.enabled || !soft {
            Some(plan.sample(&frame))
        } else {
            None
        };
        if soft {
            ambience.process(
                crate::video::ProcessInput {
                    plan,
                    frame: &frame,
                    routes: &active.lights,
                    settings: active.soft_ambience,
                    brightness: active.brightness,
                    fps: active.fps,
                    now: processing_started,
                },
                &mut previous,
            )?;
        } else {
            let target = accurate.as_ref().expect("accurate mode samples");
            core::smooth(
                &mut previous,
                target,
                last_sample.elapsed(),
                active.smoothing_ms,
            );
        }
        let sampled_colors = accurate
            .as_ref()
            .map(|colors| {
                colors
                    .iter()
                    .map(|zones| {
                        zones
                            .iter()
                            .map(|color| core::encode(*color, 1.0))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let dark_zones = accurate.as_ref().map_or(0, |accurate| {
            darkness.apply(
                accurate,
                &mut previous,
                active.dark_zones,
                processing_started,
            )
        });
        if soft {
            for (light, zones) in previous.iter().enumerate() {
                for zone in 0..zones.len() {
                    if darkness.is_off(light, zone) {
                        ambience.force_dark(light, zone);
                    }
                }
            }
        }
        last_sample = Instant::now();
        let colors = previous
            .iter()
            .map(|zones| {
                zones
                    .iter()
                    .map(|v| core::encode(*v, if soft { 1.0 } else { active.brightness }))
                    .collect()
            })
            .collect();
        count += 1;
        let ms = processing_started.elapsed().as_secs_f64() * 1000.0;
        frames.send_replace(Some(Arc::new(Processed {
            session,
            config: active.clone(),
            frame,
            colors,
            frames: count,
            ms,
            produced: progress.last_activity,
            dark_zones,
            sampled_colors,
            music: None,
            music_events: vec![],
            music_frame: None,
        })));
        next += interval;
        let now = Instant::now();
        if next > now {
            // Stop unparks this thread. Recheck the deadline after a spurious
            // wakeup, without delaying shutdown until a low-FPS interval ends.
            while !stop.load(Ordering::Relaxed) {
                let remaining = next.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                thread::park_timeout(remaining);
            }
        } else {
            next = now;
        }
    }
    Ok(())
}

fn worker(
    c: Arc<Config>,
    frames: watch::Sender<Option<Arc<Processed>>>,
    events: mpsc::Sender<Event>,
    session: u64,
    opener: CaptureFactory,
    spawner: &WorkerSpawner,
) -> anyhow::Result<Worker> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_rx = stop.clone();
    let (config, receiver) = watch::channel(c);
    let thread = spawner(Box::new(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let active = receiver.borrow().clone();
            if active.mode == crate::music::SyncMode::Music {
                let source = crate::music::open(&active.music)?;
                process_music(receiver, &stop_rx, frames, session, source)
            } else {
                process_capture(receiver, &stop_rx, frames, session, opener)
            }
        }));
        let error = match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(_) => Some("Capture backend failed. Cancel the permission dialog, check recording permissions and Start again.".into()),
        };
        if let Some(error) = error {
            let _ = events.blocking_send(Event::Failed(session, error));
        }
    }))
    .map_err(|error| anyhow::anyhow!("Could not start capture worker: {error}. Close unused applications and Start again."))?;
    Ok(Worker {
        stop,
        thread: Some(thread),
        config,
    })
}
fn same_geometry(a: &Config, b: &Config) -> bool {
    a.lights.len() == b.lights.len()
        && a.lights
            .iter()
            .zip(&b.lights)
            .all(|(a, b)| a.id == b.id && a.shape == b.shape && a.zones == b.zones)
}
fn same_routes(a: &Config, b: &Config) -> bool {
    a.mode == b.mode
        && (a.mode != crate::music::SyncMode::Music
            || (a.music.input == b.music.input
                && a.music.device == b.music.device
                && (a.music.input != crate::music::AudioInput::Demo
                    || a.music.demo_scenario == b.music.demo_scenario)
                && a.music.output_fps == b.music.output_fps))
        && a.restore_wled_state == b.restore_wled_state
        && a.source == b.source
        && a.fps == b.fps
        && a.ha_url == b.ha_url
        && a.ha_interval_ms == b.ha_interval_ms
        && a.lights.len() == b.lights.len()
        && a.lights
            .iter()
            .zip(&b.lights)
            .all(|(a, b)| a.id == b.id && a.route == b.route)
}
async fn run(commands: mpsc::Receiver<Command>, snapshots: watch::Sender<Snapshot>) {
    run_with_factory(commands, snapshots, Arc::new(capture::open)).await;
}

async fn run_with_factory(
    commands: mpsc::Receiver<Command>,
    snapshots: watch::Sender<Snapshot>,
    opener: CaptureFactory,
) {
    run_with_spawner(commands, snapshots, opener, Arc::new(spawn_capture_thread)).await;
}

async fn run_with_spawner(
    mut commands: mpsc::Receiver<Command>,
    snapshots: watch::Sender<Snapshot>,
    opener: CaptureFactory,
    spawner: WorkerSpawner,
) {
    let (frames_tx, mut frames) = watch::channel(None::<Arc<Processed>>);
    let (events_tx, mut events) = mpsc::channel(8);
    let mut worker_state: Option<Worker> = None;
    let mut outputs: Option<OutputGroup> = None;
    let mut active: Option<Arc<Config>> = None;
    let mut session = 0u64;
    let mut retired: Vec<thread::JoinHandle<()>> = vec![];
    let mut status = Snapshot::default();
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    enum Input {
        Command(Box<Option<Command>>),
        Frame,
        Event(Option<Event>),
        Tick,
    }
    loop {
        let input = tokio::select! {
            cmd=commands.recv()=>Input::Command(Box::new(cmd)),
            _=frames.changed()=>Input::Frame,
            event=events.recv()=>Input::Event(event),
            _=tick.tick()=>Input::Tick,
        };
        match input {
            Input::Command(cmd) => {
                let cmd = *cmd;
                let shutdown = matches!(cmd, Some(Command::Shutdown) | None);
                match cmd {
                    Some(Command::Start(c)) => {
                        // Explicit Start in the other workspace transfers ownership;
                        // navigating tabs never sends this command.
                        if active
                            .as_ref()
                            .is_some_and(|current| current.mode != c.mode)
                            && c.validate().is_ok()
                        {
                            if let Some(w) = worker_state.take() {
                                retired.push(w.stop());
                            }
                            if let Some(o) = outputs.take() {
                                status.devices = o.shutdown().await;
                            }
                            active = None;
                            frames_tx.send_replace(None);
                            let deadline = Instant::now() + Duration::from_millis(500);
                            while retired.iter().any(|t| !t.is_finished())
                                && Instant::now() < deadline
                            {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            status.state = SessionState::Idle;
                            status.mode = None;
                        }
                        retired.retain(|t| !t.is_finished());
                        if worker_state.is_some() || !retired.is_empty() {
                            status.message="Capture is still active or awaiting a portal response. Stop / cancel that dialog first.".into();
                        } else if let Err(e) = c.validate() {
                            status.state = SessionState::Error;
                            status.message = e.to_string();
                        } else {
                            let c = Arc::new(c);
                            frames_tx.send_replace(None);
                            status.preview = None;
                            status.frames = 0;
                            status.colors.clear();
                            status.music = None;
                            status.music_events.clear();
                            status.music_frame = None;
                            status.dark_zones = 0;
                            status.sampled_colors.clear();
                            status.mode = Some(c.mode);
                            status.state = SessionState::RequestingPermission;
                            status.message = if c.mode == crate::music::SyncMode::Music {
                                "Connecting to playback audio / first samples".into()
                            } else {
                                "Waiting for screen capture permission / first frame".into()
                            };
                            session += 1;
                            match worker(
                                c.clone(),
                                frames_tx.clone(),
                                events_tx.clone(),
                                session,
                                opener.clone(),
                                &spawner,
                            ) {
                                Ok(worker) => {
                                    // Do not acquire outputs until thread creation
                                    // succeeds; a spawn failure owns no resources.
                                    worker_state = Some(worker);
                                    outputs = Some(OutputGroup::start(&c));
                                    active = Some(c);
                                }
                                Err(error) => {
                                    status.state = SessionState::Error;
                                    status.message = error.to_string();
                                    status.devices.clear();
                                }
                            }
                        }
                    }
                    Some(Command::Apply(c)) => {
                        if let Err(e) = c.validate() {
                            status.message = e.to_string();
                        } else if let (Some(current), Some(w)) = (&active, &worker_state) {
                            if same_routes(current, &c) {
                                let c = Arc::new(c);
                                w.config.send_replace(c.clone());
                                active = Some(c);
                            } else {
                                status.message="Stop before changing sources, devices, mappings or output rates.".into();
                            }
                        }
                    }
                    Some(Command::Stop | Command::Pause | Command::Shutdown) | None => {
                        if let Some(w) = worker_state.take() {
                            retired.push(w.stop());
                        }
                        if let Some(o) = outputs.take() {
                            status.devices = o.shutdown().await;
                        }
                        active = None;
                        status.mode = None;
                        frames_tx.send_replace(None);
                        status.state = if matches!(cmd, Some(Command::Pause)) {
                            SessionState::Paused
                        } else {
                            SessionState::Idle
                        };
                        status.message="Synchronization stopped; streaming release attempted (see device status)".into();
                    }
                }
                snapshots.send_replace(status.clone());
                if shutdown {
                    break;
                }
            }
            Input::Frame => {
                let p = frames.borrow_and_update().clone();
                if let Some(p) = p
                    && active.is_some()
                    && p.session == session
                {
                    if let Some(o) = &outputs {
                        o.publish(&p);
                    }
                    status.state = SessionState::Running;
                    status.message = if p.config.mode == crate::music::SyncMode::Music {
                        if p.config.music.input == crate::music::AudioInput::Demo {
                            "SIMULATED music pulses — no audio captured".into()
                        } else {
                            "Music · Pulse · system playback".into()
                        }
                    } else if matches!(p.config.source, crate::config::CaptureSelection::Synthetic)
                    {
                        "SIMULATED desktop — no capture hardware verification".into()
                    } else {
                        "Synchronizing SDR/sRGB desktop".into()
                    };
                    status.mode = Some(p.config.mode);
                    status.preview =
                        (p.config.mode == crate::music::SyncMode::Video).then(|| p.frame.clone());
                    status.dark_zones = p.dark_zones;
                    status.sampled_colors = p.sampled_colors.clone();
                    status.music = p.music;
                    status.music_events = p.music_events.clone();
                    status.music_frame = p.music_frame;
                    status.colors = p.colors.clone();
                    status.frames = p.frames;
                    status.processing_ms = p.ms;
                    snapshots.send_replace(status.clone());
                }
            }
            Input::Event(Some(Event::Failed(event_session, message))) => {
                if event_session != session || active.is_none() {
                    continue;
                }
                if let Some(w) = worker_state.take() {
                    retired.push(w.stop());
                }
                if let Some(o) = outputs.take() {
                    status.devices = o.shutdown().await;
                }
                active = None;
                status.state = SessionState::Error;
                status.mode = None;
                status.music = None;
                status.music_events.clear();
                status.music_frame = None;
                status.preview = None;
                status.message = message;
                snapshots.send_replace(status.clone());
            }
            Input::Tick => {
                if let Some(o) = &outputs {
                    status.devices = o
                        .statuses
                        .iter()
                        .map(|(name, rx)| (name.clone(), rx.borrow().clone()))
                        .collect();
                    snapshots.send_replace(status.clone());
                }
            }
            Input::Event(None) => {}
        }
    }
    // A synchronous OS portal prompt may outlive Stop. Refuse another worker until
    // it exits; output cleanup is complete and process exit cannot leave a daemon.
    for t in retired {
        if t.is_finished() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests;
