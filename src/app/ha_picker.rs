//! Discovery is a choice, not an instruction to import every light on a server.
use crate::{
    core::{Light, Route},
    outputs::HaLight,
};
use eframe::egui;
use std::collections::HashSet;

pub(super) struct HaPicker {
    pub origin: String,
    rows: Vec<Row>,
    selected: HashSet<String>,
    query: String,
    capacity: usize,
}
struct Row {
    light: HaLight,
    searchable: String,
    reason: Option<&'static str>,
}
impl HaPicker {
    pub fn new(origin: String, discovered: Vec<HaLight>, existing: &[Light]) -> Self {
        let known: HashSet<&str> = existing
            .iter()
            .filter_map(|light| match &light.route {
                Route::HomeAssistant { entity_id } => Some(entity_id.as_str()),
                _ => None,
            })
            .collect();
        let capacity = 64usize
            .saturating_sub(existing.len())
            .min(2048usize.saturating_sub(existing.iter().map(|l| l.zones).sum()));
        let mut seen = HashSet::new();
        let mut rows: Vec<Row> = discovered
            .into_iter()
            .filter(|light| seen.insert(light.entity_id.clone()))
            .map(|mut light| {
                light.name = super::display_name(&light.name);
                let reason = if known.contains(light.entity_id.as_str()) {
                    Some("Already added")
                } else if !light.available {
                    Some("Unavailable")
                } else if !light
                    .color_modes
                    .iter()
                    .any(|m| ["rgb", "rgbw", "rgbww", "hs", "xy"].contains(&m.as_str()))
                {
                    Some("No color control")
                } else {
                    None
                };
                let searchable = format!("{} {}", light.name, light.entity_id).to_lowercase();
                Row {
                    light,
                    searchable,
                    reason,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            a.reason
                .is_some()
                .cmp(&b.reason.is_some())
                .then_with(|| a.searchable.cmp(&b.searchable))
        });
        Self {
            origin,
            rows,
            selected: HashSet::new(),
            query: String::new(),
            capacity,
        }
    }
    fn select_visible(&mut self) {
        let query = self.query.trim().to_lowercase();
        for row in &self.rows {
            if self.selected.len() >= self.capacity {
                break;
            }
            if row.reason.is_none() && row.searchable.contains(&query) {
                self.selected.insert(row.light.entity_id.clone());
            }
        }
    }
    pub fn chosen(&self) -> Vec<HaLight> {
        self.rows
            .iter()
            .filter(|r| self.selected.contains(&r.light.entity_id))
            .map(|r| r.light.clone())
            .collect()
    }
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Vec<HaLight>> {
        ui.set_width(ui.ctx().content_rect().width().min(520.0) - 40.0);
        ui.heading("Choose Home Assistant lights");
        ui.label(&self.origin);
        ui.small("Ambient updates · one color per light. Select only the lights you want to synchronize.");
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Search names or entity IDs")
                .char_limit(256)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.capacity > 0, egui::Button::new("Select visible"))
                .clicked()
            {
                self.select_visible();
            }
            if ui.button("Clear selection").clicked() {
                self.selected.clear();
            }
        });
        let query = self.query.trim().to_lowercase();
        let visible: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.searchable.contains(&query))
            .map(|(index, _)| index)
            .collect();
        // Fixed-height, truncated rows keep rendering bounded by the viewport
        // even when a server exposes thousands of lights.
        let row_height = 76.0;
        egui::ScrollArea::vertical()
            .max_height((ui.ctx().content_rect().height() * 0.4).min(320.0))
            .show_rows(ui, row_height, visible.len(), |ui, range| {
                for position in range {
                    let row = &self.rows[visible[position]];
                    ui.push_id(&row.light.entity_id, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                ui.spacing_mut().item_spacing.y = 3.0;
                                let mut selected = self.selected.contains(&row.light.entity_id);
                                let enabled = row.reason.is_none()
                                    && (selected || self.selected.len() < self.capacity);
                                let response = ui
                                    .add_enabled(
                                        enabled,
                                        egui::Checkbox::new(&mut selected, &row.light.name),
                                    )
                                    .on_hover_text(&row.light.name);
                                if response.changed() {
                                    if selected {
                                        self.selected.insert(row.light.entity_id.clone());
                                    } else {
                                        self.selected.remove(&row.light.entity_id);
                                    }
                                }
                                ui.indent(&row.light.entity_id, |ui| {
                                    ui.small(&row.light.entity_id);
                                    if let Some(reason) = row.reason {
                                        ui.small(reason);
                                    }
                                });
                            },
                        );
                    });
                }
            });
        if visible.is_empty() {
            ui.label("No matching lights. Try another search.");
        }
        ui.separator();
        ui.label(format!(
            "{} selected · room for {} more lights",
            self.selected.len(),
            self.capacity.saturating_sub(self.selected.len())
        ));
        if self.capacity == 0 {
            ui.label("The layout is full. Cancel and remove a light or reduce its zones first.");
        }
        ui.small("Keep one route per physical device. Home Assistant entity IDs alone cannot identify an existing direct WLED controller.");
        let mut chosen = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.selected.is_empty(),
                    egui::Button::new(format!("Add {} selected", self.selected.len())),
                )
                .clicked()
            {
                chosen = Some(self.chosen());
                ui.close();
            }
            if ui.button("Cancel").clicked() {
                ui.close();
            }
        });
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn light(id: &str, available: bool, mode: &str) -> HaLight {
        HaLight {
            entity_id: format!("light.{id}"),
            name: id.into(),
            available,
            color_modes: vec![mode.into()],
        }
    }
    #[test]
    fn discovery_never_selects_automatically_and_filters_unsupported_duplicates() {
        let mut config = crate::config::Config::default();
        config.lights[0].route = Route::HomeAssistant {
            entity_id: "light.existing".into(),
        };
        config.lights[0].zones = 1;
        let mut picker = HaPicker::new(
            "https://ha.example".into(),
            vec![
                light("rgb", true, "rgb"),
                light("rgb", true, "rgb"),
                light("offline", false, "rgb"),
                light("white", true, "color_temp"),
                light("existing", true, "hs"),
            ],
            &config.lights,
        );
        assert!(picker.chosen().is_empty());
        assert_eq!(picker.rows.len(), 4);
        picker.select_visible();
        assert_eq!(
            picker
                .chosen()
                .iter()
                .map(|l| l.entity_id.as_str())
                .collect::<Vec<_>>(),
            vec!["light.rgb"]
        );
    }
    #[test]
    fn search_and_capacity_limit_selection_without_discarding_hidden_choices() {
        let mut config = crate::config::Config::default();
        config.lights[0].zones = 256;
        for i in 1..8 {
            let mut l = config.lights[0].clone();
            l.id = format!("{i}");
            config.lights.push(l);
        }
        let mut full = HaPicker::new(
            "origin".into(),
            vec![light("desk", true, "hs")],
            &config.lights,
        );
        full.select_visible();
        assert!(full.chosen().is_empty());
        let mut picker = HaPicker::new(
            "origin".into(),
            vec![light("desk", true, "hs"), light("room", true, "xy")],
            &[],
        );
        picker.capacity = 1;
        picker.query = "DESK".into();
        picker.select_visible();
        assert_eq!(picker.chosen()[0].name, "desk");
        picker.query = "room".into();
        picker.select_visible();
        assert_eq!(picker.chosen()[0].name, "desk");
        picker.selected.clear();
        picker.select_visible();
        assert_eq!(picker.chosen()[0].name, "room");
    }

    #[test]
    fn large_discovery_list_only_paints_visible_rows_and_searches_all_rows() {
        let mut picker = HaPicker::new(
            "origin".into(),
            (0..1000)
                .map(|i| {
                    let mut light = light(&format!("fixture_{i:04}"), true, "rgb");
                    light.name = format!("Lamp {i:04}");
                    light
                })
                .collect(),
            &[],
        );
        let ctx = egui::Context::default();
        let draw = |picker: &mut HaPicker| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        picker.show(ui);
                    });
                },
            )
        };
        draw(&mut picker);
        let output = draw(&mut picker);
        let names: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text().starts_with("Lamp ") => {
                    Some(text.galley.text().to_owned())
                }
                _ => None,
            })
            .collect();
        assert!(!names.is_empty());
        assert!(
            names.len() <= 8,
            "offscreen rows were painted: {}",
            names.len()
        );
        picker.query = "Lamp 0999".into();
        picker.select_visible();
        assert_eq!(picker.chosen().len(), 1);
        assert_eq!(picker.chosen()[0].entity_id, "light.fixture_0999");
    }
}
