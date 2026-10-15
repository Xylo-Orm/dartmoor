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

include!("outputs/wled.rs");
include!("outputs/ha.rs");

#[cfg(test)]
mod tests;
