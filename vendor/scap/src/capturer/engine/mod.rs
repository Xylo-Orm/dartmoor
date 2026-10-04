#[cfg(not(target_os = "macos"))]
use std::sync::mpsc;

use super::{CapturerBuildError, Options};
use crate::frame::Frame;

#[cfg(target_os = "macos")]
pub mod mac;

#[cfg(any(target_os = "macos", test))]
mod mac_frame_state;
#[cfg(any(target_os = "macos", test))]
mod mac_mailbox;

#[cfg(target_os = "windows")]
mod win;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
pub type ChannelItem = (
    screencapturekit::cm_sample_buffer::CMSampleBuffer,
    screencapturekit::sc_output_handler::SCStreamOutputType,
);
#[cfg(not(target_os = "macos"))]
pub type ChannelItem = Frame;

#[cfg(target_os = "macos")]
pub type FrameSender = mac_mailbox::Sender<ChannelItem>;
#[cfg(target_os = "macos")]
pub type FrameReceiver = mac_mailbox::Receiver<ChannelItem>;
#[cfg(not(target_os = "macos"))]
pub type FrameSender = mpsc::SyncSender<ChannelItem>;
#[cfg(not(target_os = "macos"))]
pub type FrameReceiver = mpsc::Receiver<ChannelItem>;

pub(crate) fn channel() -> (FrameSender, FrameReceiver) {
    #[cfg(target_os = "macos")]
    {
        mac_mailbox::channel()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mpsc::sync_channel(1)
    }
}

pub fn get_output_frame_size(options: &Options) -> [u32; 2] {
    #[cfg(target_os = "macos")]
    {
        mac::get_output_frame_size(options)
    }

    #[cfg(target_os = "windows")]
    {
        win::get_output_frame_size(options)
    }

    #[cfg(target_os = "linux")]
    {
        let _ = options;
        // TODO: How to calculate this on Linux?
        [0, 0]
    }
}

pub struct Engine {
    options: Options,
    started: bool,

    #[cfg(target_os = "macos")]
    mac: screencapturekit::sc_stream::SCStream,
    #[cfg(target_os = "macos")]
    error_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(target_os = "macos")]
    image_state: mac_frame_state::ImageState,

    #[cfg(target_os = "windows")]
    win: win::WCStream,

    #[cfg(target_os = "linux")]
    linux: linux::LinuxCapturer,
}

impl Engine {
    pub fn new(options: &Options, tx: FrameSender) -> Result<Engine, CapturerBuildError> {
        #[cfg(target_os = "macos")]
        {
            let error_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mac = mac::create_capturer(options, tx, error_flag.clone());

            Ok(Engine {
                mac,
                error_flag,
                image_state: mac_frame_state::ImageState::default(),
                options: (*options).clone(),
                started: false,
            })
        }

        #[cfg(target_os = "windows")]
        {
            let win = win::create_capturer(options, tx);
            Ok(Engine {
                win,
                options: (*options).clone(),
                started: false,
            })
        }

        #[cfg(target_os = "linux")]
        {
            let linux = linux::create_capturer(options, tx)
                .map_err(|error| CapturerBuildError::Backend(error.to_string()))?;
            Ok(Engine {
                linux,
                options: (*options).clone(),
                started: false,
            })
        }
    }

    pub fn start(&mut self) {
        if self.started {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            // self.mac.add_output(Capturer::new(tx));
            self.mac.start_capture().expect("Failed to start capture");
        }

        #[cfg(target_os = "windows")]
        {
            self.win.start_capture();
        }

        #[cfg(target_os = "linux")]
        {
            self.linux.start_capture();
        }
        self.started = true;
    }

    pub fn stop(&mut self) {
        if !self.started {
            return;
        }
        self.started = false;
        #[cfg(target_os = "macos")]
        {
            let _ = self.mac.stop_capture();
        }

        #[cfg(target_os = "windows")]
        {
            self.win.stop_capture();
        }

        #[cfg(target_os = "linux")]
        {
            self.linux.stop_capture();
        }
    }

    pub fn get_output_frame_size(&mut self) -> [u32; 2] {
        get_output_frame_size(&self.options)
    }

    pub(crate) fn has_backend_error(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            let failed = self.error_flag.load(std::sync::atomic::Ordering::Relaxed);
            if failed {
                self.image_state.invalidate();
            }
            failed
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn process_channel_item(&self, data: ChannelItem) -> Option<Frame> {
        #[cfg(target_os = "macos")]
        {
            mac::process_sample_buffer(data.0, data.1, self.options.output_type, &self.image_state)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(data)
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}
