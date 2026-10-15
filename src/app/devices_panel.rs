//! Device discovery, connection management, and hardware properties.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum DeviceView {
    Properties,
    Add,
}

impl App {
    pub(super) fn device_details(
        &mut self,
        ui: &mut egui::Ui,
        running: bool,
        snapshot: &crate::engine::Snapshot,
    ) {
        match self.device_view {
            DeviceView::Add => self.device_connections(ui, running, snapshot),
            DeviceView::Properties if self.config.lights.is_empty() => {
                ui.heading("Your devices");
                ui.label("Choose Add device to connect WLED, discover Home Assistant lights, or pair Zigbee lights with your USB coordinator.");
            }
            DeviceView::Properties => self.device_inspector(ui, running),
        }
    }

    pub(super) fn device_connections(
        &mut self,
        ui: &mut egui::Ui,
        running: bool,
        snapshot: &crate::engine::Snapshot,
    ) {
        ui.heading("Add device");
        egui::CollapsingHeader::new("Zigbee").default_open(true).show(ui, |ui| {
            let state = crate::zigbee::snapshot();
            ui.label(&state.status);
            if ui.add_enabled(!running && !self.busy, egui::Button::new("Discover")).clicked() {
                self.busy = true;
                self.notice = "Discovering Zigbee devices…".into();
                let tx = self.results.clone();
                self.runtime.as_ref().unwrap().spawn(async move {
                    let _ = tx.try_send(JobResult::Zigbee(crate::zigbee::discover().await));
                });
            }
            ui.small("Finds your Sonoff USB coordinator. Put a new bulb in pairing mode while discovery is open.");
            if state.pairing { ui.label("Pairing is open for 60 seconds."); }
            if let Some(adapter) = &state.adapter {
                if state.devices.is_empty() { ui.small("No lights found yet. Keep the bulb powered on and nearby."); }
                egui::ScrollArea::vertical().id_salt("zigbee-discovered-lights").max_height(200.0).show(ui, |ui| {
                    for device in &state.devices {
                        let added = self.config.lights.iter().any(|light| matches!(&light.route,
                            Route::Zigbee { coordinator, ieee_address, endpoint }
                            if coordinator == &adapter.id && ieee_address == &device.ieee_address && endpoint == &device.endpoint));
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(&device.name);
                                ui.small(&device.ieee_address);
                                if let Some(reason) = &device.reason { ui.small(reason); }
                            });
                            if ui.add_enabled(!running && !self.busy && !added && device.reason.is_none(),
                                egui::Button::new(if added { "Added" } else { "Add" })).clicked()
                                && self.add(device.name.clone(), Route::Zigbee {
                                    coordinator: adapter.id.clone(), ieee_address: device.ieee_address.clone(), endpoint: device.endpoint,
                                }, 1, false) {
                                    self.notice = "Zigbee light added and connected. Place it in Layout.".into();
                                    self.compact_inspector = true;
                            }
                        });
                        ui.separator();
                    }
                });
            }
        });
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

    pub(super) fn device_inspector(&mut self, ui: &mut egui::Ui, running: bool) {
        ui.heading("Device properties");
        let Some(index) = self.selected.filter(|&i| i < self.config.lights.len()) else {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Select a light to see its properties.").color(MUTED));
            return;
        };
        let light = &mut self.config.lights[index];

        ui.label("Name");
        ui.add(
            egui::TextEdit::singleline(&mut text::ByteText {
                text: &mut light.name,
                limit: 256,
            })
            .desired_width(f32::INFINITY),
        );
        ui.add_enabled_ui(!running && !self.busy, |ui| {
            ui.label(egui::RichText::new("Color output").strong());
            let maximum_zones = match &light.shape {
                Shape::Strip { segment_leds, .. } if !segment_leds.is_empty() => {
                    segment_leds.iter().sum::<usize>().min(256)
                }
                _ => 256,
            };
            // Rendering does not rewrite configuration. Bounds apply to edits.
            if light.route.is_ambient() {
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
                        .add_enabled_ui(maximum_zones > 1, |ui| {
                            ui.selectable_label(light.zones > 1, "Addressable")
                        })
                        .inner
                        .clicked()
                        && light.zones == 1
                    {
                        light.zones = 16.min(maximum_zones);
                    }
                });
                if light.zones > 1 {
                    let minimum = match &light.shape {
                        Shape::Strip { segment_leds, .. } => segment_leds.len().max(2),
                        _ => 2,
                    };

                    ui.add(
                        egui::Slider::new(&mut light.zones, minimum..=maximum_zones)
                            .text("Color zones"),
                    );
                }
                ui.small("Choose addressable only for hardware that accepts separate LED colors.");
            }
            ui.add_space(12.0);
        });
        if running {
            ui.small("Stop synchronization to change hardware output settings.");
        }

        egui::CollapsingHeader::new("Connection and LED mapping").default_open(true).show(ui, |ui| {
            ui.add_enabled_ui(!running && !self.busy, |ui| {
                let mut route = match light.route {
                    Route::Wled { .. } => 0,
                    Route::HomeAssistant { .. } => 1,
                    Route::Zigbee { .. } => 2,
                    Route::Mock => 0,
                };
                let old = route;
                egui::ComboBox::from_id_salt("active-route")
                    .selected_text(["Direct WLED", "Home Assistant", "Zigbee USB"][route])
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut route, 0, "Direct WLED");
                        ui.selectable_value(&mut route, 1, "Home Assistant");
                        if old == 2 { ui.selectable_value(&mut route, 2, "Zigbee USB"); }
                    });
                if route != old {
                    light.route = match route {
                        0 => Route::Wled {
                            host: String::new(),
                            start: 0,
                            count: match &light.shape {
                                Shape::Strip { segment_leds, .. } if !segment_leds.is_empty() => segment_leds.iter().sum(),
                                _ => 1,
                            },
                            device_id: String::new(),
                        },
                        1 => Route::HomeAssistant {
                            entity_id: "light.".into(),
                        },
                        _ => unreachable!(),
                    };
                    if route != 0 {
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
                        ui.collapsing("Advanced identity", |ui| {
                        ui.label("Saved MAC identity");
                        ui.add(
                            egui::TextEdit::singleline(&mut text::ByteText {
                                text: device_id,
                                limit: 128,
                            })
                            .desired_width(f32::INFINITY),
                        );
                        });
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
                            let derived = matches!(&light.shape, Shape::Strip { segment_leds, .. } if !segment_leds.is_empty());
                            ui.add_enabled(!derived, egui::DragValue::new(count).range(1..=4096));
                            if derived { ui.small("Derived from segment counts in Layout."); }
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
                    Route::Zigbee { coordinator, ieee_address, endpoint } => {
                        ui.label("USB coordinator connected directly to dartmoor");
                        ui.small(format!("Coordinator: {coordinator}"));
                        ui.small(format!("Device: {ieee_address} · endpoint {endpoint}"));
                        ui.small("Use Discover to add Zigbee devices. The USB session stays connected when synchronization stops.");
                        let rgb = ui.horizontal(|ui| {
                            if ui.button("Test warm white").clicked() { Some([255, 180, 100]) }
                            else if ui.button("Turn off").clicked() { Some([0; 3]) } else { None }
                        }).inner;
                        if let Some(rgb) = rgb {
                            self.busy = true;
                            let coordinator = coordinator.clone();
                            let ieee = ieee_address.clone();
                            let endpoint = *endpoint;
                            let tx = self.results.clone();
                            self.runtime.as_ref().unwrap().spawn(async move {
                                let result = async {
                                    crate::zigbee::connect(&coordinator).await?;
                                    anyhow::ensure!(crate::zigbee::color(&coordinator, &ieee, endpoint, rgb, Instant::now()).await?, "Zigbee command expired before delivery");
                                    Ok(())
                                }.await;
                                let _ = tx.try_send(JobResult::ZigbeeCommand(result));
                            });
                        }
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
        if ui.button("Place in Layout").clicked() {
            self.navigate(Workspace::Layout);
        }
        ui.add_space(12.0);
        if ui
            .add_enabled(
                !running && !self.busy,
                egui::Button::new(egui::RichText::new("Remove device").color(AMBER)),
            )
            .clicked()
        {
            let id = self.config.lights[index].id.clone();
            self.config.music.light_overrides.remove(&id);
            self.config
                .music
                .alternating_order
                .retain(|entry| entry != &id);
            self.config.lights.remove(index);
            self.selected = None;
            self.drag = None;
            self.notice = "Light removed. Undo restores it.".into();
        }
    }
}
