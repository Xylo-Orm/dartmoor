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
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::watch,
    task::JoinHandle,
    time::{MissedTickBehavior, interval, timeout},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

const STALE_AFTER: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const DDP_PAYLOAD: usize = 1440;
const MAX_LEDS: usize = 65_536;
type HaSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

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

/// A narrow capability boundary for future adapters (for example Hue).
#[derive(Clone, Copy, Debug)]
pub struct OutputCapabilities {
    pub individually_addressable: bool,
    pub color: bool,
    pub minimum_interval: Duration,
}

pub trait OutputAdapter {
    fn capabilities(&self) -> OutputCapabilities;
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

pub async fn inspect_wled(host: &str) -> Result<WledInfo> {
    let base = base_url(host)?;
    let url = base.join("json").context("Invalid WLED endpoint")?;
    let value: Value = http_client()?
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("WLED HTTP connection failed"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED HTTP request was rejected"))?
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("WLED returned invalid JSON"))?;
    parse_wled(&value)
}

fn parse_wled(value: &Value) -> Result<WledInfo> {
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
    let reply: Value = response
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED rejected live-mode request"))?
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("WLED returned invalid live-mode acknowledgement"))?;
    if reply["success"] != true {
        bail!("WLED did not acknowledge realtime reset");
    }
    Ok(())
}

async fn finite_wled_timeout(client: &reqwest::Client, base: &Url) -> Result<Duration> {
    let cfg: Value = client
        .get(base.join("json/cfg").context("Invalid WLED endpoint")?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Could not verify WLED realtime timeout"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED realtime configuration is unavailable"))?
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("WLED returned invalid configuration JSON"))?;
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
    Ok(Duration::from_millis(units * 100))
}

async fn wled_state(client: &reqwest::Client, base: &Url) -> Result<Value> {
    let state: Value = client
        .get(base.join("json/state").context("Invalid WLED endpoint")?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("WLED state request failed"))?
        .error_for_status()
        .map_err(|_| anyhow::anyhow!("WLED state request rejected"))?
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("WLED returned invalid state JSON"))?;
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
                    Ok(response) if response.status().is_success() => response
                        .json::<Value>()
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
            match wled_session(
                &host,
                &mut pixels,
                &mut stop,
                &status,
                restore_previous,
                &expected_id,
                ddp_port,
            )
            .await
            {
                Ok(()) => return,
                Err(e) => {
                    status.send_replace(format!("WLED: {e}; retrying in {retry_seconds}s"));
                }
            }
            tokio::select! {
                _ = stop.changed() => break,
                _ = tokio::time::sleep(Duration::from_secs(retry_seconds)) => {}
            }
            retry_seconds = (retry_seconds * 2).min(30);
        }
        status.send_replace("WLED: stopped".into());
    })
}

async fn wled_session(
    host: &str,
    pixels: &mut watch::Receiver<Option<OutputFrame>>,
    stop: &mut watch::Receiver<bool>,
    status: &watch::Sender<String>,
    restore_previous: bool,
    expected_id: &str,
    ddp_port: u16,
) -> Result<()> {
    let setup = async {
        let base = base_url(host)?;
        let client = http_client()?;
        let info = inspect_wled(host).await?;
        if !expected_id.is_empty() && normalized_mac(expected_id) != normalized_mac(&info.device_id)
        {
            bail!("WLED identity differs from saved controller; reconnect or add the device again");
        }
        let led_count = info.led_count;
        let realtime_timeout = finite_wled_timeout(&client, &base).await?;
        let hostname = base.host_str().context("WLED host missing")?;
        let address = timeout(
            REQUEST_TIMEOUT,
            tokio::net::lookup_host((hostname, ddp_port)),
        )
        .await
        .map_err(|_| anyhow::anyhow!("WLED hostname lookup timed out"))?
        .map_err(|_| anyhow::anyhow!("WLED hostname lookup failed"))?
        .next()
        .context("WLED hostname resolved to no addresses")?;
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
        Ok::<_, anyhow::Error>((base, client, socket, led_count, realtime_timeout))
    };
    let (base, client, socket, led_count, realtime_timeout) = tokio::select! {
        _ = stop.changed() => return Ok(()),
        setup = setup => setup?,
    };
    let mut live_attempted = false;
    let mut active = false;
    let mut saved = None;
    let mut sequence = 0;
    let (health_tx, health_rx) = watch::channel(true);
    let health_client = client.clone();
    let health_url = base.join("json/info").context("Invalid WLED endpoint")?;
    let health_task = tokio::spawn(async move {
        let mut health_tick = interval(Duration::from_secs(5));
        health_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            health_tick.tick().await;
            let reachable = health_client
                .get(health_url.clone())
                .send()
                .await
                .is_ok_and(|response| response.status().is_success());
            health_tx.send_replace(reachable);
        }
    });
    let mut tick = interval(Duration::from_millis(33));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut cancellation = stop.clone();
    let outcome: Result<()> = tokio::select! {
        _ = cancellation.changed() => Ok(()),
        outcome = async {
        loop {
            if *stop.borrow() { break; }
            tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() { break; }
                }
                _ = tick.tick() => {
                    if pixels.has_changed().is_err() { break; }
                    let frame = pixels.borrow_and_update().clone();
                    let fresh = frame.as_ref().filter(|f| f.produced.elapsed() < STALE_AFTER && !f.colors.is_empty());
                    let Some(frame) = fresh else {
                            if live_attempted {
                            let (released, _) = release_wled(&client, &base, &saved).await;
                            if !released { bail!("WLED live release failed"); }
                            live_attempted = false;
                            active = false;
                            saved = None;
                        }
                        status.send_replace("WLED: waiting for fresh capture; live mode released".into());
                        continue;
                    };
                    if frame.colors.len() > led_count { bail!("WLED frame exceeds the controller LED count"); }
                    if !active {
                        // Whole-controller DDP does not change the stored
                        // on/bri/segment state. Keep the pre-acquisition baseline:
                        // a user change during the first frame must never become
                        // an "owned" state that we overwrite at Stop.
                        saved = if restore_previous {
                            wled_state(&client, &base).await.ok().map(|v| restorable_state(&v))
                        } else { None };
                        live_attempted = true; // Cleanup even if reset's reply is lost.
                        // Explicit Start takes controller ownership. Clear an old
                        // JSON-live infinity lease before DDP can inherit it.
                        reset_live(&client, &base).await?;
                        active = true;
                    }
                    // Acquisition can take time: never send its now-stale snapshot.
                    let newest = pixels.borrow_and_update().clone();
                    if let Some(newest) = newest.filter(|f| f.produced.elapsed() < STALE_AFTER) {
                        if newest.colors.len() > led_count { bail!("WLED frame exceeds the controller LED count"); }
                        send_ddp(&socket, &newest.colors, &mut sequence).await?;
                        status.send_replace(if *health_rx.borrow() {
                            format!("WLED: HTTP reachable; DDP unconfirmed; device timeout {}ms", realtime_timeout.as_millis())
                        } else { format!("WLED: HTTP unreachable; DDP unconfirmed; device timeout {}ms", realtime_timeout.as_millis()) });
                    }
                }
            }
        }
        Ok(())
        } => outcome,
    };
    // Health checks are read-only, and owned by this connection attempt.
    health_task.abort();
    let _ = health_task.await;
    // Deliberately outside the fallible loop: acquire errors and send errors
    // take the same cleanup path. Callers must signal stop and join, not abort.
    let (released, restored) = if live_attempted {
        release_wled(&client, &base, &saved).await
    } else {
        (true, false)
    };
    match outcome {
        Err(e) => {
            bail!(
                "{e}; live release {}",
                if released { "complete" } else { "failed" }
            );
        }
        Ok(()) => {
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
        }
    }
    if !released {
        bail!("WLED live release failed");
    }
    Ok(())
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
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("Home Assistant returned invalid JSON"));
            }
            Some(Ok(Message::Ping(data))) => socket
                .send(Message::Pong(data))
                .await
                .map_err(|_| anyhow::anyhow!("Home Assistant connection closed"))?,
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                bail!("Home Assistant connection closed")
            }
            _ => {}
        }
    }
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
        let (mut socket, _) = connect_async(endpoint.as_str())
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
    ha_request_events(socket, id, request.take(), &mut Vec::new()).await
}

async fn ha_request_events(
    socket: &mut HaSocket,
    id: u64,
    mut request: Value,
    events: &mut Vec<Value>,
) -> Result<Value> {
    request["id"] = json!(id);
    timeout(REQUEST_TIMEOUT, async {
        ws_send(socket, request).await?;
        loop {
            let reply = ws_read(socket).await?;
            if reply["type"] == "event" {
                if events.len() >= 256 {
                    bail!("Home Assistant event backlog exceeded limit");
                }
                events.push(reply);
                continue;
            }
            if reply["id"].as_u64() == Some(id) && reply["type"] == "result" {
                if reply["success"] != true {
                    bail!("Home Assistant rejected request");
                }
                return Ok(reply["result"].clone());
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Home Assistant acknowledgement timed out"))?
}

fn parse_ha_lights(states: &Value) -> Result<Vec<HaLight>> {
    let array = states
        .as_array()
        .context("Home Assistant returned invalid states")?;
    Ok(array
        .iter()
        .filter_map(|state| {
            let entity_id = state["entity_id"].as_str()?;
            if !entity_id.starts_with("light.") {
                return None;
            }
            Some(HaLight {
                entity_id: entity_id.to_owned(),
                name: state
                    .pointer("/attributes/friendly_name")
                    .and_then(Value::as_str)
                    .unwrap_or(entity_id)
                    .to_owned(),
                color_modes: state
                    .pointer("/attributes/supported_color_modes")
                    .and_then(Value::as_array)
                    .map(|m| {
                        m.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
                available: !matches!(
                    state["state"].as_str(),
                    Some("unavailable" | "unknown") | None
                ),
            })
        })
        .collect())
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
    let mut events = Vec::new();
    id += 1;
    tokio::select! {
        _ = stop.changed() => return Ok(()),
        subscribed = ha_request_events(socket, id, json!({"type":"subscribe_events", "event_type":"state_changed"}), &mut events) => { subscribed?; }
    }
    for event in events.drain(..) {
        update_ha_event(&event, &mut lights, &mut last_sent);
    }
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
            event = ws_read(socket) => {
                update_ha_event(&event?, &mut lights, &mut last_sent);
            }
            _ = tick.tick() => {
                if refreshed.elapsed() > Duration::from_secs(30) {
                    id += 1;
                    let states = tokio::select! {
                        _ = stop.changed() => return Ok(()),
                        states = ha_request_events(socket, id, json!({"type":"get_states"}), &mut events) => states?,
                    };
                    reconcile_ha_lights(parse_ha_lights(&states)?, &mut lights, &mut last_sent);
                    for event in events.drain(..) { update_ha_event(&event, &mut lights, &mut last_sent); }
                    refreshed = Instant::now();
                }
                let frame = targets.borrow_and_update().clone();
                let Some(frame) = frame.filter(|f| f.produced.elapsed() < STALE_AFTER) else {
                    status.send_replace("Home Assistant: waiting for fresh capture".into());
                    continue;
                };
                let mut selected = None;
                for offset in 0..frame.colors.len() {
                    let index = (cursor + offset) % frame.colors.len();
                    let (entity_id, rgb) = &frame.colors[index];
                    if last_sent.get(entity_id) == Some(rgb) { continue; }
                    let Some(light) = lights.get(entity_id) else { continue; };
                    if let Some(data) = service_data(light, *rgb) {
                        selected = Some((entity_id.clone(), *rgb, data));
                        cursor = index + 1;
                        break;
                    }
                }
                if let Some((entity_id, rgb, data)) = selected {
                    id += 1;
                    tokio::select! {
                        _ = stop.changed() => return Ok(()),
                        reply = ha_request_events(socket, id, json!({"type":"call_service", "domain":"light", "service":"turn_on", "service_data":data}), &mut events) => { reply?; }
                    }
                    last_sent.insert(entity_id, rgb);
                    // Begin the next interval after this acknowledgement. A
                    // slow reply cannot create a burst of overdue ticks.
                    tick.reset();
                    for event in events.drain(..) { update_ha_event(&event, &mut lights, &mut last_sent); }
                    status.send_replace("Home Assistant: service acknowledged (maximum 2 calls/s)".into());
                } else {
                    let supported = frame.colors.iter().filter(|(id, rgb)| lights.get(id).is_some_and(|l| service_data(l, *rgb).is_some())).count();
                    status.send_replace(format!("Home Assistant: {supported} color targets ready; unchanged colors coalesced"));
                }
            }
        }
    }
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
) {
    if event.pointer("/event/event_type").and_then(Value::as_str) != Some("state_changed") {
        return;
    }
    let Some(entity_id) = event
        .pointer("/event/data/entity_id")
        .and_then(Value::as_str)
    else {
        return;
    };
    if !entity_id.starts_with("light.") {
        return;
    }
    let state = &event["event"]["data"]["new_state"];
    if state.is_null() {
        lights.remove(entity_id);
        last_sent.remove(entity_id);
    } else if let Ok(mut parsed) = parse_ha_lights(&json!([state]))
        && let Some(light) = parsed.pop()
    {
        let changed = lights.get(entity_id).is_none_or(|old| {
            old.available != light.available || old.color_modes != light.color_modes
        });
        if changed {
            last_sent.remove(entity_id);
        }
        lights.insert(entity_id.to_owned(), light);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    json!({"info":{"name":"Fake WLED", "mac":"test-mac", "leds":{"count":led_count}}, "state":{"seg":[{"start":0,"stop":led_count}]}})
                } else if route.contains(" /json/cfg ") {
                    json!({"if":{"live":{"en":true,"timeout":25,"mso":false,"rlm":false,"offset":0}}})
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
        let cfg = |timeout| json!({"if":{"live":{"en":true,"timeout":timeout,"mso":false,"rlm":false,"offset":0}}});
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
        let safe =
            json!({"if":{"live":{"en":true,"timeout":25,"mso":false,"rlm":false,"offset":0}}});
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
        update_ha_event(&event("on"), &mut lights, &mut sent);
        assert_eq!(sent["light.a"], [1, 2, 3]);
        update_ha_event(&event("unavailable"), &mut lights, &mut sent);
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
