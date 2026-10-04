// Historical correction-1 review probe. Append to the reviewed outputs module
// to reproduce the unsafe consumed-message/parse cancellation boundary.
// The corrected steady loop selects raw receipt and commits parsing separately;
// it no longer uses the timer race demonstrated here. No hardware is involved.
#[cfg(test)]
mod qa_probe {
    use super::*;
    #[test]
    fn canceled_read_loses_consumed_large_event() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2).max_blocking_threads(1).enable_all().build().unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let (started, ready) = std::sync::mpsc::channel();
        let blocked = runtime.spawn_blocking(move || { started.send(()).unwrap(); held.recv().unwrap(); });
        ready.recv().unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("ws://{}", listener.local_addr().unwrap());
            let (sent, received) = tokio::sync::oneshot::channel();
            let (advance, next) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let mut large = json!({"type":"event","event":{"event_type":"state_changed","data":{"entity_id":"light.desk","new_state":{"entity_id":"light.desk","state":"unavailable","attributes":{"supported_color_modes":["rgb"]}}}}}).to_string();
                large.push_str(&" ".repeat(BLOCKING_JSON_THRESHOLD));
                ws.send(Message::Text(large.into())).await.unwrap();
                sent.send(()).unwrap();
                next.await.unwrap();
                ws.send(Message::Text(json!({"marker":"next"}).to_string().into())).await.unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            });
            let (mut socket, _) = connect_async_with_config(&endpoint, Some(ha_socket_config()), false).await.unwrap();
            received.await.unwrap();
            // This is the same cancellation boundary as ha_stream's tick branch.
            assert!(timeout(Duration::from_millis(100), ws_read(&mut socket)).await.is_err());
            release.send(()).unwrap();
            blocked.await.unwrap();
            advance.send(()).unwrap();
            let next = timeout(Duration::from_secs(1), ws_read(&mut socket)).await.unwrap().unwrap();
            println!("After canceling the consumed large unavailable event, the next actual reader returned: {next}");
            assert_eq!(next["marker"], "next");
            assert_ne!(next["type"], "event");
            server.await.unwrap();
        });
    }
}
