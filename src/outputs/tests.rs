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
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
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
    assert!(finite_timeout_from_config(&json!({"if":{"live":{"en":false,"timeout":25}}})).is_err());
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
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            let result = if request["type"] == "get_states" {
                json!([{"entity_id":"light.a", "state":"on", "attributes":{"friendly_name":"A", "supported_color_modes":["rgb"]}},
                    {"entity_id":"sensor.b", "state":"on"}])
            } else {
                json!({"context":{"id":"test"}})
            };
            ws.send(Message::Text(
                json!({"id":request["id"], "type":"result", "success":success, "result":result})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
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
