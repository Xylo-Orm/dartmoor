use std::{
    mem::size_of,
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    sync::{
        atomic::{AtomicBool, AtomicU8},
        mpsc::{sync_channel, SyncSender},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use pipewire as pw;
use pw::{
    context::ContextBox as Context,
    main_loop::MainLoopBox as MainLoop,
    properties::properties,
    spa::{
        self,
        param::{
            format::{FormatProperties, MediaSubtype, MediaType},
            video::VideoFormat,
            ParamType,
        },
        pod::{Pod, Property},
        sys::{
            spa_buffer, spa_meta_header, SPA_META_Header, SPA_PARAM_META_size, SPA_PARAM_META_type,
        },
        utils::{Direction, SpaTypes},
    },
    stream::{Stream as StreamRef, StreamState},
};

use crate::{
    capturer::Options,
    frame::{BGRxFrame, Frame, RGBFrame, RGBxFrame, XBGRFrame},
};

use self::{error::LinCapError, portal::ScreenCastPortal};
use super::FrameSender;

mod error;
mod portal;

#[derive(Default)]
struct CaptureState {
    // 0: waiting for start, 1: capturing, 2: stopped (terminal).
    phase: AtomicU8,
    failed: AtomicBool,
}

#[derive(Clone)]
struct ListenerUserData {
    pub tx: FrameSender,
    pub format: spa::param::video::VideoInfoRaw,
    state: Arc<CaptureState>,
    processor: Option<crate::capturer::MappedFrameProcessor>,
    interval: Duration,
    next_frame: Option<Instant>,
    metrics: Metrics,
}

#[derive(Clone)]
struct Metrics {
    since: Instant,
    received: u64,
    delivered: u64,
    processing: Duration,
}

impl Metrics {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            received: 0,
            delivered: 0,
            processing: Duration::ZERO,
        }
    }
    fn report(&mut self) {
        let elapsed = self.since.elapsed().as_secs_f64();
        if elapsed >= 5.0 {
            eprintln!("pipewire: incoming {:.1} fps, delivered {:.1} fps, buffer processing {:.3} ms/frame", self.received as f64 / elapsed, self.delivered as f64 / elapsed, self.processing.as_secs_f64() * 1000.0 / self.delivered.max(1) as f64);
            *self = Self::new();
        }
    }
}

fn frame_due(deadline: Option<Instant>, now: Instant, interval: Duration) -> bool {
    // Small callback jitter must not turn a negotiated 30 FPS stream into
    // 15 FPS. Accumulate deadlines rather than resetting them on every frame.
    deadline.is_none_or(|deadline| now + interval.min(Duration::from_millis(1)) >= deadline)
}

fn next_deadline(deadline: Option<Instant>, now: Instant, interval: Duration) -> Instant {
    deadline.map_or(now + interval, |deadline| {
        let next = deadline + interval;
        if next <= now {
            now + interval
        } else {
            next
        }
    })
}

fn param_changed_callback(
    _stream: &StreamRef,
    user_data: &mut ListenerUserData,
    id: u32,
    param: Option<&Pod>,
) {
    let Some(param) = param else {
        return;
    };
    if id != pw::spa::param::ParamType::Format.as_raw() {
        return;
    }
    let (media_type, media_subtype) = match pw::spa::param::format_utils::parse_format(param) {
        Ok(v) => v,
        Err(_) => return,
    };

    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
        return;
    }

    if user_data.format.parse(param).is_err() {
        user_data
            .state
            .failed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    } else {
        let size = user_data.format.size();
        eprintln!("pipewire: negotiated {}x{} {:?}, framerate {:?}, max {:?}; mapped-buffer downsampling={}", size.width, size.height, user_data.format.format(), user_data.format.framerate(), user_data.format.max_framerate(), user_data.processor.is_some());
    }
}

fn state_changed_callback(
    _stream: &StreamRef,
    user_data: &mut ListenerUserData,
    _old: StreamState,
    new: StreamState,
) {
    if let StreamState::Error(e) = new {
        eprintln!("pipewire: State changed to error({e})");
        user_data
            .state
            .failed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

unsafe fn get_timestamp(buffer: *mut spa_buffer) -> i64 {
    if (*buffer).metas.is_null() {
        return 0;
    }
    for index in 0..(*buffer).n_metas as usize {
        let meta = &*(*buffer).metas.add(index);
        if meta.type_ == SPA_META_Header
            && !meta.data.is_null()
            && meta.size as usize >= size_of::<spa_meta_header>()
        {
            return std::ptr::read_unaligned(meta.data.cast::<spa_meta_header>()).pts;
        }
    }
    0
}

/// Validate the complete mapped region before copying any row. PipeWire chunks
/// may include an offset and row padding; callers require tightly packed pixels.
fn packed_rows(
    mapped: &[u8],
    offset: usize,
    chunk_size: usize,
    stride: i32,
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
) -> Option<Vec<u8>> {
    let mapped = validated_rows(
        mapped,
        offset,
        chunk_size,
        stride,
        width,
        height,
        bytes_per_pixel,
    )?;
    let row_bytes = width.checked_mul(bytes_per_pixel)?;
    let mut result = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for row in 0..height {
        let start = row * stride as usize;
        result.extend_from_slice(&mapped[start..start + row_bytes]);
    }
    Some(result)
}

fn validated_rows(
    mapped: &[u8],
    offset: usize,
    chunk_size: usize,
    stride: i32,
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
) -> Option<&[u8]> {
    if width == 0 || height == 0 || stride <= 0 {
        return None;
    }
    let row_bytes = width.checked_mul(bytes_per_pixel)?;
    let stride = stride as usize;
    if stride < row_bytes {
        return None;
    }
    let span = (height - 1).checked_mul(stride)?.checked_add(row_bytes)?;
    if span > chunk_size || offset.checked_add(span)? > mapped.len() {
        return None;
    }
    Some(&mapped[offset..offset + span])
}

fn process_callback(stream: &StreamRef, user_data: &mut ListenerUserData) {
    let pw_buffer = unsafe { stream.dequeue_raw_buffer() };
    if pw_buffer.is_null() {
        return;
    }
    // Every successfully dequeued buffer must be returned, including malformed
    // or unsupported buffers. Never queue a null pointer.
    let now = Instant::now();
    user_data.metrics.received += 1;
    user_data.metrics.report();
    if !frame_due(user_data.next_frame, now, user_data.interval) {
        unsafe { stream.queue_raw_buffer(pw_buffer) };
        return;
    }
    let sent = user_data.tx.try_send_with(|| unsafe {
        let buffer = (*pw_buffer).buffer;
        if buffer.is_null() || (*buffer).n_datas < 1 || (*buffer).datas.is_null() {
            return None;
        }
        let data = &*(*buffer).datas;
        if data.data.is_null() || data.chunk.is_null() || data.maxsize == 0 {
            return None;
        }
        let chunk = &*data.chunk;
        let size = user_data.format.size();
        if size.width > i32::MAX as u32 || size.height > i32::MAX as u32 {
            return None;
        }
        let format = user_data.format.format();
        let bpp = match format {
            VideoFormat::RGB => 3,
            VideoFormat::RGBx | VideoFormat::xBGR | VideoFormat::BGRx => 4,
            _ => return None,
        };
        let mapped = std::slice::from_raw_parts(data.data.cast::<u8>(), data.maxsize as usize);
        let timestamp = get_timestamp(buffer).max(0) as u64;
        if let Some(processor) = user_data.processor {
            let rows = validated_rows(
                mapped,
                chunk.offset as usize,
                chunk.size as usize,
                chunk.stride,
                size.width as usize,
                size.height as usize,
                bpp,
            )?;
            let order = match format {
                VideoFormat::BGRx => [2, 1, 0],
                VideoFormat::xBGR => [3, 2, 1],
                _ => [0, 1, 2],
            };
            let mut frame = processor(
                rows,
                size.width as i32,
                size.height as i32,
                bpp,
                order,
                chunk.stride as usize,
            )?;
            if let Frame::RGB(ref mut rgb) = frame {
                rgb.display_time = timestamp;
            }
            return Some(frame);
        }
        let pixels = packed_rows(
            mapped,
            chunk.offset as usize,
            chunk.size as usize,
            chunk.stride,
            size.width as usize,
            size.height as usize,
            bpp,
        )?;
        let width = size.width as i32;
        let height = size.height as i32;
        Some(match format {
            VideoFormat::RGB => Frame::RGB(RGBFrame {
                display_time: timestamp,
                width,
                height,
                data: pixels,
            }),
            VideoFormat::RGBx => Frame::RGBx(RGBxFrame {
                display_time: timestamp,
                width,
                height,
                data: pixels,
            }),
            VideoFormat::xBGR => Frame::XBGR(XBGRFrame {
                display_time: timestamp,
                width,
                height,
                data: pixels,
            }),
            VideoFormat::BGRx => Frame::BGRx(BGRxFrame {
                display_time: timestamp,
                width,
                height,
                data: pixels,
            }),
            _ => return None,
        })
    });
    unsafe { stream.queue_raw_buffer(pw_buffer) };
    if sent {
        user_data.next_frame = Some(next_deadline(user_data.next_frame, now, user_data.interval));
        user_data.metrics.delivered += 1;
        user_data.metrics.processing += now.elapsed();
    }
}

// TODO: Format negotiation
fn pipewire_capturer(
    options: Options,
    tx: FrameSender,
    ready_sender: &SyncSender<bool>,
    stream_id: u32,
    state: Arc<CaptureState>,
    remote: OwnedFd,
) -> Result<(), LinCapError> {
    pw::init();

    let mainloop = MainLoop::new(None)?;
    let context = Context::new(mainloop.loop_(), None)?;
    let core = context.connect_fd(remote, None)?;

    let user_data = ListenerUserData {
        tx,
        format: Default::default(),
        state: state.clone(),
        processor: options.mapped_frame_processor,
        interval: Duration::from_secs_f64(1.0 / options.fps.max(1) as f64),
        next_frame: None,
        metrics: Metrics::new(),
    };

    let stream = pw::stream::StreamBox::new(
        &core,
        "scap",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let _listener = stream
        .add_local_listener_with_user_data(user_data.clone())
        .state_changed(state_changed_callback)
        .param_changed(param_changed_callback)
        .process(process_callback)
        .register()?;

    let obj = capture_format(options.fps);

    let metas_obj = pw::spa::pod::object!(
        SpaTypes::ObjectParamMeta,
        ParamType::Meta,
        Property::new(
            SPA_PARAM_META_type,
            pw::spa::pod::Value::Id(pw::spa::utils::Id(SPA_META_Header))
        ),
        Property::new(
            SPA_PARAM_META_size,
            pw::spa::pod::Value::Int(size_of::<pw::spa::sys::spa_meta_header>() as i32)
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner();
    let metas_values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(metas_obj),
    )?
    .0
    .into_inner();

    let mut params = [
        pw::spa::pod::Pod::from_bytes(&values).unwrap(),
        pw::spa::pod::Pod::from_bytes(&metas_values).unwrap(),
    ];

    stream.connect(
        Direction::Input,
        Some(stream_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    ready_sender.send(true)?;

    while state.phase.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        std::thread::sleep(Duration::from_millis(10));
    }

    let pw_loop = mainloop.loop_();

    while state.phase.load(std::sync::atomic::Ordering::Relaxed) == 1
        && !state.failed.load(std::sync::atomic::Ordering::Relaxed)
    {
        pw_loop.iterate(pw::loop_::Timeout::Finite(Duration::from_millis(100)));
    }

    Ok(())
}

fn capture_format(fps: u32) -> spa::pod::Object {
    pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pw::spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pw::spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRx,
            pw::spa::param::video::VideoFormat::xBGR,
            pw::spa::param::video::VideoFormat::RGBx,
            pw::spa::param::video::VideoFormat::RGB,
        ),
        pw::spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                // Default
                width: 128,
                height: 128,
            },
            pw::spa::utils::Rectangle {
                // Min
                width: 1,
                height: 1,
            },
            pw::spa::utils::Rectangle {
                // Max
                width: 16384,
                height: 16384,
            }
        ),
        pw::spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction {
                num: fps.max(1),
                denom: 1
            },
            pw::spa::utils::Fraction { num: 0, denom: 1 },
            pw::spa::utils::Fraction {
                num: fps.max(1),
                denom: 1
            }
        ),
        // GNOME uses variable-rate streams (framerate=0/1), and reads this
        // separate property to throttle compositor capture/readback work.
        pw::spa::pod::property!(
            FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            pw::spa::utils::Fraction {
                num: fps.max(1),
                denom: 1
            },
            pw::spa::utils::Fraction { num: 1, denom: 1 },
            pw::spa::utils::Fraction {
                num: fps.max(1),
                denom: 1
            }
        ),
    )
}

pub struct LinuxCapturer {
    capturer_join_handle: Option<JoinHandle<Result<(), LinCapError>>>,
    state: Arc<CaptureState>,
    session: dbus::Path<'static>,
    connection: dbus::blocking::Connection,
}

impl LinuxCapturer {
    pub fn new(options: &Options, tx: FrameSender) -> Result<Self, LinCapError> {
        let connection = dbus::blocking::Connection::new_session()?;
        let (stream, session, remote) = ScreenCastPortal::new(&connection)
            .show_cursor(options.show_cursor)?
            .create_stream()?;
        let stream_id = stream.pw_node_id();
        // Transfer ownership once from dbus-rs to the standard OwnedFd.
        let remote = unsafe { OwnedFd::from_raw_fd(remote.into_raw_fd()) };
        let options = options.clone();
        let state = Arc::new(CaptureState::default());
        let worker_state = state.clone();
        let (ready_sender, ready_recv) = sync_channel(1);
        let capturer_join_handle = std::thread::spawn(move || {
            let res =
                pipewire_capturer(options, tx, &ready_sender, stream_id, worker_state, remote);
            if res.is_err() {
                let _ = ready_sender.try_send(false);
            }
            res
        });
        let mut capturer = Self {
            capturer_join_handle: Some(capturer_join_handle),
            state,
            session,
            connection,
        };
        if ready_recv.recv() != Ok(true) {
            capturer
                .state
                .phase
                .store(2, std::sync::atomic::Ordering::Relaxed);
            let error = match capturer.capturer_join_handle.take().unwrap().join() {
                Ok(Err(error)) => error,
                _ => LinCapError::new("Capture worker failed during setup".into()),
            };
            return Err(error);
        }
        Ok(capturer)
    }

    pub fn start_capture(&self) {
        let _ = self.state.phase.compare_exchange(
            0,
            1,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn stop_capture(&mut self) {
        self.state
            .phase
            .store(2, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.capturer_join_handle.take() {
            if let Ok(Err(error)) = handle.join() {
                eprintln!("Error capturing: {error}");
            }
        }
    }
}

impl Drop for LinuxCapturer {
    fn drop(&mut self) {
        self.stop_capture();
        ScreenCastPortal::close_session(&self.connection, self.session.clone());
    }
}

pub fn create_capturer(options: &Options, tx: FrameSender) -> Result<LinuxCapturer, LinCapError> {
    LinuxCapturer::new(options, tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_caps_fixed_and_variable_frame_rates() {
        use spa::pod::{ChoiceValue, Value};
        for fps in [1, 30, 60] {
            let object = capture_format(fps);
            for key in [
                FormatProperties::VideoFramerate,
                FormatProperties::VideoMaxFramerate,
            ] {
                let property = object
                    .properties
                    .iter()
                    .find(|p| p.key == key.as_raw())
                    .unwrap();
                let Value::Choice(ChoiceValue::Fraction(choice)) = &property.value else {
                    panic!("expected fraction range")
                };
                let spa::utils::ChoiceEnum::Range { default, min, max } = choice.1 else {
                    panic!("expected bounded range")
                };
                assert_eq!(default.num, fps);
                assert_eq!(max.num, fps);
                assert_eq!(max.denom, 1);
                assert!(min.num <= fps);
            }
        }
    }

    #[test]
    fn pacing_tolerates_jitter_without_halving_fps_or_catching_up_after_stalls() {
        let start = Instant::now();
        let interval = Duration::from_secs_f64(1.0 / 30.0);
        let mut deadline = None;
        for i in 0..100 {
            let now = start + interval * i;
            let now = if i % 2 == 1 {
                now - Duration::from_micros(300)
            } else {
                now
            };
            assert!(frame_due(deadline, now, interval));
            deadline = Some(next_deadline(deadline, now, interval));
            assert!(!frame_due(
                deadline,
                now + Duration::from_millis(1),
                interval
            ));
        }
        let later = start + Duration::from_secs(10);
        assert!(frame_due(deadline, later, interval));
        assert_eq!(next_deadline(deadline, later, interval), later + interval);
    }

    #[test]
    fn strips_offset_and_row_padding() {
        let mapped = [99, 1, 2, 3, 4, 5, 6, 88, 88, 7, 8, 9, 10, 11, 12, 88, 88];
        assert_eq!(
            packed_rows(&mapped, 1, 16, 8, 2, 2, 3),
            Some((1..=12).collect())
        );
    }

    #[test]
    fn rejects_truncated_or_invalid_layouts() {
        let data = [0; 16];
        assert!(packed_rows(&data, 0, 8, 8, 2, 2, 4).is_none());
        assert!(packed_rows(&data, 1, 16, 8, 2, 2, 4).is_none());
        assert!(packed_rows(&data, 0, 16, 7, 2, 2, 4).is_none());
        assert!(packed_rows(&data, 0, 16, -8, 2, 2, 4).is_none());
        assert!(packed_rows(&data, 0, 16, 8, usize::MAX, 2, 4).is_none());
        assert!(packed_rows(&data, 0, 16, 8, 2, 0, 4).is_none());
    }
}
