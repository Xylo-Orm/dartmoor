//! Native PipeWire playback-monitor capture. All objects live on the worker.
use super::{AudioSource, CHANNELS, MAX_SAMPLES, RATE};
use anyhow::{Context, Result, ensure};
use pipewire::{self as pw, properties::properties, spa};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

struct Buffer {
    samples: VecDeque<f32>,
    error: Option<String>,
    format_ready: bool,
    discontinuity: bool,
    received_at: Option<Instant>,
}
pub(super) struct Playback {
    // Unregister the callback before dropping its stream and loop.
    _listener: pw::stream::StreamListener<Rc<RefCell<Buffer>>>,
    stream: pw::stream::StreamRc,
    main_loop: pw::main_loop::MainLoopRc,
    buffer: Rc<RefCell<Buffer>>,
    last_queue_age: Option<Duration>,
}
impl Playback {
    pub fn open(device: &str) -> Result<Self> {
        pw::init();
        let main_loop = pw::main_loop::MainLoopRc::new(None).context("create audio loop")?;
        let context = pw::context::ContextRc::new(&main_loop, None)?;
        let core = context
            .connect_rc(None)
            .context("connect to PipeWire for playback capture")?;
        let mut props = properties! {
            "media.type" => "Audio", "media.category" => "Capture", "media.role" => "Music",
            "application.name" => "Dartmoor", "node.name" => "dartmoor-music",
            "stream.capture.sink" => "true", "node.latency" => "480/48000",
        };
        if !device.is_empty() {
            props.insert("target.object", device);
        }
        let stream = pw::stream::StreamRc::new(core, "Dartmoor playback monitor", props)?;
        let buffer = Rc::new(RefCell::new(Buffer {
            samples: VecDeque::with_capacity(MAX_SAMPLES),
            error: None,
            format_ready: false,
            discontinuity: false,
            received_at: None,
        }));
        let listener = stream
            .add_local_listener_with_user_data(buffer.clone())
            .state_changed(|_, data, _, state| {
                if let pw::stream::StreamState::Error(error) = state {
                    data.borrow_mut().error = Some(format!("PipeWire audio error: {error}"));
                }
            })
            .param_changed(|_, data, id, param| {
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let mut state = data.borrow_mut();
                let had_format = state.format_ready;
                state.samples.clear();
                state.received_at = None;
                state.discontinuity |= had_format;
                state.format_ready = false;
                let Some(param) = param else {
                    return;
                };
                let mut format = spa::param::audio::AudioInfoRaw::new();
                if format.parse(param).is_err()
                    || format.rate() != RATE as u32
                    || format.channels() != CHANNELS as u32
                    || format.format() != spa::param::audio::AudioFormat::F32LE
                {
                    state.error = Some("Playback capture requires stereo F32 at 48 kHz".into());
                } else {
                    state.format_ready = true;
                }
            })
            .process(|stream, data| {
                let Some(mut block) = stream.dequeue_buffer() else {
                    return;
                };
                let mut state = data.borrow_mut();
                if !state.format_ready {
                    return;
                }
                let Some(data) = block.datas_mut().first_mut() else {
                    return;
                };
                let offset = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
                let stride = data.chunk().stride();
                let Some(mapped) = data.data() else {
                    return;
                };
                if let Some(bytes) = audio_bytes(mapped, offset, size, stride) {
                    if state.samples.is_empty() && !bytes.is_empty() {
                        state.received_at = Some(Instant::now());
                    }
                    if size > MAX_SAMPLES * 4 {
                        state.discontinuity = true;
                    }
                    // Keep the latest complete stereo frames when the UI/output
                    // cadence is slower than the audio graph. Capacity never grows.
                    for pair in bytes.as_chunks::<8>().0 {
                        if state.samples.len() + CHANNELS > MAX_SAMPLES {
                            state.discontinuity = true;
                            state.samples.pop_front();
                            state.samples.pop_front();
                        }
                        state
                            .samples
                            .push_back(f32::from_le_bytes(pair[..4].try_into().unwrap()));
                        state
                            .samples
                            .push_back(f32::from_le_bytes(pair[4..].try_into().unwrap()));
                    }
                } else {
                    state.error = Some("Invalid PipeWire audio buffer bounds or stride".into());
                }
            })
            .register()?;
        let mut info = spa::param::audio::AudioInfoRaw::new();
        info.set_format(spa::param::audio::AudioFormat::F32LE);
        info.set_rate(RATE as u32);
        info.set_channels(CHANNELS as u32);
        let object = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        };
        let bytes = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &spa::pod::Value::Object(object),
        )
        .map_err(|error| anyhow::anyhow!("serialize audio format: {error}"))?
        .0
        .into_inner();
        let pod = spa::pod::Pod::from_bytes(&bytes).context("invalid audio format pod")?;
        // No RT_PROCESS: callbacks run on our event loop, with bounded copying;
        // analysis and output rendering happen after returning the graph buffer.
        stream.connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )?;
        Ok(Self {
            _listener: listener,
            stream,
            main_loop,
            buffer,
            last_queue_age: None,
        })
    }
}
impl AudioSource for Playback {
    fn next_samples(&mut self, timeout: Duration) -> Result<Option<Vec<f32>>> {
        self.last_queue_age = None;
        let deadline = Instant::now() + timeout.min(Duration::from_millis(50));
        loop {
            let dispatched = self
                .main_loop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(5)));
            ensure!(dispatched >= 0, "PipeWire audio loop failed");
            let mut buffer = self.buffer.borrow_mut();
            if let Some(error) = buffer.error.take() {
                anyhow::bail!("{error}");
            }
            if !buffer.samples.is_empty() {
                self.last_queue_age = buffer.received_at.take().map(|at| at.elapsed());
                return Ok(Some(buffer.samples.drain(..).collect()));
            }
            if buffer.format_ready && matches!(self.stream.state(), pw::stream::StreamState::Paused)
            {
                // Explicit negotiated idle state means connected silence, unlike
                // a timeout/disconnection. Keep output fresh while fading to zero.
                return Ok(Some(vec![]));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }
    fn take_discontinuity(&mut self) -> bool {
        std::mem::take(&mut self.buffer.borrow_mut().discontinuity)
    }
    fn estimated_queue_age(&self) -> Option<Duration> {
        self.last_queue_age
    }
}

fn audio_bytes(mapped: &[u8], offset: usize, size: usize, stride: i32) -> Option<&[u8]> {
    if !matches!(stride, 0 | 8) || !size.is_multiple_of(8) {
        return None;
    }
    let end = offset.checked_add(size)?;
    let bytes = mapped.get(offset..end)?;
    // Bound callback work even if a server supplies a very large valid buffer.
    Some(&bytes[bytes.len().saturating_sub(MAX_SAMPLES * 4)..])
}

/// Device discovery runs on a background UI job, never in the capture callback.
pub(crate) fn playback_sinks() -> Result<Vec<(String, String)>> {
    let output = std::process::Command::new("pw-dump")
        .output()
        .context("run pw-dump to list PipeWire playback sinks")?;
    ensure!(
        output.status.success(),
        "pw-dump failed while listing playback sinks"
    );
    ensure!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "PipeWire device list is too large"
    );
    parse_playback_sinks(&output.stdout)
}

fn parse_playback_sinks(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let nodes: Vec<serde_json::Value> =
        serde_json::from_slice(bytes).context("parse PipeWire device list")?;
    let mut sinks = Vec::new();
    for node in nodes {
        if node
            .pointer("/info/props/media.class")
            .and_then(serde_json::Value::as_str)
            != Some("Audio/Sink")
        {
            continue;
        }
        let Some(name) = node
            .pointer("/info/props/node.name")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if name.is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
            continue;
        }
        let description = node
            .pointer("/info/props/node.description")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 1024)
            .unwrap_or(name);
        if sinks.len() >= 256 {
            anyhow::bail!("too many PipeWire playback sinks");
        }
        sinks.push((name.to_owned(), description.to_owned()));
    }
    sinks.sort();
    sinks.dedup_by(|a, b| a.0 == b.0);
    Ok(sinks)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_reads_sink_nodes_only() {
        let json = br#"[{"info":{"props":{"media.class":"Audio/Sink","node.name":"sink.one","node.description":"Speakers"}}},{"info":{"props":{"media.class":"Audio/Source","node.name":"mic"}}}]"#;
        assert_eq!(
            parse_playback_sinks(json).unwrap(),
            vec![("sink.one".into(), "Speakers".into())]
        );
    }
    #[test]
    fn audio_buffer_bounds_offsets_and_work_are_bounded() {
        let bytes = vec![0; MAX_SAMPLES * 8 + 16];
        assert_eq!(
            audio_bytes(&bytes, 16, MAX_SAMPLES * 8, 8).unwrap().len(),
            MAX_SAMPLES * 4
        );
        assert!(audio_bytes(&bytes, usize::MAX, 8, 8).is_none());
        assert!(audio_bytes(&bytes, 0, bytes.len() + 8, 8).is_none());
        assert!(audio_bytes(&bytes, 0, 7, 8).is_none());
        assert!(audio_bytes(&bytes, 0, 8, -8).is_none());
    }
}
