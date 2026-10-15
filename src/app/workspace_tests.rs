use super::tests::{click, draw, headless_app, text_rect};
use super::*;

fn has(output: &egui::FullOutput, label: &str) -> bool {
    output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text() == label))
}

fn wait_for_state(app: &App, state: SessionState) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.engine.as_ref().unwrap().snapshots.borrow().state != state {
        assert!(Instant::now() < deadline, "engine did not reach {state:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn mode_start_stays_disabled_until_devices_settings_and_dialogs_are_ready() {
    for workspace in [Workspace::Video, Workspace::Music] {
        for blocked in ["busy", "invalid", "empty", "dialog"] {
            let (mut app, _directory) = headless_app();
            app.navigate(workspace);
            app.config.music.input = crate::music::AudioInput::Demo;
            match blocked {
                "busy" => app.busy = true,
                "invalid" => app.config.fps = 0,
                "empty" => app.config.lights.clear(),
                "dialog" => app.layout_dialog = Some(layout_dialog::LayoutDialog::import()),
                _ => unreachable!(),
            }
            let original = app.config.clone();
            let ctx = egui::Context::default();
            let size = Vec2::new(720.0, 900.0);
            draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let label = if workspace == Workspace::Video {
                "Start Video"
            } else {
                "Start Music"
            };
            click(&mut app, &ctx, size, text_rect(&output, label).center());
            std::thread::sleep(Duration::from_millis(10));
            assert_eq!(
                app.engine.as_ref().unwrap().snapshots.borrow().state,
                SessionState::Idle,
                "{workspace:?} {blocked}"
            );
            assert_eq!(app.config, original);
        }
    }
}

#[test]
fn bottom_transport_is_centered_and_only_mode_workspaces_can_start() {
    for width in [720.0, 1440.0] {
        let (mut app, _directory) = headless_app();
        app.config.music.input = crate::music::AudioInput::Demo;
        let ctx = egui::Context::default();
        let size = Vec2::new(width, 900.0);
        for workspace in Workspace::ALL {
            app.navigate(workspace);
            draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let label = match workspace {
                Workspace::Video => "Start Video",
                Workspace::Music => "Start Music",
                _ => "Start",
            };
            assert!(!has(&output, "Ready"));
            assert!(!has(&output, "Pause"));
            let rect = text_rect(&output, label);
            assert!((rect.center().x - width / 2.0).abs() < 1.0);
            assert!(rect.center().y > size.y - 100.0);
            assert_eq!(output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text() == label)).count(), 1);
            if has(&output, "Diagnostics") {
                let panel =
                    egui::containers::panel::PanelState::load(&ctx, egui::Id::new("transport"))
                        .unwrap();
                assert!(text_rect(&output, "Diagnostics").bottom() < panel.rect.top());
            }
            if workspace.sync_mode().is_none() {
                let original = app.config.clone();
                click(&mut app, &ctx, size, rect.center());
                std::thread::sleep(Duration::from_millis(10));
                assert_eq!(
                    app.engine.as_ref().unwrap().snapshots.borrow().state,
                    SessionState::Idle
                );
                assert_eq!(app.config, original);
            } else {
                click(&mut app, &ctx, size, rect.center());
                wait_for_state(&app, SessionState::Running);
                assert_eq!(
                    app.engine.as_ref().unwrap().snapshots.borrow().mode,
                    workspace.sync_mode()
                );
                let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
                let stop = if workspace == Workspace::Video {
                    "Stop Video"
                } else {
                    "Stop Music"
                };
                assert!(!has(&output, "Pause"));
                assert!(!has(&output, "Video · Running") && !has(&output, "Music · Running"));
                let rect = text_rect(&output, stop);
                assert!((rect.center().x - width / 2.0).abs() < 1.0);
                click(&mut app, &ctx, size, rect.center());
                wait_for_state(&app, SessionState::Idle);
            }
        }
    }
}

#[test]
fn stop_remains_available_outside_mode_workspaces_with_invalid_configuration() {
    for mode in [crate::music::SyncMode::Video, crate::music::SyncMode::Music] {
        for workspace in [Workspace::Devices, Workspace::Layout, Workspace::Studio] {
            let (mut app, _directory) = headless_app();
            app.config.mode = mode;
            app.config.music.input = crate::music::AudioInput::Demo;
            app.send(Command::Start(app.config.clone()));
            wait_for_state(&app, SessionState::Running);
            app.navigate(workspace);
            app.config.fps = 0;
            app.busy = true;
            let ctx = egui::Context::default();
            let size = Vec2::new(720.0, 900.0);
            draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let stop = if mode == crate::music::SyncMode::Video {
                "Stop Video"
            } else {
                "Stop Music"
            };
            click(&mut app, &ctx, size, text_rect(&output, stop).center());
            wait_for_state(&app, SessionState::Idle);
        }
    }
}

#[test]
fn add_device_and_device_cards_switch_context_without_editing_the_layout() {
    for width in [720.0, 1440.0] {
        let (mut app, _directory) = headless_app();
        assert!(app.add(
            "Desk bulb".into(),
            Route::HomeAssistant {
                entity_id: "light.desk".into()
            },
            1,
            false
        ));
        app.navigate(Workspace::Devices);
        let original = app.config.clone();
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let size = Vec2::new(width, 900.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert!(has(&output, "Device properties"));
        assert!(!has(&output, "Discover"));
        assert!(!has(&output, "Connections and integration settings"));
        let panel =
            egui::containers::panel::PanelState::load(&ctx, egui::Id::new("workspace-lights"))
                .unwrap();
        let add_button = text_rect(&output, "Add device");
        assert!((add_button.center().x - panel.rect.center().x).abs() < 2.0);
        click(&mut app, &ctx, size, add_button.center());
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert!(has(&output, "Discover"));
        assert!(!has(&output, "Device properties"));
        assert!(!has(&output, "Done adding devices"));
        assert!(!has(&output, "WLED behavior"));
        // Select a different device, then reopen Add and select the same device.
        // Both actions must immediately return to properties.
        for expected in [0, 0, 1] {
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let label = if expected == 0 {
                "Demo strip\nDemo output\nNot connected"
            } else {
                "Desk bulb\nHome Assistant · ambient\nNot connected"
            };
            click(&mut app, &ctx, size, text_rect(&output, label).center());
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            assert_eq!(app.selected, Some(expected));
            assert!(has(&output, "Device properties"));
            assert!(!has(&output, "Discover"));
            click(
                &mut app,
                &ctx,
                size,
                text_rect(&output, "Add device").center(),
            );
        }
        // Returning from another workspace does not reopen the setup context.
        app.navigate(Workspace::Video);
        app.navigate(Workspace::Devices);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert!(has(&output, "Device properties"));
        assert!(!has(&output, "Discover"));
        assert_eq!(app.config, original);
        assert!(!app.history.can_undo());
    }
}

#[test]
fn empty_devices_wait_for_explicit_add_and_new_devices_stay_in_setup() {
    let (mut app, _directory) = headless_app();
    app.config.lights.clear();
    app.selected = None;
    app.navigate(Workspace::Devices);
    let ctx = egui::Context::default();
    let size = Vec2::new(1024.0, 900.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(has(&output, "Your devices"));
    assert!(!has(&output, "Discover"));
    assert!(!has(&output, "+ Add WLED"));
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Add device").center(),
    );
    assert!(app.add(
        "First bulb".into(),
        Route::HomeAssistant {
            entity_id: "light.first".into()
        },
        1,
        false
    ));
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(has(&output, "Discover"));
    assert!(!has(&output, "Device properties"));
    assert!(!has(&output, "Done adding devices"));
    let label = "First bulb\nHome Assistant · ambient\nNot connected";
    click(&mut app, &ctx, size, text_rect(&output, label).center());
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(has(&output, "Device properties"));
    assert!(!has(&output, "Discover"));
}

#[test]
fn video_owns_wled_behavior_with_persistence_undo_and_session_guard() {
    for width in [720.0, 1440.0] {
        let (mut app, _directory) = headless_app();
        app.navigate(Workspace::Video);
        let ctx = egui::Context::default();
        ctx.style_mut(|style| style.animation_time = 0.0);
        let size = Vec2::new(width, 1000.0);
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        click(
            &mut app,
            &ctx,
            size,
            text_rect(&output, "WLED behavior").center(),
        );
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let label = "Try restoring WLED state on stop";
        click(&mut app, &ctx, size, text_rect(&output, label).center());
        assert!(app.config.restore_wled_state);
        app.save();
        let saved = std::fs::read_to_string(&app.config_path).unwrap();
        assert!(config::decode_layout(&saved).unwrap().restore_wled_state);
        app.undo(false);
        assert!(!app.config.restore_wled_state);
        // Only a synthetic capture and mock output run in this test.
        app.send(Command::Start(app.config.clone()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.engine.as_ref().unwrap().snapshots.borrow().state != SessionState::Running {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        click(&mut app, &ctx, size, text_rect(&output, label).center());
        assert!(!app.config.restore_wled_state);
        app.send(Command::Stop);
    }
}

#[test]
fn workspace_ownership_and_rendering_preserve_configuration_at_supported_sizes() {
    let (mut app, _directory) = headless_app();
    let original = app.config.clone();
    for scale in [1.0, 1.5] {
        for size in [
            Vec2::new(1440.0, 900.0),
            Vec2::new(1024.0, 768.0),
            Vec2::new(720.0, 480.0),
        ] {
            for workspace in Workspace::ALL {
                let ctx = egui::Context::default();
                app.styled = false;
                ctx.set_pixels_per_point(scale);
                app.navigate(workspace);
                app.compact_inspector = false;
                draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
                let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
                let screen = Rect::from_min_size(Pos2::ZERO, size);
                for tab in Workspace::ALL {
                    assert!(screen.contains_rect(text_rect(&output, tab.label())));
                }
                match workspace {
                    Workspace::Devices => {
                        assert!(has(&output, "Device properties"));
                        assert!(!has(&output, "Desktop placement"));
                    }
                    Workspace::Layout => {
                        assert!(has(&output, "Desktop placement"));
                        assert!(!has(&output, "Brightness"));
                        assert!(screen.contains_rect(app.canvas_rect.unwrap()));
                    }
                    Workspace::Video => {
                        assert!(has(&output, "Color strength"));
                        assert!(!has(&output, "Device properties"));
                        assert!(app.canvas_rect.is_none());
                    }
                    Workspace::Music => {
                        assert!(has(&output, "Audio source"));
                        assert!(!has(&output, "Color strength"));
                        assert!(app.canvas_rect.is_none());
                    }
                    Workspace::Studio => {
                        assert!(has(&output, "Coming later"));
                        assert!(!has(&output, "Brightness"));
                        assert!(app.canvas_rect.is_none());
                    }
                }
                assert_eq!(
                    app.config, original,
                    "rendering {workspace:?} at {size:?} changed configuration"
                );
            }
        }
    }
}

#[test]
fn all_workspace_tabs_leave_active_video_and_music_sessions_running() {
    for mode in [crate::music::SyncMode::Video, crate::music::SyncMode::Music] {
        let (mut app, _directory) = headless_app();
        app.config.mode = mode;
        app.config.music.input = crate::music::AudioInput::Demo;
        let original = app.config.clone();
        app.send(Command::Start(original.clone()));
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.engine.as_ref().unwrap().snapshots.borrow().state != SessionState::Running {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let ctx = egui::Context::default();
        let size = Vec2::new(1440.0, 900.0);
        for workspace in Workspace::ALL {
            draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            click(
                &mut app,
                &ctx,
                size,
                text_rect(&output, workspace.label()).center(),
            );
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            assert!(
                !has(&output, "Stop"),
                "extra transport button in {workspace:?}"
            );
            assert_eq!(app.workspace, workspace);
            assert_eq!(app.config, original);
            let snapshot = app.engine.as_ref().unwrap().snapshots.borrow().clone();
            assert_eq!(snapshot.mode, Some(mode));
            assert_eq!(snapshot.state, SessionState::Running);
            assert!(!app.history.can_undo());
        }
        if mode == crate::music::SyncMode::Music {
            app.navigate(Workspace::Devices);
            let deadline = Instant::now() + Duration::from_secs(2);
            while app.music_frames.len() < 2 {
                assert!(Instant::now() < deadline);
                draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(app.music_frames.len() <= 180 && app.music_events.len() <= 128);
        }
        app.send(Command::Pause);
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.engine.as_ref().unwrap().snapshots.borrow().state != SessionState::Paused {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        app.navigate(Workspace::Studio);
        let stop = if mode == crate::music::SyncMode::Video {
            "Stop Video"
        } else {
            "Stop Music"
        };
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        assert!(has(&output, stop));
        assert!(!has(&output, "Resume Video") && !has(&output, "Resume Music"));
        app.navigate(Workspace::from_mode(mode));
        draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let resume = if mode == crate::music::SyncMode::Video {
            "Resume Video"
        } else {
            "Resume Music"
        };
        assert!(has(&output, resume));
        click(&mut app, &ctx, size, text_rect(&output, resume).center());
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.engine.as_ref().unwrap().snapshots.borrow().state != SessionState::Running {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            app.engine.as_ref().unwrap().snapshots.borrow().mode,
            Some(mode)
        );
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        click(&mut app, &ctx, size, text_rect(&output, stop).center());
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.engine.as_ref().unwrap().snapshots.borrow().state != SessionState::Idle {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn device_removal_and_undo_restore_related_music_configuration() {
    let (mut app, _directory) = headless_app();
    app.navigate(Workspace::Devices);
    let id = app.config.lights[0].id.clone();
    app.config.music.light_overrides.insert(
        id.clone(),
        crate::music::LightMusicOverride {
            gain: 1.5,
            ..Default::default()
        },
    );
    app.config.music.alternating_order.push(id.clone());
    let before = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1440.0, 900.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Remove device").center(),
    );
    assert!(app.config.lights.is_empty());
    assert!(!app.config.music.light_overrides.contains_key(&id));
    assert!(!app.config.music.alternating_order.contains(&id));
    app.undo(false);
    assert_eq!(app.config, before);
    assert_eq!(app.workspace, Workspace::Devices);
}

#[test]
fn undo_reconciles_selection_by_identity_and_navigation_finishes_drag() {
    let (mut app, _directory) = headless_app();
    let mut second = app.config.lights[0].clone();
    second.id = "second".into();
    app.config.lights.push(second);
    let original = app.config.clone();
    app.config.lights.swap(0, 1);
    app.history.checkpoint(&original, &app.config);
    app.selected = Some(1);
    app.undo(false);
    assert_eq!(app.selected, Some(0));
    app.navigate(Workspace::Layout);
    app.edit_before = Some(app.config.clone());
    app.config.lights[0].name = "Edited during a transaction".into();
    let edited = app.config.clone();
    app.drag = Some((0, 0));
    app.navigate(Workspace::Music);
    assert!(app.drag.is_none() && app.edit_before.is_none());
    app.undo(false);
    assert_eq!(app.config, original);
    assert_eq!(app.workspace, Workspace::Music);
    app.undo(true);
    assert_eq!(app.config, edited);
}

/// Export actual egui meshes and the font atlas for software visual review.
/// This does not acquire a screen/audio source or activate a physical device.
#[test]
#[ignore = "writes workspace review artifacts to /tmp/dartmoor-ui-review"]
fn export_workspace_review_meshes() {
    use serde_json::json;
    let directory = std::path::Path::new("/tmp/dartmoor-ui-review");
    std::fs::create_dir_all(directory).unwrap();
    let (mut app, _fixture) = headless_app();
    app.config.source = CaptureSelection::Desktop { id: None };
    app.config.dark_zones = crate::dark_zones::DarkZones::default();
    app.config.black_bar_detection = true;
    app.config.lights[0].name = "Desk strip".into();
    app.config.lights[0].route = Route::Wled {
        host: "desk.local".into(),
        start: 0,
        count: 105,
        device_id: "aabbccddeeff".into(),
    };
    app.config.lights[0].zones = 105;
    app.config.lights[0].shape = Shape::Strip {
        points: vec![
            Point { x: 0.05, y: 0.95 },
            Point { x: 0.95, y: 0.95 },
            Point { x: 0.95, y: 0.05 },
            Point { x: 0.05, y: 0.05 },
            Point { x: 0.05, y: 0.95 },
        ],
        radius: 0.08,
        segment_leds: vec![34, 18, 33, 20],
        reverse: false,
    };
    for (width, height) in [(1440, 900), (1024, 768), (720, 480)] {
        for workspace in Workspace::ALL {
            let ctx = egui::Context::default();
            app.styled = false;
            app.navigate(workspace);
            app.compact_inspector = false;
            let size = Vec2::new(width as f32, height as f32);
            for _ in 0..3 {
                draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            }
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            let atlas = ctx.fonts(|fonts| fonts.image());
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            let meshes: Vec<_>=primitives.into_iter().filter_map(|primitive| {
                if let egui::epaint::Primitive::Mesh(mesh)=primitive.primitive {
                    Some(json!({"clip":[primitive.clip_rect.min.x,primitive.clip_rect.min.y,primitive.clip_rect.max.x,primitive.clip_rect.max.y],"vertices":mesh.vertices.iter().map(|v|json!([v.pos.x,v.pos.y,v.uv.x,v.uv.y,v.color.to_array()])).collect::<Vec<_>>(),"indices":mesh.indices}))
                } else {None}
            }).collect();
            let data = json!({"size":[width,height],"atlas_size":atlas.size,"atlas":atlas.pixels.iter().map(|p|p.to_array()).collect::<Vec<_>>(),"meshes":meshes});
            std::fs::write(
                directory.join(format!("{}-{width}.json", workspace.label().to_lowercase())),
                serde_json::to_vec(&data).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
fn add_device_offers_wled_home_assistant_and_usb_zigbee() {
    let (mut app, _directory) = headless_app();
    app.navigate(Workspace::Devices);
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1440.0, 1100.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Add device").center(),
    );
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    for label in ["+ Add WLED", "+ Home Assistant", "Zigbee", "Discover"] {
        assert!(has(&output, label), "missing device option: {label}");
    }
    assert!(!has(&output, "+ MQTT light (advanced)"));
    assert!(!has(&output, "Broker URL"));
    assert_eq!(app.config, original);
}

#[test]
fn zigbee_workspace_exposes_discover_and_saved_device_needs_no_connection_fields() {
    let (mut app, _directory) = headless_app();
    assert!(app.add(
        "Innr bulb".into(),
        Route::Zigbee {
            coordinator: "sonoff-usb-id".into(),
            ieee_address: "00:11:22:33:44:55:66:77".into(),
            endpoint: 1,
        },
        1,
        false
    ));
    app.navigate(Workspace::Devices);
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1440.0, 1100.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(has(
        &output,
        "USB coordinator connected directly to dartmoor"
    ));
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Add device").center(),
    );
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let discovery = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(has(&discovery, "Discover"));
    assert!(!has(&output, "Command topic"));
    assert!(!has(&output, "Broker URL"));
    assert_eq!(app.config, original);
    let json = serde_json::to_string(&original).unwrap();
    assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), original);
    app.config.validate().unwrap();
    let mut duplicate = app.config.lights.last().unwrap().clone();
    duplicate.id = "duplicate".into();
    app.config.lights.push(duplicate);
    assert!(app.config.validate().is_err());
}
