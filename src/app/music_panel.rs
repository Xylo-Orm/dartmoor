//! Music controls and live visualization; tab navigation never starts capture.
use super::*;
impl App {
    pub(super) fn music_controls(&mut self, ui: &mut egui::Ui, running: bool) {
        use crate::music::AudioInput;
        section_heading(ui, "01", "Audio source");
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.config.music.input,
                    AudioInput::Playback,
                    "System playback",
                );
                ui.selectable_value(
                    &mut self.config.music.input,
                    AudioInput::Demo,
                    "Demo pulses",
                );
            });
            if self.config.music.input == AudioInput::Playback {
                ui.small("Listen to your playback device. Start music begins audio capture.");
                #[cfg(target_os = "linux")]
                ui.collapsing("Playback device", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut text::ByteText {
                            text: &mut self.config.music.device,
                            limit: 1024,
                        })
                        .hint_text("Blank uses the default playback device")
                        .desired_width(f32::INFINITY),
                    );
                    ui.small("Optional PipeWire sink node name. Stop music before changing it.");
                });
                #[cfg(not(target_os = "linux"))]
                ui.colored_label(
                    AMBER,
                    "System playback capture currently requires Linux. Try Demo pulses.",
                );
            } else {
                ui.colored_label(AMBER, "Simulated bass pulses. No audio is captured.");
            }
        });
        ui.add_space(18.0);
        section_heading(ui, "02", "Music · Pulse");
        ui.horizontal(|ui| {
            ui.label("Detector");
            ui.selectable_value(
                &mut self.config.music.detector,
                crate::music::DetectorMode::Classic,
                "Classic",
            );
            ui.selectable_value(
                &mut self.config.music.detector,
                crate::music::DetectorMode::Advanced,
                "Advanced",
            );
        });
        let advanced = self.config.music.detector == crate::music::DetectorMode::Advanced;
        ui.small(if advanced {
            "Midbass drives pulses; upper percussion adds sparkles."
        } else {
            "Bass attacks create pulses; musical energy adds a softer glow."
        });
        ui.add(
            egui::Slider::new(&mut self.config.music.sensitivity, 0.0..=1.0)
                .text("Sensitivity")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        )
        .on_hover_text("Higher values detect quieter bass and smaller beat changes. Try 75–100% for quiet or compressed music; reduce it if extra pulses appear.");
        if advanced {
            for (label, value) in [
                ("Subbass weight", &mut self.config.music.subbass_weight),
                ("Sparkle amount", &mut self.config.music.sparkle_amount),
            ] {
                ui.add(
                    egui::Slider::new(value, 0.0..=1.0)
                        .text(label)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
            }
            ui.checkbox(&mut self.config.music.follow_beat, "Follow beat")
                .on_hover_text("Predict weaker pulses only after consistent rhythm is established. Direct attacks continue when confidence is low.");
        }
        ui.add(
            egui::Slider::new(&mut self.config.music.attack_ms, 0.0..=500.0)
                .text("Attack")
                .suffix(" ms"),
        );
        ui.add(
            egui::Slider::new(&mut self.config.music.decay_ms, 20.0..=2000.0)
                .text("Decay")
                .suffix(" ms"),
        );
        ui.add(
            egui::Slider::new(&mut self.config.music.brightness, 0.0..=1.0)
                .text("Brightness")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        ui.horizontal(|ui| {
            ui.label("Pulse color");
            ui.color_edit_button_srgb(&mut self.config.music.color);
        });
        ui.small("Music settings apply live and are saved separately from video settings.");
        if self
            .config
            .lights
            .iter()
            .any(|l| matches!(l.route, Route::HomeAssistant { .. }))
        {
            ui.colored_label(
                AMBER,
                "Home Assistant uses slower ambient updates. Fast pulses work best with WLED.",
            );
        }
    }

    pub(super) fn music_canvas(&mut self, ui: &mut egui::Ui, snapshot: &crate::engine::Snapshot) {
        self.canvas_rect = None;
        self.drag = None;
        ui.heading("Music · Pulse");
        ui.label(
            egui::RichText::new("Let your lights follow the energy of your music.").color(MUTED),
        );
        if snapshot.mode != Some(crate::music::SyncMode::Music) {
            ui.add_space(12.0);
            ui.label("Choose an audio source, then Start music.");
            if snapshot.mode == Some(crate::music::SyncMode::Video) {
                ui.small("Video is still synchronizing. Start music transfers control to this workspace.");
            }
        }
        let telemetry = if snapshot.mode == Some(crate::music::SyncMode::Music) {
            snapshot.music.unwrap_or_default()
        } else {
            crate::music::Telemetry::default()
        };
        ui.add_space(16.0);
        for (label, value) in [("Audio level", telemetry.rms), ("Pulse", telemetry.pulse)] {
            ui.label(label);
            ui.add(
                egui::ProgressBar::new(value.clamp(0.0, 1.0))
                    .desired_width(ui.available_width())
                    .show_percentage(),
            );
            ui.add_space(6.0);
        }
        ui.small(format!("{} main attacks detected", telemetry.onsets));
        if self.config.music.detector == crate::music::DetectorMode::Advanced {
            ui.horizontal(|ui| {
                for (label, level) in ["Sub", "Midbass", "Body", "Air"]
                    .into_iter()
                    .zip(telemetry.bands)
                {
                    ui.vertical(|ui| {
                        ui.small(label);
                        ui.add(
                            egui::ProgressBar::new(level.sqrt().clamp(0.0, 1.0))
                                .desired_width(65.0),
                        )
                        .on_hover_text(format!(
                            "Band RMS {level:.4}; display uses a square-root scale."
                        ));
                    });
                }
            });
            ui.small(format!(
                "{} sparkles · {} predicted pulses · {} stream resets",
                telemetry.sparkle_onsets, telemetry.predicted_beats, telemetry.discontinuities
            ));
            let state = match telemetry.tracking {
                crate::music::TrackingState::Listening => "Listening",
                crate::music::TrackingState::Following => "Following beat",
                crate::music::TrackingState::Reacquiring => "Reacquiring",
            };
            let tempo = telemetry
                .bpm
                .map_or(String::new(), |bpm| format!(" · {bpm:.0} BPM"));
            ui.label(format!("{state}{tempo}"));
            ui.add(egui::ProgressBar::new(telemetry.confidence).text("Rhythm confidence"))
                .on_hover_text("Internal timing evidence, not a measured recognition accuracy.");
        }
        ui.add_space(18.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (index, light) in self.config.lights.iter().enumerate() {
                if ui
                    .selectable_label(self.selected == Some(index), &light.name)
                    .clicked()
                {
                    self.selected = Some(index);
                }
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
                let colors = snapshot
                    .colors
                    .get(index)
                    .filter(|_| snapshot.mode == Some(crate::music::SyncMode::Music));
                let zones = colors.map_or(1, |colors| colors.len().max(1));
                for zone in 0..zones {
                    let color = colors
                        .and_then(|colors| colors.get(zone))
                        .copied()
                        .unwrap_or([0; 3]);
                    let part = Rect::from_min_max(
                        Pos2::new(
                            rect.left() + rect.width() * zone as f32 / zones as f32,
                            rect.top(),
                        ),
                        Pos2::new(
                            rect.left() + rect.width() * (zone + 1) as f32 / zones as f32,
                            rect.bottom(),
                        ),
                    );
                    ui.painter().rect_filled(
                        part,
                        0.0,
                        Color32::from_rgb(color[0], color[1], color[2]),
                    );
                }
                ui.small(route_label(&light.route, light.zones));
                ui.add_space(10.0);
            }
        });
    }
}
