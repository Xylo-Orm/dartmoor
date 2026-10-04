//! One owner for session configuration and routing; blocking capture never runs on Tokio.
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
            tasks.push(outputs::spawn_wled_device(
                host.clone(),
                rx,
                stop_rx.clone(),
                st,
                c.restore_wled_state,
                expected_id,
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
                stop_rx,
                st,
            ));
            statuses.push(("Home Assistant".into(), sr));
            Some(tx)
        } else {
            None
        };
        Self {
            wled,
            ha,
            stop,
            tasks,
            statuses,
        }
    }
    fn publish(&self, p: &Processed) {
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
                for i in 0..*count {
                    buf[start + i] = colors[i * colors.len() / count];
                }
            }
            Route::HomeAssistant { entity_id } => ha.push((entity_id.clone(), colors[0])),
            Route::Mock => {}
        }
    }
    (pixels, ha)
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
    let mut previous = vec![];
    let mut count = 0;
    let mut last_sample = Instant::now();
    let mut progress = CaptureProgress::new(last_sample);
    let interval = Duration::from_secs_f64(1.0 / active.fps as f64);
    let mut next = last_sample;
    while !stop.load(Ordering::Relaxed) {
        let updated = config.borrow_and_update().clone();
        if !Arc::ptr_eq(&active, &updated) {
            if !same_geometry(&active, &updated) {
                plan = None;
                previous.clear();
            }
            active = updated;
        }
        let frame = source.0.next_frame(Duration::from_millis(100))?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let processing_started = Instant::now();
        let Some(frame) = progress.observe(frame, source.0.confirms_idle(), processing_started)?
        else {
            continue;
        };
        if dimensions != (frame.width, frame.height) {
            dimensions = (frame.width, frame.height);
            plan = None;
        }
        if plan.is_none() {
            plan = Some(SamplingPlan::compile(
                &active.lights,
                dimensions.0,
                dimensions.1,
            )?);
        }
        let target = plan.as_ref().unwrap().sample(&frame);
        core::smooth(
            &mut previous,
            &target,
            last_sample.elapsed(),
            active.smoothing_ms,
        );
        last_sample = Instant::now();
        let colors = previous
            .iter()
            .map(|zones| {
                zones
                    .iter()
                    .map(|v| core::encode(*v, active.brightness))
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
            produced: processing_started,
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
            process_capture(receiver, &stop_rx, frames, session, opener)
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
    a.restore_wled_state == b.restore_wled_state
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
        Command(Option<Command>),
        Frame,
        Event(Option<Event>),
        Tick,
    }
    loop {
        let input = tokio::select! {
            cmd=commands.recv()=>Input::Command(cmd),
            _=frames.changed()=>Input::Frame,
            event=events.recv()=>Input::Event(event),
            _=tick.tick()=>Input::Tick,
        };
        match input {
            Input::Command(cmd) => {
                let shutdown = matches!(cmd, Some(Command::Shutdown) | None);
                match cmd {
                    Some(Command::Start(c)) => {
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
                            status.state = SessionState::RequestingPermission;
                            status.message =
                                "Waiting for screen capture permission / first frame".into();
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
                    status.message =
                        if matches!(p.config.source, crate::config::CaptureSelection::Synthetic) {
                            "SIMULATED desktop — no capture hardware verification".into()
                        } else {
                            "Synchronizing SDR/sRGB desktop".into()
                        };
                    status.preview = Some(p.frame.clone());
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
mod tests {
    use super::*;
    fn solid_frame() -> Frame {
        Frame {
            width: 2,
            height: 2,
            pixels: vec![[255, 0, 0]; 4],
        }
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
        let (_, config) = watch::channel(Arc::new(Config {
            fps: 120,
            ..Config::default()
        }));
        let (frames, received) = watch::channel(None);
        let error =
            process_capture(config, &AtomicBool::new(false), frames, 1, factory).unwrap_err();
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
        };
        let (frames, _) = route_colors(&processed);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames["aabbccddeeff"],
            vec![[255, 0, 0], [255, 0, 0], [0, 0, 255], [0, 0, 255]]
        );
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
                if state.borrow().message.contains("fps") || state.borrow().message.contains("FPS")
                {
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
}
