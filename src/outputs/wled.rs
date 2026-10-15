// WLED transport, parsing, lease and retry implementation.
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

/// Music can request 60 Hz after the same bounded analysis/render path has been
/// measured; video and existing callers keep the original 30 Hz cadence.
pub fn spawn_wled_device_at_rate(
    host: String,
    pixels: watch::Receiver<Option<OutputFrame>>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
    restore_previous: bool,
    expected_id: String,
    fps: u16,
) -> JoinHandle<()> {
    spawn_wled_destination_with_options(
        host,
        pixels,
        stop,
        status,
        restore_previous,
        expected_id,
        WledDestination {
            ddp_port: 4048,
            fps,
        },
    )
}

#[derive(Clone, Copy)]
struct WledDestination {
    ddp_port: u16,
    fps: u16,
}

// Internal destination injection keeps mocked sessions on ephemeral loopback
// ports. The public production adapter always uses WLED's fixed DDP port 4048.
fn spawn_wled_destination(
    host: String,
    pixels: watch::Receiver<Option<OutputFrame>>,
    stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
    restore_previous: bool,
    expected_id: String,
    ddp_port: u16,
) -> JoinHandle<()> {
    spawn_wled_destination_with_options(
        host,
        pixels,
        stop,
        status,
        restore_previous,
        expected_id,
        WledDestination { ddp_port, fps: 30 },
    )
}

fn spawn_wled_destination_with_options(
    host: String,
    mut pixels: watch::Receiver<Option<OutputFrame>>,
    mut stop: watch::Receiver<bool>,
    status: watch::Sender<String>,
    restore_previous: bool,
    expected_id: String,
    destination: WledDestination,
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
                destination,
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
    destination: WledDestination,
) -> (Result<()>, bool) {
    let mut streamed = false;
    let result = async {
        let connection = tokio::select! {
            _ = stop.changed() => return Ok(()),
            setup = connect_wled(host, expected_id, destination.ddp_port) => setup?,
        };
        let (health_task, health_rx) = spawn_wled_health(&connection)?;
        let _health_guard = AbortTaskOnDrop(health_task.abort_handle());
        let mut lease = WledLease::default();
        let cadence = if destination.fps == 30 {
            Duration::from_millis(33)
        } else {
            Duration::from_secs_f64(1.0 / f64::from(destination.fps.max(1)))
        };
        let mut tick = interval(cadence);
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
