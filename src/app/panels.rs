//! Placement canvas and side panels. Application state and lifecycle stay in the parent.
use super::*;

impl App {
    pub(super) fn setup(
        &mut self,
        ui: &mut egui::Ui,
        running: bool,
        snapshot: &crate::engine::Snapshot,
    ) {
        section_heading(ui, "01", "Desktop source");
        let synthetic = matches!(self.config.source, CaptureSelection::Synthetic);
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(!synthetic, "Screen / window").clicked() {
                    select_desktop(&mut self.config.source);
                }
                if ui.selectable_label(synthetic, "Demo").clicked() {
                    self.config.source = CaptureSelection::Synthetic;
                }
            });
            if !synthetic {
                #[cfg(target_os = "linux")]
                ui.label(egui::RichText::new("Start opens your desktop’s sharing dialog. Choose one screen or window there.").small().color(MUTED));
                #[cfg(not(target_os = "linux"))]
                {
                    if ui.button("Find screens and windows").clicked() { self.sources = crate::capture::sources(); }
                    if let CaptureSelection::Desktop { id } = &mut self.config.source {
                        egui::ComboBox::from_id_salt("source").width(ui.available_width())
                            .selected_text(self.sources.iter().find(|s| id.as_ref() == Some(&s.id)).map(|s|s.name.as_str()).unwrap_or("Primary display / saved source"))
                            .show_ui(ui, |ui| { for source in &self.sources { ui.selectable_value(id, Some(source.id.clone()), &source.name); } });
                    }
                }
            } else {
                ui.label(egui::RichText::new("Generated colors for trying the editor. No desktop is captured.").small().color(AMBER));
            }
        });
        if running {
            ui.small("Stop to choose a different source.");
        }
        ui.add_space(18.0);
        section_heading(ui, "02", "Synchronization");
        ui.add(
            egui::Slider::new(&mut self.config.brightness, 0.0..=1.0)
                .text("Brightness")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        ui.add(
            egui::Slider::new(&mut self.config.smoothing_ms, 0.0..=5000.0)
                .text("Smoothing")
                .suffix(" ms"),
        );
        ui.label(
            egui::RichText::new("More smoothing makes color changes gentler.")
                .small()
                .color(MUTED),
        );
        ui.collapsing("Capture and control options", |ui| {
            ui.add_enabled(!running, egui::Slider::new(&mut self.config.fps, 1..=120).text("Capture fps"));
            ui.add_enabled(!running, egui::Checkbox::new(&mut self.config.restore_wled_state, "Try restoring WLED state on stop"));
            ui.small("Restoration is best effort. Manual changes and automations may compete with synchronization.");
        });
        ui.add_space(18.0);
        section_heading(
            ui,
            "03",
            &format!("Your lights · {}", self.config.lights.len()),
        );
        if self.config.lights.is_empty() {
            ui.label(
                egui::RichText::new("Add a light, then place it on the desktop preview.")
                    .color(MUTED),
            );
        }
        for (i, light) in self.config.lights.iter().enumerate() {
            let selected = self.selected == Some(i);
            let color = snapshot
                .colors
                .get(i)
                .and_then(|c| c.first())
                .copied()
                .unwrap_or([115, 140, 160]);
            let label = format!("{}\n{}", light.name, route_label(&light.route, light.zones));
            let response = ui.add(
                egui::Button::new(egui::RichText::new(label).size(13.0))
                    .selected(selected)
                    .min_size(Vec2::new(ui.available_width(), 44.0)),
            );
            let dot = Pos2::new(response.rect.right() - 13.0, response.rect.center().y);
            ui.painter()
                .circle_filled(dot, 4.0, Color32::from_rgb(color[0], color[1], color[2]));
            if response.clicked() {
                self.selected = Some(i);
                self.selected_point = 0;
                self.compact_inspector = true;
            }
        }
        ui.add_space(10.0);
        ui.add_enabled_ui(!running && !self.busy, |ui| {
            egui::CollapsingHeader::new("+ Add WLED").default_open(self.config.lights.is_empty()).show(ui, |ui| {
                ui.label("Controller address");
                let address = ui.add(egui::TextEdit::singleline(&mut text::ByteText { text: &mut self.host, limit: 253 }).hint_text("wled.local or 192.168.1.50").desired_width(f32::INFINITY));
                let submit = ui.add_enabled(!self.host.trim().is_empty(), egui::Button::new("Inspect and add").min_size(Vec2::new(ui.available_width(), 28.0))).clicked()
                    || (address.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.host.trim().is_empty());
                if submit {
                    self.busy = true;
                    self.notice = "Reading WLED device information…".into();
                    let host = self.host.trim().to_owned(); let tx = self.results.clone();
                    self.runtime.as_ref().unwrap().spawn(async move {
                        let info = outputs::inspect_wled(&host).await.map(|info|(host,info));
                        let _ = tx.try_send(JobResult::Wled(info));
                    });
                }
                ui.small("Direct, smooth streaming. The whole controller is used; unmapped LEDs are black.");
            });
            ui.collapsing("+ Home Assistant", |ui| {
                ui.label("Server URL");
                ui.add(egui::TextEdit::singleline(&mut text::ByteText { text: &mut self.config.ha_url, limit: 2048 }).hint_text("https://homeassistant.example").desired_width(f32::INFINITY));
                ui.label("Access token");
                ui.add(egui::TextEdit::singleline(&mut self.token).char_limit(8192).password(true).hint_text("Blank uses your saved token").desired_width(f32::INFINITY));
                ui.small("Saved securely in your OS credential store.");
                ui.add(egui::Slider::new(&mut self.config.ha_interval_ms, 500..=60000).text("Update interval").suffix(" ms"));
                if ui.button("Connect and discover lights").clicked() {
                    self.busy = true; self.notice = "Connecting to Home Assistant…".into();
                    let url = self.config.ha_url.trim().to_owned(); let token = std::mem::take(&mut self.token); let tx=self.results.clone();
                    self.runtime.as_ref().unwrap().spawn(async move {
                        let saved = if token.is_empty() { Ok(()) } else { outputs::save_token_async(&url,&token).await };
                        let result = match saved { Ok(()) => outputs::discover_ha(&url).await, Err(e) => Err(e) };
                        let _=tx.try_send(JobResult::Ha(url, result));
                    });
                }
                ui.small("Slower ambient updates. Each entity receives one color. Add each physical light once.");
            });
            ui.collapsing("+ Demo lights", |ui| {
                ui.small("Simulated outputs send no device commands.");
                ui.horizontal(|ui| {
                    if ui.button("Bulb").clicked() { self.add("Demo bulb".into(),Route::Mock,1,false); }
                    if ui.button("Strip").clicked() { self.add("Demo strip".into(),Route::Mock,16,true); }
                });
            });
        });
        if self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Connecting…");
            });
        }
        if !snapshot.devices.is_empty() {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Connections").strong());
            for (name, status) in &snapshot.devices {
                ui.label(name);
                ui.label(egui::RichText::new(status).small().color(MUTED));
            }
        }
    }

    pub(super) fn inspector(&mut self, ui: &mut egui::Ui, running: bool) {
        ui.heading("Light properties");
        let Some(index) = self.selected.filter(|&i| i < self.config.lights.len()) else {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(
                    "Select a light in the list or on the desktop to edit its placement.",
                )
                .color(MUTED),
            );
            return;
        };
        let light = &mut self.config.lights[index];
        ui.add_space(10.0);
        ui.label("Name");
        ui.add(
            egui::TextEdit::singleline(&mut text::ByteText {
                text: &mut light.name,
                limit: 256,
            })
            .desired_width(f32::INFINITY),
        );
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Placement").strong());
        ui.horizontal(|ui| {
            if ui
                .selectable_label(matches!(light.shape, Shape::Bulb { .. }), "Bulb")
                .clicked()
                && !matches!(light.shape, Shape::Bulb { .. })
            {
                let center = crate::editor::zone_positions(&light.shape, 1)[0];
                light.shape = Shape::Bulb {
                    center,
                    radius: 0.08,
                };
                self.selected_point = 0;
            }
            if ui
                .selectable_label(matches!(light.shape, Shape::Strip { .. }), "Strip path")
                .clicked()
                && !matches!(light.shape, Shape::Strip { .. })
            {
                let center = crate::editor::zone_positions(&light.shape, 1)[0];
                light.shape = Shape::Strip {
                    points: vec![
                        Point {
                            x: (center.x - 0.2).max(0.0),
                            y: center.y,
                        },
                        Point {
                            x: (center.x + 0.2).min(1.0),
                            y: center.y,
                        },
                    ],
                    radius: 0.05,
                    reverse: false,
                };
                self.selected_point = 0;
            }
        });
        match &mut light.shape {
            Shape::Bulb { center, radius } => {
                ui.add(
                    egui::Slider::new(radius, 0.005..=1.0)
                        .clamping(egui::SliderClamping::Edits)
                        .text("Sample radius"),
                );
                position_controls(ui, center);
                ui.small("Drag the marker. Arrow keys move it precisely.");
            }
            Shape::Strip {
                points,
                radius,
                reverse,
            } => {
                ui.menu_button("Place along desktop…", |ui| {
                    ui.small("Replace the path; Undo restores your placement.");
                    for (label, preset) in [
                        ("Top edge", crate::editor::StripPreset::Top),
                        ("Bottom edge", crate::editor::StripPreset::Bottom),
                        ("Left edge", crate::editor::StripPreset::Left),
                        ("Right edge", crate::editor::StripPreset::Right),
                        ("Three sides", crate::editor::StripPreset::ThreeSides),
                        ("Full perimeter", crate::editor::StripPreset::Perimeter),
                    ] {
                        if ui.button(label).clicked() {
                            *points = crate::editor::strip_preset_points(
                                preset,
                                crate::editor::DEFAULT_STRIP_INSET,
                            );
                            self.selected_point = 0;
                            ui.close();
                        }
                    }
                });
                ui.add(
                    egui::Slider::new(radius, 0.005..=1.0)
                        .clamping(egui::SliderClamping::Edits)
                        .text("Sample extent"),
                );
                ui.checkbox(reverse, "Reverse color order");
                self.selected_point = self.selected_point.min(points.len() - 1);
                egui::ComboBox::from_id_salt("path-point")
                    .selected_text(format!(
                        "Point {} of {}",
                        self.selected_point + 1,
                        points.len()
                    ))
                    .show_ui(ui, |ui| {
                        for i in 0..points.len() {
                            ui.selectable_value(
                                &mut self.selected_point,
                                i,
                                format!("Point {}", i + 1),
                            );
                        }
                    });
                position_controls(ui, &mut points[self.selected_point]);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(points.len() < 128, egui::Button::new("Insert point"))
                        .clicked()
                    {
                        let segment = self.selected_point.min(points.len() - 2);
                        let point = Point {
                            x: (points[segment].x + points[segment + 1].x) * 0.5,
                            y: (points[segment].y + points[segment + 1].y) * 0.5,
                        };
                        if crate::editor::insert_path_point_at(points, segment, point) {
                            self.selected_point = segment + 1;
                        }
                    }
                    if ui
                        .add_enabled(points.len() > 2, egui::Button::new("Remove point"))
                        .clicked()
                    {
                        crate::editor::remove_path_point(points, self.selected_point);
                        self.selected_point = self.selected_point.min(points.len() - 1);
                    }
                });
                ui.small("Drag a numbered point. Double-click the selected path to insert a point. The arrow shows first → last color.");
            }
        }
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Color output").strong());
        if matches!(light.route, Route::HomeAssistant { .. }) {
            light.zones = 1;
            ui.label("Single color · ambient updates");
        } else {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(light.zones == 1, "Single color")
                    .clicked()
                {
                    light.zones = 1;
                }
                if ui
                    .selectable_label(light.zones > 1, "Addressable")
                    .clicked()
                    && light.zones == 1
                {
                    light.zones = 16;
                }
            });
            if light.zones > 1 {
                ui.add(egui::Slider::new(&mut light.zones, 2..=256).text("Color zones"));
            }
            ui.small("Choose addressable only for hardware that accepts separate LED colors.");
        }
        ui.add_space(12.0);
        ui.collapsing("Device route and mapping", |ui| {
            ui.add_enabled_ui(!running && !self.busy, |ui| {
                let mut route = match light.route {
                    Route::Wled { .. } => 0,
                    Route::HomeAssistant { .. } => 1,
                    Route::Mock => 2,
                };
                let old = route;
                egui::ComboBox::from_id_salt("active-route")
                    .selected_text(["Direct WLED", "Home Assistant", "Demo output"][route])
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut route, 0, "Direct WLED");
                        ui.selectable_value(&mut route, 1, "Home Assistant");
                        ui.selectable_value(&mut route, 2, "Demo output");
                    });
                if route != old {
                    light.route = match route {
                        0 => Route::Wled {
                            host: String::new(),
                            start: 0,
                            count: 1,
                            device_id: String::new(),
                        },
                        1 => Route::HomeAssistant {
                            entity_id: "light.".into(),
                        },
                        _ => Route::Mock,
                    };
                    if route == 1 {
                        light.zones = 1;
                    }
                }
                match &mut light.route {
                    Route::Wled {
                        host,
                        start,
                        count,
                        device_id,
                    } => {
                        ui.label("Address");
                        ui.add(
                            egui::TextEdit::singleline(&mut text::ByteText {
                                text: host,
                                limit: 253,
                            })
                            .desired_width(f32::INFINITY),
                        );
                        ui.label("Saved MAC identity");
                        ui.add(
                            egui::TextEdit::singleline(&mut text::ByteText {
                                text: device_id,
                                limit: 128,
                            })
                            .desired_width(f32::INFINITY),
                        );
                        if ui
                            .add_enabled(
                                !host.trim().is_empty(),
                                egui::Button::new("Inspect this controller"),
                            )
                            .clicked()
                        {
                            self.busy = true;
                            let expected = Route::Wled {
                                host: host.clone(),
                                start: *start,
                                count: *count,
                                device_id: device_id.clone(),
                            };
                            let host = host.trim().to_owned();
                            let id = light.id.clone();
                            let tx = self.results.clone();
                            self.runtime.as_ref().unwrap().spawn(async move {
                                let result =
                                    outputs::inspect_wled(&host).await.map(|info| (host, info));
                                let _ = tx.try_send(JobResult::WledRoute(id, expected, result));
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.label("First LED");
                            ui.add(egui::DragValue::new(start).range(0..=65535));
                        });
                        ui.horizontal(|ui| {
                            ui.label("LED count");
                            ui.add(egui::DragValue::new(count).range(1..=4096));
                        });
                    }
                    Route::HomeAssistant { entity_id } => {
                        ui.label("Entity ID");
                        ui.add(
                            egui::TextEdit::singleline(&mut text::ByteText {
                                text: entity_id,
                                limit: 256,
                            })
                            .hint_text("light.desk")
                            .desired_width(f32::INFINITY),
                        );
                    }
                    Route::Mock => {
                        ui.small("Simulated output · no physical device");
                    }
                }
            });
            ui.small(if running {
                "Stop to change devices or hardware mappings."
            } else {
                "One active route per light. Enter the same device identity when changing route."
            });
        });
        ui.add_space(16.0);
        if ui
            .add_enabled(
                !running && !self.busy,
                egui::Button::new(egui::RichText::new("Remove light").color(AMBER)),
            )
            .clicked()
        {
            self.config.lights.remove(index);
            self.selected = None;
            self.drag = None;
            self.notice = "Light removed. Undo restores it.".into();
        }
    }

    pub(super) fn canvas(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.engine.as_ref().unwrap().snapshots.borrow().clone();
        if snapshot.preview.is_none() {
            self.texture = None;
            self.preview = None;
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
        ui.horizontal(|ui| {
            ui.heading("Desktop placement");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.show_zones, "Show color zones");
            });
        });
        ui.label(
            egui::RichText::new("Place each light where it should sample the desktop.")
                .color(MUTED),
        );
        ui.add_space(12.0);
        let aspect = snapshot
            .preview
            .as_ref()
            .map(|f| f.width as f32 / f.height as f32)
            .unwrap_or(16.0 / 9.0);
        let available = ui.available_size() - Vec2::new(0.0, 42.0);
        let width = available.x.min(available.y.max(1.0) * aspect).max(1.0);
        let size = Vec2::new(width, width / aspect);
        let (area, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), size.y), Sense::hover());
        let rect = Rect::from_center_size(area.center(), size);
        self.canvas_rect = Some(rect);
        let response = ui.interact(rect, ui.id().with("placement"), Sense::click_and_drag());
        let painter = ui.painter_at(rect.expand(1.0));
        painter.rect_filled(rect, 6.0, Color32::from_rgb(18, 24, 34));
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            for step in 1..8 {
                let x = rect.left() + rect.width() * step as f32 / 8.0;
                let y = rect.top() + rect.height() * step as f32 / 8.0;
                painter.line_segment(
                    [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                    Stroke::new(1.0_f32, Color32::from_gray(38)),
                );
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    Stroke::new(1.0_f32, Color32::from_gray(38)),
                );
            }
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Arrange your lights, then Start",
                egui::FontId::proportional(17.0),
                MUTED,
            );
        }
        let screen = |p: Point| {
            Pos2::new(
                rect.left() + p.x * rect.width(),
                rect.top() + p.y * rect.height(),
            )
        };
        let mut handles = vec![];
        for (index, light) in self.config.lights.iter().enumerate() {
            let selected = self.selected == Some(index);
            let marker = if selected {
                ACCENT
            } else {
                Color32::from_rgb(188, 207, 224)
            };
            let colors = snapshot.colors.get(index);
            match &light.shape {
                Shape::Bulb { center, radius } => {
                    let pos = screen(*center);
                    painter.circle_filled(
                        pos,
                        *radius * rect.width().min(rect.height()),
                        marker.gamma_multiply(0.10),
                    );
                    painter.circle_stroke(
                        pos,
                        *radius * rect.width().min(rect.height()),
                        Stroke::new(1.0_f32, marker),
                    );
                    handles.push((index, 0, pos));
                }
                Shape::Strip {
                    points,
                    radius,
                    reverse,
                } => {
                    for pair in points.windows(2) {
                        let a = screen(pair[0]);
                        let b = screen(pair[1]);
                        painter.line_segment(
                            [a, b],
                            Stroke::new(
                                (*radius * rect.width().min(rect.height()) * 2.0).max(4.0),
                                marker.gamma_multiply(0.12),
                            ),
                        );
                        painter.line_segment([a, b], Stroke::new(2.0_f32, marker));
                    }
                    for (point, position) in points.iter().enumerate() {
                        handles.push((index, point, screen(*position)));
                    }
                    let (start, end) = if *reverse {
                        (points[1], points[0])
                    } else {
                        (points[points.len() - 2], points[points.len() - 1])
                    };
                    let vector = (screen(end) - screen(start)) * 0.22;
                    painter.arrow(screen(end) - vector, vector, Stroke::new(2.5_f32, marker));
                }
            }
            if self.show_zones {
                for (zone, position) in crate::editor::zone_positions_scaled(
                    &light.shape,
                    light.zones,
                    rect.width(),
                    rect.height(),
                )
                .iter()
                .enumerate()
                {
                    let color = colors
                        .and_then(|c| c.get(zone))
                        .copied()
                        .unwrap_or([90, 110, 135]);
                    painter.circle_filled(
                        screen(*position),
                        3.5,
                        Color32::from_rgb(color[0], color[1], color[2]),
                    );
                }
            }
            let anchor = match &light.shape {
                Shape::Bulb { center, .. } => screen(*center),
                Shape::Strip { points, .. } => screen(points[0]),
            };
            let name = display_name(&light.name);
            painter.text(
                anchor + Vec2::new(0.0, 19.0),
                egui::Align2::CENTER_TOP,
                name,
                egui::FontId::proportional(12.0),
                marker,
            );
        }
        // Paint the active handle last, including when another handle occupies
        // exactly the same point. Hit testing follows the same preference.
        handles.sort_by_key(|(index, point, _)| {
            self.selected == Some(*index) && self.selected_point == *point
        });
        for (index, point, position) in &handles {
            let focused = self.selected == Some(*index) && self.selected_point == *point;
            painter.circle_filled(
                *position,
                if focused { 9.0 } else { 7.0 },
                Color32::from_rgb(16, 22, 31),
            );
            painter.circle_stroke(
                *position,
                if focused { 9.0 } else { 7.0 },
                Stroke::new(2.0_f32, if focused { ACCENT } else { Color32::WHITE }),
            );
            if matches!(self.config.lights[*index].shape, Shape::Strip { .. }) {
                painter.text(
                    *position,
                    egui::Align2::CENTER_CENTER,
                    format!("{}", point + 1),
                    egui::FontId::proportional(10.0),
                    Color32::WHITE,
                );
            }
        }
        // Choose a handle at pointer-down, before the drag threshold can move
        // the pointer away from a small marker.
        if response.hovered()
            && ui.input(|i| i.pointer.primary_pressed())
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.drag = handles
                .iter()
                .filter(|(_, _, p)| p.distance(pos) < 20.0)
                .min_by(|a, b| {
                    a.2.distance(pos)
                        .total_cmp(&b.2.distance(pos))
                        .then_with(|| {
                            (self.selected != Some(a.0)).cmp(&(self.selected != Some(b.0)))
                        })
                        .then_with(|| {
                            (self.selected_point != a.1).cmp(&(self.selected_point != b.1))
                        })
                })
                .map(|(i, j, _)| (*i, *j));
            if let Some((index, point)) = self.drag {
                self.selected = Some(index);
                self.selected_point = point;
                self.compact_inspector = true;
            }
        }
        if response.double_clicked()
            && let (Some(index), Some(pos)) = (self.selected, response.interact_pointer_pos())
            && let Some(light) = self.config.lights.get_mut(index)
            && let Shape::Strip { points, .. } = &mut light.shape
        {
            let pixel_points: Vec<Point> = points
                .iter()
                .map(|p| Point {
                    x: screen(*p).x,
                    y: screen(*p).y,
                })
                .collect();
            if let Some((segment, projected, distance)) =
                crate::editor::nearest_segment(&pixel_points, Point { x: pos.x, y: pos.y })
                && distance < 16.0
            {
                let position = Point {
                    x: (projected.x - rect.left()) / rect.width(),
                    y: (projected.y - rect.top()) / rect.height(),
                };
                if crate::editor::insert_path_point_at(points, segment, position) {
                    self.selected_point = segment + 1;
                }
            }
        }
        if response.dragged()
            && let (Some((index, point)), Some(pos)) = (self.drag, response.interact_pointer_pos())
            && let Some(light) = self.config.lights.get_mut(index)
        {
            let position = Point {
                x: ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
                y: ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
            };
            match &mut light.shape {
                Shape::Bulb { center, .. } => *center = position,
                Shape::Strip { points, .. } => {
                    if let Some(p) = points.get_mut(point) {
                        *p = position;
                    }
                }
            }
        }
        if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            self.drag = None;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            badge(
                ui,
                if matches!(self.config.source, CaptureSelection::Synthetic) {
                    "DEMO SOURCE"
                } else {
                    "SCREEN SOURCE"
                },
                if matches!(self.config.source, CaptureSelection::Synthetic) {
                    AMBER
                } else {
                    MUTED
                },
            );
            if self.texture.is_some() && snapshot.state != SessionState::Running {
                badge(ui, "FROZEN PREVIEW", AMBER);
            }
            ui.label(
                egui::RichText::new("Drag to place · arrows to nudge · Shift for larger steps")
                    .small()
                    .color(MUTED),
            );
        });
    }
}
