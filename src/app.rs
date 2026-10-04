mod ha_picker;
mod layout_dialog;
mod panels;
mod text;
use crate::{
    config::{self, CaptureSelection, Config},
    core::{Light, Point, Route, Shape},
    engine::{Command, Engine, SessionState},
    outputs,
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};

const ACCENT: Color32 = Color32::from_rgb(102, 190, 237);
const MUTED: Color32 = Color32::from_rgb(149, 166, 185);
const AMBER: Color32 = Color32::from_rgb(239, 189, 110);

fn section_heading(ui: &mut egui::Ui, number: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(number).small().color(ACCENT));
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

use std::{
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    time::{Duration, Instant},
};

#[derive(Debug)]
enum JobResult {
    Wled(anyhow::Result<(String, outputs::WledInfo)>),
    WledRoute(String, Route, anyhow::Result<(String, outputs::WledInfo)>),
    Ha(String, anyhow::Result<Vec<outputs::HaLight>>),
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
    selected_point: usize,
    show_zones: bool,
    compact_inspector: bool,
    canvas_rect: Option<Rect>,
    history: crate::editor::EditHistory,
    edit_before: Option<Config>,
    history_changed: bool,
    saved_config: Config,
    config_path: std::path::PathBuf,
    styled: bool,
    diagnostics: bool,
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
        let (config, warning) = config::load();
        let (results, jobs) = sync_channel(4);
        let mut app = Self {
            saved_config: config.clone(),
            config_path: config::path(),
            config,
            engine: Some(engine),
            runtime: Some(runtime),
            selected: Some(0),
            selected_point: 0,
            show_zones: true,
            compact_inspector: false,
            canvas_rect: None,
            history: crate::editor::EditHistory::default(),
            edit_before: None,
            history_changed: false,
            styled: false,
            diagnostics: false,
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
            app.config = Config::default();
            if real_capture {
                app.config.source = CaptureSelection::Desktop { id: None };
            }
            app.send(Command::Start(app.config.clone()));
        }
        Ok(app)
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
                "Layout saved".into()
            }
            Err(e) => format!("Could not save layout: {e}"),
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
                reverse: false,
            }
        } else {
            Shape::Bulb {
                center: Point { x: 0.5, y: 0.5 },
                radius: 0.08,
            }
        };
        let light = Light {
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
            self.busy = false;
            match result {
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
            self.history_changed = true;
            self.config = config;
            self.selected = self.selected.filter(|&i| i < self.config.lights.len());
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
        let state_label = match snapshot.state {
            SessionState::Idle => "READY",
            SessionState::RequestingPermission => "AWAITING CAPTURE",
            SessionState::Running => "SYNCING",
            SessionState::Paused => "PAUSED",
            SessionState::Error => "NEEDS ATTENTION",
        };
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Lumen Desktop").size(21.0).strong());
                badge(
                    ui,
                    state_label,
                    if snapshot.state == SessionState::Running {
                        Color32::from_rgb(116, 213, 171)
                    } else if snapshot.state == SessionState::Error {
                        AMBER
                    } else {
                        MUTED
                    },
                );
                ui.separator();
                let start_enabled = !running
                    && !self.busy
                    && self.ha_picker.is_none()
                    && self.layout_dialog.is_none()
                    && validation.is_none()
                    && !self.config.lights.is_empty();
                let start = ui.add_enabled(
                    start_enabled,
                    egui::Button::new(if snapshot.state == SessionState::Paused {
                        "Resume"
                    } else {
                        "Start sync"
                    })
                    .fill(Color32::from_rgb(35, 89, 118)),
                );
                if start.clicked() {
                    self.send(Command::Start(self.config.clone()));
                    running = true;
                }
                if !start_enabled {
                    start.on_hover_text(if running {
                        "Synchronization is already active."
                    } else if self.busy {
                        "Wait for the device connection to finish."
                    } else if self.ha_picker.is_some() || self.layout_dialog.is_some() {
                        "Finish or close the open dialog first."
                    } else if self.config.lights.is_empty() {
                        "Add a light first."
                    } else {
                        "Resolve the layout message below before starting."
                    });
                }
                if ui
                    .add_enabled(running, egui::Button::new("Pause"))
                    .clicked()
                {
                    self.send(Command::Pause);
                }
                if ui
                    .add_enabled(
                        running
                            || snapshot.state == SessionState::Paused
                            || snapshot.state == SessionState::Error,
                        egui::Button::new("Stop"),
                    )
                    .clicked()
                {
                    self.send(Command::Stop);
                }
                ui.separator();
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
                if ui
                    .add_enabled(
                        validation.is_none() && dirty,
                        egui::Button::new("Save layout"),
                    )
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
                        ui.menu_button("Layouts", |ui| {
                            if ui.button("Import JSON…").clicked() {
                                self.layout_dialog = Some(layout_dialog::LayoutDialog::import());
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    validation.is_none(),
                                    egui::Button::new("Export JSON…"),
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
            ui.add_space(4.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            if let Some(error) = &validation {
                ui.label(
                    egui::RichText::new(format!("Layout needs attention: {error}")).color(AMBER),
                );
            }
            if !self.notice.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    if ui.small_button("Dismiss").clicked() {
                        self.notice.clear();
                    }
                    ui.label(&self.notice);
                });
            }
            ui.horizontal_wrapped(|ui| {
                if snapshot.state == SessionState::RequestingPermission {
                    ui.spinner();
                }
                ui.label(&snapshot.message);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.diagnostics, "Diagnostics");
                });
            });
            if self.diagnostics {
                ui.small(format!(
                    "{} frames · {:.2} ms sampling · {:?}",
                    snapshot.frames, snapshot.processing_ms, snapshot.state
                ));
            }
            ui.label(
                egui::RichText::new("Minimize to keep syncing · close the window to stop")
                    .small()
                    .color(MUTED),
            );
        });
        let compact = ctx.content_rect().width() < 1000.0;
        egui::SidePanel::left("setup")
            .default_width(270.0)
            .width_range(230.0..=350.0)
            .resizable(true)
            .show(ctx, |ui| {
                if compact {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.compact_inspector, false, "Setup");
                        ui.selectable_value(&mut self.compact_inspector, true, "Light properties");
                    });
                    ui.separator();
                }
                egui::ScrollArea::vertical()
                    .id_salt(if compact && self.compact_inspector {
                        "compact-inspector-scroll"
                    } else {
                        "setup-scroll"
                    })
                    .show(ui, |ui| {
                        if compact && self.compact_inspector {
                            self.inspector(ui, running);
                        } else {
                            self.setup(ui, running, &snapshot);
                        }
                    });
            });
        if !compact {
            egui::SidePanel::right("inspector")
                .default_width(265.0)
                .width_range(235.0..=350.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("inspector-scroll")
                        .show(ui, |ui| {
                            self.inspector(ui, running);
                        });
                });
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .fill(Color32::from_rgb(14, 20, 29))
                    .inner_margin(16),
            )
            .show(ctx, |ui| {
                self.canvas(ui);
            });
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
                if !input.modifiers.command
                    && let Some(index) = self.selected
                    && let Some(light) = self.config.lights.get_mut(index)
                {
                    let step = if input.modifiers.shift { 0.02 } else { 0.002 };
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
                    crate::editor::move_selected(&mut light.shape, self.selected_point, delta);
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
                    self.config = config;
                    self.selected = (!self.config.lights.is_empty()).then_some(0);
                    self.selected_point = 0;
                    self.drag = None;
                    self.notice="Layout imported. Review device addresses and source, then Save when ready.".into();
                }
            } else if !modal.should_close() {
                self.layout_dialog = Some(dialog);
            }
        }
        self.finish_edit(
            &before,
            ctx.input(|i| i.pointer.any_down()) || ctx.wants_keyboard_input(),
        );
        if before != self.config && running && self.config.validate().is_ok() {
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
            runtime.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless_app() -> (App, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new(None, false).unwrap();
        app.config = Config::default();
        app.saved_config = app.config.clone();
        app.config_path = directory.path().join("config.json");
        app.notice.clear();
        // Suppress automatic persistence in Drop; explicit saves target the
        // temporary directory. No native window or capture is constructed.
        app.smoke = Some((Instant::now(), Duration::from_secs(3600)));
        (app, directory)
    }
    fn draw(
        app: &mut App,
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                events,
                modifiers,
                ..Default::default()
            },
            |ctx| {
                app.render(ctx);
            },
        )
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    fn text_rect(output: &egui::FullOutput, label: &str) -> Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing text {label}"))
    }
    fn click(app: &mut App, ctx: &egui::Context, size: Vec2, pos: Pos2) -> egui::FullOutput {
        draw(
            app,
            ctx,
            size,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            egui::Modifiers::NONE,
        );
        draw(
            app,
            ctx,
            size,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            egui::Modifiers::NONE,
        )
    }

    #[test]
    fn headless_layout_preserves_valid_configuration_and_fits_small_window() {
        let (mut app, _directory) = headless_app();
        app.config.smoothing_ms = 4900.0;
        app.config.ha_interval_ms = 59000;
        app.config.lights[0].shape = Shape::Bulb {
            center: Point { x: 0.5, y: 0.5 },
            radius: 0.95,
        };
        let before = app.config.clone();
        let ctx = egui::Context::default();
        for size in [Vec2::new(1080.0, 700.0), Vec2::new(720.0, 480.0)] {
            for _ in 0..2 {
                let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
                assert!(!output.shapes.is_empty());
                let rect = app.canvas_rect.unwrap();
                assert!(
                    rect.width() > 200.0 && rect.height() > 100.0,
                    "canvas squeezed: {rect:?}"
                );
                assert!(Rect::from_min_size(Pos2::ZERO, size).contains_rect(rect));
                assert_eq!(app.config, before, "merely rendering changed the layout");
            }
        }
    }

    #[test]
    fn headless_nudge_undo_redo_and_text_focus_are_independent() {
        let (mut app, _directory) = headless_app();
        app.config.lights[0].shape = Shape::Bulb {
            center: Point { x: 0.5, y: 0.5 },
            radius: 0.08,
        };
        let original = app.config.clone();
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        let moved = app.config.clone();
        assert_ne!(moved, original);
        let command = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::Z, command)],
            command,
        );
        assert_eq!(app.config, original);
        assert!(app.history.can_redo());
        let redo = egui::Modifiers {
            shift: true,
            ..command
        };
        draw(&mut app, &ctx, size, vec![key(egui::Key::Z, redo)], redo);
        assert_eq!(app.config, moved);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        // The current name's TextEdit appears before its canvas label.
        let name = text_rect(&output, "Demo strip");
        click(&mut app, &ctx, size, name.center());
        assert!(ctx.wants_keyboard_input());
        let before = app.config.lights[0].shape.clone();
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert_eq!(
            app.config.lights[0].shape, before,
            "text cursor key moved light"
        );
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::S, command)],
            command,
        );
        let saved: Config =
            serde_json::from_slice(&std::fs::read(&app.config_path).unwrap()).unwrap();
        assert_eq!(saved, app.config);
        assert_eq!(app.saved_config, app.config);
    }

    #[test]
    fn headless_drag_is_one_undo_transaction() {
        let (mut app, _directory) = headless_app();
        app.config.lights[0].shape = Shape::Bulb {
            center: Point { x: 0.5, y: 0.5 },
            radius: 0.08,
        };
        let before = app.config.clone();
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let start = app.canvas_rect.unwrap().center();
        draw(
            &mut app,
            &ctx,
            size,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            egui::Modifiers::NONE,
        );
        for offset in [20.0, 40.0, 60.0] {
            draw(
                &mut app,
                &ctx,
                size,
                vec![egui::Event::PointerMoved(start + Vec2::new(offset, 0.0))],
                egui::Modifiers::NONE,
            );
        }
        let end = start + Vec2::new(60.0, 0.0);
        draw(
            &mut app,
            &ctx,
            size,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            egui::Modifiers::NONE,
        );
        assert_ne!(app.config, before, "drag did not move marker");
        app.undo(false);
        assert_eq!(app.config, before);
        assert!(!app.history.can_undo());
    }

    #[test]
    fn coincident_markers_keep_selected_light_and_release_drag_state() {
        let (mut app, _directory) = headless_app();
        app.config.lights[0].shape = Shape::Bulb {
            center: Point { x: 0.5, y: 0.5 },
            radius: 0.08,
        };
        let mut second = app.config.lights[0].clone();
        second.id = "second-marker".into();
        second.name = "Selected bulb".into();
        app.config.lights.push(second);
        app.selected = Some(1);
        let original = app.config.clone();
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let start = app.canvas_rect.unwrap().center();
        click(&mut app, &ctx, size, start);
        assert_eq!(app.selected, Some(1));
        assert!(app.drag.is_none(), "a click left active drag state");
        draw(
            &mut app,
            &ctx,
            size,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            egui::Modifiers::NONE,
        );
        let end = start + Vec2::new(60.0, 0.0);
        draw(
            &mut app,
            &ctx,
            size,
            vec![egui::Event::PointerMoved(end)],
            egui::Modifiers::NONE,
        );
        draw(
            &mut app,
            &ctx,
            size,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            egui::Modifiers::NONE,
        );
        assert_eq!(app.config.lights[0], original.lights[0]);
        assert_ne!(app.config.lights[1].shape, original.lights[1].shape);
        assert!(app.drag.is_none());
        app.undo(false);
        assert_eq!(app.config, original);
        assert!(!app.history.can_undo());
    }

    #[test]
    fn preset_path_preserves_hardware_and_sampling_settings_and_has_one_undo() {
        let (mut app, _directory) = headless_app();
        app.config.lights[0].shape = Shape::Strip {
            points: vec![Point { x: 0.1, y: 0.6 }, Point { x: 0.8, y: 0.4 }],
            radius: 0.23,
            reverse: true,
        };
        app.selected_point = 1;
        let original = app.config.clone();
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "Place along desktop…").center(),
        );
        let output = if output.shapes.iter().any(|s| matches!(&s.shape, egui::epaint::Shape::Text(t) if t.galley.text() == "Full perimeter")) {
            output
        } else {
            draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE)
        };
        click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "Full perimeter").center(),
        );
        assert_eq!(app.selected_point, 0);
        let light = &app.config.lights[0];
        assert_eq!(light.id, original.lights[0].id);
        assert_eq!(light.route, original.lights[0].route);
        assert_eq!(light.zones, original.lights[0].zones);
        assert!(
            matches!(&light.shape, Shape::Strip { radius, reverse: true, points }
            if *radius == 0.23 && points.len() == 5 && points.first() == points.last())
        );
        assert!(app.config.validate().is_ok());
        app.undo(false);
        assert_eq!(app.config, original);
        assert!(!app.history.can_undo());
    }

    #[test]
    fn unicode_name_paste_respects_persisted_limit_and_undo() {
        let (mut app, _directory) = headless_app();
        let original = app.config.clone();
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "Demo strip").center(),
        );
        assert!(ctx.wants_keyboard_input());
        let command = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::A, command)],
            command,
        );
        draw(
            &mut app,
            &ctx,
            size,
            vec![egui::Event::Paste("東京".repeat(100))],
            egui::Modifiers::NONE,
        );
        assert_eq!(app.config.lights[0].name, "東京".repeat(42) + "東");
        assert!(app.config.validate().is_ok());
        let name = app.config.lights[0].name.clone();
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert_eq!(
            app.config.lights[0].name, name,
            "rendering changed the name"
        );
        // Leave the editor so its text transaction becomes one layout Undo.
        let point = app.canvas_rect.unwrap().left_bottom() + Vec2::new(12.0, -12.0);
        click(&mut app, &ctx, size, point);
        app.undo(false);
        assert_eq!(app.config, original);
    }

    #[test]
    fn discovery_cannot_be_retargeted_by_undo_or_a_stale_reply() {
        let (mut app, _directory) = headless_app();
        let original = app.config.clone();
        app.config.ha_url = "https://server-b.example".into();
        app.history.checkpoint(&original, &app.config);
        app.busy = true;
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let command = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::Z, command)],
            command,
        );
        assert_eq!(app.config.ha_url, "https://server-b.example");
        app.config.ha_url = "https://server-a.example".into();
        let before = app.config.clone();
        app.results
            .try_send(JobResult::Ha(
                "https://server-b.example".into(),
                Ok(vec![outputs::HaLight {
                    entity_id: "light.server_b".into(),
                    name: "Different server".into(),
                    color_modes: vec!["rgb".into()],
                    available: true,
                }]),
            ))
            .unwrap();
        app.poll_jobs();
        assert_eq!(app.config, before);
        assert!(!app.busy);
        assert!(app.notice.contains("server changed"));
    }

    #[test]
    fn headless_discovery_requires_selection_and_adds_only_chosen_light() {
        let (mut app, _directory) = headless_app();
        let original = app.config.clone();
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        let light = |id: &str, name: &str| outputs::HaLight {
            entity_id: format!("light.{id}"),
            name: name.into(),
            color_modes: vec!["rgb".into()],
            available: true,
        };
        app.results
            .try_send(JobResult::Ha(
                app.config.ha_url.clone(),
                Ok(vec![light("desk", "Desk lamp"), light("room", "Room lamp")]),
            ))
            .unwrap();
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert_eq!(app.config, original);
        assert!(app.ha_picker.is_some());
        // Global arrow shortcuts must not move the background layout while
        // a discovery choice is open, even if its search field lacks focus.
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert_eq!(app.config, original);
        let checkbox = text_rect(&output, "Desk lamp").center();
        click(&mut app, &ctx, size, checkbox);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let add = text_rect(&output, "Add 1 selected").center();
        click(&mut app, &ctx, size, add);
        assert!(app.ha_picker.is_none());
        assert_eq!(app.config.lights.len(), original.lights.len() + 1);
        assert_eq!(
            app.config.lights.last().unwrap().route,
            Route::HomeAssistant {
                entity_id: "light.desk".into()
            }
        );
        assert!(app.config.validate().is_ok());
        app.undo(false);
        assert_eq!(app.config, original);
    }

    #[test]
    fn headless_export_copies_strict_layout_without_credentials_or_edits() {
        let (mut app, _directory) = headless_app();
        let original = app.config.clone();
        app.token = "private-test-token-never-export".into();
        app.layout_dialog = Some(layout_dialog::LayoutDialog::export(&app.config).unwrap());
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "Copy JSON").center(),
        );
        let json = output
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(json) => Some(json),
                _ => None,
            })
            .expect("export produced no clipboard command");
        assert_eq!(config::decode_layout(json).unwrap(), original);
        assert!(!json.contains(&app.token));
        assert_eq!(app.config, original);
        assert!(!app.history.can_undo());
    }

    #[test]
    fn headless_import_is_validated_and_undoable_as_one_edit() {
        let (mut app, _directory) = headless_app();
        let original = app.config.clone();
        let mut imported = original.clone();
        imported.lights[0].name = "Imported strip".into();
        imported.brightness = 0.4;
        app.layout_dialog = Some(layout_dialog::LayoutDialog::from_json(
            config::encode_layout(&imported).unwrap(),
        ));
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert_eq!(app.config, original, "modal allowed background movement");
        click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "Use this layout").center(),
        );
        assert!(app.layout_dialog.is_none());
        assert_eq!(app.config, imported);
        assert_eq!(app.saved_config, original, "import unexpectedly persisted");
        app.undo(false);
        assert_eq!(app.config, original);
        assert!(!app.history.can_undo());
        app.undo(true);
        assert_eq!(app.config, imported);
    }

    #[test]
    fn headless_invalid_import_keeps_dialog_and_original_layout() {
        let (mut app, _directory) = headless_app();
        let original = app.config.clone();
        let mut json: serde_json::Value =
            serde_json::from_str(&config::encode_layout(&original).unwrap()).unwrap();
        json["access_token"] = "not-a-layout-field".into();
        app.layout_dialog = Some(layout_dialog::LayoutDialog::from_json(json.to_string()));
        let ctx = egui::Context::default();
        let size = Vec2::new(1080.0, 700.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "Use this layout").center(),
        );
        assert!(app.layout_dialog.is_some());
        assert_eq!(app.config, original);
        assert!(!app.history.can_undo());
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains("Could not import"))));
    }

    #[test]
    fn reinspection_preserves_verified_mapping_and_rejects_shrunken_controller() {
        let expected = Route::Wled {
            host: "old.local".into(),
            start: 20,
            count: 40,
            device_id: "AA:BB:CC:DD:EE:FF".into(),
        };
        let mut info = outputs::WledInfo {
            name: "Desk".into(),
            device_id: "aabbccddeeff".into(),
            led_count: 120,
            segments: vec![(0, 120)],
        };
        assert_eq!(
            inspected_route(&expected, "new.local".into(), &info).unwrap(),
            Route::Wled {
                host: "new.local".into(),
                start: 20,
                count: 40,
                device_id: info.device_id.clone()
            }
        );
        info.led_count = 50;
        assert!(inspected_route(&expected, "new.local".into(), &info).is_err());
        info.device_id = "001122334455".into();
        assert!(matches!(
            inspected_route(&expected, "other.local".into(), &info).unwrap(),
            Route::Wled {
                start: 0,
                count: 50,
                ..
            }
        ));
    }

    #[test]
    fn repeated_source_selection_preserves_saved_target() {
        let mut source = CaptureSelection::Desktop {
            id: Some("display:42".into()),
        };
        assert!(!select_desktop(&mut source));
        assert_eq!(
            source,
            CaptureSelection::Desktop {
                id: Some("display:42".into())
            }
        );
        source = CaptureSelection::Synthetic;
        assert!(select_desktop(&mut source));
    }

    #[test]
    fn discovery_add_is_atomic_and_names_are_bounded() {
        assert_eq!(display_name("  "), "Unnamed light");
        let unicode = display_name(&"é".repeat(200));
        assert_eq!(unicode.len(), 256);
        let mut config = Config::default();
        let before = config.clone();
        let mut light = config.lights[0].clone();
        light.id = "invalid-import".into();
        light.route = Route::HomeAssistant {
            entity_id: "light.".into(),
        };
        light.zones = 1;
        assert!(insert_light(&mut config, light).is_err());
        assert_eq!(config, before);
        config.lights.clear();
        for i in 0..8 {
            let mut light = before.lights[0].clone();
            light.id = format!("{i}");
            light.zones = 256;
            insert_light(&mut config, light).unwrap();
        }
        let full = config.clone();
        let mut light = before.lights[0].clone();
        light.id = "overflow".into();
        assert!(insert_light(&mut config, light).is_err());
        assert_eq!(config, full);
    }

    #[test]
    fn stale_inspection_does_not_overwrite_edited_or_deleted_route() {
        let mut config = Config::default();
        let id = config.lights[0].id.clone();
        let inspected = Route::Wled {
            host: "old.local".into(),
            start: 0,
            count: 10,
            device_id: "aabbcc".into(),
        };
        let replacement = Route::Wled {
            host: "verified.local".into(),
            start: 0,
            count: 10,
            device_id: "aabbcc".into(),
        };
        assert!(!apply_inspection(&mut config, &id, &inspected, replacement.clone()).unwrap());
        assert_eq!(config.lights[0].route, Route::Mock);
        config.lights[0].route = inspected.clone();
        assert!(apply_inspection(&mut config, &id, &inspected, replacement.clone()).unwrap());
        assert_eq!(config.lights[0].route, replacement);
        config.lights.clear();
        assert!(!apply_inspection(&mut config, &id, &inspected, Route::Mock).unwrap());
    }

    #[test]
    fn preview_identity_handles_new_sessions_and_clearing() {
        let old = Some(std::sync::Arc::new(crate::capture::synthetic_frame(0.0)));
        let same = old.clone();
        let next_session = Some(std::sync::Arc::new(crate::capture::synthetic_frame(0.0)));
        assert!(!preview_changed(&old, &same));
        assert!(preview_changed(&old, &next_session));
        assert!(preview_changed(&old, &None));
        assert!(preview_changed(&None, &next_session));
    }
}
