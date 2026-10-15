//! Interactive placement and physical LED distribution along paths.
use super::*;

impl App {
    pub(super) fn placement_inspector(&mut self, ui: &mut egui::Ui, running: bool) {
        ui.heading("Placement");
        let Some(index) = self.selected.filter(|&i| i < self.config.lights.len()) else {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Select a light to see its properties.").color(MUTED));
            return;
        };
        let global_response = self.config.soft_ambience.response_ms;
        let light = &mut self.config.lights[index];
        ui.label(egui::RichText::new(&light.name).strong());

        ui.add_space(12.0);
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
                    segment_leds: vec![],
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
                ui.small("Drag to snap to the grid. Hold Alt for free placement; X/Y entries stay exact.");
            }
            Shape::Strip {
                points,
                radius,
                segment_leds,
                reverse,
            } => {
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
                        .add_enabled(
                            points.len() < 128
                                && (segment_leds.is_empty()
                                    || segment_leds[self.selected_point.min(points.len() - 2)]
                                        >= 2),
                            egui::Button::new("Insert point"),
                        )
                        .clicked()
                    {
                        let segment = self.selected_point.min(points.len() - 2);
                        let point = Point {
                            x: (points[segment].x + points[segment + 1].x) * 0.5,
                            y: (points[segment].y + points[segment + 1].y) * 0.5,
                        };
                        if crate::editor::insert_strip_point(points, segment_leds, segment, point) {
                            self.selected_point = segment + 1;
                            if light.zones > 1 {
                                light.zones = light.zones.max(segment_leds.len());
                            }
                        }
                    }
                    if ui
                        .add_enabled(points.len() > 2, egui::Button::new("Remove point"))
                        .clicked()
                    {
                        crate::editor::remove_strip_point(
                            points,
                            segment_leds,
                            self.selected_point,
                        );
                        self.selected_point = self.selected_point.min(points.len() - 1);
                    }
                });
                ui.small("Drag a numbered point. Double-click the selected path to insert a point. The arrow shows first to last color.");
                if !light.route.is_ambient() {
                    ui.add_space(10.0);
                    let previous_total: usize = segment_leds.iter().sum();
                    let mut custom = !segment_leds.is_empty();
                    ui.add_enabled_ui(!running && !self.busy, |ui| {
                        let total = match light.route {
                            Route::Wled { count, .. } => count,
                            _ => 60usize.max(points.len() - 1),
                        };
                        let enough_leds = custom || total >= points.len() - 1;
                        if ui
                            .add_enabled(
                                enough_leds,
                                egui::Checkbox::new(&mut custom, "Set LEDs per segment"),
                            )
                            .changed()
                        {
                            if custom {
                                *segment_leds =
                                    crate::editor::evenly_distributed_leds(total, points.len() - 1);
                                if light.zones > 1 {
                                    light.zones = light.zones.max(segment_leds.len());
                                }
                            } else {
                                segment_leds.clear();
                            }
                        }
                        if !enough_leds {
                            ui.small(
                                "Map at least one LED per path segment to enable segment counts.",
                            );
                        }
                        if custom {
                            for (i, (pair, count)) in
                                points.windows(2).zip(segment_leds.iter_mut()).enumerate()
                            {
                                ui.horizontal(|ui| {
                                    ui.label(segment_label(pair[0], pair[1], i));
                                    ui.add(
                                        egui::DragValue::new(count).range(1..=4096).suffix(" LEDs"),
                                    );
                                });
                            }
                        }
                    });
                    if custom {
                        let total: usize = segment_leds.iter().sum();
                        if total != previous_total {
                            light.zones = light.zones.min(total.min(256));
                        }
                        if let Route::Wled { count, .. } = &mut light.route {
                            *count = total;
                        }
                        ui.label(format!("Total: {total} LEDs"));
                        ui.small(
                            "Counts follow the numbered path. Reverse starts at the last segment.",
                        );
                        if running {
                            ui.small("Stop to change LED counts.");
                        }
                    }
                }
            }
        }
        if matches!(light.shape, Shape::Bulb { .. }) {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Video response").strong());
            let mut inherit = light.video_response_ms.is_none();
            if ui.checkbox(&mut inherit, "Use global Response").changed() {
                light.video_response_ms = if inherit { None } else { Some(global_response) };
            }
            let mut response = light.video_response_ms.unwrap_or(global_response);
            if ui
                .add_enabled(
                    !inherit,
                    egui::Slider::new(&mut response, 0.0..=1500.0)
                        .clamping(egui::SliderClamping::Edits)
                        .text("Response")
                        .suffix(" ms"),
                )
                .changed()
            {
                light.video_response_ms = Some(response);
            }
            ui.small("Applies to this bulb in Video. Changes apply live.");
            if light.route.is_ambient() {
                ui.small("The device's ambient update interval still limits physical response.");
            }
        }
        ui.add_space(12.0);
        if ui.button("Device properties").clicked() {
            self.navigate(Workspace::Devices);
        }
    }

    pub(super) fn canvas(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.engine.as_ref().unwrap().snapshots.borrow().clone();
        self.sync_desktop_preview(ui, &snapshot);
        ui.heading("Desktop placement");
        ui.label(
            egui::RichText::new("Place each light where it should sample the desktop.")
                .color(MUTED),
        );
        ui.add_space(12.0);
        let aspect = self
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
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Start video to see your screen here",
                egui::FontId::proportional(17.0),
                MUTED,
            );
        }
        // Keep the same normalized grid over live video and the empty canvas.
        for step in 1..crate::editor::PLACEMENT_GRID_DIVISIONS {
            let fraction = step as f32 / crate::editor::PLACEMENT_GRID_DIVISIONS as f32;
            let x = rect.left() + rect.width() * fraction;
            let y = rect.top() + rect.height() * fraction;
            let major = step % 5 == 0;
            let color = Color32::from_white_alpha(if major { 40 } else { 14 });
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.0_f32, color),
            );
            painter.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.0_f32, color),
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
                    ..
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
            let anchor = match &light.shape {
                Shape::Bulb { center, .. } => screen(*center),
                Shape::Strip { points, .. } => screen(points[0]),
            };
            let name = display_name(&light.name);
            let label = painter.layout_no_wrap(name, egui::FontId::proportional(12.0), marker);
            let x = (anchor.x - label.size().x * 0.5).clamp(
                rect.left(),
                (rect.right() - label.size().x).max(rect.left()),
            );
            let y = if anchor.y + 19.0 + label.size().y < rect.bottom() {
                anchor.y + 19.0
            } else {
                anchor.y - 19.0 - label.size().y
            };
            painter.galley(Pos2::new(x, y.max(rect.top())), label, marker);
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
            && let Shape::Strip {
                points,
                segment_leds,
                ..
            } = &mut light.shape
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
                if crate::editor::insert_strip_point(points, segment_leds, segment, position) {
                    self.selected_point = segment + 1;
                    if light.zones > 1 {
                        light.zones = light.zones.max(segment_leds.len());
                    }
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
            let position = if ui.input(|input| input.modifiers.alt) {
                position
            } else {
                crate::editor::snap_placement(position)
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
                if self.preview.is_some() {
                    "VIDEO PREVIEW"
                } else {
                    "PLACEMENT GRID"
                },
                MUTED,
            );
            if self.texture.is_some() && snapshot.state != SessionState::Running {
                badge(ui, "FROZEN PREVIEW", AMBER);
            }
            ui.label(
                egui::RichText::new("Grid snap 2% · arrows to nudge · Shift for larger steps · Alt for free placement")
                    .small()
                    .color(MUTED),
            );
        });
    }
}

fn segment_label(a: Point, b: Point, index: usize) -> String {
    let side = if (a.y - b.y).abs() < 0.0001 && a.y <= 0.1 {
        "Top"
    } else if (a.y - b.y).abs() < 0.0001 && a.y >= 0.9 {
        "Bottom"
    } else if (a.x - b.x).abs() < 0.0001 && a.x <= 0.1 {
        "Left"
    } else if (a.x - b.x).abs() < 0.0001 && a.x >= 0.9 {
        "Right"
    } else {
        "Segment"
    };
    format!("{side} · {} to {}", index + 1, index + 2)
}
