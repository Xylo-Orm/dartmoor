//! Network adapters. Watch channels keep only the newest frame; a capture stall
//! cannot leave a queue of old colors to replay. WLED live mode is released on
//! stop/stall/error; prior-state restoration is opt-in and guarded by an observed
//! state comparison. Home Assistant keeps the last applied color.
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::watch,
    task::{AbortHandle, JoinHandle},
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

const STALE_AFTER: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const DDP_PAYLOAD: usize = 1440;
const MAX_LEDS: usize = 65_536;
const MAX_WLED_JSON_BYTES: usize = 1024 * 1024;
// get_states includes non-light entities and their attributes. Give it more
// room than WLED, retaining Tungstenite's normal read/write buffer sizes.
const MAX_HA_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_HA_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_HA_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HA_EVENTS: usize = 256;
const MAX_HA_LIGHTS: usize = 10_000;
const MAX_HA_ENTITY_BYTES: usize = 256;
const MAX_HA_NAME_BYTES: usize = 1024;
const MAX_HA_MODES: usize = 32;
const MAX_HA_MODE_BYTES: usize = 64;
const BLOCKING_JSON_THRESHOLD: usize = 64 * 1024;
type HaSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[cfg(test)]
tokio::task_local! {
    // Scoped to a test's actual stream task, never shared across parallel tests.
    static JSON_PARSER_QUEUED: tokio::sync::mpsc::UnboundedSender<()>;
}

// A JoinHandle detaches its task when dropped. Connection helpers and queued
// parsers must instead stop when their owner is canceled or unwinds. A running
// blocking parser cannot be interrupted; its input size is independently capped.
struct AbortTaskOnDrop(AbortHandle);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone, Debug)]
pub struct OutputFrame {
    pub colors: Vec<[u8; 3]>,
    pub produced: Instant,
}

#[derive(Clone, Debug)]
pub struct HaFrame {
    pub colors: Vec<(String, [u8; 3])>,
    pub produced: Instant,
}

#[derive(Clone, Debug)]
pub struct WledInfo {
    pub name: String,
    pub device_id: String,
    pub led_count: usize,
    /// Exclusive segment end indices, matching WLED's JSON API.
    pub segments: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct HaLight {
    pub entity_id: String,
    pub name: String,
    pub color_modes: Vec<String>,
    pub available: bool,
}

fn base_url(input: &str) -> Result<Url> {
    let text = if input.contains("://") {
        input.to_owned()
    } else {
        format!("http://{input}")
    };
    let mut url = Url::parse(&text).map_err(|_| anyhow::anyhow!("Invalid device URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("Device URL must use HTTP or HTTPS and contain a host");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("Device URL must not contain credentials, query parameters, or a fragment");
    }
    // Retain a reverse-proxy prefix while making subsequent joins predictable.
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| anyhow::anyhow!("Could not initialize HTTP transport"))
}

async fn parse_response_json<T>(bytes: T, invalid: &'static str) -> Result<Value>
where
    T: AsRef<[u8]> + Send + 'static,
{
    let parsed = if bytes.as_ref().len() >= BLOCKING_JSON_THRESHOLD {
        // Move the owned Vec/Utf8Bytes; do not copy a full response to offload it.
        let parser = tokio::task::spawn_blocking(move || serde_json::from_slice(bytes.as_ref()));
        let _parser_guard = AbortTaskOnDrop(parser.abort_handle());
        #[cfg(test)]
        let _ = JSON_PARSER_QUEUED.try_with(|queued| queued.send(()));
        parser
            .await
            .map_err(|_| anyhow::anyhow!("Response JSON parser failed"))?
    } else {
        serde_json::from_slice(bytes.as_ref())
    };
    parsed.map_err(|_| anyhow::anyhow!(invalid))
}

async fn wled_json(response: reqwest::Response, invalid: &'static str) -> Result<Value> {
    bounded_wled_json(response, MAX_WLED_JSON_BYTES, invalid).await
}

async fn bounded_wled_json(
    mut response: reqwest::Response,
    limit: usize,
    invalid: &'static str,
) -> Result<Value> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        bail!("WLED response exceeds the supported JSON size limit");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!(invalid))?
    {
        if chunk.len() > limit.saturating_sub(body.len()) {
            bail!("WLED response exceeds the supported JSON size limit");
        }
        body.extend_from_slice(&chunk);
    }
    parse_response_json(body, invalid).await
}

pub async fn inspect_wled(host: &str) -> Result<WledInfo> {
    let base = base_url(host)?;
    let url = base.join("json").context("Invalid WLED endpoint")?;
    let response = http_client()?
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("WLED HTTP connection failed"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED HTTP request was rejected"))?;
    let value = wled_json(response, "WLED returned invalid JSON").await?;
    parse_wled(&value)
}

fn parse_wled(value: &Value) -> Result<WledInfo> {
    if value.pointer("/state/lor").and_then(Value::as_u64) != Some(0) {
        bail!(
            "Disable WLED realtime override (lor=0) before streaming; its state must be readable"
        );
    }
    let info = value
        .get("info")
        .context("WLED response has no device info")?;
    let led_count = info
        .pointer("/leds/count")
        .and_then(Value::as_u64)
        .context("WLED response has no LED count")? as usize;
    if led_count == 0 || led_count > MAX_LEDS {
        bail!("WLED LED count is out of range");
    }
    let device_id = info["mac"]
        .as_str()
        .filter(|v| !v.is_empty())
        .context("WLED response has no MAC identity")?
        .to_owned();
    let segments = value
        .pointer("/state/seg")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|seg| {
            let start = seg["start"].as_u64()? as usize;
            let end = seg["stop"].as_u64()? as usize;
            (start < end && end <= led_count).then_some((start, end))
        })
        .collect();
    Ok(WledInfo {
        name: info["name"].as_str().unwrap_or("WLED").to_owned(),
        device_id,
        led_count,
        segments,
    })
}

/// Reset an inherited indefinite JSON-live lease or promptly end our DDP lease.
/// Never send live:true: WLED 0.15.1 turns that into a UINT32_MAX timeout.
async fn reset_live(client: &reqwest::Client, base: &Url) -> Result<()> {
    let response = client
        .post(base.join("json/state").context("Invalid WLED endpoint")?)
        .json(&json!({"live": false}))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("WLED live-mode request failed"))?;
    let response = response
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED rejected live-mode request"))?;
    let reply = wled_json(response, "WLED returned invalid live-mode acknowledgement").await?;
    if reply["success"] != true {
        bail!("WLED did not acknowledge realtime reset");
    }
    Ok(())
}

async fn finite_wled_timeout(client: &reqwest::Client, base: &Url) -> Result<Duration> {
    let response = client
        .get(base.join("json/cfg").context("Invalid WLED endpoint")?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Could not verify WLED realtime timeout"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED realtime configuration is unavailable"))?;
    let cfg = wled_json(response, "WLED returned invalid configuration JSON").await?;
    finite_timeout_from_config(&cfg)
}

fn finite_timeout_from_config(cfg: &Value) -> Result<Duration> {
    // WLED0.15.1 cfg.cpp stores timeout in 100ms units. 65000ms is a
    // special indefinite sentinel in realtimeLock(). Refuse missing/unsafe
    // values; never change persistent device settings to fix them.
    let units = cfg
        .pointer("/if/live/timeout")
        .and_then(Value::as_u64)
        .context("WLED realtime timeout cannot be verified; check device Sync settings")?;
    if !(1..=100).contains(&units) {
        bail!(
            "Set WLED realtime timeout to 0.1–10 seconds in device Sync settings before streaming"
        );
    }
    if cfg.pointer("/if/live/en").and_then(Value::as_bool) != Some(true) {
        bail!("Enable WLED realtime UDP reception in device Sync settings before streaming");
    }
    // Verified against WLED v0.15.1 cfg.cpp serialization and udp.cpp's
    // setRealtimePixel(). Require physical whole-controller indices, rather
    // than guessing defaults when this undocumented schema changes.
    if cfg.pointer("/if/live/mso").and_then(Value::as_bool) != Some(false) {
        bail!("Disable WLED main-segment-only realtime mode in Sync settings before streaming");
    }
    if cfg.pointer("/if/live/rlm").and_then(Value::as_bool) != Some(false) {
        bail!("Disable WLED realtime LED maps in Sync settings before streaming");
    }
    if cfg.pointer("/if/live/offset").and_then(Value::as_i64) != Some(0) {
        bail!("Set WLED realtime LED offset to zero in Sync settings before streaming");
    }
    // WLED applies the DMX address even to DDP, and its optional sequence
    // filter assumes at most four packets per frame. Our frames may be larger.
    if cfg.pointer("/if/live/dmx/addr").and_then(Value::as_u64) != Some(1) {
        bail!("Set WLED DMX start address to 1 in Sync settings for physical LED mapping");
    }
    if cfg.pointer("/if/live/dmx/seqskip").and_then(Value::as_bool) != Some(false) {
        bail!("Disable WLED Skip out-of-sequence packets in Sync settings before streaming");
    }
    Ok(Duration::from_millis(units * 100))
}

async fn wled_state(client: &reqwest::Client, base: &Url) -> Result<Value> {
    let response = client
        .get(base.join("json/state").context("Invalid WLED endpoint")?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("WLED state request failed"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED state request rejected"))?;
    let state = wled_json(response, "WLED returned invalid state JSON").await?;
    if !state["on"].is_boolean() || !state["bri"].is_u64() || !state["seg"].is_array() {
        bail!("WLED returned incomplete state; restoration is unavailable");
    }
    Ok(state)
}

fn restorable_state(state: &Value) -> Value {
    let mut result = json!({});
    for key in ["on", "bri", "transition", "mainseg", "seg"] {
        if let Some(value) = state.get(key) {
            result[key] = value.clone();
        }
    }
    result
}

async fn release_wled(client: &reqwest::Client, base: &Url, saved: &Option<Value>) -> (bool, bool) {
    // WLED has no compare-and-swap API. Compare immediately before release;
    // restoration is a user-selected, best-effort operation, never a guarantee.
    let restore = if let Some(previous) = saved {
        wled_state(client, base)
            .await
            .ok()
            .filter(|current| restorable_state(current) == *previous)
            .map(|_| previous)
    } else {
        None
    };
    let released = reset_live(client, base).await.is_ok();
    let restored = if released {
        if let Some(previous) = restore {
            match base.join("json/state") {
                Ok(url) => match client.post(url).json(previous).send().await {
                    Ok(response) if response.status().is_success() => wled_json(
                        response,
                        "WLED returned invalid restoration acknowledgement",
                    )
                    .await
                    .is_ok_and(|reply| reply["success"] == true),
                    _ => false,
                },
                Err(_) => false,
            }
        } else {
            false
        }
    } else {
        false
    };
    (released, restored)
}

fn ddp_packets(colors: &[[u8; 3]], sequence: &mut u8) -> Result<Vec<Vec<u8>>> {
    if colors.is_empty() || colors.len() > MAX_LEDS {
        bail!("WLED frame LED count is out of range");
    }
    let rgb: Vec<u8> = colors.iter().flatten().copied().collect();
    let count = rgb.len().div_ceil(DDP_PAYLOAD);
    let mut result = Vec::with_capacity(count);
    for (index, chunk) in rgb.chunks(DDP_PAYLOAD).enumerate() {
        *sequence = (*sequence % 15) + 1;
        let mut packet = Vec::with_capacity(10 + chunk.len());
        // RGB24 (RGB, 8 bits/channel) = 0x0b; destination 1 = display.
        // See WLED's src/dependencies/e131/ESPAsyncE131.h DDP definitions.
        packet.extend_from_slice(&[0x40 | u8::from(index + 1 == count), *sequence, 0x0b, 1]);
        packet.extend_from_slice(&((index * DDP_PAYLOAD) as u32).to_be_bytes());
        packet.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        packet.extend_from_slice(chunk);
        result.push(packet);
    }
    Ok(result)
}

async fn send_ddp(socket: &UdpSocket, colors: &[[u8; 3]], sequence: &mut u8) -> Result<()> {
    for packet in ddp_packets(colors, sequence)? {
        socket
            .send(&packet)
            .await
            .map_err(|_| anyhow::anyhow!("WLED UDP send failed"))?;
    }
    Ok(())
}

/// UDP sends are not delivery acknowledgements. Periodic HTTP checks report
/// control-plane reachability separately from unconfirmed DDP delivery.
pub fn spawn_wled(
    host: String,
    pixels: watch::Receiver<Option<OutputFrame>>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
) -> JoinHandle<()> {
    spawn_wled_with_restore(host, pixels, stop, status, false)
}

/// Opt-in state restoration is attempted only if the observable state still
/// matches the original state read before acquisition. This controls the entire
/// WLED controller; unassigned LEDs are blanked when DDP realtime starts.
pub fn spawn_wled_with_restore(
    host: String,
    pixels: watch::Receiver<Option<OutputFrame>>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
    restore_previous: bool,
) -> JoinHandle<()> {
    spawn_wled_device(host, pixels, stop, status, restore_previous, String::new())
}

/// Bind a saved route to the physical MAC identity rather than trusting a
/// potentially reassigned DHCP address. Empty expected_id supports manual tests.
pub fn spawn_wled_device(
    host: String,
    pixels: watch::Receiver<Option<OutputFrame>>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
    restore_previous: bool,
    expected_id: String,
) -> JoinHandle<()> {
    spawn_wled_destination(
        host,
        pixels,
        stop,
        status,
        restore_previous,
        expected_id,
        4048,
    )
}

// Internal destination injection keeps mocked sessions on ephemeral loopback
// ports. The public production adapter always uses WLED's fixed DDP port 4048.
fn spawn_wled_destination(
    host: String,
    mut pixels: watch::Receiver<Option<OutputFrame>>,
    mut stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
    restore_previous: bool,
    expected_id: String,
    ddp_port: u16,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut retry_seconds = 1;
        while !*stop.borrow() && stop.has_changed().is_ok() && pixels.has_changed().is_ok() {
            let (result, streamed) = wled_session(
                &host,
                &mut pixels,
                &mut stop,
                &status,
                restore_previous,
                &expected_id,
                ddp_port,
            )
            .await;
            let delay = next_wled_retry_delay(&mut retry_seconds, streamed);
            match result {
                Ok(()) => return,
                Err(e) => {
                    if *stop.borrow()
                        || stop.has_changed().is_err()
                        || pixels.has_changed().is_err()
                    {
                        status.send_replace(format!("WLED: stopped; {e}"));
                        return;
                    }
                    status.send_replace(format!("WLED: {e}; retrying in {delay}s"));
                }
            }
            tokio::select! {
                _ = stop.changed() => break,
                _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
            }
        }
        status.send_replace("WLED: stopped".into());
    })
}

fn next_wled_retry_delay(retry_seconds: &mut u64, streamed: bool) -> u64 {
    if streamed {
        *retry_seconds = 1;
    }
    let delay = *retry_seconds;
    *retry_seconds = (delay * 2).min(30);
    delay
}

fn literal_wled_address(base: &Url, port: u16) -> Option<SocketAddr> {
    let hostname = base.host_str()?;
    // Url::host_str includes square brackets around IPv6 literals, whereas
    // the tuple form of lookup_host treats those brackets as hostname text.
    let hostname = hostname
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(hostname);
    hostname
        .parse::<IpAddr>()
        .ok()
        .map(|ip| SocketAddr::new(ip, port))
}

/// Transport and verified controller settings for one connection attempt.
struct WledConnection {
    base: Url,
    client: reqwest::Client,
    socket: UdpSocket,
    led_count: usize,
    realtime_timeout: Duration,
}

async fn connect_wled(host: &str, expected_id: &str, ddp_port: u16) -> Result<WledConnection> {
    let base = base_url(host)?;
    let client = http_client()?;
    let info = inspect_wled(host).await?;
    if !expected_id.is_empty() && normalized_mac(expected_id) != normalized_mac(&info.device_id) {
        bail!("WLED identity differs from saved controller; reconnect or add the device again");
    }
    let realtime_timeout = finite_wled_timeout(&client, &base).await?;
    let address = resolve_wled_address(&base, ddp_port).await?;
    let socket = UdpSocket::bind(if address.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    })
    .await
    .map_err(|_| anyhow::anyhow!("Could not create WLED UDP socket"))?;
    socket
        .connect(address)
        .await
        .map_err(|_| anyhow::anyhow!("WLED UDP connection failed"))?;
    Ok(WledConnection {
        base,
        client,
        socket,
        led_count: info.led_count,
        realtime_timeout,
    })
}

async fn resolve_wled_address(base: &Url, ddp_port: u16) -> Result<SocketAddr> {
    if let Some(address) = literal_wled_address(base, ddp_port) {
        return Ok(address);
    }
    let hostname = base.host_str().context("WLED host missing")?;
    timeout(
        REQUEST_TIMEOUT,
        tokio::net::lookup_host((hostname, ddp_port)),
    )
    .await
    .map_err(|_| anyhow::anyhow!("WLED hostname lookup timed out"))?
    .map_err(|_| anyhow::anyhow!("WLED hostname lookup failed"))?
    .next()
    .context("WLED hostname resolved to no addresses")
}

fn spawn_wled_health(
    connection: &WledConnection,
) -> Result<(JoinHandle<()>, watch::Receiver<bool>)> {
    let (health_tx, health_rx) = watch::channel(true);
    let client = connection.client.clone();
    let url = connection
        .base
        .join("json/info")
        .context("Invalid WLED endpoint")?;
    let task = tokio::spawn(async move {
        let mut tick = interval(HEALTH_INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = health_tx.closed() => break,
                _ = tick.tick() => {}
            }
            let reachable = tokio::select! {
                biased;
                _ = health_tx.closed() => break,
                response = client.get(url.clone()).send() => {
                    response.is_ok_and(|response| response.status().is_success())
                }
            };
            if health_tx.send(reachable).is_err() {
                break;
            }
        }
    });
    Ok((task, health_rx))
}

#[derive(Default)]
struct WledLease {
    attempted: bool,
    active: bool,
    saved: Option<Value>,
    sequence: u8,
    streamed: bool,
}

impl WledLease {
    async fn acquire(&mut self, connection: &WledConnection, restore_previous: bool) -> Result<()> {
        // Whole-controller DDP does not change the stored on/bri/segment state.
        // Preserve the pre-acquisition baseline, including changes during the
        // first frame; never adopt a manual change as our state to restore.
        self.saved = if restore_previous {
            wled_state(&connection.client, &connection.base)
                .await
                .ok()
                .map(|v| restorable_state(&v))
        } else {
            None
        };
        self.attempted = true; // Cleanup even if reset's reply is lost.
        // Explicit Start clears an inherited indefinite JSON-live lease.
        reset_live(&connection.client, &connection.base).await?;
        self.active = true;
        Ok(())
    }

    async fn release(&self, connection: &WledConnection) -> (bool, bool) {
        if self.attempted {
            release_wled(&connection.client, &connection.base, &self.saved).await
        } else {
            (true, false)
        }
    }

    async fn send_latest(
        &mut self,
        connection: &WledConnection,
        pixels: &mut watch::Receiver<Option<OutputFrame>>,
        status: &watch::Sender<String>,
        restore_previous: bool,
        health: &watch::Receiver<bool>,
    ) -> Result<()> {
        let frame = pixels.borrow_and_update().clone();
        let fresh = frame.filter(|f| f.produced.elapsed() < STALE_AFTER && !f.colors.is_empty());
        let Some(frame) = fresh else {
            if self.attempted {
                let (released, _) = self.release(connection).await;
                if !released {
                    bail!("WLED live release failed");
                }
                self.attempted = false;
                self.active = false;
                self.saved = None;
            }
            status.send_replace("WLED: waiting for fresh capture; live mode released".into());
            return Ok(());
        };
        validate_wled_frame(&frame, connection.led_count)?;
        if !self.active {
            self.acquire(connection, restore_previous).await?;
        }
        // Acquisition may take time; read the mailbox again before sending.
        let newest = pixels.borrow_and_update().clone();
        if let Some(newest) = newest.filter(|f| f.produced.elapsed() < STALE_AFTER) {
            validate_wled_frame(&newest, connection.led_count)?;
            send_ddp(&connection.socket, &newest.colors, &mut self.sequence).await?;
            self.streamed = true; // Local recovery, not UDP delivery confirmation.
            status.send_replace(format!(
                "WLED: HTTP {}; DDP unconfirmed; device timeout {}ms",
                if *health.borrow() {
                    "reachable"
                } else {
                    "unreachable"
                },
                connection.realtime_timeout.as_millis(),
            ));
        }
        Ok(())
    }
}

fn validate_wled_frame(frame: &OutputFrame, led_count: usize) -> Result<()> {
    if frame.colors.len() > led_count {
        bail!("WLED frame exceeds the controller LED count");
    }
    Ok(())
}

async fn wled_session(
    host: &str,
    pixels: &mut watch::Receiver<Option<OutputFrame>>,
    stop: &mut watch::Receiver<bool>,
    status: &watch::Sender<String>,
    restore_previous: bool,
    expected_id: &str,
    ddp_port: u16,
) -> (Result<()>, bool) {
    let mut streamed = false;
    let result = async {
        let connection = tokio::select! {
            _ = stop.changed() => return Ok(()),
            setup = connect_wled(host, expected_id, ddp_port) => setup?,
        };
        let (health_task, health_rx) = spawn_wled_health(&connection)?;
        let _health_guard = AbortTaskOnDrop(health_task.abort_handle());
        let mut lease = WledLease::default();
        let mut tick = interval(Duration::from_millis(33));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut cancellation = stop.clone();
        let streaming = async {
            loop {
                if *stop.borrow() {
                    break;
                }
                tokio::select! {
                    biased;
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            break;
                        }
                    }
                    _ = tick.tick() => {
                        if pixels.has_changed().is_err() {
                            break;
                        }
                        lease.send_latest(
                            &connection, pixels, status, restore_previous, &health_rx,
                        ).await?;
                    }
                }
            }
            Ok(())
        };
        let outcome: Result<()> = tokio::select! {
            _ = cancellation.changed() => Ok(()),
            outcome = streaming => outcome,
        };
        streamed = lease.streamed;
        health_task.abort();
        let _ = health_task.await;
        // Release remains outside the fallible/cancellable loop. Acquire and
        // send errors share this path; callers signal Stop and join, not abort.
        let (released, restored) = lease.release(&connection).await;
        if let Err(error) = outcome {
            bail!(
                "{error}; live release {}",
                if released { "complete" } else { "failed" }
            );
        }
        status.send_replace(
            if restored {
                "WLED: stopped; live released and prior state restored"
            } else if released {
                "WLED: stopped; live mode released"
            } else {
                "WLED: stopped; live release failed"
            }
            .into(),
        );
        if !released {
            bail!("WLED live release failed");
        }
        Ok(())
    }
    .await;
    (result, streamed)
}

fn normalized_mac(mac: &str) -> String {
    mac.replace([':', '-'], "").to_ascii_lowercase()
}

fn token_entry(url: &str) -> Result<keyring::Entry> {
    let account = base_url(url)?.to_string();
    keyring::Entry::new("lumen-desktop.home-assistant", &account)
        .map_err(|_| anyhow::anyhow!("OS credential store is unavailable"))
}

/// The token is stored only in the OS credential store, never in app settings.
fn save_token(url: &str, token: &str) -> Result<()> {
    if token.trim().is_empty() {
        bail!("Home Assistant token is empty");
    }
    token_entry(url)?
        .set_password(token.trim())
        .map_err(|_| anyhow::anyhow!("Could not save token to the OS credential store"))
}

fn load_token(url: &str) -> Result<String> {
    token_entry(url)?.get_password().map_err(|_| {
        anyhow::anyhow!("Home Assistant token is not available in the OS credential store")
    })
}

// One process-wide slot for reads AND saves. The blocking closure owns the
// lease, so timing out or dropping its async caller cannot admit another OS
// operation while a locked credential service still holds the first call.
static CREDENTIAL_BUSY: LazyLock<Arc<AtomicBool>> =
    LazyLock::new(|| Arc::new(AtomicBool::new(false)));

struct CredentialLease(Arc<AtomicBool>);

impl Drop for CredentialLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// OS credential APIs may block on a locked credential store. This bounded
/// lookup runs off the async executor, and remains gated even after timeout.
pub async fn load_token_async(url: &str) -> Result<String> {
    let url = base_url(url)?.to_string();
    credential_operation(move || load_token(&url)).await
}

/// A bounded save shares the lookup gate; a timed-out OS save may still finish
/// later. Its lease is released only when that native operation returns.
pub async fn save_token_async(url: &str, token: &str) -> Result<()> {
    let url = base_url(url)?.to_string();
    let token = token.trim().to_owned();
    if token.is_empty() {
        bail!("Home Assistant token is empty");
    }
    credential_operation(move || save_token(&url, &token)).await
}

async fn credential_operation<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    credential_operation_on(CREDENTIAL_BUSY.clone(), operation).await
}

async fn credential_operation_on<T, F>(gate: Arc<AtomicBool>, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        bail!(
            "An OS credential operation is still pending; unlock or dismiss the credential-store prompt before trying again"
        );
    }
    let lease = CredentialLease(gate);
    timeout(REQUEST_TIMEOUT, tokio::task::spawn_blocking(move || {
        let _lease = lease;
        operation()
    })).await
        .map_err(|_| anyhow::anyhow!("OS credential operation timed out; it may still complete; unlock or dismiss the credential-store prompt before trying again"))?
        .map_err(|_| anyhow::anyhow!("OS credential operation failed"))?
}

async fn ws_read(socket: &mut HaSocket) -> Result<Value> {
    Ok(ws_read_sized(socket).await?.0)
}

async fn ws_read_sized(socket: &mut HaSocket) -> Result<(Value, usize)> {
    loop {
        let message = socket.next().await;
        if let Some(value) = decode_ha_message(socket, message).await? {
            return Ok(value);
        }
    }
}

async fn decode_ha_message(
    socket: &mut HaSocket,
    message: Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>,
) -> Result<Option<(Value, usize)>> {
    match message {
        Some(Ok(Message::Text(text))) => {
            let wire_bytes = text.len();
            let value = parse_response_json(text, "Home Assistant returned invalid JSON").await?;
            Ok(Some((value, wire_bytes)))
        }
        Some(Ok(Message::Ping(data))) => {
            socket
                .send(Message::Pong(data))
                .await
                .map_err(|_| anyhow::anyhow!("Home Assistant connection closed"))?;
            Ok(None)
        }
        Some(Err(tokio_tungstenite::tungstenite::Error::Capacity(_))) => {
            bail!("Home Assistant response exceeds the supported WebSocket size limit");
        }
        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
            bail!("Home Assistant connection closed")
        }
        _ => Ok(None),
    }
}

fn ha_socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_HA_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_HA_FRAME_BYTES))
}

async fn ws_send(socket: &mut HaSocket, value: Value) -> Result<()> {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .map_err(|_| anyhow::anyhow!("Home Assistant send failed"))
}

async fn ha_connect(url: &str, token: &str) -> Result<HaSocket> {
    let mut endpoint = base_url(url)?
        .join("api/websocket")
        .context("Invalid Home Assistant endpoint")?;
    let scheme = if endpoint.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("Invalid Home Assistant endpoint"))?;
    timeout(REQUEST_TIMEOUT, async {
        let (mut socket, _) =
            connect_async_with_config(endpoint.as_str(), Some(ha_socket_config()), false)
                .await
                .map_err(|_| anyhow::anyhow!("Home Assistant WebSocket connection failed"))?;
        if ws_read(&mut socket).await?["type"] != "auth_required" {
            bail!("Unexpected Home Assistant authentication handshake");
        }
        ws_send(&mut socket, json!({"type":"auth", "access_token":token})).await?;
        if ws_read(&mut socket).await?["type"] != "auth_ok" {
            bail!("Home Assistant authentication rejected");
        }
        Ok(socket)
    })
    .await
    .map_err(|_| anyhow::anyhow!("Home Assistant authentication timed out"))?
}

async fn ha_request(socket: &mut HaSocket, id: u64, mut request: Value) -> Result<Value> {
    ha_request_events(socket, id, request.take(), &mut HaEvents::default()).await
}

#[derive(Default)]
struct HaEvents {
    values: Vec<Value>,
    wire_bytes: usize,
}

impl HaEvents {
    fn push(&mut self, event: Value, wire_bytes: usize, byte_limit: usize) -> Result<()> {
        if self.values.len() >= MAX_HA_EVENTS
            || wire_bytes > byte_limit.saturating_sub(self.wire_bytes)
        {
            bail!("Home Assistant event backlog exceeded the supported count or byte limit");
        }
        self.values.push(event);
        self.wire_bytes += wire_bytes;
        Ok(())
    }
}

async fn ha_request_events(
    socket: &mut HaSocket,
    id: u64,
    request: Value,
    events: &mut HaEvents,
) -> Result<Value> {
    ha_request_events_bounded(socket, id, request, events, MAX_HA_EVENT_BYTES).await
}

async fn ha_request_events_bounded(
    socket: &mut HaSocket,
    id: u64,
    mut request: Value,
    events: &mut HaEvents,
    byte_limit: usize,
) -> Result<Value> {
    request["id"] = json!(id);
    timeout(REQUEST_TIMEOUT, async {
        ws_send(socket, request).await?;
        loop {
            let (mut reply, wire_bytes) = ws_read_sized(socket).await?;
            if reply["type"] == "event" {
                events.push(reply, wire_bytes, byte_limit)?;
                continue;
            }
            if reply["id"].as_u64() == Some(id) && reply["type"] == "result" {
                if reply["success"] != true {
                    bail!("Home Assistant rejected request");
                }
                return Ok(reply["result"].take());
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Home Assistant acknowledgement timed out"))?
}

fn parse_ha_light(state: &Value) -> Result<Option<HaLight>> {
    let Some(entity_id) = state["entity_id"]
        .as_str()
        .filter(|id| id.starts_with("light."))
    else {
        return Ok(None);
    };
    let name = state
        .pointer("/attributes/friendly_name")
        .and_then(Value::as_str)
        .unwrap_or(entity_id);
    let modes = state
        .pointer("/attributes/supported_color_modes")
        .and_then(Value::as_array);
    if entity_id.len() > MAX_HA_ENTITY_BYTES
        || name.len() > MAX_HA_NAME_BYTES
        || modes.is_some_and(|modes| modes.len() > MAX_HA_MODES)
        || modes.is_some_and(|modes| {
            modes
                .iter()
                .filter_map(Value::as_str)
                .any(|mode| mode.len() > MAX_HA_MODE_BYTES)
        })
    {
        bail!("Home Assistant light metadata exceeds the supported field size limits");
    }
    Ok(Some(HaLight {
        entity_id: entity_id.to_owned(),
        name: name.to_owned(),
        color_modes: modes
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        available: !matches!(
            state["state"].as_str(),
            Some("unavailable" | "unknown") | None
        ),
    }))
}

fn parse_ha_lights(states: &Value) -> Result<Vec<HaLight>> {
    let array = states
        .as_array()
        .context("Home Assistant returned invalid states")?;
    let mut lights = Vec::new();
    for state in array {
        if let Some(light) = parse_ha_light(state)? {
            if lights.len() >= MAX_HA_LIGHTS {
                bail!("Home Assistant discovery exceeds the supported 10000-light limit");
            }
            lights.push(light);
        }
    }
    Ok(lights)
}

pub async fn discover_ha(url: &str) -> Result<Vec<HaLight>> {
    let token = load_token_async(url).await?;
    let mut socket = ha_connect(url, &token).await?;
    let result = ha_request(&mut socket, 1, json!({"type":"get_states"})).await;
    let _ = timeout(Duration::from_millis(500), socket.close(None)).await;
    parse_ha_lights(&result?)
}

fn service_data(light: &HaLight, rgb: [u8; 3]) -> Option<Value> {
    if !light.available {
        return None;
    }
    let supports = |mode: &str| light.color_modes.iter().any(|m| m == mode);
    let mut data = json!({"entity_id":light.entity_id, "transition":0});
    let brightness = *rgb.iter().max().unwrap_or(&0);
    let normalized = if brightness == 0 {
        [0; 3]
    } else {
        rgb.map(|v| ((u16::from(v) * 255) / u16::from(brightness)) as u8)
    };
    if supports("rgb") {
        // HA brightness is independent of chromaticity; normalize RGB so a
        // dim captured color is not attenuated a second time by brightness.
        data["rgb_color"] = json!(normalized);
    } else if supports("rgbw") {
        data["rgbw_color"] = json!([normalized[0], normalized[1], normalized[2], 0]);
    } else if supports("rgbww") {
        data["rgbww_color"] = json!([normalized[0], normalized[1], normalized[2], 0, 0]);
    } else if supports("hs") {
        data["hs_color"] = json!(rgb_to_hs(rgb));
    } else if supports("xy") {
        data["xy_color"] = json!(rgb_to_xy(rgb));
    } else {
        return None;
    }
    data["brightness"] = json!(brightness);
    Some(data)
}

fn rgb_to_hs(rgb: [u8; 3]) -> [f64; 2] {
    let [r, g, b] = rgb.map(|v| f64::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    [hue, if max == 0.0 { 0.0 } else { 100.0 * delta / max }]
}

fn rgb_to_xy(rgb: [u8; 3]) -> [f64; 2] {
    let [r, g, b] = rgb.map(|v| {
        let v = f64::from(v) / 255.0;
        if v > 0.04045 {
            ((v + 0.055) / 1.055).powf(2.4)
        } else {
            v / 12.92
        }
    });
    let x = r * 0.664511 + g * 0.154324 + b * 0.162028;
    let y = r * 0.283881 + g * 0.668433 + b * 0.047685;
    let z = r * 0.000088 + g * 0.072310 + b * 0.986039;
    let sum = x + y + z;
    if sum == 0.0 {
        [0.0, 0.0]
    } else {
        [x / sum, y / sum]
    }
}

/// At most one service call per interval (minimum 500ms), with round-robin
/// fairness between entities. Acknowledgements are bounded and unchanged RGB
/// values are coalesced. No commands are sent for stale capture or white-only lights.
pub fn spawn_ha(
    url: String,
    interval_ms: u64,
    mut targets: watch::Receiver<Option<HaFrame>>,
    mut stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if *stop.borrow() {
            status.send_replace("Home Assistant: stopped".into());
            return;
        }
        let loaded = tokio::select! {
            _ = stop.changed() => { status.send_replace("Home Assistant: stopped".into()); return; }
            loaded = load_token_async(&url) => loaded,
        };
        let token = match loaded {
            Ok(token) => token,
            Err(e) => {
                status.send_replace(format!("Home Assistant: {e}"));
                return;
            }
        };
        while !*stop.borrow() {
            let connected = tokio::select! {
                biased;
                _ = stop.changed() => break,
                result = ha_connect(&url, &token) => result,
            };
            let result = match connected {
                Ok(mut socket) => {
                    let result =
                        ha_stream(&mut socket, interval_ms, &mut targets, &mut stop, &status).await;
                    let _ = timeout(Duration::from_millis(500), socket.close(None)).await;
                    result
                }
                Err(e) => Err(e),
            };
            if *stop.borrow() || stop.has_changed().is_err() || targets.has_changed().is_err() {
                break;
            }
            if let Err(e) = result {
                status.send_replace(format!("Home Assistant: {e}; retrying"));
            }
            tokio::select! {
                _ = stop.changed() => break,
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
            }
        }
        status.send_replace("Home Assistant: stopped".into());
    })
}

async fn ha_stream(
    socket: &mut HaSocket,
    interval_ms: u64,
    targets: &mut watch::Receiver<Option<HaFrame>>,
    stop: &mut watch::Receiver<bool>,
    status: &watch::Sender<String>,
) -> Result<()> {
    let mut id = 1;
    let states = tokio::select! {
        _ = stop.changed() => return Ok(()),
        states = ha_request(socket, id, json!({"type":"get_states"})) => states?,
    };
    let mut lights: HashMap<String, HaLight> = parse_ha_lights(&states)?
        .into_iter()
        .map(|l| (l.entity_id.clone(), l))
        .collect();
    let mut last_sent = HashMap::<String, [u8; 3]>::new();
    let mut events = HaEvents::default();
    id += 1;
    tokio::select! {
        _ = stop.changed() => return Ok(()),
        subscribed = ha_request_events(socket, id, json!({"type":"subscribe_events", "event_type":"state_changed"}), &mut events) => { subscribed?; }
    }
    apply_ha_events(&mut events, &mut lights, &mut last_sent)?;
    let mut cursor = 0usize;
    let mut refreshed = Instant::now();
    let mut tick = interval(Duration::from_millis(interval_ms.clamp(500, 60_000)));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() || targets.has_changed().is_err() {
            return Ok(());
        }
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            message = socket.next() => {
                // Receiving a message commits it to this iteration. A timer
                // must not drop an in-progress parse and lose a state event.
                // Stop may cancel parsing because it ends this connection.
                let decoded = tokio::select! {
                    biased;
                    _ = stop.changed() => return Ok(()),
                    decoded = decode_ha_message(socket, message) => decoded?,
                };
                if let Some((event, _)) = decoded {
                    update_ha_event(&event, &mut lights, &mut last_sent)?;
                }
            }
            _ = tick.tick() => {
                if refreshed.elapsed() > Duration::from_secs(30) {
                    tokio::select! {
                        _ = stop.changed() => return Ok(()),
                        refreshed = refresh_ha_lights(socket, &mut id, &mut lights, &mut last_sent, &mut events) => { refreshed?; }
                    }
                    refreshed = Instant::now();
                }
                let frame = targets.borrow_and_update().clone();
                let Some(frame) = frame.filter(|f| f.produced.elapsed() < STALE_AFTER) else {
                    status.send_replace("Home Assistant: waiting for fresh capture".into());
                    continue;
                };
                let selected = select_ha_command(&frame, &lights, &last_sent, &mut cursor);
                if let Some(command) = selected {
                    id += 1;
                    tokio::select! {
                        _ = stop.changed() => return Ok(()),
                        reply = send_ha_command(socket, id, &command, &mut events) => { reply?; }
                    }
                    last_sent.insert(command.entity_id, command.rgb);
                    // Begin the next interval after this acknowledgement. A
                    // slow reply cannot create a burst of overdue ticks.
                    tick.reset();
                    apply_ha_events(&mut events, &mut lights, &mut last_sent)?;
                    status.send_replace("Home Assistant: service acknowledged (maximum 2 calls/s)".into());
                } else {
                    let supported = frame.colors.iter().filter(|(id, rgb)| {
                        lights.get(id).is_some_and(|light| service_data(light, *rgb).is_some())
                    }).count();
                    status.send_replace(format!("Home Assistant: {supported} color targets ready; unchanged colors coalesced"));
                }
            }
        }
    }
}

async fn refresh_ha_lights(
    socket: &mut HaSocket,
    id: &mut u64,
    lights: &mut HashMap<String, HaLight>,
    last_sent: &mut HashMap<String, [u8; 3]>,
    events: &mut HaEvents,
) -> Result<()> {
    *id += 1;
    let states = ha_request_events(socket, *id, json!({"type":"get_states"}), events).await?;
    reconcile_ha_lights(parse_ha_lights(&states)?, lights, last_sent);
    apply_ha_events(events, lights, last_sent)?;
    Ok(())
}

fn apply_ha_events(
    events: &mut HaEvents,
    lights: &mut HashMap<String, HaLight>,
    last_sent: &mut HashMap<String, [u8; 3]>,
) -> Result<()> {
    events.wire_bytes = 0;
    for event in events.values.drain(..) {
        update_ha_event(&event, lights, last_sent)?;
    }
    Ok(())
}

struct HaCommand {
    entity_id: String,
    rgb: [u8; 3],
    data: Value,
}

fn select_ha_command(
    frame: &HaFrame,
    lights: &HashMap<String, HaLight>,
    last_sent: &HashMap<String, [u8; 3]>,
    cursor: &mut usize,
) -> Option<HaCommand> {
    for offset in 0..frame.colors.len() {
        let index = (*cursor + offset) % frame.colors.len();
        let (entity_id, rgb) = &frame.colors[index];
        if last_sent.get(entity_id) == Some(rgb) {
            continue;
        }
        let Some(light) = lights.get(entity_id) else {
            continue;
        };
        if let Some(data) = service_data(light, *rgb) {
            *cursor = index + 1;
            return Some(HaCommand {
                entity_id: entity_id.clone(),
                rgb: *rgb,
                data,
            });
        }
    }
    None
}

async fn send_ha_command(
    socket: &mut HaSocket,
    id: u64,
    command: &HaCommand,
    events: &mut HaEvents,
) -> Result<()> {
    ha_request_events(socket, id, json!({
        "type":"call_service", "domain":"light", "service":"turn_on", "service_data":command.data,
    }), events).await?;
    Ok(())
}

fn reconcile_ha_lights(
    refreshed: Vec<HaLight>,
    lights: &mut HashMap<String, HaLight>,
    last_sent: &mut HashMap<String, [u8; 3]>,
) {
    let refreshed: HashMap<_, _> = refreshed
        .into_iter()
        .map(|light| (light.entity_id.clone(), light))
        .collect();
    last_sent.retain(|id, _| matches!((lights.get(id), refreshed.get(id)),
        (Some(old), Some(new)) if old.available == new.available && old.color_modes == new.color_modes));
    *lights = refreshed;
}

fn update_ha_event(
    event: &Value,
    lights: &mut HashMap<String, HaLight>,
    last_sent: &mut HashMap<String, [u8; 3]>,
) -> Result<()> {
    if event.pointer("/event/event_type").and_then(Value::as_str) != Some("state_changed") {
        return Ok(());
    }
    let Some(entity_id) = event
        .pointer("/event/data/entity_id")
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("light."))
    else {
        return Ok(());
    };
    if entity_id.len() > MAX_HA_ENTITY_BYTES {
        bail!("Home Assistant light metadata exceeds the supported field size limits");
    }
    let state = &event["event"]["data"]["new_state"];
    if state.is_null() {
        lights.remove(entity_id);
        last_sent.remove(entity_id);
    } else {
        let light =
            parse_ha_light(state)?.context("Home Assistant returned invalid light state")?;
        if light.entity_id != entity_id {
            bail!("Home Assistant returned mismatched light state identity");
        }
        if !lights.contains_key(entity_id) && lights.len() >= MAX_HA_LIGHTS {
            bail!("Home Assistant discovery exceeds the supported 10000-light limit");
        }
        let changed = lights.get(entity_id).is_none_or(|old| {
            old.available != light.available || old.color_modes != light.color_modes
        });
        if changed {
            last_sent.remove(entity_id);
        }
        lights.insert(entity_id.to_owned(), light);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ha_stream_commits_large_state_events_across_ticks_and_stop_interrupts_parsing() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for (availability, mode, stop_while_held) in [
                ("unavailable", "rgb", false),
                ("on", "onoff", false),
                ("unavailable", "rgb", true),
            ] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let (permit, permitted) = tokio::sync::oneshot::channel();
                let (calls, mut received_calls) = tokio::sync::mpsc::unbounded_channel();
                let server = tokio::spawn(async move {
                    let mut socket = accept_fake_ha(&listener).await;
                    for _ in 0..2 {
                        let request: Value = serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                        let result = if request["type"] == "get_states" {
                            json!([{"entity_id":"light.desk", "state":"on", "attributes":{"supported_color_modes":["rgb"]}}])
                        } else {
                            assert_eq!(request["type"], "subscribe_events");
                            Value::Null
                        };
                        socket.send(Message::Text(json!({"type":"result", "id":request["id"], "success":true,"result":result}).to_string().into())).await.unwrap();
                    }
                    let baseline: Value = serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
                    assert_eq!(baseline["type"], "call_service");
                    calls.send(baseline.clone()).unwrap();
                    socket.send(Message::Text(json!({"type":"result", "id":baseline["id"], "success":true, "result":null}).to_string().into())).await.unwrap();
                    permitted.await.unwrap();
                    let mut event = json!({"type":"event", "event":{"event_type":"state_changed", "data":{
                        "entity_id":"light.desk", "new_state":{"entity_id":"light.desk", "state":availability,
                            "attributes":{"supported_color_modes":[mode]}}}}}).to_string();
                    event.push_str(&" ".repeat(BLOCKING_JSON_THRESHOLD));
                    socket.send(Message::Text(event.into())).await.unwrap();
                    while let Some(Ok(Message::Text(text))) = socket.next().await {
                        let request: Value = serde_json::from_str(&text).unwrap();
                        if request["type"] == "call_service" {
                            calls.send(request.clone()).unwrap();
                        }
                        socket.send(Message::Text(json!({"type":"result", "id":request["id"], "success":true, "result":null}).to_string().into())).await.unwrap();
                    }
                });
                let mut socket = ha_connect(&url, "fake-token").await.unwrap();
                let (release, held) = tokio::sync::oneshot::channel();
                let (started, ready) = tokio::sync::oneshot::channel();
                let blocking = tokio::task::spawn_blocking(move || {
                    let _ = started.send(());
                    let _ = held.blocking_recv();
                });
                ready.await.unwrap();
                let (targets, mut frames) = watch::channel(Some(HaFrame {
                    colors: vec![("light.desk".into(), [20, 40, 80])],
                    produced: Instant::now(),
                }));
                let (stop, mut stopping) = watch::channel(false);
                let (status, mut statuses) = watch::channel(String::new());
                let (queued, mut parser_queued) = tokio::sync::mpsc::unbounded_channel();
                let stream = tokio::spawn(JSON_PARSER_QUEUED.scope(queued, async move {
                    ha_stream(&mut socket, 500, &mut frames, &mut stopping, &status).await
                }));
                timeout(Duration::from_secs(2), async {
                    while !statuses.borrow_and_update().contains("service acknowledged") {
                        statuses.changed().await.unwrap();
                    }
                }).await.unwrap();
                let baseline = received_calls.recv().await.unwrap();
                assert_eq!(baseline["service_data"]["rgb_color"], json!([63, 127, 255]));
                permit.send(()).unwrap();
                // Only this scoped stream can notify us. The sole blocking
                // worker is held, so the consumed event's parser cannot run.
                timeout(Duration::from_secs(2), parser_queued.recv()).await.unwrap().unwrap();
                tokio::time::pause();
                targets.send_replace(Some(HaFrame {
                    colors: vec![("light.desk".into(), [80, 40, 20])],
                    produced: Instant::now(),
                }));
                tokio::time::advance(Duration::from_secs(2)).await;
                for _ in 0..32 { tokio::task::yield_now().await; }
                assert!(received_calls.try_recv().is_err(), "cadence sent a color using stale state while parsing was held");
                assert!(!stream.is_finished());
                if stop_while_held {
                    stop.send_replace(true);
                    timeout(Duration::from_millis(500), stream).await.unwrap().unwrap().unwrap();
                    // Stop completed before the blocked parser was permitted.
                    release.send(()).unwrap();
                    blocking.await.unwrap();
                } else {
                    release.send(()).unwrap();
                    blocking.await.unwrap();
                    // A FIFO blocking-pool barrier avoids a virtual timeout
                    // racing the real thread that is finishing JSON parsing.
                    tokio::task::spawn_blocking(|| ()).await.unwrap();
                    tokio::time::advance(Duration::from_millis(500)).await;
                    timeout(Duration::from_secs(1), async {
                        while !statuses.borrow_and_update().contains("0 color targets ready") {
                            statuses.changed().await.unwrap();
                        }
                    }).await.unwrap();
                    assert!(received_calls.try_recv().is_err(), "availability/capability event was lost and stale state sent a color");
                    stop.send_replace(true);
                    stream.await.unwrap().unwrap();
                }
                tokio::time::resume();
                server.await.unwrap();
            }
        });
    }

    #[test]
    fn canceled_queued_json_parser_never_runs_and_releases_its_owned_buffer() {
        struct ObservedBytes {
            body: Vec<u8>,
            reads: Arc<std::sync::atomic::AtomicUsize>,
            dropped: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl AsRef<[u8]> for ObservedBytes {
            fn as_ref(&self) -> &[u8] {
                self.reads.fetch_add(1, Ordering::SeqCst);
                &self.body
            }
        }
        impl Drop for ObservedBytes {
            fn drop(&mut self) {
                let _ = self.dropped.take().unwrap().send(());
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, held) = tokio::sync::oneshot::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let blocking = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = held.blocking_recv();
            });
            ready.await.unwrap();
            let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (dropped, released_buffer) = tokio::sync::oneshot::channel();
            let bytes = ObservedBytes {
                body: vec![b' '; BLOCKING_JSON_THRESHOLD],
                reads: reads.clone(),
                dropped: Some(dropped),
            };
            let (queued, mut parser_queued) = tokio::sync::mpsc::unbounded_channel();
            let parser = tokio::spawn(
                JSON_PARSER_QUEUED.scope(queued, parse_response_json(bytes, "Invalid JSON")),
            );
            parser_queued.recv().await.unwrap();
            parser.abort();
            assert!(parser.await.unwrap_err().is_cancelled());
            release.send(()).unwrap();
            blocking.await.unwrap();
            released_buffer.await.unwrap();
            // Once to select the offload path; a running parser would read it
            // again. Abort removed the queued work without parsing its input.
            assert_eq!(reads.load(Ordering::SeqCst), 1);
        });
    }

    #[tokio::test]
    async fn wled_body_limits_reject_headers_and_chunked_overflow_before_eof_and_recover() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let limit = 256;
        let advertised = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", limit + 1);
        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n80\r\n{}\r\n81\r\n{}\r\n",
            "x".repeat(128),
            "x".repeat(129),
        );
        async fn read_headers(socket: &mut TcpStream) {
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 8192);
            }
        }
        for oversized in [advertised, chunked] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut first, _) = listener.accept().await.unwrap();
                read_headers(&mut first).await;
                first.write_all(oversized.as_bytes()).await.unwrap();
                // Keep the incomplete response open while accepting recovery.
                // Rejection must happen without EOF or the request timeout.
                let (mut second, _) = listener.accept().await.unwrap();
                read_headers(&mut second).await;
                let body = json!({"state":{"lor":0,"seg":[]},
                    "info":{"mac":"aabbccddeeff","name":"Desk","leds":{"count":2}}})
                .to_string();
                second
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                drop(first);
            });
            let response = http_client().unwrap().get(&url).send().await.unwrap();
            let error = timeout(
                Duration::from_millis(500),
                bounded_wled_json(response, limit, "Invalid JSON"),
            )
            .await
            .unwrap()
            .unwrap_err()
            .to_string();
            assert!(error.contains("size limit"));
            assert_eq!(error, "WLED response exceeds the supported JSON size limit");
            assert_eq!(inspect_wled(&url).await.unwrap().name, "Desk");
            server.await.unwrap();
        }
    }

    async fn accept_fake_ha(listener: &tokio::net::TcpListener) -> WebSocketStream<TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                json!({"type":"auth_required"}).to_string().into(),
            ))
            .await
            .unwrap();
        socket.next().await.unwrap().unwrap();
        socket
            .send(Message::Text(json!({"type":"auth_ok"}).to_string().into()))
            .await
            .unwrap();
        socket
    }

    #[tokio::test]
    async fn ha_event_byte_budget_includes_previous_requests_and_recovers_on_reconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        // Whitespace is wire memory too; budget the actual message rather than
        // a smaller reserialization of the parsed JSON.
        let event = format!(
            "{}{}",
            json!({"type":"event","event":{"event_type":"other"}}),
            " ".repeat(40)
        );
        let byte_limit = event.len() * 2 - 1;
        let event_bytes = event.len();
        let server = tokio::spawn(async move {
            let mut socket = accept_fake_ha(&listener).await;
            for _ in 0..2 {
                let request: Value =
                    serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                socket
                    .send(Message::Text(event.clone().into()))
                    .await
                    .unwrap();
                socket
                    .send(Message::Text(
                        json!({"type":"result","id":request["id"],"success":true,"result":[]})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            drop(socket);
            let mut socket = accept_fake_ha(&listener).await;
            let request: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"result","id":request["id"],"success":true,"result":[]})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let mut socket = ha_connect(&url, "fake-token").await.unwrap();
        let mut events = HaEvents::default();
        ha_request_events_bounded(
            &mut socket,
            1,
            json!({"type":"get_states"}),
            &mut events,
            byte_limit,
        )
        .await
        .unwrap();
        assert_eq!(events.values.len(), 1);
        assert_eq!(events.wire_bytes, event_bytes);
        let error = ha_request_events_bounded(
            &mut socket,
            2,
            json!({"type":"get_states"}),
            &mut events,
            byte_limit,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("byte limit"));
        assert_eq!(
            events.values.len(),
            1,
            "the rejected event must not be retained"
        );
        assert_eq!(events.wire_bytes, event_bytes);
        apply_ha_events(&mut events, &mut HashMap::new(), &mut HashMap::new()).unwrap();
        assert_eq!(events.wire_bytes, 0);
        drop(socket);
        let mut socket = ha_connect(&url, "fake-token").await.unwrap();
        assert_eq!(
            ha_request(&mut socket, 1, json!({"type":"get_states"}))
                .await
                .unwrap(),
            json!([])
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn ha_oversized_frame_header_is_rejected_without_body_and_reconnects() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut socket = accept_fake_ha(&listener).await;
            let mut header = vec![0x81, 127]; // Server text frame, 64-bit size.
            header.extend_from_slice(&((MAX_HA_FRAME_BYTES + 1) as u64).to_be_bytes());
            socket.get_mut().write_all(&header).await.unwrap();
            // Do not send the announced payload or close this connection yet.
            let mut recovered = accept_fake_ha(&listener).await;
            recovered
                .send(Message::Text(
                    json!({"type":"event","event":{"event_type":"other"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            drop(socket);
        });
        let mut socket = ha_connect(&url, "fake-token").await.unwrap();
        let error = timeout(Duration::from_millis(500), ws_read(&mut socket))
            .await
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(error.contains("WebSocket size limit"));
        drop(socket);
        let mut socket = ha_connect(&url, "fake-token").await.unwrap();
        assert_eq!(ws_read(&mut socket).await.unwrap()["type"], "event");
        server.await.unwrap();
        let config = ha_socket_config();
        let defaults = WebSocketConfig::default();
        assert_eq!(config.max_message_size, Some(MAX_HA_MESSAGE_BYTES));
        assert_eq!(config.max_frame_size, Some(MAX_HA_FRAME_BYTES));
        assert_eq!(config.read_buffer_size, defaults.read_buffer_size);
        assert_eq!(config.write_buffer_size, defaults.write_buffer_size);
    }

    #[tokio::test]
    async fn response_parser_accepts_large_normal_json_and_keeps_errors_private() {
        let body = json!({"name":"n".repeat(BLOCKING_JSON_THRESHOLD)}).to_string();
        let parsed = parse_response_json(body.into_bytes(), "Invalid JSON")
            .await
            .unwrap();
        assert_eq!(
            parsed["name"].as_str().unwrap().len(),
            BLOCKING_JSON_THRESHOLD
        );
        let error = parse_response_json(b"secret-invalid-json".to_vec(), "Invalid JSON")
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(error, "Invalid JSON");
    }

    #[test]
    fn ha_discovery_and_state_events_bound_retained_lights_and_metadata() {
        fn state(id: &str) -> Value {
            json!({"entity_id":id,"state":"on","attributes":{
                "friendly_name":"Desk", "supported_color_modes":["rgb","rgbw","rgbww","hs","xy","future_mode"]}})
        }
        let normal = state("light.desk");
        let accepted = parse_ha_lights(&json!([normal.clone()])).unwrap();
        assert_eq!(
            accepted[0].color_modes,
            ["rgb", "rgbw", "rgbww", "hs", "xy", "future_mode"]
        );
        let boundary = json!([{
            "entity_id":format!("light.{}", "a".repeat(MAX_HA_ENTITY_BYTES - 6)),
            "attributes":{"friendly_name":"n".repeat(MAX_HA_NAME_BYTES),
                "supported_color_modes":vec!["m".repeat(MAX_HA_MODE_BYTES); MAX_HA_MODES]},
            "state":"on",
        }]);
        assert_eq!(parse_ha_lights(&boundary).unwrap().len(), 1);
        for (pointer, huge) in [
            (
                "/entity_id",
                json!(format!("light.{}", "secret".repeat(MAX_HA_ENTITY_BYTES))),
            ),
            (
                "/attributes/friendly_name",
                json!("secret".repeat(MAX_HA_NAME_BYTES)),
            ),
            (
                "/attributes/supported_color_modes",
                json!(vec!["rgb"; MAX_HA_MODES + 1]),
            ),
            (
                "/attributes/supported_color_modes",
                json!(["secret".repeat(MAX_HA_MODE_BYTES)]),
            ),
        ] {
            let mut oversized = normal.clone();
            *oversized.pointer_mut(pointer).unwrap() = huge;
            let error = parse_ha_lights(&json!([oversized.clone()]))
                .unwrap_err()
                .to_string();
            assert!(error.contains("field size"));
            assert!(!error.contains("secret"));
            let event = json!({"event":{"event_type":"state_changed","data":{
                "entity_id":oversized["entity_id"],"new_state":oversized}}});
            assert!(update_ha_event(&event, &mut HashMap::new(), &mut HashMap::new()).is_err());
        }
        let mut states = vec![normal.clone(); MAX_HA_LIGHTS];
        assert_eq!(
            parse_ha_lights(&json!(states)).unwrap().len(),
            MAX_HA_LIGHTS
        );
        states.push(normal.clone());
        assert!(
            parse_ha_lights(&json!(states))
                .unwrap_err()
                .to_string()
                .contains("10000-light")
        );
        let mut lights: HashMap<_, _> = (0..MAX_HA_LIGHTS)
            .map(|i| {
                let id = format!("light.{i}");
                (id.clone(), parse_ha_light(&state(&id)).unwrap().unwrap())
            })
            .collect();
        let event = json!({"event":{"event_type":"state_changed","data":{
            "entity_id":"light.new","new_state":state("light.new")}}});
        assert!(update_ha_event(&event, &mut lights, &mut HashMap::new()).is_err());
        assert_eq!(lights.len(), MAX_HA_LIGHTS);
        let malformed = json!({"event":{"event_type":"state_changed","data":{
            "entity_id":"light.0","new_state":{"state":"on"}}}});
        assert!(update_ha_event(&malformed, &mut lights, &mut HashMap::new()).is_err());
        let mismatched = json!({"event":{"event_type":"state_changed","data":{
            "entity_id":"light.0","new_state":state("light.other")}}});
        assert!(update_ha_event(&mismatched, &mut lights, &mut HashMap::new()).is_err());
        let ordinary_update = json!({"event":{"event_type":"state_changed","data":{
            "entity_id":"light.0","new_state":state("light.0")}}});
        update_ha_event(&ordinary_update, &mut lights, &mut HashMap::new()).unwrap();
        assert_eq!(lights.len(), MAX_HA_LIGHTS);
        let mut events = HaEvents::default();
        for _ in 0..MAX_HA_EVENTS {
            events
                .push(json!({"type":"event"}), 1, MAX_HA_EVENT_BYTES)
                .unwrap();
        }
        assert!(
            events
                .push(json!({"type":"event"}), 1, MAX_HA_EVENT_BYTES)
                .is_err()
        );
    }

    async fn fake_wled(
        led_count: usize,
        fail_reset: bool,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<(String, Value)>,
        JoinHandle<()>,
    ) {
        fake_wled_states(led_count, fail_reset, Vec::new()).await
    }

    async fn fake_wled_states(
        led_count: usize,
        fail_reset: bool,
        states: Vec<Value>,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<(String, Value)>,
        JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (requests_tx, requests_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut failures_remaining = usize::from(fail_reset);
            let mut states = states.into_iter();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                let header_end = loop {
                    let length = socket.read(&mut buffer).await.unwrap();
                    if length == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..length]);
                    if let Some(i) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]).to_string();
                let body_length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while request.len() < header_end + body_length {
                    let length = socket.read(&mut buffer).await.unwrap();
                    if length == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..length]);
                }
                let route = headers.lines().next().unwrap().to_owned();
                let body = serde_json::from_slice(&request[header_end..]).unwrap_or(Value::Null);
                assert_ne!(
                    body["live"], true,
                    "Indefinite JSON-live acquisition is forbidden"
                );
                let status_code = if failures_remaining > 0 && body["live"] == false {
                    failures_remaining -= 1;
                    "500 Failed"
                } else {
                    "200 OK"
                };
                requests_tx.send((route.clone(), body)).unwrap();
                let response = if route.contains(" /json ") {
                    json!({"info":{"name":"Fake WLED", "mac":"test-mac", "leds":{"count":led_count}}, "state":{"lor":0,"seg":[{"start":0,"stop":led_count}]}})
                } else if route.contains(" /json/cfg ") {
                    json!({"if":{"live":{"en":true,"timeout":25,"mso":false,"rlm":false,"offset":0,"dmx":{"addr":1,"seqskip":false}}}})
                } else if route.starts_with("GET /json/state ") {
                    states.next().unwrap_or_else(|| json!({"on":true,"bri":128,"transition":0,"mainseg":0,"seg":[{"id":0,"start":0,"stop":led_count}]}))
                } else { json!({"success":true}) }.to_string();
                let response = format!(
                    "HTTP/1.1 {status_code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (url, requests_rx, task)
    }

    #[test]
    fn wled_refuses_dmx_offsets_sequence_filter_and_realtime_override() {
        let safe = json!({"if":{"live":{"en":true,"timeout":25,"mso":false,"rlm":false,"offset":0,"dmx":{"addr":1,"seqskip":false}}}});
        for value in [json!(0), json!(2), json!(4), json!(-1), Value::Null] {
            let mut cfg = safe.clone();
            cfg["if"]["live"]["dmx"]["addr"] = value;
            assert!(finite_timeout_from_config(&cfg).is_err());
        }
        for value in [json!(true), json!(0), Value::Null] {
            let mut cfg = safe.clone();
            cfg["if"]["live"]["dmx"]["seqskip"] = value;
            assert!(finite_timeout_from_config(&cfg).is_err());
        }
        let mut state =
            json!({"info":{"name":"Mock","mac":"aa","leds":{"count":10}},"state":{"lor":0}});
        assert!(parse_wled(&state).is_ok());
        for value in [json!(1), json!(2), Value::Null] {
            state["state"]["lor"] = value;
            assert!(
                parse_wled(&state)
                    .unwrap_err()
                    .to_string()
                    .contains("override")
            );
        }
    }

    #[tokio::test]
    async fn wled_failed_stop_release_remains_visible() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (url, _requests, server) = fake_wled(10, false).await;
        let (_pixels, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 10],
            produced: Instant::now(),
        }));
        let (stop, stop_rx) = watch::channel(false);
        let (status, observed) = watch::channel(String::new());
        let task = spawn_wled_destination(
            url,
            pixels_rx,
            stop_rx,
            status,
            false,
            String::new(),
            receiver.local_addr().unwrap().port(),
        );
        let mut packet = [0; 2048];
        timeout(Duration::from_secs(2), receiver.recv(&mut packet))
            .await
            .unwrap()
            .unwrap();
        // Acquisition succeeded. Loss of the mock HTTP server now makes the
        // explicit release fail, even though the finite DDP lease still expires.
        server.abort();
        let _ = server.await;
        stop.send_replace(true);
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            observed.borrow().contains("release failed"),
            "{}",
            observed.borrow().as_str()
        );
        assert!(!observed.borrow().contains("retrying"));
    }

    #[test]
    fn wled_retry_sequence_caps_failures_and_resets_only_after_streaming() {
        let mut delay = 1;
        let failures: Vec<_> = (0..8)
            .map(|_| next_wled_retry_delay(&mut delay, false))
            .collect();
        assert_eq!(failures, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(next_wled_retry_delay(&mut delay, true), 1);
        assert_eq!(next_wled_retry_delay(&mut delay, false), 2);
        assert_eq!(next_wled_retry_delay(&mut delay, false), 4);
        assert_eq!(next_wled_retry_delay(&mut delay, true), 1);
    }

    #[tokio::test]
    async fn wled_retry_backoff_resets_after_mock_streaming_recovers() {
        async fn status_contains(status: &mut watch::Receiver<String>, expected: &str) {
            timeout(Duration::from_secs(4), async {
                loop {
                    if status.borrow_and_update().contains(expected) {
                        return;
                    }
                    status.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
        }

        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (url, _requests, server) = fake_wled(10, false).await;
        let (pixels_tx, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 11],
            produced: Instant::now(),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, mut status_rx) = watch::channel(String::new());
        let task = spawn_wled_destination(
            url,
            pixels_rx,
            stop_rx,
            status_tx,
            false,
            String::new(),
            receiver.local_addr().unwrap().port(),
        );
        status_contains(&mut status_rx, "retrying in 1s").await;
        pixels_tx.send_replace(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 11],
            produced: Instant::now(),
        }));
        status_contains(&mut status_rx, "retrying in 2s").await;
        // Mere setup/waiting is not streaming recovery.
        pixels_tx.send_replace(None);
        status_contains(&mut status_rx, "waiting for fresh capture").await;
        pixels_tx.send_replace(Some(OutputFrame {
            colors: vec![[20, 40, 80]; 10],
            produced: Instant::now(),
        }));
        let mut buffer = [0; 1500];
        assert_eq!(
            timeout(Duration::from_secs(1), receiver.recv(&mut buffer))
                .await
                .unwrap()
                .unwrap(),
            40
        );
        pixels_tx.send_replace(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 11],
            produced: Instant::now(),
        }));
        status_contains(&mut status_rx, "retrying in 1s").await;
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn wled_literal_addresses_support_ipv6_without_dns_and_preserve_ddp_port() {
        for (host, expected) in [
            ("http://[::1]:8888", "[::1]:4048"),
            ("http://[2001:db8::1234]/prefix", "[2001:db8::1234]:4048"),
            ("http://127.0.0.1:8888", "127.0.0.1:4048"),
        ] {
            assert_eq!(
                literal_wled_address(&base_url(host).unwrap(), 4048).unwrap(),
                expected.parse::<SocketAddr>().unwrap()
            );
        }
        assert!(literal_wled_address(&base_url("wled.example").unwrap(), 4048).is_none());
        assert_eq!(
            literal_wled_address(&base_url("[::1]").unwrap(), 12345)
                .unwrap()
                .port(),
            12345
        );
    }

    #[tokio::test]
    async fn canceled_wled_session_stops_health_polls_and_leaves_other_output_responsive() {
        let (url, mut requests, server) = fake_wled(10, false).await;
        let (_pixels_tx, pixels_rx) = watch::channel(None);
        let (_stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, _) = watch::channel(String::new());
        let task = spawn_wled(url, pixels_rx, stop_rx, status_tx);
        timeout(Duration::from_secs(1), async {
            loop {
                let (route, _) = requests.recv().await.unwrap();
                if route.starts_with("GET /json/info ") {
                    break;
                }
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        // Cancellation of one output must not hold up an independent session.
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (other_url, _other_requests, other_server) = fake_wled(10, false).await;
        let (_other_pixels_tx, other_pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[20, 40, 80]; 10],
            produced: Instant::now(),
        }));
        let (other_stop_tx, other_stop_rx) = watch::channel(false);
        let (other_status_tx, _) = watch::channel(String::new());
        let other = spawn_wled_destination(
            other_url,
            other_pixels_rx,
            other_stop_rx,
            other_status_tx,
            false,
            String::new(),
            receiver.local_addr().unwrap().port(),
        );
        let mut buffer = [0; 1500];
        assert_eq!(
            timeout(Duration::from_secs(1), receiver.recv(&mut buffer))
                .await
                .unwrap()
                .unwrap(),
            40
        );
        other_stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), other)
            .await
            .unwrap()
            .unwrap();

        // Keep the first mock alive across its next scheduled health tick. A
        // detached helper would issue another GET here despite owner abortion.
        assert!(
            timeout(
                HEALTH_INTERVAL + Duration::from_millis(100),
                requests.recv()
            )
            .await
            .is_err()
        );
        server.abort();
        let _ = server.await;
        other_server.abort();
        let _ = other_server.await;
    }

    #[tokio::test]
    async fn helper_guard_aborts_task_when_owner_panics() {
        let (child_started_tx, child_started_rx) = tokio::sync::oneshot::channel();
        let (child_finished_tx, child_finished_rx) = tokio::sync::oneshot::channel::<()>();
        let (owner_unwind_tx, owner_unwind_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let child = tokio::spawn(async move {
                let _finished = child_finished_tx;
                child_started_tx.send(()).unwrap();
                std::future::pending::<()>().await;
            });
            let _guard = AbortTaskOnDrop(child.abort_handle());
            let _ = owner_unwind_rx.await;
            panic!("synthetic owner panic");
        });
        child_started_rx.await.unwrap();
        owner_unwind_tx.send(()).unwrap();
        assert!(owner.await.unwrap_err().is_panic());
        // Dropping the child future closes this channel; detaching it would
        // leave it open forever instead.
        assert!(
            timeout(Duration::from_secs(1), child_finished_rx)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn wled_stop_before_first_frame_never_acquires_live_mode() {
        let (url, mut requests, server) = fake_wled(10, false).await;
        let (_pixels_tx, pixels_rx) = watch::channel(None);
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, status_rx) = watch::channel(String::new());
        let task = spawn_wled(url, pixels_rx, stop_rx, status_tx);
        let (route, _) = timeout(Duration::from_secs(1), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(route, "GET /json HTTP/1.1");
        tokio::time::sleep(Duration::from_millis(80)).await;
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        while let Ok((route, body)) = requests.try_recv() {
            assert!(!route.starts_with("POST"), "Unexpected acquisition: {body}");
        }
        assert!(status_rx.borrow().contains("released"));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn wled_refuses_frames_past_device_led_count_before_acquisition() {
        let (url, mut requests, server) = fake_wled(10, false).await;
        let (_pixels_tx, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 11],
            produced: Instant::now(),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, status_rx) = watch::channel(String::new());
        let task = spawn_wled(url, pixels_rx, stop_rx, status_tx);
        let _ = timeout(Duration::from_secs(1), requests.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            status_rx
                .borrow()
                .contains("exceeds the controller LED count")
        );
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        while let Ok((route, _)) = requests.try_recv() {
            assert!(!route.starts_with("POST"));
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn wled_failed_finite_reset_is_retried_by_cleanup_without_indefinite_acquisition() {
        let (url, mut requests, server) = fake_wled(10, true).await;
        let (_pixels_tx, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 10],
            produced: Instant::now(),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, _) = watch::channel(String::new());
        let task = spawn_wled(url, pixels_rx, stop_rx, status_tx);
        let mut live_calls = Vec::new();
        timeout(Duration::from_secs(2), async {
            while live_calls.len() < 2 {
                let (route, body) = requests.recv().await.unwrap();
                if route.starts_with("POST") {
                    live_calls.push(body["live"].as_bool().unwrap());
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(live_calls, vec![false, false]);
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn wled_saved_identity_prevents_dhcp_reassignment_from_acquiring_live() {
        let (url, mut requests, server) = fake_wled(10, false).await;
        let (_pixels_tx, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 10],
            produced: Instant::now(),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, status_rx) = watch::channel(String::new());
        let task = spawn_wled_device(
            url,
            pixels_rx,
            stop_rx,
            status_tx,
            false,
            "different-mac".into(),
        );
        let _ = timeout(Duration::from_secs(1), requests.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(status_rx.borrow().contains("identity differs"));
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        while let Ok((route, _)) = requests.try_recv() {
            assert!(!route.starts_with("POST"));
        }
        assert_eq!(normalized_mac("AA:BB:CC"), normalized_mac("aa-bb-cc"));
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn wled_requires_verified_finite_device_timeout_without_changing_config() {
        let cfg = |timeout| json!({"if":{"live":{"en":true,"timeout":timeout,"mso":false,"rlm":false,"offset":0,"dmx":{"addr":1,"seqskip":false}}}});
        assert_eq!(
            finite_timeout_from_config(&cfg(25)).unwrap(),
            Duration::from_millis(2500)
        );
        for units in [0, 650, 2550, u64::MAX] {
            assert!(finite_timeout_from_config(&cfg(units)).is_err());
        }
        assert!(finite_timeout_from_config(&json!({})).is_err());
        assert!(
            finite_timeout_from_config(&json!({"if":{"live":{"en":false,"timeout":25}}})).is_err()
        );
    }

    #[test]
    fn wled_requires_explicit_physical_indexing_configuration() {
        let safe = json!({"if":{"live":{"en":true,"timeout":25,"mso":false,"rlm":false,"offset":0,"dmx":{"addr":1,"seqskip":false}}}});
        assert!(finite_timeout_from_config(&safe).is_ok());
        for (key, unsafe_values) in [
            ("mso", vec![json!(true), json!(0), Value::Null]),
            ("rlm", vec![json!(true), json!(0), Value::Null]),
            (
                "offset",
                vec![json!(1), json!(-1), json!("0"), json!(0.5), Value::Null],
            ),
        ] {
            for value in unsafe_values {
                let mut cfg = safe.clone();
                cfg["if"]["live"][key] = value;
                assert!(finite_timeout_from_config(&cfg).is_err(), "accepted {cfg}");
            }
            let mut cfg = safe.clone();
            cfg["if"]["live"].as_object_mut().unwrap().remove(key);
            assert!(finite_timeout_from_config(&cfg).is_err());
        }
    }

    #[tokio::test]
    async fn wled_restores_only_unchanged_observed_state() {
        for changed in [false, true] {
            let (url, mut requests, server) = fake_wled(10, false).await;
            let base = base_url(&url).unwrap();
            let client = http_client().unwrap();
            let mut previous = restorable_state(&wled_state(&client, &base).await.unwrap());
            if changed {
                previous["bri"] = json!(127);
            }
            let (released, restored) = release_wled(&client, &base, &Some(previous.clone())).await;
            assert!(released);
            assert_eq!(restored, !changed);
            let posts: Vec<Value> = std::iter::from_fn(|| requests.try_recv().ok())
                .filter(|(route, _)| route.starts_with("POST"))
                .map(|(_, body)| body)
                .collect();
            assert_eq!(posts[0], json!({"live":false}));
            if changed {
                assert_eq!(posts.len(), 1);
            } else {
                assert_eq!(posts, vec![json!({"live":false}), previous]);
            }
            server.abort();
            let _ = server.await;
        }
    }

    #[tokio::test]
    async fn wled_change_during_first_ddp_frame_cannot_be_adopted_and_restored_over() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let previous = json!({"on":true,"bri":128,"seg":[{"id":0,"start":0,"stop":10}]});
        let mut changed = previous.clone();
        changed["bri"] = json!(127);
        let (url, mut requests, server) =
            fake_wled_states(10, false, vec![previous, changed.clone(), changed]).await;
        let (_pixels_tx, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[1, 2, 3]; 10],
            produced: Instant::now(),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, status_rx) = watch::channel(String::new());
        let task = spawn_wled_destination(
            url,
            pixels_rx,
            stop_rx,
            status_tx,
            true,
            String::new(),
            receiver.local_addr().unwrap().port(),
        );
        let mut buffer = [0; 1500];
        timeout(Duration::from_secs(1), receiver.recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        // A former post-DDP snapshot would accept the changed brightness as
        // owned here, then overwrite it with 128 on Stop.
        tokio::time::sleep(Duration::from_millis(80)).await;
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        let requests: Vec<_> = std::iter::from_fn(|| requests.try_recv().ok()).collect();
        assert_eq!(
            requests
                .iter()
                .filter(|(route, _)| route.starts_with("GET /json/state "))
                .count(),
            2
        );
        let posts: Vec<_> = requests
            .into_iter()
            .filter(|(route, _)| route.starts_with("POST"))
            .map(|(_, body)| body)
            .collect();
        assert_eq!(posts, vec![json!({"live":false}), json!({"live":false})]);
        assert!(!status_rx.borrow().contains("prior state restored"));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn wled_finite_ddp_keepalive_uses_latest_frame_then_stale_and_cancel_release() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (url, mut requests, server) = fake_wled(10, false).await;
        let (pixels_tx, pixels_rx) = watch::channel(Some(OutputFrame {
            colors: vec![[20, 40, 80]; 10],
            produced: Instant::now(),
        }));
        let (stop_tx, stop_rx) = watch::channel(false);
        let (status_tx, _) = watch::channel(String::new());
        let task = spawn_wled_destination(
            url,
            pixels_rx,
            stop_rx,
            status_tx,
            false,
            String::new(),
            receiver.local_addr().unwrap().port(),
        );
        let mut buffer = [0; 1500];
        let length = timeout(Duration::from_secs(1), receiver.recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            &buffer[10..length],
            vec![[20, 40, 80]; 10]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        );
        pixels_tx.send_replace(Some(OutputFrame {
            colors: vec![[80, 40, 20]; 10],
            produced: Instant::now(),
        }));
        let begin = Instant::now();
        for _ in 0..12 {
            let length = timeout(Duration::from_millis(200), receiver.recv(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                &buffer[10..length],
                vec![[80, 40, 20]; 10]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            begin.elapsed() < Duration::from_millis(650),
            "DDP cadence stalled"
        );
        pixels_tx.send_replace(Some(OutputFrame {
            colors: vec![[200, 0, 0]; 10],
            produced: Instant::now() - Duration::from_secs(3),
        }));
        tokio::time::sleep(Duration::from_millis(80)).await;
        while receiver.try_recv(&mut buffer).is_ok() {}
        assert!(
            timeout(Duration::from_millis(100), receiver.recv(&mut buffer))
                .await
                .is_err()
        );
        pixels_tx.send_replace(Some(OutputFrame {
            colors: vec![[0, 80, 0]; 10],
            produced: Instant::now(),
        }));
        let _ = timeout(Duration::from_secs(1), receiver.recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        let mut resets = 0;
        while let Ok((route, body)) = requests.try_recv() {
            assert!(
                !route.starts_with("POST /json/cfg"),
                "Persistent device settings must not be changed"
            );
            if route.starts_with("POST") {
                assert_eq!(body["live"], false);
                resets += 1;
            }
        }
        // Reset before each of two DDP sessions, stale cleanup, final cleanup.
        assert_eq!(resets, 4);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn credential_operations_survive_timeout_and_gate_reads_and_saves() {
        use std::sync::atomic::AtomicUsize;
        let gate = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let running_gate = gate.clone();
        let task = tokio::spawn(credential_operation_on(running_gate, move || {
            count.fetch_add(1, Ordering::SeqCst);
            let _ = release_rx.recv();
            Ok("fake-token".to_owned())
        }));
        // Native prompts must not stall Tokio's current-thread executor.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!task.is_finished());
        let result = timeout(REQUEST_TIMEOUT + Duration::from_millis(500), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        // Both a repeated read and a save fail promptly without another worker.
        for _ in 0..3 {
            assert!(
                credential_operation_on(gate.clone(), || Ok(String::new()))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("still pending")
            );
            assert!(
                credential_operation_on(gate.clone(), || Ok(()))
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("still pending")
            );
        }
        release_tx.send(()).unwrap();
        timeout(Duration::from_secs(1), async {
            while gate.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(credential_operation_on(gate, || Ok(())).await.is_ok());
    }

    #[tokio::test]
    async fn canceled_credential_call_retains_gate_until_native_call_returns() {
        let gate = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let running_gate = gate.clone();
        let task = tokio::spawn(credential_operation_on(running_gate, move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
            Ok(())
        }));
        started_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(
            credential_operation_on(gate.clone(), || Ok(()))
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        timeout(Duration::from_secs(1), async {
            while gate.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(credential_operation_on(gate, || Ok(())).await.is_ok());
    }

    #[test]
    fn ddp_offsets_lengths_sequence_and_push() {
        let mut sequence = 14;
        let packets = ddp_packets(&vec![[12, 34, 56]; 1001], &mut sequence).unwrap();
        assert_eq!(packets.len(), 3);
        for (i, packet) in packets.iter().enumerate() {
            assert!(packet.len() <= 1450);
            assert_eq!(
                u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                (i * 1440) as u32
            );
            assert_eq!(
                u16::from_be_bytes(packet[8..10].try_into().unwrap()) as usize,
                packet.len() - 10
            );
            assert_eq!(packet[0] & 1, u8::from(i == 2));
            assert_eq!(&packet[2..4], &[0x0b, 1]);
            assert!((1..=15).contains(&packet[1]));
        }
        assert_eq!(
            packets.iter().map(|p| p[1]).collect::<Vec<_>>(),
            vec![15, 1, 2]
        );
        assert_eq!(packets[2].len(), 133);
        assert!(ddp_packets(&[], &mut sequence).is_err());
        assert!(ddp_packets(&vec![[0; 3]; MAX_LEDS + 1], &mut sequence).is_err());
    }

    #[tokio::test]
    async fn ddp_local_udp_receives_complete_rgb_frame() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .connect(receiver.local_addr().unwrap())
            .await
            .unwrap();
        let colors = vec![[255, 20, 3]; 481];
        send_ddp(&sender, &colors, &mut 0).await.unwrap();
        let mut output = Vec::new();
        for _ in 0..2 {
            let mut buffer = [0; 1500];
            let length = timeout(Duration::from_secs(1), receiver.recv(&mut buffer))
                .await
                .unwrap()
                .unwrap();
            output.extend_from_slice(&buffer[10..length]);
        }
        assert_eq!(output, colors.into_iter().flatten().collect::<Vec<_>>());
    }

    #[test]
    fn ha_capabilities_convert_and_skip_white_only_unavailable() {
        let mut light = HaLight {
            entity_id: "light.test".into(),
            name: "Test".into(),
            color_modes: vec!["hs".into()],
            available: true,
        };
        let data = service_data(&light, [0, 255, 0]).unwrap();
        assert_eq!(data["hs_color"], json!([120.0, 100.0]));
        assert_eq!(data["brightness"], 255);
        light.color_modes = vec!["xy".into()];
        assert_eq!(
            service_data(&light, [0, 0, 0]).unwrap()["xy_color"],
            json!([0.0, 0.0])
        );
        light.color_modes = vec!["color_temp".into()];
        assert!(service_data(&light, [1, 2, 3]).is_none());
        light.color_modes = vec!["rgb".into()];
        light.available = false;
        assert!(service_data(&light, [1, 2, 3]).is_none());
    }

    #[test]
    fn ha_rgbw_and_rgbww_use_native_arrays_and_disable_white_channels() {
        let mut light = HaLight {
            entity_id: "light.a".into(),
            name: "A".into(),
            color_modes: vec!["rgbw".into()],
            available: true,
        };
        let data = service_data(&light, [20, 40, 80]).unwrap();
        assert_eq!(data["rgbw_color"], json!([63, 127, 255, 0]));
        assert!(data.get("rgb_color").is_none());
        assert_eq!(data["brightness"], 80);
        light.color_modes = vec!["rgbww".into()];
        let data = service_data(&light, [20, 40, 80]).unwrap();
        assert_eq!(data["rgbww_color"], json!([63, 127, 255, 0, 0]));
        assert!(data.get("rgb_color").is_none());
    }

    #[test]
    fn urls_never_accept_embedded_secrets() {
        assert!(base_url("https://user:secret@example.org").is_err());
        assert!(base_url("https://example.org?token=secret").is_err());
        assert!(base_url("ftp://example.org").is_err());
        assert_eq!(
            base_url("192.168.1.20").unwrap().as_str(),
            "http://192.168.1.20/"
        );
    }

    #[test]
    fn ha_state_events_update_availability_without_invalidating_own_rgb_updates() {
        let light = HaLight {
            entity_id: "light.a".into(),
            name: "A".into(),
            color_modes: vec!["rgb".into()],
            available: true,
        };
        let mut lights = HashMap::from([("light.a".into(), light)]);
        let mut sent = HashMap::from([("light.a".into(), [1, 2, 3])]);
        let event = |state: &str| json!({"type":"event", "event":{"event_type":"state_changed", "data":{"entity_id":"light.a", "new_state":{"entity_id":"light.a", "state":state, "attributes":{"supported_color_modes":["rgb"]}}}}});
        update_ha_event(&event("on"), &mut lights, &mut sent).unwrap();
        assert_eq!(sent["light.a"], [1, 2, 3]);
        update_ha_event(&event("unavailable"), &mut lights, &mut sent).unwrap();
        assert!(!lights["light.a"].available);
        assert!(sent.is_empty());
    }

    #[test]
    fn ha_refresh_preserves_unchanged_cache_and_invalidates_changed_or_removed_targets() {
        let light = |id: &str, available, mode: &str| HaLight {
            entity_id: id.to_owned(),
            name: id.to_owned(),
            color_modes: vec![mode.to_owned()],
            available,
        };
        let previous = [
            light("light.same", true, "rgb"),
            light("light.unavailable", true, "rgb"),
            light("light.mode", true, "rgb"),
            light("light.removed", true, "rgb"),
        ];
        let mut lights = previous
            .iter()
            .map(|l| (l.entity_id.clone(), l.clone()))
            .collect();
        let mut sent = previous
            .iter()
            .map(|l| (l.entity_id.clone(), [1, 2, 3]))
            .collect();
        reconcile_ha_lights(
            vec![
                light("light.same", true, "rgb"),
                light("light.unavailable", false, "rgb"),
                light("light.mode", true, "xy"),
            ],
            &mut lights,
            &mut sent,
        );
        assert_eq!(sent, HashMap::from([("light.same".to_owned(), [1, 2, 3])]));
        assert!(!lights.contains_key("light.removed"));
        assert!(!lights["light.unavailable"].available);
        assert_eq!(lights["light.mode"].color_modes, ["xy"]);
    }

    #[tokio::test]
    async fn ha_stream_coalesces_and_never_sends_stale_frames() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (calls_tx, mut calls_rx) = tokio::sync::mpsc::channel(4);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(
                json!({"type":"auth_required"}).to_string().into(),
            ))
            .await
            .unwrap();
            ws.next().await.unwrap().unwrap();
            ws.send(Message::Text(json!({"type":"auth_ok"}).to_string().into()))
                .await
                .unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(text) = message else {
                    break;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                let result = if request["type"] == "get_states" {
                    json!([{"entity_id":"light.a", "state":"on", "attributes":{"supported_color_modes":["rgb"]}}])
                } else {
                    if request["type"] == "call_service" {
                        calls_tx
                            .send(request["service_data"].clone())
                            .await
                            .unwrap();
                    }
                    Value::Null
                };
                ws.send(Message::Text(
                    json!({"id":request["id"], "type":"result", "success":true, "result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            }
        });
        let mut socket = ha_connect(&url, "fake-token").await.unwrap();
        let (targets_tx, mut targets_rx) = watch::channel(Some(HaFrame {
            colors: vec![("light.a".into(), [20, 40, 80])],
            produced: Instant::now() - Duration::from_secs(3),
        }));
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let (status_tx, _) = watch::channel(String::new());
        let task = tokio::spawn(async move {
            ha_stream(&mut socket, 1, &mut targets_rx, &mut stop_rx, &status_tx)
                .await
                .unwrap();
            let _ = socket.close(None).await;
        });
        assert!(
            timeout(Duration::from_millis(600), calls_rx.recv())
                .await
                .is_err()
        );
        targets_tx.send_replace(Some(HaFrame {
            colors: vec![("light.a".into(), [20, 40, 80])],
            produced: Instant::now(),
        }));
        let command = timeout(Duration::from_secs(1), calls_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(command["brightness"], 80);
        assert_eq!(command["rgb_color"], json!([63, 127, 255]));
        assert!(
            timeout(Duration::from_millis(600), calls_rx.recv())
                .await
                .is_err()
        );
        stop_tx.send_replace(true);
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn ha_fake_server_auth_discovery_ack_and_rejected_call() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            ws.send(Message::Text(
                json!({"type":"auth_required"}).to_string().into(),
            ))
            .await
            .unwrap();
            let auth: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(auth["access_token"], "test-token");
            ws.send(Message::Text(json!({"type":"auth_ok"}).to_string().into()))
                .await
                .unwrap();
            for success in [true, true, false] {
                let request: Value =
                    serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                let result = if request["type"] == "get_states" {
                    json!([{"entity_id":"light.a", "state":"on", "attributes":{"friendly_name":"A", "supported_color_modes":["rgb"]}},
                        {"entity_id":"sensor.b", "state":"on"}])
                } else {
                    json!({"context":{"id":"test"}})
                };
                ws.send(Message::Text(json!({"id":request["id"], "type":"result", "success":success, "result":result}).to_string().into())).await.unwrap();
            }
        });
        let mut socket = ha_connect(&url, "test-token").await.unwrap();
        let states = ha_request(&mut socket, 1, json!({"type":"get_states"}))
            .await
            .unwrap();
        let lights = parse_ha_lights(&states).unwrap();
        assert_eq!(lights.len(), 1);
        assert_eq!(lights[0].name, "A");
        ha_request(&mut socket, 2, json!({"type":"call_service"}))
            .await
            .unwrap();
        assert!(
            ha_request(&mut socket, 3, json!({"type":"call_service"}))
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}
