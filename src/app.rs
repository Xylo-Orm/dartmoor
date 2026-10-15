mod devices_panel;
mod ha_picker;
mod layout_dialog;
mod layout_panel;
mod music_diagnostics;
mod music_panel;
mod video_panel;
mod workspace;
#[cfg(test)]
mod workspace_tests;
use devices_panel::DeviceView;
use workspace::Workspace;
mod text;
mod transport;
use crate::{
    config::{self, CaptureSelection, Config},
    core::{Light, Point, Route, Shape},
    engine::{Command, Engine, SessionState},
    outputs,
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use std::{
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    time::{Duration, Instant},
};

const ACCENT: Color32 = Color32::from_rgb(102, 190, 237);
const MUTED: Color32 = Color32::from_rgb(149, 166, 185);
const AMBER: Color32 = Color32::from_rgb(239, 189, 110);

fn section_heading(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong());
    });
    ui.add_space(6.0);
}
fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).small().strong().color(color));
        });
}

fn route_label(route: &Route, zones: usize) -> String {
    match route {
        Route::Wled { .. } => format!(
            "WLED · {zones} color {}",
            if zones == 1 { "zone" } else { "zones" }
        ),
        Route::HomeAssistant { .. } => "Home Assistant · ambient".into(),
        Route::Zigbee { .. } => "Zigbee USB · ambient".into(),
        Route::Mock => "Demo output".into(),
    }
}
fn position_controls(ui: &mut egui::Ui, point: &mut Point) {
    ui.horizontal(|ui| {
        ui.label("X");
        ui.add(
            egui::DragValue::new(&mut point.x)
                .speed(0.005)
                .range(0.0..=1.0)
                .fixed_decimals(3),
        );
        ui.label("Y");
        ui.add(
            egui::DragValue::new(&mut point.y)
                .speed(0.005)
                .range(0.0..=1.0)
                .fixed_decimals(3),
        );
    });
}

#[derive(Debug)]
enum JobResult {
    Wled(anyhow::Result<(String, outputs::WledInfo)>),
    WledRoute(String, Route, anyhow::Result<(String, outputs::WledInfo)>),
    Zigbee(anyhow::Result<()>),
    ZigbeeCommand(anyhow::Result<()>),
    Ha(String, anyhow::Result<Vec<outputs::HaLight>>),
    #[cfg(target_os = "linux")]
    AudioDevices(anyhow::Result<Vec<(String, String)>>),
}

fn display_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "Unnamed light".into();
    }
    let mut end = name.len().min(256);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

fn insert_light(config: &mut Config, light: Light) -> anyhow::Result<()> {
    let mut candidate = config.clone();
    candidate.lights.push(light);
    candidate.validate()?;
    *config = candidate;
    Ok(())
}

fn apply_inspection(
    config: &mut Config,
    id: &str,
    expected: &Route,
    route: Route,
) -> anyhow::Result<bool> {
    let Some(index) = config
        .lights
        .iter()
        .position(|l| l.id == id && &l.route == expected)
    else {
        return Ok(false);
    };
    let mut candidate = config.clone();
    candidate.lights[index].route = route;
    candidate.validate()?;
    *config = candidate;
    Ok(true)
}

fn inspected_route(
    expected: &Route,
    host: String,
    info: &outputs::WledInfo,
) -> anyhow::Result<Route> {
    // Refreshing a known controller must not discard a carefully configured
    // segment mapping. A different identity is a new mapping to review.
    let (start, count) = match expected {
        Route::Wled {
            start,
            count,
            device_id,
            ..
        } if !device_id.is_empty()
            && crate::core::canonical_device_id(device_id)
                == crate::core::canonical_device_id(&info.device_id) =>
        {
            anyhow::ensure!(
                start
                    .checked_add(*count)
                    .is_some_and(|end| end <= info.led_count),
                "The saved LED range exceeds this controller’s current LED count. Adjust First LED and LED count, then inspect again."
            );
            (*start, *count)
        }
        _ => (0, info.led_count.min(4096)),
    };
    Ok(Route::Wled {
        host,
        start,
        count,
        device_id: info.device_id.clone(),
    })
}

fn select_desktop(source: &mut CaptureSelection) -> bool {
    if matches!(source, CaptureSelection::Desktop { .. }) {
        return false;
    }
    *source = CaptureSelection::Desktop { id: None };
    true
}

fn interactive_config(mut config: Config) -> Config {
    select_desktop(&mut config.source);
    config
        .lights
        .retain(|light| !matches!(light.route, Route::Mock));
    config.fps = 30;
    config.soft_ambience.enabled = true;
    config
}

fn preview_changed(
    old: &Option<std::sync::Arc<crate::core::Frame>>,
    new: &Option<std::sync::Arc<crate::core::Frame>>,
) -> bool {
    match (old, new) {
        (Some(old), Some(new)) => !std::sync::Arc::ptr_eq(old, new),
        (None, None) => false,
        _ => true,
    }
}

pub struct App {
    config: Config,
    engine: Option<Engine>,
    runtime: Option<tokio::runtime::Runtime>,
    selected: Option<usize>,
    workspace: Workspace,
    music_taps: std::collections::VecDeque<Instant>,
    music_frames: std::collections::VecDeque<crate::music::AnalysisFrame>,
    music_events: std::collections::VecDeque<crate::music::AnalysisEvent>,
    music_last_snapshot: u64,
    #[cfg(target_os = "linux")]
    music_devices: Vec<(String, String)>,
    #[cfg(target_os = "linux")]
    music_devices_pending: bool,
    #[cfg(target_os = "linux")]
    music_device_error: Option<String>,
    selected_point: usize,
    compact_inspector: bool,
    device_view: DeviceView,
    canvas_rect: Option<Rect>,
    history: crate::editor::EditHistory,
    edit_before: Option<Config>,
    history_changed: bool,
    saved_config: Config,
    config_path: std::path::PathBuf,
    styled: bool,
    host: String,
    token: String,
    notice: String,
    texture: Option<egui::TextureHandle>,
    preview: Option<std::sync::Arc<crate::core::Frame>>,
    #[cfg(not(target_os = "linux"))]
    sources: Vec<crate::capture::Source>,
    jobs: Receiver<JobResult>,
    results: SyncSender<JobResult>,
    busy: bool,
    ha_picker: Option<ha_picker::HaPicker>,
    layout_dialog: Option<layout_dialog::LayoutDialog>,
    pending_apply: Option<Config>,
    drag: Option<(usize, usize)>,
    smoke: Option<(Instant, Duration)>,
    smoke_scheduled: bool,
}
impl App {
    pub fn new(smoke: Option<Duration>, real_capture: bool) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| anyhow::anyhow!("Could not start networking: {error}"))?;
        let engine = Engine::new(&runtime);
        let (loaded, warning) = config::load();
        let config = interactive_config(loaded);
        let has_lights = !config.lights.is_empty();
        let (results, jobs) = sync_channel(4);
        let mut app = Self {
            saved_config: config.clone(),
            config_path: config::path(),
            workspace: if has_lights {
                Workspace::from_mode(config.mode)
            } else {
                Workspace::Devices
            },
            music_taps: std::collections::VecDeque::with_capacity(6),
            music_frames: std::collections::VecDeque::with_capacity(180),
            music_events: std::collections::VecDeque::with_capacity(128),
            music_last_snapshot: 0,
            #[cfg(target_os = "linux")]
            music_devices: vec![],
            #[cfg(target_os = "linux")]
            music_devices_pending: false,
            #[cfg(target_os = "linux")]
            music_device_error: None,
            config,
            engine: Some(engine),
            runtime: Some(runtime),
            selected: has_lights.then_some(0),
            selected_point: 0,
            compact_inspector: false,
            device_view: DeviceView::Properties,
            canvas_rect: None,
            history: crate::editor::EditHistory::default(),
            edit_before: None,
            history_changed: false,
            styled: false,
            host: String::new(),
            token: String::new(),
            notice: warning.unwrap_or_default(),
            texture: None,
            preview: None,
            #[cfg(not(target_os = "linux"))]
            sources: vec![],
            results,
            jobs,
            busy: false,
            ha_picker: None,
            layout_dialog: None,
            pending_apply: None,
            drag: None,
            smoke: smoke.map(|d| (Instant::now(), d)),
            smoke_scheduled: false,
        };
        if smoke.is_some() {
            // The explicit CLI smoke probe keeps its synthetic test fixture.
            app.config = Config::default();
            app.workspace = Workspace::from_mode(app.config.mode);
            if real_capture {
                app.config.source = CaptureSelection::Desktop { id: None };
            }
            app.send(Command::Start(app.config.clone()));
        }
        if let Some(coordinator) = app.config.lights.iter().find_map(|light| {
            if let Route::Zigbee { coordinator, .. } = &light.route {
                Some(coordinator.clone())
            } else {
                None
            }
        }) {
            app.runtime.as_ref().unwrap().spawn(async move {
                let _ = crate::zigbee::connect(&coordinator).await;
            });
        }
        Ok(app)
    }
    fn navigate(&mut self, workspace: Workspace) {
        if self.workspace == workspace {
            return;
        }
        if let Some(before) = self.edit_before.take() {
            self.history.checkpoint(&before, &self.config);
        }
        self.workspace = workspace;
        self.device_view = DeviceView::Properties;
        self.drag = None;
        self.canvas_rect = None;
    }
    pub fn set_smoke_workspace(&mut self, name: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.smoke.is_some(),
            "Workspace smoke selection requires --ui-smoke-seconds"
        );
        let workspace = Workspace::ALL
            .into_iter()
            .find(|workspace| workspace.label().eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow::anyhow!("Unknown workspace: {name}"))?;
        self.navigate(workspace);
        Ok(())
    }
    fn send(&mut self, cmd: Command) {
        if matches!(
            cmd,
            Command::Start(_) | Command::Stop | Command::Pause | Command::Shutdown
        ) {
            self.pending_apply = None;
        }
        if let Err(e) = self.engine.as_ref().unwrap().send(cmd) {
            self.notice = e.to_string();
        }
    }
    fn save(&mut self) {
        self.notice = match config::save_to(&self.config, &self.config_path) {
            Ok(()) => {
                self.saved_config = self.config.clone();
                "Configuration saved".into()
            }
            Err(e) => format!("Could not save configuration: {e}"),
        };
    }
    fn add(&mut self, name: String, route: Route, zones: usize, strip: bool) -> bool {
        if self.config.lights.len() >= 64 {
            self.notice = "Layout supports at most 64 lights".into();
            return false;
        }
        let id = format!(
            "light-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let shape = if strip {
            Shape::Strip {
                points: vec![Point { x: 0.1, y: 0.9 }, Point { x: 0.9, y: 0.9 }],
                radius: 0.05,
                segment_leds: vec![],
                reverse: false,
            }
        } else {
            Shape::Bulb {
                center: Point { x: 0.5, y: 0.5 },
                radius: 0.08,
            }
        };
        let light = Light {
            video_response_ms: None,
            id,
            name: display_name(&name),
            shape,
            zones,
            route,
        };
        if let Err(e) = insert_light(&mut self.config, light) {
            self.notice = format!("Could not add light: {e}");
            return false;
        }
        self.selected = Some(self.config.lights.len() - 1);
        self.selected_point = 0;
        true
    }
    fn poll_jobs(&mut self) {
        while let Ok(result) = self.jobs.try_recv() {
            #[cfg(target_os = "linux")]
            if !matches!(&result, JobResult::AudioDevices(_)) {
                self.busy = false;
            }
            #[cfg(not(target_os = "linux"))]
            {
                self.busy = false;
            }
            match result {
                JobResult::ZigbeeCommand(result) => {
                    self.notice = match result {
                        Ok(()) => "Zigbee bulb acknowledged the command.".into(),
                        Err(error) => format!("Zigbee: {error:#}"),
                    };
                }
                JobResult::Zigbee(result) => {
                    self.notice = match result {
                        Ok(()) => {
                            "Zigbee coordinator connected. Select discovered lights below.".into()
                        }
                        Err(error) => format!("Zigbee: {error:#}"),
                    };
                }
                #[cfg(target_os = "linux")]
                JobResult::AudioDevices(result) => {
                    self.music_devices_pending = false;
                    match result {
                        Ok(devices) => {
                            self.music_devices = devices;
                            self.music_device_error = None;
                        }
                        Err(error) => self.music_device_error = Some(error.to_string()),
                    }
                }
                JobResult::Wled(Ok((host, info))) => {
                    let existing = self.config.lights.iter().position(|light| {
                        matches!(&light.route, Route::Wled { device_id, .. }
                            if crate::core::canonical_device_id(device_id)
                                == crate::core::canonical_device_id(&info.device_id))
                    });
                    if let Some(index) = existing {
                        self.selected = Some(index);
                        self.selected_point = 0;
                        self.compact_inspector = true;
                        self.notice = "This WLED controller is already in the layout. Its properties are selected.".into();
                    } else {
                        let count = info.led_count.min(4096);
                        self.notice = format!(
                            "{}: {count} LEDs; segments {:?}. Mapping starts at physical LED 0; adjust in properties.",
                            info.name, info.segments
                        );
                        self.add(
                            info.name,
                            Route::Wled {
                                host,
                                start: 0,
                                count,
                                device_id: info.device_id,
                            },
                            count.min(128),
                            true,
                        );
                    }
                }
                JobResult::WledRoute(id, expected, Ok((host, info))) => {
                    self.notice = match inspected_route(&expected, host, &info)
                        .and_then(|route| apply_inspection(&mut self.config, &id, &expected, route))
                    {
                        Ok(true) => format!(
                            "Verified {}. Review the physical LED mapping in properties.",
                            info.name
                        ),
                        Ok(false) => {
                            "Inspection ignored because this route was edited or removed.".into()
                        }
                        Err(error) => format!("Could not apply inspection: {error}"),
                    };
                }
                JobResult::Ha(origin, Ok(lights)) => {
                    if self.config.ha_url.trim() != origin {
                        self.notice =
                            "Discovery ignored because the Home Assistant server changed.".into();
                        continue;
                    }
                    self.ha_picker = Some(ha_picker::HaPicker::new(
                        origin,
                        lights,
                        &self.config.lights,
                    ));
                    self.notice = "Discovery complete. Choose the lights to add.".into();
                }
                JobResult::Wled(Err(error))
                | JobResult::WledRoute(_, _, Err(error))
                | JobResult::Ha(_, Err(error)) => self.notice = error.to_string(),
            }
        }
    }
    fn finish_edit(&mut self, before: &Config, pointer_down: bool) {
        if self.history_changed {
            self.edit_before = None;
            return;
        }
        if before != &self.config && self.edit_before.is_none() {
            self.edit_before = Some(before.clone());
        }
        if !pointer_down && let Some(before) = self.edit_before.take() {
            self.history.checkpoint(&before, &self.config);
        }
    }
    fn undo(&mut self, redo: bool) {
        if self.busy || self.ha_picker.is_some() || self.layout_dialog.is_some() {
            return;
        }
        if let Some(before) = self.edit_before.take() {
            self.history.checkpoint(&before, &self.config);
        }
        let config = if redo {
            self.history.redo(&self.config)
        } else {
            self.history.undo(&self.config)
        };
        if let Some(config) = config {
            let selected_id = self
                .selected
                .and_then(|i| self.config.lights.get(i))
                .map(|light| light.id.clone());
            self.history_changed = true;
            self.config = config;
            self.selected = selected_id
                .and_then(|id| self.config.lights.iter().position(|light| light.id == id));
            self.drag = None;
            self.selected_point = 0;
            self.notice = if redo { "Edit restored" } else { "Edit undone" }.into();
        }
    }
    fn render(&mut self, ctx: &egui::Context) -> bool {
        if !self.styled {
            let mut style = (*ctx.style()).clone();
            style.spacing.item_spacing = Vec2::new(8.0, 8.0);
            style.spacing.button_padding = Vec2::new(10.0, 7.0);
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = Color32::from_rgb(23, 30, 41);
            style.visuals.window_fill = Color32::from_rgb(28, 36, 48);
            style.visuals.selection.bg_fill = Color32::from_rgb(36, 78, 104);
            style.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
            ctx.set_style(style);
            self.styled = true;
        }
        self.history_changed = false;
        let before = self.config.clone();
        self.poll_jobs();
        let snapshot = self.engine.as_ref().unwrap().snapshots.borrow().clone();
        let mut running = matches!(
            snapshot.state,
            SessionState::Running | SessionState::RequestingPermission
        );
        let validation = self.config.validate().err().map(|e| e.to_string());
        let dirty = self.config != self.saved_config;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Dartmoor").size(21.0).strong());
                ui.separator();
                ui.menu_button("Edit", |ui| {
                    if ui
                        .add_enabled(
                            !running
                                && !self.busy
                                && self.ha_picker.is_none()
                                && self.layout_dialog.is_none()
                                && (self.history.can_undo() || self.edit_before.is_some()),
                            egui::Button::new("Undo"),
                        )
                        .on_hover_text("Ctrl/Cmd+Z · stop synchronization to undo layout changes")
                        .clicked()
                    {
                        self.undo(false);
                    }
                    if ui
                        .add_enabled(
                            !running
                                && !self.busy
                                && self.ha_picker.is_none()
                                && self.layout_dialog.is_none()
                                && self.history.can_redo(),
                            egui::Button::new("Redo"),
                        )
                        .on_hover_text("Ctrl/Cmd+Shift+Z")
                        .clicked()
                    {
                        self.undo(true);
                    }
                });
                if ui
                    .add_enabled(validation.is_none() && dirty, egui::Button::new("Save"))
                    .clicked()
                {
                    self.save();
                }
                ui.add_enabled_ui(
                    !running
                        && !self.busy
                        && self.ha_picker.is_none()
                        && self.layout_dialog.is_none(),
                    |ui| {
                        ui.menu_button("Configuration", |ui| {
                            if ui.button("Import configuration…").clicked() {
                                self.layout_dialog = Some(layout_dialog::LayoutDialog::import());
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    validation.is_none(),
                                    egui::Button::new("Export configuration…"),
                                )
                                .clicked()
                            {
                                match layout_dialog::LayoutDialog::export(&self.config) {
                                    Ok(dialog) => self.layout_dialog = Some(dialog),
                                    Err(e) => self.notice = e.to_string(),
                                };
                                ui.close();
                            }
                        });
                    },
                );
                ui.label(
                    egui::RichText::new(if dirty { "Unsaved changes" } else { "Saved" })
                        .small()
                        .color(if dirty { AMBER } else { MUTED }),
                );
            });
            ui.separator();
            let previous = self.workspace;
            ui.horizontal_wrapped(|ui| {
                for workspace in Workspace::ALL {
                    if ui
                        .selectable_label(self.workspace == workspace, workspace.label())
                        .clicked()
                    {
                        self.navigate(workspace);
                    }
                }
            });
            if previous != self.workspace {
                self.drag = None;
                self.canvas_rect = None;
                if let Some(before) = self.edit_before.take() {
                    self.history.checkpoint(&before, &self.config);
                }
            }
            ui.add_space(4.0);
        });
        if self.transport_bar(ctx, &snapshot, validation.as_deref()) {
            running = true;
        }
        self.observe_music(&snapshot);
        self.render_workspace(ctx, running, &snapshot);
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S))
            && self.config.validate().is_ok()
        {
            self.save();
        }
        if !ctx.wants_keyboard_input() && self.ha_picker.is_none() && self.layout_dialog.is_none() {
            ctx.input(|input| {
                if !running
                    && !self.busy
                    && self.ha_picker.is_none()
                    && self.layout_dialog.is_none()
                    && input.modifiers.command
                    && input.key_pressed(egui::Key::Z)
                {
                    self.undo(input.modifiers.shift);
                }
                if self.workspace == Workspace::Layout
                    && !input.modifiers.command
                    && let Some(index) = self.selected
                    && let Some(light) = self.config.lights.get_mut(index)
                {
                    let step = if input.modifiers.alt {
                        if input.modifiers.shift { 0.02 } else { 0.002 }
                    } else {
                        crate::editor::PLACEMENT_GRID_STEP
                            * if input.modifiers.shift { 5.0 } else { 1.0 }
                    };
                    let mut delta = Point { x: 0.0, y: 0.0 };
                    if input.key_pressed(egui::Key::ArrowLeft) {
                        delta.x -= step;
                    }
                    if input.key_pressed(egui::Key::ArrowRight) {
                        delta.x += step;
                    }
                    if input.key_pressed(egui::Key::ArrowUp) {
                        delta.y -= step;
                    }
                    if input.key_pressed(egui::Key::ArrowDown) {
                        delta.y += step;
                    }
                    if input.modifiers.alt {
                        crate::editor::move_selected(&mut light.shape, self.selected_point, delta);
                    } else {
                        crate::editor::move_selected_on_grid(
                            &mut light.shape,
                            self.selected_point,
                            delta,
                        );
                    }
                }
            });
        }
        if let Some(mut picker) = self.ha_picker.take() {
            let modal = egui::Modal::new(egui::Id::new("ha-discovery-picker"))
                .show(ctx, |ui| picker.show(ui));
            if let Some(chosen) = modal.inner {
                if picker.origin != self.config.ha_url.trim() || running {
                    self.notice="The connection or session changed. Stop and discover again before adding lights.".into();
                } else {
                    let mut added = 0;
                    for light in chosen {
                        if self.add(
                            light.name,
                            Route::HomeAssistant {
                                entity_id: light.entity_id,
                            },
                            1,
                            false,
                        ) {
                            added += 1;
                        }
                    }
                    self.notice = format!(
                        "Added {added} selected Home Assistant lights · ambient synchronization."
                    );
                    self.compact_inspector = true;
                }
            } else if !modal.should_close() {
                self.ha_picker = Some(picker);
            }
        }
        if let Some(mut dialog) = self.layout_dialog.take() {
            let modal = egui::Modal::new(egui::Id::new("layout-interchange"))
                .show(ctx, |ui| dialog.show(ui));
            if let Some(config) = modal.inner {
                if running || self.busy {
                    self.notice =
                        "Stop synchronization and finish pending connections before importing."
                            .into();
                } else {
                    let selected_id = self
                        .selected
                        .and_then(|i| self.config.lights.get(i))
                        .map(|light| light.id.clone());
                    self.config = interactive_config(config);
                    // Import preserves navigation.
                    self.selected = selected_id
                        .and_then(|id| self.config.lights.iter().position(|light| light.id == id))
                        .or_else(|| (!self.config.lights.is_empty()).then_some(0));
                    self.selected_point = 0;
                    self.drag = None;
                    self.notice =
                        "Configuration imported. Review devices and sources, then Save when ready."
                            .into();
                }
            } else if !modal.should_close() {
                self.layout_dialog = Some(dialog);
            }
        }
        self.finish_edit(
            &before,
            ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input(),
        );
        if before != self.config
            && before.mode == self.config.mode
            && running
            && self.config.validate().is_ok()
        {
            self.pending_apply = Some(self.config.clone());
        }
        if !running {
            self.pending_apply = None;
        } else if let Some(config) = &self.pending_apply
            && self
                .engine
                .as_ref()
                .unwrap()
                .send(Command::Apply(config.clone()))
                .is_ok()
        {
            self.pending_apply = None;
        }
        ctx.request_repaint_after(if running || self.busy || self.drag.is_some() {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(250)
        });
        running
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.render(ctx);
        if let Some((_, duration)) = self.smoke
            && !self.smoke_scheduled
        {
            self.smoke_scheduled = true;
            let context = ctx.clone();
            let snapshots = self.engine.as_ref().unwrap().snapshots.clone();
            self.runtime.as_ref().unwrap().spawn(async move {
                tokio::time::sleep(duration / 3).await;
                println!("UI visible frames: {}", snapshots.borrow().frames);
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                tokio::time::sleep(duration / 3).await;
                println!("UI minimized frames: {}", snapshots.borrow().frames);
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                tokio::time::sleep(duration / 3).await;
                println!("UI restored frames: {}", snapshots.borrow().frames);
                context.send_viewport_cmd(egui::ViewportCommand::Close);
            });
        }
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if self.smoke.is_none()
            && let Err(e) = config::save_to(&self.config, &self.config_path)
        {
            tracing::warn!("Layout save failed: {e}");
        }
        if let (Some(engine), Some(runtime)) = (self.engine.take(), self.runtime.take()) {
            runtime.block_on(engine.shutdown());
            runtime.block_on(crate::zigbee::shutdown());
            runtime.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
mod tests;
