use crate::{
    capturer::{Area, Options, Point, Resolution, Size},
    frame::{BGRAFrame, Frame},
    targets::{self, Target},
};
use std::cmp;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows_capture::capture::Context;
use windows_capture::{
    capture::{CaptureControl, GraphicsCaptureApiHandler},
    frame::Frame as WCFrame,
    graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl},
    monitor::Monitor as WCMonitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings as WCSettings,
    },
    window::Window as WCWindow,
};

#[derive(Debug)]
struct Capturer {
    pub tx: mpsc::SyncSender<Frame>,
    pub crop: Option<Area>,
}

#[derive(Clone)]
enum Settings {
    Window(WCSettings<FlagStruct, WCWindow>),
    Display(WCSettings<FlagStruct, WCMonitor>),
}

pub struct WCStream {
    settings: Settings,
    capture_control: Option<CaptureControl<Capturer, Box<dyn std::error::Error + Send + Sync>>>,
}

impl GraphicsCaptureApiHandler for Capturer {
    type Flags = FlagStruct;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(context: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            tx: context.flags.tx,
            crop: context.flags.crop,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WCFrame,
        _: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let mut buffer = if let Some(area) = &self.crop {
            // A window can shrink between selection and this callback. Validate
            // against the current frame, not the original GetWindowRect value.
            let bounds = [
                area.origin.x,
                area.origin.y,
                area.size.width,
                area.size.height,
            ];
            if bounds.iter().any(|value| !value.is_finite()) {
                return Ok(());
            }
            let start_x = (area.origin.x.max(0.0) as u32).min(frame.width());
            let start_y = (area.origin.y.max(0.0) as u32).min(frame.height());
            let end_x = ((area.origin.x + area.size.width).max(0.0) as u32).min(frame.width());
            let end_y = ((area.origin.y + area.size.height).max(0.0) as u32).min(frame.height());
            if start_x >= end_x || start_y >= end_y {
                return Ok(());
            }
            frame.buffer_crop(start_x, start_y, end_x, end_y)?
        } else {
            frame.buffer()?
        };
        let width = i32::try_from(buffer.width())?;
        let height = i32::try_from(buffer.height())?;
        if width == 0 || height == 0 {
            return Ok(());
        }
        let data = buffer.as_nopadding_buffer()?.to_vec();
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|size| size.checked_mul(4));
        if expected != Some(data.len()) {
            return Err("Invalid packed BGRA buffer length".into());
        }
        let frame = BGRAFrame {
            display_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            width,
            height,
            data,
        };
        let _ = self.tx.try_send(Frame::BGRA(frame));
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        println!("Closed");
        Ok(())
    }
}

impl WCStream {
    pub fn start_capture(&mut self) {
        let cc = match &self.settings {
            Settings::Display(st) => Capturer::start_free_threaded(st.to_owned()).unwrap(),
            Settings::Window(st) => Capturer::start_free_threaded(st.to_owned()).unwrap(),
        };

        self.capture_control = Some(cc)
    }

    pub fn stop_capture(&mut self) {
        if let Some(capture_control) = self.capture_control.take() {
            let _ = capture_control.stop();
        }
    }
}

#[derive(Clone, Debug)]
struct FlagStruct {
    pub tx: mpsc::SyncSender<Frame>,
    pub crop: Option<Area>,
}

pub fn create_capturer(options: &Options, tx: mpsc::SyncSender<Frame>) -> WCStream {
    let target = options
        .target
        .clone()
        .unwrap_or_else(|| Target::Display(targets::get_main_display()));

    // This backend emits Frame::BGRA; request that exact native format.
    let color_format = ColorFormat::Bgra8;
    let update_interval = if options.fps > 0
        && GraphicsCaptureApi::is_minimum_update_interval_supported().unwrap_or(false)
    {
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / options.fps as f64))
    } else {
        MinimumUpdateIntervalSettings::Default
    };

    let show_cursor = match options.show_cursor {
        true => CursorCaptureSettings::WithCursor,
        false => CursorCaptureSettings::WithoutCursor,
    };

    let settings = match target {
        Target::Display(display) => Settings::Display(WCSettings::new(
            WCMonitor::from_raw_hmonitor(display.raw_handle.0),
            show_cursor,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            update_interval,
            DirtyRegionSettings::Default,
            color_format,
            FlagStruct {
                tx,
                crop: options.crop_area.as_ref().map(|_| get_crop_area(options)),
            },
        )),
        Target::Window(window) => Settings::Window(WCSettings::new(
            WCWindow::from_raw_hwnd(window.raw_handle.0),
            show_cursor,
            DrawBorderSettings::Default,
            SecondaryWindowSettings::Default,
            update_interval,
            DirtyRegionSettings::Default,
            color_format,
            FlagStruct {
                tx,
                crop: options.crop_area.as_ref().map(|_| get_crop_area(options)),
            },
        )),
    };

    WCStream {
        settings,
        capture_control: None,
    }
}

pub fn get_output_frame_size(options: &Options) -> [u32; 2] {
    let crop_area = get_crop_area(options);

    let mut output_width = (crop_area.size.width) as u32;
    let mut output_height = (crop_area.size.height) as u32;

    match options.output_resolution {
        Resolution::Captured => {}
        _ => {
            let [resolved_width, resolved_height] = options
                .output_resolution
                .value((crop_area.size.width as f32) / (crop_area.size.height as f32));
            // 1280 x 853
            output_width = cmp::min(output_width, resolved_width);
            output_height = cmp::min(output_height, resolved_height);
        }
    }

    output_width -= output_width % 2;
    output_height -= output_height % 2;

    [output_width, output_height]
}

fn get_absolute_value(value: f64, scale_factor: f64) -> f64 {
    let value = (value * scale_factor).floor();
    value + value % 2.0
}

pub fn get_crop_area(options: &Options) -> Area {
    let target = options
        .target
        .clone()
        .unwrap_or_else(|| Target::Display(targets::get_main_display()));

    let (width, height) = targets::get_target_dimensions(&target);

    let scale_factor = targets::get_scale_factor(&target);
    options
        .crop_area
        .as_ref()
        .map(|val| {
            // WINDOWS: limit values [input-width, input-height] = [146, 50]
            Area {
                origin: Point {
                    x: get_absolute_value(val.origin.x, scale_factor),
                    y: get_absolute_value(val.origin.y, scale_factor),
                },
                size: Size {
                    width: get_absolute_value(val.size.width, scale_factor),
                    height: get_absolute_value(val.size.height, scale_factor),
                },
            }
        })
        .unwrap_or_else(|| Area {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: width as f64,
                height: height as f64,
            },
        })
}
