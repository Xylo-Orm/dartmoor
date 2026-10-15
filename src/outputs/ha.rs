// Home Assistant credentials, discovery and ambient command scheduler.
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
