//! UI navigation is independent from the engine's two output modes.
use super::*;
use crate::music::SyncMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Workspace {
    Devices,
    Layout,
    Video,
    Music,
    Studio,
}
impl Workspace {
    pub const ALL: [Self; 5] = [
        Self::Devices,
        Self::Layout,
        Self::Video,
        Self::Music,
        Self::Studio,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Devices => "Devices",
            Self::Layout => "Layout",
            Self::Video => "Video",
            Self::Music => "Music",
            Self::Studio => "Studio",
        }
    }
    pub fn sync_mode(self) -> Option<SyncMode> {
        match self {
            Self::Video => Some(SyncMode::Video),
            Self::Music => Some(SyncMode::Music),
            _ => None,
        }
    }
    pub fn from_mode(mode: SyncMode) -> Self {
        match mode {
            SyncMode::Video => Self::Video,
            SyncMode::Music => Self::Music,
        }
    }
}
fn connection_summary(status: &str) -> &str {
    if status.contains("HTTP unreachable") {
        "HTTP unavailable · DDP unconfirmed"
    } else if status.contains("HTTP reachable") {
        "HTTP reachable · DDP unconfirmed"
    } else if status.contains("service acknowledged") {
        "Service acknowledged"
    } else if status.contains("color targets ready") {
        "Ready for ambient updates"
    } else if status.contains("waiting for fresh capture") {
        "Waiting for capture"
    } else if status.ends_with("stopped") {
        "Stopped"
    } else if status.starts_with("Connecting") {
        "Connecting"
    } else {
        "Needs attention"
    }
}

impl App {
    pub(super) fn render_workspace(
        &mut self,
        ctx: &egui::Context,
        running: bool,
        snapshot: &crate::engine::Snapshot,
    ) {
        let compact = ctx.content_rect().width() < 1000.0;
        let workspace = self.workspace;
        if workspace != Workspace::Layout {
            self.canvas_rect = None;
            self.drag = None;
        }
        if matches!(workspace, Workspace::Devices | Workspace::Layout) {
            egui::SidePanel::left("workspace-lights")
                .default_width(230.0)
                .width_range(190.0..=300.0)
                .resizable(true)
                .show(ctx, |ui| {
                    if compact && workspace == Workspace::Layout {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.compact_inspector, false, "Lights");
                            ui.selectable_value(&mut self.compact_inspector, true, "Placement");
                        });
                        ui.separator();
                    }
                    if workspace == Workspace::Devices {
                        ui.vertical_centered(|ui| {
                            if ui
                                .add(
                                    egui::Button::new("Add device")
                                        .selected(self.device_view == DeviceView::Add)
                                        .min_size(Vec2::new(150.0, 32.0)),
                                )
                                .clicked()
                            {
                                self.device_view = DeviceView::Add;
                            }
                        });
                        ui.add_space(8.0);
                    }
                    egui::ScrollArea::vertical()
                        .id_salt((workspace.label(), "list"))
                        .show(ui, |ui| {
                            if compact && self.compact_inspector && workspace == Workspace::Layout {
                                self.inspector(ui, running);
                            } else {
                                self.light_list(ui, snapshot);
                            }
                        });
                });
        }
        if workspace == Workspace::Layout && !compact {
            egui::SidePanel::right("placement-inspector")
                .default_width(265.0)
                .width_range(235.0..=340.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("placement-properties")
                        .show(ui, |ui| {
                            self.inspector(ui, running);
                        });
                });
        }
        if matches!(workspace, Workspace::Video | Workspace::Music)
            && ctx.content_rect().width() >= 900.0
        {
            egui::SidePanel::left("mode-settings")
                .default_width(320.0)
                .width_range(280.0..=400.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt((workspace.label(), "settings"))
                        .show(ui, |ui| {
                            if workspace == Workspace::Video {
                                self.video_controls(ui, running, snapshot);
                            } else {
                                self.music_controls(
                                    ui,
                                    running && snapshot.mode == Some(SyncMode::Music),
                                );
                            }
                        });
                });
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .fill(Color32::from_rgb(14, 20, 29))
                    .inner_margin(16),
            )
            .show(ctx, |ui| match workspace {
                Workspace::Devices => {
                    egui::ScrollArea::vertical()
                        .id_salt(("device-details", self.device_view))
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width().min(720.0));
                            self.device_details(ui, running, snapshot);
                        });
                }
                Workspace::Layout => {
                    self.canvas(ui);
                }
                Workspace::Video | Workspace::Music => {
                    let stacked = ctx.content_rect().width() < 900.0;
                    egui::ScrollArea::vertical()
                        .id_salt((workspace.label(), "content"))
                        .show(ui, |ui| {
                            if stacked {
                                if workspace == Workspace::Video {
                                    self.video_controls(ui, running, snapshot);
                                } else {
                                    self.music_controls(
                                        ui,
                                        running && snapshot.mode == Some(SyncMode::Music),
                                    );
                                }
                                ui.add_space(20.0);
                                ui.separator();
                            }
                            if workspace == Workspace::Video {
                                self.video_preview(ui, snapshot);
                            } else {
                                self.music_canvas(ui, snapshot);
                            }
                        });
                }
                Workspace::Studio => {
                    ui.heading("Studio");
                    ui.label(egui::RichText::new("Coming later").color(MUTED));
                }
            });
    }
    pub(super) fn music_light_selector(&mut self, ui: &mut egui::Ui) {
        if self.config.lights.is_empty() {
            ui.label("Add devices to synchronize your music.");
            if ui.button("Go to Devices").clicked() {
                self.navigate(Workspace::Devices);
            }
        } else {
            egui::ComboBox::from_id_salt("music-light-selection")
                .width(ui.available_width())
                .selected_text(
                    self.selected
                        .and_then(|i| self.config.lights.get(i))
                        .map_or("Choose a light", |l| l.name.as_str()),
                )
                .show_ui(ui, |ui| {
                    for (index, light) in self.config.lights.iter().enumerate() {
                        ui.selectable_value(&mut self.selected, Some(index), &light.name);
                    }
                });
        }
    }
}

impl App {
    pub(super) fn light_list(&mut self, ui: &mut egui::Ui, snapshot: &crate::engine::Snapshot) {
        ui.heading(if self.workspace == Workspace::Devices {
            "Devices"
        } else {
            "Lights"
        });
        if self.config.lights.is_empty() {
            if self.workspace == Workspace::Devices {
                ui.label("No devices yet.");
            } else {
                ui.label("Add devices to build your layout.");
                if ui.button("Go to Devices").clicked() {
                    self.navigate(Workspace::Devices);
                }
            }
        }
        for (i, light) in self.config.lights.iter().enumerate() {
            let selected = self.selected == Some(i)
                && (self.workspace != Workspace::Devices
                    || self.device_view == DeviceView::Properties);
            let color = snapshot
                .colors
                .get(i)
                .filter(|_| snapshot.mode.is_some() && snapshot.state == SessionState::Running)
                .and_then(|c| c.first())
                .copied()
                .unwrap_or([115, 140, 160]);
            let status = if self.workspace == Workspace::Devices {
                match &light.route {
                    Route::Wled { host, .. } => {
                        let host = crate::core::canonical_host(host);
                        snapshot
                            .devices
                            .iter()
                            .find(|(name, _)| crate::core::canonical_host(name) == host)
                            .map(|(_, status)| status)
                    }
                    Route::HomeAssistant { .. } => snapshot
                        .devices
                        .iter()
                        .find(|(name, _)| name == "Home Assistant")
                        .map(|(_, status)| status),
                    Route::Zigbee { ieee_address, .. } => {
                        let name = format!("Zigbee {ieee_address}");
                        snapshot
                            .devices
                            .iter()
                            .find(|(key, _)| key == &name)
                            .map(|(_, status)| status)
                    }
                    Route::Mock => None,
                }
            } else {
                None
            };
            let label = format!(
                "{}\n{}{}",
                light.name,
                route_label(&light.route, light.zones),
                if self.workspace == Workspace::Devices {
                    format!(
                        "\n{}",
                        status.map_or("Not connected", |status| connection_summary(status))
                    )
                } else {
                    String::new()
                }
            );
            let response = ui.add(
                egui::Button::new(egui::RichText::new(label).size(13.0))
                    .selected(selected)
                    .min_size(Vec2::new(ui.available_width(), 44.0)),
            );
            let response = if let Some(status) = status {
                response.on_hover_text(status)
            } else {
                response
            };
            let dot = Pos2::new(response.rect.right() - 13.0, response.rect.center().y);
            ui.painter()
                .circle_filled(dot, 4.0, Color32::from_rgb(color[0], color[1], color[2]));
            if response.clicked() {
                self.device_view = DeviceView::Properties;
                self.selected = Some(i);
                self.selected_point = 0;
                self.compact_inspector = true;
            }
        }
    }
    pub(super) fn inspector(&mut self, ui: &mut egui::Ui, running: bool) {
        if self.workspace == Workspace::Devices {
            self.device_inspector(ui, running);
        } else {
            self.placement_inspector(ui, running);
        }
    }
}
