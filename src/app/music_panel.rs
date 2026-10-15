//! Music controls and live visualization; tab navigation never starts capture.
use super::*;
impl App {
    pub(super) fn music_controls(&mut self, ui: &mut egui::Ui, running: bool) {
        use crate::music::AudioInput;
        section_heading(ui, "Audio source");
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.config.music.input,
                    AudioInput::Playback,
                    "System playback",
                );

            });
            ui.collapsing("Test audio", |ui| {
                ui.selectable_value(&mut self.config.music.input, AudioInput::Demo, "Demo pulses");
                ui.small("Generate a test rhythm without capturing audio.");
            });
            ui.horizontal(|ui| {
                ui.label("Music output");
                ui.selectable_value(&mut self.config.music.output_fps, 30, "30 Hz");
                ui.selectable_value(&mut self.config.music.output_fps, 60, "60 Hz");
            });
            if self.config.music.input == AudioInput::Playback {
                ui.small("Listen to your playback device. Start Music begins audio capture.");
                #[cfg(target_os = "linux")]
                ui.collapsing("Playback device", |ui| {
                    let selected = self.music_devices.iter()
                        .find(|(name, _)| *name == self.config.music.device)
                        .map_or_else(|| if self.config.music.device.is_empty() { "Default playback device".to_owned() } else { "Saved device unavailable".to_owned() },
                            |(_, description)| description.clone());
                    egui::ComboBox::from_id_salt("music-playback-sink")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.config.music.device, String::new(), "Default playback device");
                            for (name, description) in &self.music_devices {
                                ui.selectable_value(&mut self.config.music.device, name.clone(), description);
                            }
                        });
                    if ui.add_enabled(!self.music_devices_pending, egui::Button::new(if self.music_devices_pending { "Refreshing…" } else { "Refresh devices" })).clicked() {
                        self.music_devices_pending = true;
                        let results = self.results.clone();
                        std::thread::spawn(move || {
                            let _ = results.send(JobResult::AudioDevices(crate::music::playback_devices()));
                        });
                    }
                    if let Some(error) = &self.music_device_error { ui.colored_label(AMBER, error); }
                    if !self.config.music.device.is_empty() && !self.music_devices.is_empty()
                        && !self.music_devices.iter().any(|(name, _)| *name == self.config.music.device) {
                        ui.colored_label(AMBER, "Saved playback device is unavailable. Select another device or use default.");
                    }
                    ui.small("Device changes require stopping Music.");
                    ui.separator();
                    ui.small("Expert PipeWire node name");
                    ui.add(
                        egui::TextEdit::singleline(&mut text::ByteText {
                            text: &mut self.config.music.device,
                            limit: 1024,
                        })
                        .hint_text("Blank uses the default playback device")
                        .desired_width(f32::INFINITY),
                    );
                    ui.small("Leave blank to follow the system default.");
                });
                #[cfg(not(target_os = "linux"))]
                ui.colored_label(
                    AMBER,
                    "System playback capture currently requires Linux. Try Demo pulses.",
                );
            } else {
                ui.colored_label(AMBER, "Simulated bass pulses. No audio is captured.");
                egui::ComboBox::from_id_salt("music-demo-scenario")
                    .selected_text(format!("{:?}", self.config.music.demo_scenario))
                    .show_ui(ui, |ui| {
                        for (scenario, name) in [
                            (crate::music::DemoScenario::Straight, "Straight beat"),
                            (crate::music::DemoScenario::Hats, "Beat and hats"),
                            (crate::music::DemoScenario::Swing, "Swing"),
                            (crate::music::DemoScenario::TempoStep, "Tempo change"),
                            (crate::music::DemoScenario::Quiet, "Quiet playback"),
                            (crate::music::DemoScenario::SilenceGap, "Silence gap"),
                        ] { ui.selectable_value(&mut self.config.music.demo_scenario, scenario, name); }
                    });
            }
        });
        ui.add_space(18.0);
        section_heading(ui, &format!("Music · {}", self.config.music.effect.label()));
        ui.add(
            egui::Slider::new(&mut self.config.music.brightness, 0.0..=1.0)
                .text("Brightness")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        );
        ui.horizontal(|ui| {
            ui.label("Main color");
            ui.color_edit_button_srgb(&mut self.config.music.color);
        });
        ui.horizontal(|ui| {
            ui.label("Effect");
            egui::ComboBox::from_id_salt("music-effect")
                .selected_text(self.config.music.effect.label())
                .show_ui(ui, |ui| {
                    for effect in crate::music::MusicEffect::ALL {
                        ui.selectable_value(&mut self.config.music.effect, effect, effect.label());
                    }
                });
        });
        ui.add_space(12.0);
        section_heading(ui, "Dynamics");
        ui.small("Midbass drives pulses; upper percussion adds sparkles.");
        ui.add(
            egui::Slider::new(&mut self.config.music.sensitivity, 0.0..=1.0)
                .text("Sensitivity")
                .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
        )
        .on_hover_text("Higher values detect quieter bass and smaller beat changes. Try 75–100% for quiet or compressed music; reduce it if extra pulses appear.");
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
        ui.collapsing("Rhythm", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.config.music.rhythm_mode,
                    crate::music::RhythmMode::Auto,
                    "Auto",
                );
                ui.selectable_value(
                    &mut self.config.music.rhythm_mode,
                    crate::music::RhythmMode::Manual,
                    "Manual / Tap",
                );
            });
            if self.config.music.rhythm_mode == crate::music::RhythmMode::Manual {
                ui.add(
                    egui::Slider::new(&mut self.config.music.manual_bpm, 40.0..=240.0).text("BPM"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Tap").clicked() {
                        let now = Instant::now();
                        if self
                            .music_taps
                            .back()
                            .is_some_and(|last| now.duration_since(*last).as_secs_f32() > 2.0)
                        {
                            self.music_taps.clear();
                        }
                        self.music_taps.push_back(now);
                        if self.music_taps.len() > 5 {
                            self.music_taps.pop_front();
                        }
                        let mut intervals: Vec<f32> = self
                            .music_taps
                            .iter()
                            .zip(self.music_taps.iter().skip(1))
                            .map(|(a, b)| b.duration_since(*a).as_secs_f32())
                            .filter(|dt| (0.25..=1.5).contains(dt))
                            .collect();
                        if intervals.len() >= 2 {
                            intervals.sort_by(f32::total_cmp);
                            self.config.music.manual_bpm =
                                (60.0 / intervals[intervals.len() / 2]).clamp(40.0, 240.0);
                        }
                    }
                    if ui.button("½").clicked() {
                        self.config.music.manual_bpm =
                            (self.config.music.manual_bpm / 2.0).max(40.0);
                    }
                    if ui.button("2×").clicked() {
                        self.config.music.manual_bpm =
                            (self.config.music.manual_bpm * 2.0).min(240.0);
                    }
                    if ui.button("Resync").clicked() {
                        self.config.music.resync_revision =
                            self.config.music.resync_revision.wrapping_add(1);
                    }
                });
            } else {
                ui.collapsing("Tempo range", |ui| {
                    ui.add(
                        egui::Slider::new(&mut self.config.music.tempo_min_bpm, 40..=239)
                            .text("Minimum BPM"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.config.music.tempo_max_bpm, 41..=240)
                            .text("Maximum BPM"),
                    );
                    if self.config.music.tempo_min_bpm >= self.config.music.tempo_max_bpm {
                        self.config.music.tempo_max_bpm =
                            (self.config.music.tempo_min_bpm + 1).min(240);
                    }
                });
            }
            ui.add(
                egui::Slider::new(&mut self.config.music.phase_offset_beats, -0.5..=0.5)
                    .text("Phase nudge")
                    .suffix(" beat"),
            );
        });
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
        ui.collapsing("Palette and effect settings", |ui| {
            use crate::music::MusicEffect;
            ui.horizontal(|ui| {
                ui.label("Second color");
                ui.color_edit_button_srgb(&mut self.config.music.secondary_color);
            });
            let mut custom_sparkle = self.config.music.sparkle_color.is_some();
            if ui
                .checkbox(&mut custom_sparkle, "Separate sparkle color")
                .changed()
            {
                self.config.music.sparkle_color = custom_sparkle.then_some(self.config.music.color);
            }
            if let Some(color) = &mut self.config.music.sparkle_color {
                ui.color_edit_button_srgb(color);
            }
            if self.config.music.effect == MusicEffect::Alternating {
                ui.small("Lights alternate by stable ID unless an explicit order is set.");
                if ui.button("Use current light order").clicked() {
                    self.config.music.alternating_order = self
                        .config
                        .lights
                        .iter()
                        .map(|light| light.id.clone())
                        .collect();
                }
                if !self.config.music.alternating_order.is_empty()
                    && ui.button("Use ID order").clicked()
                {
                    self.config.music.alternating_order.clear();
                }
            }
        });
        ui.small("Music settings apply live and are saved separately from video settings.");
        if self.config.lights.iter().any(|l| l.route.is_ambient()) {
            ui.colored_label(
                AMBER,
                "Home Assistant and Zigbee use slower ambient updates. Fast pulses work best with WLED.",
            );
        }
    }

    fn music_light_controls(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        section_heading(ui, "Per-light settings");
        self.music_light_selector(ui);
        if let Some(id) = self
            .selected
            .and_then(|index| self.config.lights.get(index))
            .map(|light| light.id.clone())
        {
            ui.collapsing("Selected light · music", |ui| {
                let before = self
                    .config
                    .music
                    .light_overrides
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                let mut value = before.clone();
                ui.checkbox(&mut value.enabled, "Enabled");
                ui.add(egui::Slider::new(&mut value.gain, 0.0..=2.0).text("Gain"));
                let mut custom = value.effect.is_some();
                if ui.checkbox(&mut custom, "Override effect").changed() {
                    value.effect = custom.then_some(self.config.music.effect);
                }
                if let Some(effect) = &mut value.effect {
                    for kind in crate::music::MusicEffect::ALL {
                        ui.selectable_value(effect, kind, kind.label());
                    }
                }
                if value != before {
                    if value == crate::music::LightMusicOverride::default() {
                        self.config.music.light_overrides.remove(&id);
                    } else {
                        self.config.music.light_overrides.insert(id, value);
                    }
                }
            });
        }
    }

    pub(super) fn observe_music(&mut self, snapshot: &crate::engine::Snapshot) {
        if snapshot.mode == Some(crate::music::SyncMode::Music)
            && snapshot.frames != self.music_last_snapshot
        {
            if snapshot.frames < self.music_last_snapshot
                || snapshot.music_frame.is_some_and(|frame| {
                    self.music_frames
                        .back()
                        .is_some_and(|last| frame.time_seconds < last.time_seconds)
                })
            {
                self.music_frames.clear();
                self.music_events.clear();
            }
            self.music_last_snapshot = snapshot.frames;
            if let Some(frame) = snapshot.music_frame {
                if self.music_frames.len() == 180 {
                    self.music_frames.pop_front();
                }
                self.music_frames.push_back(frame);
            }
            for event in &snapshot.music_events {
                if self.music_events.len() == 128 {
                    self.music_events.pop_front();
                }
                self.music_events.push_back(*event);
            }
        }
    }

    pub(super) fn music_canvas(&mut self, ui: &mut egui::Ui, snapshot: &crate::engine::Snapshot) {
        self.canvas_rect = None;
        self.drag = None;
        ui.heading(format!("Music · {}", self.config.music.effect.label()));
        ui.label(
            egui::RichText::new("Let your lights follow the energy of your music.").color(MUTED),
        );
        if snapshot.mode != Some(crate::music::SyncMode::Music) {
            ui.add_space(12.0);
            ui.label("Choose an audio source, then Start Music.");
            if snapshot.mode == Some(crate::music::SyncMode::Video) {
                ui.small("Video is still synchronizing. Start Music transfers control to this workspace.");
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
        ui.horizontal(|ui| {
            for (label, level) in ["Sub", "Midbass", "Body", "Air"]
                .into_iter()
                .zip(telemetry.bands)
            {
                ui.vertical(|ui| {
                    ui.small(label);
                    ui.add(
                        egui::ProgressBar::new(level.sqrt().clamp(0.0, 1.0)).desired_width(65.0),
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
        if snapshot.mode == Some(crate::music::SyncMode::Music) {
            ui.label(format!("{state}{tempo}"));
        } else {
            ui.label("Music inactive");
        }
        ui.add(egui::ProgressBar::new(telemetry.confidence).text("Rhythm confidence"))
            .on_hover_text("Internal timing evidence, not a measured recognition accuracy.");
        ui.collapsing("Diagnostics", |ui| {
            ui.small(format!(
                "Capture epoch {} · {} samples · {} estimated queue ms · {} dropped events",
                telemetry.capture_epoch,
                telemetry.sample_cursor,
                telemetry
                    .estimated_queue_age_ms
                    .map_or("unknown".into(), |age| format!("{age:.1}")),
                telemetry.dropped_events
            ));
            if self.music_frames.len() >= 2 {
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 85.0), Sense::hover());
                ui.painter()
                    .rect_filled(rect, 3.0, Color32::from_rgb(13, 17, 24));
                let first = self.music_frames.front().unwrap().time_seconds;
                let last = self.music_frames.back().unwrap().time_seconds;
                let span = (last - first).max(0.01);
                for (band, color) in [
                    Color32::from_rgb(70, 130, 255),
                    Color32::from_rgb(70, 220, 170),
                    Color32::from_rgb(255, 185, 70),
                    Color32::from_rgb(220, 90, 190),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut previous = None;
                    for frame in &self.music_frames {
                        let x = rect.left()
                            + ((frame.time_seconds - first) / span) as f32 * rect.width();
                        let y = rect.bottom() - frame.novelty[band].clamp(0.0, 1.0) * rect.height();
                        let point = Pos2::new(x, y);
                        if let Some(from) = previous {
                            ui.painter()
                                .line_segment([from, point], Stroke::new(1.0_f32, color));
                        }
                        previous = Some(point);
                    }
                }
                for event in &self.music_events {
                    if let crate::music::AnalysisEvent::Beat(beat) = event
                        && beat.starts_visual
                        && (first..=last).contains(&beat.time_seconds)
                    {
                        let x = rect.left()
                            + ((beat.time_seconds - first) / span) as f32 * rect.width();
                        ui.painter().line_segment(
                            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                            Stroke::new(1.0_f32, if beat.observed { ACCENT } else { AMBER }),
                        );
                    }
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Export JSON").clicked() {
                    self.export_music_json(telemetry);
                }
                if ui.button("Export CSV").clicked() {
                    self.export_music_csv();
                }
            });
            ui.small("Exports settings, features and events. Audio samples are excluded.");
        });
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
        self.music_light_controls(ui);
    }
}
