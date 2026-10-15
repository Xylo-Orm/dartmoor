//! Video settings and the shared read-only desktop texture.
use super::*;

impl App {
    pub(super) fn video_controls(
        &mut self,
        ui: &mut egui::Ui,
        running: bool,
        snapshot: &crate::engine::Snapshot,
    ) {
        section_heading(ui, "Desktop source");
        ui.add_enabled_ui(!running, |ui| {
            ui.label("Screen / window");
            #[cfg(target_os = "linux")]
            ui.label(
                egui::RichText::new(
                    "Start opens your desktop’s sharing dialog. Choose one screen or window there.",
                )
                .small()
                .color(MUTED),
            );
            #[cfg(not(target_os = "linux"))]
            {
                if ui.button("Find screens and windows").clicked() {
                    self.sources = crate::capture::sources();
                }
                if let CaptureSelection::Desktop { id } = &mut self.config.source {
                    egui::ComboBox::from_id_salt("source")
                        .width(ui.available_width())
                        .selected_text(
                            self.sources
                                .iter()
                                .find(|s| id.as_ref() == Some(&s.id))
                                .map(|s| s.name.as_str())
                                .unwrap_or("Primary display / saved source"),
                        )
                        .show_ui(ui, |ui| {
                            for source in &self.sources {
                                ui.selectable_value(id, Some(source.id.clone()), &source.name);
                            }
                        });
                }
            }
        });
        if running {
            ui.small("Stop to choose a different source.");
        }
        ui.add_space(18.0);
        section_heading(ui, "Ambience");
        ui.add(
            egui::Slider::new(&mut self.config.brightness, 0.0..=1.0)
                .text("Brightness")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        ui.add(
            egui::Slider::new(&mut self.config.soft_ambience.color_strength, 0.0..=1.0)
                .text("Color strength")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        )
        .on_hover_text("Strengthens substantial screen colors.");
        ui.add(
                egui::Slider::new(&mut self.config.soft_ambience.response_ms, 0.0..=1500.0)
                    .text("Response")
                    .suffix(" ms"),
            )
            .on_hover_text("0 ms applies color and brightness changes immediately after color selection. Higher values make changes gentler.");
        if self
            .config
            .lights
            .iter()
            .any(|light| light.route.is_ambient())
        {
            ui.small(
                "Home Assistant and Zigbee lights follow their configured ambient update intervals.",
            );
        }
        ui.add_space(16.0);
        section_heading(ui, "Dark scenes");
        ui.checkbox(&mut self.config.dark_zones.enabled, "Turn off dark zones")
            .on_hover_text("Turn LEDs off when their own region is dark, after black-bar cropping. Applies while synchronizing.");
        if self.config.dark_zones.enabled {
            ui.add(
                egui::Slider::new(&mut self.config.dark_zones.threshold, 0..=64)
                    .text("Black sensitivity"),
            )
            .on_hover_text(
                "Higher sensitivity turns lights off in brighter shadows. 0 requires exact black.",
            );
            ui.small("Higher sensitivity counts brighter shadows as black.");
            if snapshot.mode == Some(crate::music::SyncMode::Video) {
                ui.small(format!(
                    "Dark zones: {}/{}",
                    snapshot.dark_zones,
                    snapshot.colors.iter().map(Vec::len).sum::<usize>()
                ));
            }
        }
        ui.checkbox(&mut self.config.black_bar_detection, "Black bar detection")
                .on_hover_text("Ignore stable top/bottom or left/right black bars. The preview and lights follow the active image. Can be changed while synchronizing.");
        ui.add_space(16.0);
        ui.collapsing("WLED behavior", |ui| {
            ui.add_enabled(
                !running && !self.busy,
                egui::Checkbox::new(
                    &mut self.config.restore_wled_state,
                    "Try restoring WLED state on stop",
                ),
            );
            ui.small("Applies to all WLED outputs. Restoration is best effort.");
        });
    }

    pub(super) fn sync_desktop_preview(
        &mut self,
        ui: &egui::Ui,
        snapshot: &crate::engine::Snapshot,
    ) {
        if snapshot.mode != Some(crate::music::SyncMode::Video) || snapshot.preview.is_none() {
            self.texture = None;
            self.preview = None;
            return;
        }
        if preview_changed(&self.preview, &snapshot.preview)
            && let Some(frame) = &snapshot.preview
        {
            let bytes: Vec<u8> = frame.pixels.iter().flatten().copied().collect();
            let image = egui::ColorImage::from_rgb([frame.width, frame.height], &bytes);
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture = Some(ui.ctx().load_texture(
                    "desktop-preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            self.preview = Some(frame.clone());
        }
    }

    pub(super) fn video_preview(&mut self, ui: &mut egui::Ui, snapshot: &crate::engine::Snapshot) {
        self.sync_desktop_preview(ui, snapshot);
        ui.heading("Video preview");
        ui.label(
            egui::RichText::new("Your screen and the colors sent to your lights.").color(MUTED),
        );
        ui.add_space(12.0);
        if let Some(texture) = &self.texture {
            let aspect = texture.size()[0] as f32 / texture.size()[1] as f32;
            let width = ui.available_width().min(960.0);
            ui.image((texture.id(), Vec2::new(width, width / aspect)));
        } else {
            let width = ui.available_width().min(960.0);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(width, width * 9.0 / 16.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, 6, Color32::from_rgb(18, 24, 34));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Start Video to preview your screen",
                egui::FontId::proportional(16.0),
                MUTED,
            );
            if snapshot.mode == Some(crate::music::SyncMode::Music) {
                ui.small(
                    "Music is currently running. Use Switch to Video to change the output mode.",
                );
            }
        }
        ui.add_space(12.0);
        if ui.button("Edit placement").clicked() {
            self.navigate(Workspace::Layout);
        }
        ui.add_space(12.0);
        ui.heading("Light output");
        for (index, light) in self.config.lights.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.label(&light.name);
                if snapshot.mode == Some(crate::music::SyncMode::Video) {
                    if let Some(zones) = snapshot.colors.get(index) {
                        for color in zones.iter().step_by((zones.len() / 32).max(1)) {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                            ui.painter().rect_filled(
                                rect,
                                2,
                                Color32::from_rgb(color[0], color[1], color[2]),
                            );
                        }
                    }
                } else {
                    ui.small("Video inactive");
                }
            });
        }
        if self.config.lights.is_empty() && ui.button("Go to Devices").clicked() {
            self.navigate(Workspace::Devices);
        }
    }
}
