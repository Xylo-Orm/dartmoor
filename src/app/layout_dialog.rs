//! Clipboard layout interchange uses the same strict schema as persistence.
use crate::config::{self, Config};
use eframe::egui;

pub(super) struct LayoutDialog {
    json: String,
    exporting: bool,
    error: Option<String>,
}
impl LayoutDialog {
    pub fn import() -> Self {
        Self::from_json(String::new())
    }
    pub(super) fn from_json(json: String) -> Self {
        Self {
            json,
            exporting: false,
            error: None,
        }
    }
    pub fn export(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            json: config::encode_layout(config)?,
            exporting: true,
            error: None,
        })
    }
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Config> {
        ui.set_width((ui.ctx().content_rect().width() - 60.0).clamp(240.0, 640.0));
        ui.heading(if self.exporting {
            "Export layout"
        } else {
            "Import layout"
        });
        ui.label(if self.exporting {"Copy this JSON to a file or another installation."}else{"Paste a saved layout JSON document. Import replaces the current layout; Undo restores it."});
        ui.small("Layouts include device addresses and settings. Access tokens stay in the OS credential store.");
        egui::ScrollArea::both()
            .max_height((ui.ctx().content_rect().height() * 0.45).min(360.0))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut super::text::ByteText {
                        text: &mut self.json,
                        limit: config::MAX_CONFIG_BYTES as usize,
                    })
                    .code_editor()
                    .desired_width(f32::INFINITY)
                    .desired_rows(16)
                    .interactive(!self.exporting),
                );
            });
        if let Some(error) = &self.error {
            ui.colored_label(super::AMBER, error);
        }
        let mut imported = None;
        ui.horizontal(|ui| {
            if self.exporting {
                if ui.button("Copy JSON").clicked() {
                    ui.ctx().copy_text(self.json.clone());
                }
                if ui.button("Done").clicked() {
                    ui.close();
                }
            } else {
                if ui
                    .add_enabled(
                        !self.json.trim().is_empty(),
                        egui::Button::new("Use this layout"),
                    )
                    .clicked()
                {
                    match config::decode_layout(&self.json) {
                        Ok(config) => {
                            imported = Some(config);
                            ui.close();
                        }
                        Err(error) => self.error = Some(format!("Could not import: {error:#}")),
                    }
                }
                if ui.button("Cancel").clicked() {
                    ui.close();
                }
            }
        });
        imported
    }
}
