use crate::{
    config::{self, CaptureSelection, Config},
    core::{Light, Point, Route, Shape},
    engine::{Command, Engine, SessionState},
    outputs,
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use std::{
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    time::{Duration, Instant},
};

enum JobResult {
    Wled(anyhow::Result<(String, outputs::WledInfo)>),
    WledRoute(String, Route, anyhow::Result<(String, outputs::WledInfo)>),
    Ha(anyhow::Result<Vec<outputs::HaLight>>),
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
    pending_apply: Option<Config>,
    drag: Option<(usize, usize)>,
    smoke: Option<(Instant, Duration)>,
    smoke_scheduled: bool,
}
impl App {
    pub fn new(smoke: Option<Duration>, real_capture: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("network runtime");
        let engine = Engine::new(&runtime);
        let (config, warning) = config::load();
        let (results, jobs) = sync_channel(4);
        let mut app = Self {
            config,
            engine: Some(engine),
            runtime: Some(runtime),
            selected: Some(0),
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
        app
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
        self.notice = match config::save(&self.config) {
            Ok(()) => "Layout saved".into(),
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
        true
    }
    fn poll_jobs(&mut self) {
        while let Ok(result) = self.jobs.try_recv() {
            self.busy = false;
            match result {
                JobResult::Wled(Ok((host, info))) => {
                    if self.config.lights.iter().any(|l|matches!(&l.route,Route::Wled{device_id,..} if crate::core::canonical_device_id(device_id)==crate::core::canonical_device_id(&info.device_id))) {self.notice="This WLED controller is already in the layout. Edit its mapping below.".into();}
                else {let count=info.led_count.min(4096);self.notice=format!("{}: {count} LEDs; segments {:?}. Mapping starts at physical LED 0; adjust below.",info.name,info.segments);self.add(info.name,Route::Wled{host,start:0,count,device_id:info.device_id},count.min(128),true);}
                }
                JobResult::WledRoute(id, expected, Ok((host,info)))=> {
                    let route=Route::Wled{host,start:0,count:info.led_count.min(4096),device_id:info.device_id};
                    self.notice=match apply_inspection(&mut self.config,&id,&expected,route) {
                        Ok(true)=>format!("Verified {}. Adjust physical LED mapping for this record.",info.name),
                        Ok(false)=>"Inspection ignored because this route was edited or removed.".into(),
                        Err(e)=>format!("Inspection would invalidate the layout: {e}"),
                    };
                },
                JobResult::Ha(Ok(lights)) => {
                    let mut added = 0;
                    let mut skipped = 0;
                    for l in lights {
                        if l.available && l.color_modes.iter().any(|m|["rgb","rgbw","rgbww","hs","xy"].contains(&m.as_str())) && !self.config.lights.iter().any(|old|matches!(&old.route,Route::HomeAssistant{entity_id} if *entity_id==l.entity_id)) {
                    if self.add(l.name,Route::HomeAssistant{entity_id:l.entity_id},1,false){added+=1;}else{skipped+=1;}
                }
                    }
                    self.notice = format!(
                        "Added {added} available color lights; skipped {skipped} invalid or over-limit additions. Home Assistant uses slower ambient updates; avoid adding a second route for the same physical device."
                    );
                }
                JobResult::Wled(Err(e)) | JobResult::WledRoute(_,_,Err(e)) | JobResult::Ha(Err(e)) => self.notice = e.to_string(),
            }
        }
    }
    fn settings(&mut self, ui: &mut egui::Ui, running: bool) -> bool {
        let mut changed = false;
        ui.heading("Lumen Desktop");
        ui.label("Desktop colors → your lights");
        ui.separator();
        ui.add_enabled_ui(!running,|ui| {
            ui.label("Capture source");
            let synthetic=matches!(self.config.source,CaptureSelection::Synthetic);
            if ui.selectable_label(synthetic,"Simulated desktop (development)").clicked(){self.config.source=CaptureSelection::Synthetic;changed=true;}
            if ui.selectable_label(!synthetic,"Screen / window").clicked(){changed |= select_desktop(&mut self.config.source);}
            if !synthetic {
                #[cfg(target_os="linux")] ui.label("Start opens the Wayland screen-sharing portal. Select one display or window there.");
                #[cfg(not(target_os="linux"))] {
                    if ui.button("Refresh sources").clicked(){self.sources=crate::capture::sources();}
                    if let CaptureSelection::Desktop{id}=&mut self.config.source {egui::ComboBox::from_id_salt("source").selected_text(self.sources.iter().find(|s|id.as_ref()==Some(&s.id)).map(|s|s.name.as_str()).unwrap_or("Primary display / saved source")).show_ui(ui,|ui| {
                        for source in &self.sources {changed|=ui.selectable_value(id,Some(source.id.clone()),&source.name).changed();}
                    });}
                }
            }
            ui.add(egui::Slider::new(&mut self.config.fps,5..=60).text("Capture fps"));
        });
        changed |= ui
            .add(egui::Slider::new(&mut self.config.brightness, 0.0..=1.0).text("Brightness limit"))
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.config.smoothing_ms, 0.0..=1500.0).text("Smoothing ms"),
            )
            .changed();
        ui.separator();
        ui.label("Lights");
        for (i, l) in self.config.lights.iter().enumerate() {
            if ui
                .selectable_label(self.selected == Some(i), &l.name)
                .clicked()
            {
                self.selected = Some(i);
            }
        }
        ui.add_enabled_ui(!running && !self.busy,|ui| {
            ui.horizontal(|ui| {if ui.button("+ Mock bulb").clicked(){self.add("Simulated bulb".into(),Route::Mock,1,false);changed=true;}
if ui.button("+ Mock strip").clicked(){self.add("Simulated strip".into(),Route::Mock,16,true);changed=true;}});
            ui.collapsing("Add WLED controller",|ui| {
                ui.label("Hostname or IP (no http://)");ui.label("Streaming owns the whole controller; unmapped LEDs are black. Adjust physical LED range below.");ui.checkbox(&mut self.config.restore_wled_state,"Try restoring previous WLED state on stop");ui.text_edit_singleline(&mut self.host);
                if ui.button("Inspect and add").clicked(){self.busy=true;let host=self.host.trim().to_owned();let tx=self.results.clone();self.runtime.as_ref().unwrap().spawn(async move {let info=outputs::inspect_wled(&host).await.map(|i|(host,i));let _=tx.try_send(JobResult::Wled(info));});}
            });
            ui.collapsing("Home Assistant · ambient",|ui| {
                ui.label("Server URL, e.g. https://ha.example");ui.text_edit_singleline(&mut self.config.ha_url);
                ui.label("Long-lived access token (stored in OS keyring)");ui.add(egui::TextEdit::singleline(&mut self.token).password(true));
                ui.add(egui::Slider::new(&mut self.config.ha_interval_ms,500..=10000).text("ms between service calls"));
                if ui.button("Connect and discover").clicked(){
                    self.busy=true;let url=self.config.ha_url.trim().to_owned();let token=std::mem::take(&mut self.token);let tx=self.results.clone();
                    self.runtime.as_ref().unwrap().spawn(async move {
                        let secret=if token.is_empty(){Ok(())}else{outputs::save_token_async(&url,&token).await};
                        let result=match secret {Ok(())=>outputs::discover_ha(&url).await,Err(e)=>Err(e)};let _=tx.try_send(JobResult::Ha(result));
                    });
                }
            });
        });
        if self.busy {
            ui.spinner();
            ui.label("Connecting…");
        }
        ui.separator();
        if let Some(i) = self.selected.filter(|&i| i < self.config.lights.len()) {
            let l = &mut self.config.lights[i];
            ui.label("Selected light");
            changed |= ui.text_edit_singleline(&mut l.name).changed();
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(matches!(l.shape, Shape::Bulb { .. }), "Bulb")
                    .clicked()
                    && !matches!(l.shape, Shape::Bulb { .. })
                {
                    l.shape = Shape::Bulb {
                        center: Point { x: 0.5, y: 0.5 },
                        radius: 0.08,
                    };
                    changed = true;
                }
                if ui
                    .selectable_label(matches!(l.shape, Shape::Strip { .. }), "Strip path")
                    .clicked()
                    && !matches!(l.shape, Shape::Strip { .. })
                {
                    l.shape = Shape::Strip {
                        points: vec![Point { x: 0.1, y: 0.9 }, Point { x: 0.9, y: 0.9 }],
                        radius: 0.05,
                        reverse: false,
                    };
                    changed = true;
                }
            });
            match &mut l.shape {
                Shape::Bulb { radius, .. } => {
                    changed |= ui
                        .add(egui::Slider::new(radius, 0.005..=0.4).text("Sample radius"))
                        .changed()
                }
                Shape::Strip {
                    points,
                    radius,
                    reverse,
                } => {
                    changed |= ui
                        .add(egui::Slider::new(radius, 0.005..=0.4).text("Sample extent"))
                        .changed();
                    changed |= ui.checkbox(reverse, "Reverse strip direction").changed();
                    ui.horizontal(|ui| {
                        if ui.button("Add path point").clicked() && points.len() < 64 {
                            let p = *points.last().unwrap();
                            points.push(Point {
                                x: (p.x + 0.05).min(1.0),
                                y: (p.y - 0.1).max(0.0),
                            });
                            changed = true;
                        }
                        if ui
                            .add_enabled(points.len() > 2, egui::Button::new("Remove last point"))
                            .clicked()
                        {
                            points.pop();
                            changed = true;
                        }
                    });
                    ui.label("Drag the numbered points in the preview.");
                }
            }
            if matches!(l.route, Route::HomeAssistant { .. }) {
                l.zones = 1;
                ui.label("One color · ambient updates");
            } else {
                changed |= ui
                    .add(egui::Slider::new(&mut l.zones, 1..=256).text("Logical zones"))
                    .changed();
                ui.label(
                    "1 zone averages the whole strip; more zones require addressable hardware.",
                );
            }
            ui.add_enabled_ui(!running, |ui| {
                let mut route=match l.route {Route::Wled{..}=>0,Route::HomeAssistant{..}=>1,Route::Mock=>2};
                let old=route;
                egui::ComboBox::from_id_salt("active-route").selected_text(["Direct WLED","Home Assistant","Mock output"][route]).show_ui(ui,|ui| {
                    ui.selectable_value(&mut route,0,"Direct WLED");ui.selectable_value(&mut route,1,"Home Assistant");ui.selectable_value(&mut route,2,"Mock output");
                });
                if route!=old {l.route=match route {0=>Route::Wled{host:"".into(),start:0,count:1,device_id:"".into()},1=>Route::HomeAssistant{entity_id:"light.".into()},_=>Route::Mock};if route==1{l.zones=1;}changed=true;}
                ui.label("One active route. When changing route, enter the same physical device's address/identity. No automatic fallback.");
            });
            ui.add_enabled_ui(!running, |ui| match &mut l.route {
                Route::Wled {
                    host,
                    start,
                    count,
                    device_id,
                } => {
                    ui.label("Direct WLED · saved MAC identity");
                    ui.text_edit_singleline(device_id);
                    ui.label("Hostname / IP (inspect with Add WLED to obtain MAC)");
                    ui.text_edit_singleline(host);
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Inspect this route"))
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
                        let id = l.id.clone();
                        let tx = self.results.clone();
                        self.runtime.as_ref().unwrap().spawn(async move {
                            let result =
                                outputs::inspect_wled(&host).await.map(|info| (host, info));
                            let _ = tx.try_send(JobResult::WledRoute(id, expected, result));
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("First LED");
                        ui.add(egui::DragValue::new(start).range(0..=65534));
                        ui.label("LED count");
                        ui.add(egui::DragValue::new(count).range(1..=4096));
                    });
                }
                Route::HomeAssistant { entity_id } => {
                    ui.label("Home Assistant route");
                    ui.text_edit_singleline(entity_id);
                }
                Route::Mock => {
                    ui.label("Mock output — no physical device");
                }
            });
            let mut remove = false;
            ui.add_enabled_ui(!running, |ui| {
                remove = ui.button("Remove light").clicked();
            });
            if remove {
                self.config.lights.remove(i);
                self.selected = None;
                changed = true;
            }
        }
        ui.separator();
        if ui.button("Save layout").clicked() {
            self.save();
        }
        if !self.notice.is_empty() {
            ui.label(&self.notice);
        }
        changed
    }
    fn canvas(&mut self, ui: &mut egui::Ui) -> bool {
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
        let aspect = snapshot
            .preview
            .as_ref()
            .map(|f| f.width as f32 / f.height as f32)
            .unwrap_or(16.0 / 9.0);
        let available = ui.available_size();
        let width = available.x.min(available.y * aspect).max(1.0);
        let size = Vec2::new(width, width / aspect);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(22, 27, 35));
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
                "Start synchronization for a desktop preview",
                egui::FontId::proportional(17.0),
                Color32::LIGHT_GRAY,
            );
        }
        let screen = |p: Point| {
            Pos2::new(
                rect.left() + p.x * rect.width(),
                rect.top() + p.y * rect.height(),
            )
        };
        let mut handles = vec![];
        for (i, l) in self.config.lights.iter().enumerate() {
            let color = snapshot
                .colors
                .get(i)
                .and_then(|v| v.first())
                .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
                .unwrap_or(Color32::YELLOW);
            let selected = self.selected == Some(i);
            let stroke = Stroke::new(if selected { 3.0 } else { 1.5 }, color);
            match &l.shape {
                Shape::Bulb { center, radius } => {
                    let p = screen(*center);
                    painter.circle_stroke(p, *radius * rect.width().min(rect.height()), stroke);
                    painter.circle_filled(p, 7.0, color);
                    handles.push((i, 0, p));
                }
                Shape::Strip { points, radius, .. } => {
                    for pair in points.windows(2) {
                        painter.line_segment(
                            [screen(pair[0]), screen(pair[1])],
                            Stroke::new(
                                (*radius * rect.width().min(rect.height()) * 2.0).max(3.0),
                                color.gamma_multiply(0.3),
                            ),
                        );
                        painter.line_segment([screen(pair[0]), screen(pair[1])], stroke);
                    }
                    for (j, p) in points.iter().enumerate() {
                        let p = screen(*p);
                        painter.circle_filled(p, 6.0, color);
                        painter.text(
                            p + Vec2::new(0.0, -12.0),
                            egui::Align2::CENTER_CENTER,
                            format!("{}", j + 1),
                            egui::FontId::proportional(12.0),
                            Color32::WHITE,
                        );
                        handles.push((i, j, p));
                    }
                }
            }
        }
        if (response.clicked() || response.drag_started())
            && let Some(pos) = response.interact_pointer_pos()
        {
            self.drag = handles
                .iter()
                .filter(|(_, _, p)| p.distance(pos) < 24.0)
                .min_by(|a, b| a.2.distance(pos).total_cmp(&b.2.distance(pos)))
                .map(|(i, j, _)| (*i, *j));
            if let Some((i, _)) = self.drag {
                self.selected = Some(i);
            }
        }
        let mut changed = false;
        if response.dragged()
            && let (Some((i, j)), Some(pos)) = (self.drag, response.interact_pointer_pos())
        {
            let p = Point {
                x: ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
                y: ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
            };
            if let Some(l) = self.config.lights.get_mut(i) {
                match &mut l.shape {
                    Shape::Bulb { center, .. } => *center = p,
                    Shape::Strip { points, .. } => {
                        if let Some(point) = points.get_mut(j) {
                            *point = p;
                        }
                    }
                }
                changed = true;
            }
        }
        if response.drag_stopped() {
            self.drag = None;
        }
        changed
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll_jobs();
        let snapshot = self.engine.as_ref().unwrap().snapshots.borrow().clone();
        let running = matches!(
            snapshot.state,
            SessionState::Running | SessionState::RequestingPermission
        );
        let mut changed = false;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!running && !self.busy, egui::Button::new("▶ Start"))
                    .clicked()
                {
                    self.send(Command::Start(self.config.clone()));
                }
                if ui
                    .add_enabled(running, egui::Button::new("Ⅱ Pause"))
                    .clicked()
                {
                    self.send(Command::Pause);
                }
                if ui.button("■ Stop").clicked() {
                    self.send(Command::Stop);
                }
                ui.separator();
                ui.label(format!("{:?}", snapshot.state));
                ui.label(&snapshot.message);
            });
        });
        egui::SidePanel::left("settings")
            .default_width(310.0)
            .resizable(true)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    changed |= self.settings(ui, running);
                });
            });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(format!(
                "{} frames · sampling {:.2} ms · minimize to keep syncing; close to stop",
                snapshot.frames, snapshot.processing_ms
            ));
            for (name, status) in &snapshot.devices {
                ui.label(format!("{name}: {status}"));
            }
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            changed |= self.canvas(ui);
        });
        if changed && running {
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
        ctx.request_repaint_after(if running {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(300)
        });
    }
}
impl Drop for App {
    fn drop(&mut self) {
        if self.smoke.is_none()
            && let Err(e) = config::save(&self.config)
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
