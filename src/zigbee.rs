//! Shared, app-lifetime USB Zigbee session. Discovery and output use the same
//! coordinator connection; stopping synchronization does not close the radio.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::LazyLock, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{Mutex, mpsc, oneshot, watch},
    time::timeout,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ZigbeeSnapshot {
    pub adapter: Option<Adapter>,
    pub status: String,
    pub pairing: bool,
    pub devices: Vec<ZigbeeDevice>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Adapter {
    pub id: String,
    pub name: String,
    pub path: String,
    pub supported: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ZigbeeDevice {
    pub ieee_address: String,
    pub endpoint: u8,
    pub name: String,
    pub reason: Option<String>,
}
struct Request {
    value: Value,
    reply: oneshot::Sender<Result<Value>>,
}
#[derive(Clone)]
struct Session {
    tx: mpsc::Sender<Request>,
}
static SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));
static STATE: LazyLock<watch::Sender<ZigbeeSnapshot>> = LazyLock::new(|| {
    watch::channel(ZigbeeSnapshot {
        status: "Connect your Sonoff USB coordinator and press Discover.".into(),
        ..Default::default()
    })
    .0
});

pub fn snapshot() -> ZigbeeSnapshot {
    STATE.borrow().clone()
}
fn status(message: &str) {
    STATE.send_modify(|state| state.status = message.into());
}

fn data_directory() -> Result<PathBuf> {
    Ok(
        directories::ProjectDirs::from("org", "dartmoor", "dartmoor")
            .context("Could not locate Zigbee data directory")?
            .data_local_dir()
            .join("zigbee"),
    )
}

async fn command_ok(command: &mut Command, limit: Duration, message: &str) -> Result<()> {
    command.kill_on_drop(true);
    let output = timeout(limit, command.output())
        .await
        .context("Zigbee runtime setup timed out")?
        .with_context(|| message.to_owned())?;
    ensure!(
        output.status.success(),
        "{message}: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2048)
            .collect::<String>()
    );
    Ok(())
}

async fn python(data: &std::path::Path) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DARTMOOR_ZIGBEE_PYTHON") {
        return Ok(path.into());
    }
    // Development builds can reuse the isolated runtime prepared in the repo.
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/zigbee-runtime/bin/python");
    if cfg!(debug_assertions) && local.exists() {
        return Ok(local);
    }
    let runtime = data.join("runtime");
    let python = runtime.join(if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    });
    let ready = runtime.join("dartmoor-driver-version");
    if !python.exists() {
        status("Preparing Zigbee support…");
        let executable = if cfg!(windows) { "python" } else { "python3" };
        command_ok(
            Command::new(executable).args(["-m", "venv"]).arg(&runtime),
            Duration::from_secs(60),
            "Python 3.12+ with venv is required for USB Zigbee support",
        )
        .await?;
    }
    if std::fs::read_to_string(&ready).unwrap_or_default()
        != include_str!("zigbee/requirements.txt")
    {
        status("Downloading Zigbee support (first use)…");
        let requirements = data.join("requirements.txt");
        std::fs::write(&requirements, include_str!("zigbee/requirements.txt"))?;
        command_ok(
            Command::new(&python)
                .args([
                    "-m",
                    "pip",
                    "install",
                    "--disable-pip-version-check",
                    "--no-cache-dir",
                    "-r",
                ])
                .arg(requirements),
            Duration::from_secs(300),
            "Could not install Zigbee support; check your internet connection",
        )
        .await?;
        std::fs::write(ready, include_str!("zigbee/requirements.txt"))?;
    }
    Ok(python)
}

async fn session() -> Result<Session> {
    let mut current = SESSION.lock().await;
    if let Some(session) = current.as_ref().filter(|s| !s.tx.is_closed()) {
        return Ok(session.clone());
    }
    let data = data_directory()?;
    std::fs::create_dir_all(&data)?;
    let python = python(&data).await?;
    let helper = data.join("helper.py");
    std::fs::write(&helper, include_str!("zigbee/helper.py"))?;
    let mut child = Command::new(python)
        .arg("-u")
        .arg(helper)
        .arg(data)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("Could not start USB Zigbee support")?;
    let mut input = child.stdin.take().context("Missing Zigbee command pipe")?;
    let output = child.stdout.take().context("Missing Zigbee event pipe")?;
    let (tx, mut requests) = mpsc::channel::<Request>(8);
    tokio::spawn(async move {
        let mut reader = BufReader::new(output);
        let mut buffer = Vec::new();
        let mut next_id = 0u64;
        let mut shutdown_reply = None;
        let mut pending: Option<(u64, oneshot::Sender<Result<Value>>)> = None;
        let result: Result<()> = async {
            loop {
                tokio::select! {
                    request = requests.recv(), if pending.is_none() => {
                        let Some(mut request) = request else { break };
                        if request.reply.is_closed() { continue; }
                        next_id += 1;
                        request.value["id"] = json!(next_id);
                        if request.value["op"] == "shutdown" {
                            input.write_all(format!("{}\n", request.value).as_bytes()).await?;
                            shutdown_reply = Some(request.reply);
                            break;
                        }
                        input.write_all(format!("{}\n", request.value).as_bytes()).await?;
                        input.flush().await?;
                        pending = Some((next_id, request.reply));
                    }
                    length = reader.read_until(b'\n', &mut buffer) => {
                        ensure!(length? > 0, "USB Zigbee helper stopped unexpectedly");
                        ensure!(buffer.len() <= 1024 * 1024, "Zigbee response too large");
                        let value: Value = serde_json::from_slice(&buffer)?;
                        buffer.clear();
                        if value["event"] == "devices" {
                            update_snapshot(value["snapshot"].clone())?;
                        } else if pending.as_ref().is_some_and(|(id, _)| value["id"].as_u64() == Some(*id)) {
                            let (_, reply) = pending.take().unwrap();
                            let result = if let Some(error) = value["error"].as_str() {
                                Err(anyhow::anyhow!("{}", error))
                            } else { Ok(value["result"].clone()) };
                            let _ = reply.send(result);
                        }
                    }
                }
            }
            input.shutdown().await?;
            let _ = timeout(Duration::from_secs(5), child.wait()).await;
            if let Some(reply) = shutdown_reply.take() { let _ = reply.send(Ok(Value::Null)); }
            Ok(())
        }.await;
        if let Err(error) = result {
            status(&format!(
                "Zigbee disconnected: {error:#}. Press Discover to reconnect."
            ));
            if let Some((_, reply)) = pending {
                let _ = reply.send(Err(error));
            }
        }
        // kill_on_drop closes a crashed or hung helper and releases the serial port.
    });
    let session = Session { tx };
    *current = Some(session.clone());
    Ok(session)
}

fn update_snapshot(value: Value) -> Result<()> {
    let mut state: ZigbeeSnapshot = serde_json::from_value(value)?;
    ensure!(state.devices.len() <= 1024, "Too many Zigbee devices");
    for device in &mut state.devices {
        validate_identity(&device.ieee_address, device.endpoint)?;
        device.name = device.name.chars().take(256).collect();
    }
    STATE.send_replace(state);
    Ok(())
}

async fn request(value: Value, limit: Duration) -> Result<Value> {
    let result = timeout(limit, async {
        let session = session().await?;
        let (reply, result) = oneshot::channel();
        session
            .tx
            .send(Request { value, reply })
            .await
            .context("Zigbee helper disconnected")?;
        result.await.context("Zigbee helper disconnected")?
    })
    .await
    .context("Zigbee operation timed out")?;
    if let Err(error) = &result {
        status(&format!("{error:#}"));
    }
    result
}

pub async fn discover() -> Result<()> {
    status("Finding USB Zigbee coordinator…");
    request(json!({"op":"discover"}), Duration::from_secs(420)).await?;
    Ok(())
}
pub async fn connect(coordinator: &str) -> Result<()> {
    request(
        json!({"op":"connect", "coordinator":coordinator}),
        Duration::from_secs(420),
    )
    .await?;
    Ok(())
}
// Zigbee runs the fade locally between sparse targets; WLED continues to
// receive the common RGB frames directly. These durations stay matched so
// continuous changes do not become a sequence of jumps or pauses.
const UPDATE_INTERVAL: Duration = Duration::from_millis(500);
const TRANSITION_MS: u32 = 500;
struct ColorTarget {
    rgb: [u8; 3],
    transition_ms: u32,
    force: bool,
}
pub async fn color(
    coordinator: &str,
    ieee: &str,
    endpoint: u8,
    rgb: [u8; 3],
    produced: std::time::Instant,
) -> Result<bool> {
    apply_target(
        coordinator,
        ieee,
        endpoint,
        ColorTarget {
            rgb,
            transition_ms: TRANSITION_MS,
            force: true,
        },
        produced,
    )
    .await
}
async fn apply_target(
    coordinator: &str,
    ieee: &str,
    endpoint: u8,
    target: ColorTarget,
    produced: std::time::Instant,
) -> Result<bool> {
    validate_identity(ieee, endpoint)?;
    if produced.elapsed() >= Duration::from_secs(2) {
        return Ok(false);
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs_f64()
        - produced.elapsed().as_secs_f64();
    let result = request(
        json!({"op":"color", "coordinator":coordinator,
        "ieee_address":ieee, "endpoint":endpoint, "rgb":target.rgb,
        "transition_ms":target.transition_ms, "force":target.force, "produced":timestamp}),
        Duration::from_secs(20),
    )
    .await?;
    Ok(result["applied"] == true)
}
pub async fn shutdown() {
    // Closing the window must also interrupt a first-use package installation
    // or a radio connection still starting up. Runtime teardown then drops
    // kill_on_drop children and releases their USB handles.
    let _ = timeout(Duration::from_secs(6), async {
        if let Some(session) = SESSION.lock().await.take() {
            let (reply, result) = oneshot::channel();
            if session
                .tx
                .send(Request {
                    value: json!({"op":"shutdown"}),
                    reply,
                })
                .await
                .is_ok()
            {
                let _ = result.await;
            }
        }
    })
    .await;
}
pub fn validate_identity(ieee: &str, endpoint: u8) -> Result<()> {
    ensure!(
        ieee.len() == 23
            && ieee.split(':').count() == 8
            && ieee
                .split(':')
                .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()))
            && (1..=240).contains(&endpoint),
        "Invalid Zigbee device identity"
    );
    Ok(())
}

pub fn spawn_output(
    route: crate::core::Route,
    mut frames: watch::Receiver<Option<crate::outputs::OutputFrame>>,
    mut stop: watch::Receiver<bool>,
    output_status: watch::Sender<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let crate::core::Route::Zigbee {
            coordinator,
            ieee_address,
            endpoint,
        } = route
        else {
            return;
        };
        let work = async {
            loop {
                match connect(&coordinator).await {
                    Ok(()) => break,
                    Err(error) => {
                        output_status.send_replace(format!("Zigbee: {error:#}; retrying"));
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
            let mut cadence = tokio::time::interval(UPDATE_INTERVAL);
            cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut sent = None;
            loop {
                cadence.tick().await;
                let frame = frames.borrow_and_update().clone();
                if let Some(frame) = frame.filter(|f| f.produced.elapsed() < Duration::from_secs(2))
                {
                    if let Some(&rgb) = frame.colors.first()
                        && sent != Some(rgb)
                    {
                        match apply_target(
                            &coordinator,
                            &ieee_address,
                            endpoint,
                            ColorTarget {
                                rgb,
                                transition_ms: TRANSITION_MS,
                                force: sent.is_none(),
                            },
                            frame.produced,
                        )
                        .await
                        {
                            Ok(true) => {
                                sent = Some(rgb);
                                output_status.send_replace("Zigbee color applied".into());
                            }
                            Ok(false) => {
                                output_status.send_replace("Waiting for fresh color".into());
                            }
                            Err(error) => {
                                sent = None;
                                output_status.send_replace(format!("Zigbee: {error:#}"));
                            }
                        }
                    }
                } else {
                    sent = None;
                    output_status.send_replace("Waiting for fresh color".into());
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        };
        if !*stop.borrow() {
            tokio::select! {
                biased;
                _ = stop.changed() => {},
                result = work => { if let Err(error) = result { output_status.send_replace(format!("Zigbee: {error:#}")); } return; }
            }
        }
        output_status.send_replace("Stopped · USB coordinator stays connected".into());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovered_identities_are_bounded_and_validated() {
        assert!(validate_identity("00:11:22:33:44:55:66:77", 1).is_ok());
        for id in ["", "00:11:22:33:44:55:66:gg", "0011223344556677"] {
            assert!(validate_identity(id, 1).is_err());
        }
        assert!(validate_identity("00:11:22:33:44:55:66:77", 0).is_err());
    }
}
