use super::*;

#[test]
fn interactive_defaults_hide_legacy_demo_and_lock_video_mode() {
    let mut legacy = Config {
        fps: 60,
        ..Default::default()
    };
    legacy.soft_ambience.enabled = false;
    let config = interactive_config(legacy);
    assert!(matches!(
        config.source,
        CaptureSelection::Desktop { id: None }
    ));
    assert!(config.lights.is_empty());
    assert_eq!(config.fps, 30);
    assert!(config.soft_ambience.enabled);
    assert_eq!(config.brightness, 1.0);
    assert_eq!(config.soft_ambience.color_strength, 0.5);
    assert_eq!(config.soft_ambience.response_ms, 50.0);
    assert!(config.dark_zones.enabled);
    assert_eq!(config.dark_zones.threshold, 50);
    assert!(config.black_bar_detection);
}

#[test]
fn video_setup_shows_only_active_controls() {
    let (mut app, _directory) = headless_app();
    app.config = interactive_config(Config::default());
    app.selected = None;
    let output = draw(
        &mut app,
        &egui::Context::default(),
        Vec2::new(1080.0, 900.0),
        vec![],
        egui::Modifiers::NONE,
    );
    let labels: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape {
                Some(text.galley.text().to_owned())
            } else {
                None
            }
        })
        .collect();
    for absent in [
        "Demo",
        "+ Demo lights",
        "Soft ambience",
        "Show color zones",
        "Capture fps",
        "Place along desktop",
    ] {
        assert!(
            !labels.iter().any(|label| label.contains(absent)),
            "unexpected {absent}"
        );
    }
    assert!(labels.iter().any(|label| label == "Color strength"));
    assert!(labels.iter().any(|label| label == "Response"));
}

pub(super) fn headless_app() -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new(None, false).unwrap();
    app.config = Config::default();
    app.config.black_bar_detection = false;
    app.config.dark_zones.enabled = false;
    app.selected = Some(0);
    app.workspace = Workspace::from_mode(app.config.mode);
    app.saved_config = app.config.clone();
    app.config_path = directory.path().join("config.json");
    app.notice.clear();
    // Suppress automatic persistence in Drop; explicit saves target the
    // temporary directory. No native window or capture is constructed.
    app.smoke = Some((Instant::now(), Duration::from_secs(3600)));
    (app, directory)
}

#[test]
fn music_controls_are_saved_and_undoable_without_retired_features() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Music;
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 1200.0);
    let visible = |output: &egui::FullOutput, label: &str| {
        output
            .shapes
            .iter()
            .any(|s| matches!(&s.shape, egui::epaint::Shape::Text(t) if t.galley.text() == label))
    };
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(visible(&output, "Subbass weight"));
    assert!(visible(&output, "Sparkle amount"));
    let before = app.config.clone();
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Follow beat").center(),
    );
    assert!(!app.config.music.follow_beat);
    app.undo(false);
    assert_eq!(app.config, before);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(visible(&output, "Subbass weight"));
    assert!(visible(&output, "Follow beat"));
    for label in [
        "Preset",
        "Calm",
        "Balanced",
        "Energetic",
        "Detector",
        "Classic",
        "Timing",
    ] {
        assert!(
            !visible(&output, label),
            "retired control still visible: {label}"
        );
    }
    click(&mut app, &ctx, size, text_rect(&output, "Pulse").center());
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(visible(&output, "Alternating"));
    for label in ["Wave", "Spectrum", "Ambient"] {
        assert!(!visible(&output, label));
    }
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Alternating").center(),
    );
    assert_eq!(
        app.config.music.effect,
        crate::music::MusicEffect::Alternating
    );
    app.save();
    let saved: Config = serde_json::from_slice(&std::fs::read(&app.config_path).unwrap()).unwrap();
    assert_eq!(saved, app.config);
}

#[test]
fn dark_controls_are_conditional_saved_and_undoable() {
    let (mut app, _directory) = headless_app();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 900.0);
    let has = |out: &egui::FullOutput, label: &str| {
        out.shapes
            .iter()
            .any(|s| matches!(&s.shape, egui::epaint::Shape::Text(t) if t.galley.text() == label))
    };
    let before = app.config.clone();
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(!has(&output, "Black sensitivity"));
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Turn off dark zones").center(),
    );
    assert!(app.config.dark_zones.enabled);
    let enabled = app.config.clone();
    app.undo(false);
    assert_eq!(app.config, before);
    app.undo(true);
    assert_eq!(app.config, enabled);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let rect = text_rect(&output, "Black sensitivity");
    click(
        &mut app,
        &ctx,
        size,
        Pos2::new(rect.left() - 60.0, rect.center().y),
    );
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_ne!(app.config.dark_zones.threshold, 8);
    app.save();
    let saved: Config = serde_json::from_slice(&std::fs::read(&app.config_path).unwrap()).unwrap();
    assert_eq!(saved, app.config);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Turn off dark zones").center(),
    );
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(!has(&output, "Black sensitivity"));
}

#[test]
fn music_navigation_keeps_video_running_and_music_edits_preserve_video_layout() {
    let (mut app, _directory) = headless_app();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 900.0);
    app.send(Command::Start(app.config.clone()));
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.engine.as_ref().unwrap().snapshots.borrow().state != SessionState::Running {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let geometry = app.config.lights.clone();
    let video = (
        app.config.source.clone(),
        app.config.brightness,
        app.config.smoothing_ms,
        app.config.soft_ambience,
    );
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(&mut app, &ctx, size, text_rect(&output, "Music").center());
    assert_eq!(app.workspace, Workspace::Music);
    assert_eq!(app.config.mode, crate::music::SyncMode::Video);
    assert_eq!(
        app.engine.as_ref().unwrap().snapshots.borrow().mode,
        Some(crate::music::SyncMode::Video)
    );
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Test audio").center(),
    );
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Demo pulses").center(),
    );
    let before = app.config.music.sensitivity;
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let slider = text_rect(&output, "Sensitivity");
    click(
        &mut app,
        &ctx,
        size,
        Pos2::new(slider.left() - 70.0, slider.center().y),
    );
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_ne!(before, app.config.music.sensitivity);
    assert_eq!(app.config.lights, geometry);
    assert_eq!(
        (
            app.config.source.clone(),
            app.config.brightness,
            app.config.smoothing_ms,
            app.config.soft_ambience
        ),
        video
    );
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Switch to Music").center(),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while app
        .engine
        .as_ref()
        .unwrap()
        .snapshots
        .borrow()
        .music
        .is_none()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        app.engine.as_ref().unwrap().snapshots.borrow().mode,
        Some(crate::music::SyncMode::Music)
    );
    assert_eq!(app.config.lights, geometry);
    app.navigate(Workspace::Layout);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(
        app.texture.is_none() && app.preview.is_none(),
        "Music must not appear as a live desktop preview"
    );
    assert!(app.canvas_rect.is_some());
    app.send(Command::Stop);
}
pub(super) fn draw(
    app: &mut App,
    ctx: &egui::Context,
    size: Vec2,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            events,
            modifiers,
            ..Default::default()
        },
        |ctx| {
            app.render(ctx);
        },
    )
}
fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
pub(super) fn text_rect(output: &egui::FullOutput, label: &str) -> Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing text {label}"))
}
pub(super) fn click(app: &mut App, ctx: &egui::Context, size: Vec2, pos: Pos2) -> egui::FullOutput {
    draw(
        app,
        ctx,
        size,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        egui::Modifiers::NONE,
    );
    draw(
        app,
        ctx,
        size,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    )
}

#[test]
fn bulb_response_controls_persist_validate_and_undo() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Layout;
    app.config.lights[0].shape = Shape::Bulb {
        center: Point { x: 0.5, y: 0.5 },
        radius: 0.08,
    };
    let mut other = app.config.lights[0].clone();
    other.id = "other".into();
    app.config.lights.push(other);
    let old = config::encode_layout(&app.config).unwrap();
    assert!(!old.contains("video_response_ms"));
    assert_eq!(
        config::decode_layout(&old).unwrap().lights[0].video_response_ms,
        None
    );
    let ctx = egui::Context::default();
    let size = Vec2::new(1440.0, 900.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Use global Response").center(),
    );
    assert_eq!(app.config.lights[0].video_response_ms, Some(50.0));
    let before = app.config.clone();
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(&mut app, &ctx, size, text_rect(&output, "50 ms").center());
    draw(
        &mut app,
        &ctx,
        size,
        vec![
            key(egui::Key::A, egui::Modifiers::CTRL),
            egui::Event::Text("350".into()),
            key(egui::Key::Enter, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert_eq!(app.config.lights[0].video_response_ms, Some(350.0));
    assert_eq!(app.config.lights[1].video_response_ms, None);
    assert_eq!(app.config.soft_ambience.response_ms, 50.0);
    let changed = app.config.clone();
    app.undo(false);
    assert_eq!(app.config, before);
    app.undo(true);
    assert_eq!(app.config, changed);
    app.save();
    let saved = std::fs::read_to_string(&app.config_path).unwrap();
    assert_eq!(config::decode_layout(&saved).unwrap(), changed);
    for value in [-1.0, 1501.0, f32::NAN, f32::INFINITY] {
        let mut invalid = changed.clone();
        invalid.lights[0].video_response_ms = Some(value);
        assert!(invalid.validate().is_err());
    }
    let mut zero = changed.clone();
    zero.lights[0].video_response_ms = Some(0.0);
    assert!(zero.validate().is_ok());
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Use global Response").center(),
    );
    assert_eq!(app.config.lights[0].video_response_ms, None);
}

#[test]
fn headless_layout_preserves_valid_configuration_and_fits_small_window() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Layout;
    app.config.smoothing_ms = 4900.0;
    app.config.ha_interval_ms = 59000;
    app.config.lights[0].shape = Shape::Bulb {
        center: Point { x: 0.5, y: 0.5 },
        radius: 0.95,
    };
    let before = app.config.clone();
    let ctx = egui::Context::default();
    for size in [Vec2::new(1080.0, 700.0), Vec2::new(720.0, 480.0)] {
        for _ in 0..2 {
            let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
            assert!(!output.shapes.is_empty());
            let rect = app.canvas_rect.unwrap();
            assert!(
                rect.width() > 200.0 && rect.height() > 100.0,
                "canvas squeezed: {rect:?}"
            );
            assert!(Rect::from_min_size(Pos2::ZERO, size).contains_rect(rect));
            assert_eq!(app.config, before, "merely rendering changed the layout");
        }
    }
}

#[test]
fn headless_nudge_undo_redo_and_text_focus_are_independent() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Layout;
    app.config.lights[0].shape = Shape::Bulb {
        center: Point { x: 0.5, y: 0.5 },
        radius: 0.08,
    };
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    let moved = app.config.clone();
    assert_ne!(moved, original);
    let command = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::Z, command)],
        command,
    );
    assert_eq!(app.config, original);
    assert!(app.history.can_redo());
    let redo = egui::Modifiers {
        shift: true,
        ..command
    };
    draw(&mut app, &ctx, size, vec![key(egui::Key::Z, redo)], redo);
    assert_eq!(app.config, moved);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    app.workspace = Workspace::Devices;
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    // Device names are edited in Devices.
    let name = text_rect(&output, "Demo strip");
    click(&mut app, &ctx, size, name.center());
    assert!(ctx.wants_keyboard_input());
    let before = app.config.lights[0].shape.clone();
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(
        app.config.lights[0].shape, before,
        "text cursor key moved light"
    );
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::S, command)],
        command,
    );
    let saved: Config = serde_json::from_slice(&std::fs::read(&app.config_path).unwrap()).unwrap();
    assert_eq!(saved, app.config);
    assert_eq!(app.saved_config, app.config);
}

#[test]
fn black_bar_checkbox_changes_setting_in_both_directions() {
    let (mut app, _directory) = headless_app();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    for expected in [true, false] {
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let checkbox = text_rect(&output, "Black bar detection");
        click(&mut app, &ctx, size, checkbox.center());
        assert_eq!(app.config.black_bar_detection, expected);
    }
}

#[test]
fn soft_ambience_controls_are_always_visible_and_editable() {
    let (mut app, _directory) = headless_app();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 900.0);
    let contains = |output: &egui::FullOutput, label: &str| {
        output.shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text() == label)
    })
    };
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(contains(&output, "Color strength"));
    assert!(contains(&output, "Response"));
    assert!(app.config.soft_ambience.enabled);
    // Exercise a focused slider edit; both controls remain visible above.
    {
        let label = "Color strength";
        let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
        let rect = text_rect(&output, label);
        let old = app.config.soft_ambience;
        click(
            &mut app,
            &ctx,
            size,
            Pos2::new(rect.left() - 70.0, rect.center().y),
        );
        draw(
            &mut app,
            &ctx,
            size,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert_ne!(
            app.config.soft_ambience, old,
            "{label} did not edit its value"
        );
    }
    assert!(app.config.soft_ambience.enabled);
}

#[test]
fn headless_drag_is_one_undo_transaction() {
    for strip in [false, true] {
        for free in [false, true] {
            let modifiers = egui::Modifiers {
                alt: free,
                ..Default::default()
            };
            let (mut app, _directory) = headless_app();
            app.workspace = Workspace::Layout;
            app.config.lights[0].shape = if strip {
                Shape::Strip {
                    points: vec![Point { x: 0.5, y: 0.5 }, Point { x: 0.8, y: 0.5 }],
                    radius: 0.08,
                    segment_leds: vec![60],
                    reverse: false,
                }
            } else {
                Shape::Bulb {
                    center: Point { x: 0.5, y: 0.5 },
                    radius: 0.08,
                }
            };
            let before = app.config.clone();
            let ctx = egui::Context::default();
            let size = Vec2::new(1080.0, 700.0);
            draw(&mut app, &ctx, size, vec![], modifiers);
            draw(&mut app, &ctx, size, vec![], modifiers);
            let start = app.canvas_rect.unwrap().center();
            draw(
                &mut app,
                &ctx,
                size,
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers,
                    },
                ],
                modifiers,
            );
            for offset in [20.0, 40.0, 60.0] {
                draw(
                    &mut app,
                    &ctx,
                    size,
                    vec![egui::Event::PointerMoved(start + Vec2::new(offset, 0.0))],
                    modifiers,
                );
            }
            let end = start + Vec2::new(60.0, 0.0);
            draw(
                &mut app,
                &ctx,
                size,
                vec![egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                }],
                modifiers,
            );
            let raw_x = 0.5 + 60.0 / app.canvas_rect.unwrap().width();
            let expected_x = if free {
                raw_x
            } else {
                (raw_x * 50.0).round() / 50.0
            };
            let actual = match &app.config.lights[0].shape {
                Shape::Bulb { center, .. } => *center,
                Shape::Strip {
                    points,
                    segment_leds,
                    ..
                } => {
                    assert_eq!(points[1], Point { x: 0.8, y: 0.5 });
                    assert_eq!(segment_leds, &[60]);
                    points[0]
                }
            };
            assert!(
                (actual.x - expected_x).abs() < 1e-5,
                "strip={strip}, free={free}: {actual:?}"
            );
            assert_eq!(actual.y, 0.5);
            assert_ne!(app.config, before, "drag did not move marker");
            app.undo(false);
            assert_eq!(app.config, before);
            assert!(!app.history.can_undo());
        }
    }
}

#[test]
fn coincident_markers_keep_selected_light_and_release_drag_state() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Layout;
    app.config.lights[0].shape = Shape::Bulb {
        center: Point { x: 0.5, y: 0.5 },
        radius: 0.08,
    };
    let mut second = app.config.lights[0].clone();
    second.id = "second-marker".into();
    second.name = "Selected bulb".into();
    app.config.lights.push(second);
    app.selected = Some(1);
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let start = app.canvas_rect.unwrap().center();
    click(&mut app, &ctx, size, start);
    assert_eq!(app.selected, Some(1));
    assert!(app.drag.is_none(), "a click left active drag state");
    draw(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    let end = start + Vec2::new(60.0, 0.0);
    draw(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::PointerMoved(end)],
        egui::Modifiers::NONE,
    );
    draw(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.config.lights[0], original.lights[0]);
    assert_ne!(app.config.lights[1].shape, original.lights[1].shape);
    assert!(app.drag.is_none());
    app.undo(false);
    assert_eq!(app.config, original);
    assert!(!app.history.can_undo());
}

#[test]
fn strip_placement_has_no_preset_menu() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Layout;
    app.config.lights[0].shape = Shape::Strip {
        points: vec![Point { x: 0.1, y: 0.6 }, Point { x: 0.8, y: 0.4 }],
        radius: 0.23,
        segment_leds: vec![],
        reverse: true,
    };
    app.selected_point = 1;
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains("Place along desktop"))));
    assert!(
        matches!(&app.config.lights[0].shape, Shape::Strip { radius, reverse: true, points, .. } if *radius == 0.23 && points.len() == 2)
    );
}

#[test]
fn segment_count_controls_update_wled_total_and_undo_as_one_edit() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Layout;
    app.selected = Some(0);
    if let Shape::Strip { points, .. } = &mut app.config.lights[0].shape {
        *points = crate::editor::strip_preset_points(crate::editor::StripPreset::Perimeter, 0.04);
    }
    app.config.lights[0].route = Route::Wled {
        host: "desk.local".into(),
        start: 5,
        count: 127,
        device_id: "abcdef".into(),
    };
    app.config.lights[0].zones = 127;
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 1100.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Set LEDs per segment").center(),
    );
    assert!(
        matches!(&app.config.lights[0].shape, Shape::Strip { segment_leds, .. } if segment_leds == &[32, 32, 32, 31])
    );
    for label in [
        "Left · 1 to 2",
        "Top · 2 to 3",
        "Right · 3 to 4",
        "Bottom · 4 to 5",
        "Total: 127 LEDs",
    ] {
        text_rect(&output, label);
    }
    let enabled = app.config.clone();
    click(&mut app, &ctx, size, text_rect(&output, "32 LEDs").center());
    draw(
        &mut app,
        &ctx,
        size,
        vec![
            key(egui::Key::A, egui::Modifiers::CTRL),
            egui::Event::Text("24".into()),
            key(egui::Key::Enter, egui::Modifiers::NONE),
        ],
        egui::Modifiers::NONE,
    );
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(
        matches!(&app.config.lights[0].shape, Shape::Strip { segment_leds, .. } if segment_leds == &[24, 32, 32, 31])
    );
    assert_eq!(app.config.lights[0].zones, 119);
    assert!(app.config.validate().is_ok());
    assert!(matches!(
        &app.config.lights[0].route,
        Route::Wled {
            start: 5,
            count: 119,
            ..
        }
    ));
    app.config.validate().unwrap();
    app.undo(false);
    assert_eq!(app.config, enabled);
    app.undo(false);
    assert_eq!(app.config, original);
}

#[test]
fn unicode_name_paste_respects_persisted_limit_and_undo() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Devices;
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Demo strip").center(),
    );
    assert!(ctx.wants_keyboard_input());
    let command = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::A, command)],
        command,
    );
    draw(
        &mut app,
        &ctx,
        size,
        vec![egui::Event::Paste("東京".repeat(100))],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.config.lights[0].name, "東京".repeat(42) + "東");
    assert!(app.config.validate().is_ok());
    let name = app.config.lights[0].name.clone();
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert_eq!(
        app.config.lights[0].name, name,
        "rendering changed the name"
    );
    // Leave the editor so its text transaction becomes one layout Undo.
    let point = Pos2::new(700.0, 160.0);
    click(&mut app, &ctx, size, point);
    app.undo(false);
    assert_eq!(app.config, original);
}

#[test]
fn discovery_cannot_be_retargeted_by_undo_or_a_stale_reply() {
    let (mut app, _directory) = headless_app();
    let original = app.config.clone();
    app.config.ha_url = "https://server-b.example".into();
    app.history.checkpoint(&original, &app.config);
    app.busy = true;
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let command = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::Z, command)],
        command,
    );
    assert_eq!(app.config.ha_url, "https://server-b.example");
    app.config.ha_url = "https://server-a.example".into();
    let before = app.config.clone();
    app.results
        .try_send(JobResult::Ha(
            "https://server-b.example".into(),
            Ok(vec![outputs::HaLight {
                entity_id: "light.server_b".into(),
                name: "Different server".into(),
                color_modes: vec!["rgb".into()],
                available: true,
            }]),
        ))
        .unwrap();
    app.poll_jobs();
    assert_eq!(app.config, before);
    assert!(!app.busy);
    assert!(app.notice.contains("server changed"));
}

#[test]
fn headless_discovery_requires_selection_and_adds_only_chosen_light() {
    let (mut app, _directory) = headless_app();
    app.workspace = Workspace::Devices;
    let original = app.config.clone();
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    let light = |id: &str, name: &str| outputs::HaLight {
        entity_id: format!("light.{id}"),
        name: name.into(),
        color_modes: vec!["rgb".into()],
        available: true,
    };
    app.results
        .try_send(JobResult::Ha(
            app.config.ha_url.clone(),
            Ok(vec![light("desk", "Desk lamp"), light("room", "Room lamp")]),
        ))
        .unwrap();
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert_eq!(app.config, original);
    assert!(app.ha_picker.is_some());
    // Global arrow shortcuts must not move the background layout while
    // a discovery choice is open, even if its search field lacks focus.
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.config, original);
    let checkbox = text_rect(&output, "Desk lamp").center();
    click(&mut app, &ctx, size, checkbox);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let add = text_rect(&output, "Add 1 selected").center();
    click(&mut app, &ctx, size, add);
    assert!(app.ha_picker.is_none());
    assert_eq!(app.config.lights.len(), original.lights.len() + 1);
    assert_eq!(
        app.config.lights.last().unwrap().route,
        Route::HomeAssistant {
            entity_id: "light.desk".into()
        }
    );
    assert!(app.config.validate().is_ok());
    app.undo(false);
    assert_eq!(app.config, original);
}

#[test]
fn headless_export_copies_strict_layout_without_credentials_or_edits() {
    let (mut app, _directory) = headless_app();
    let original = app.config.clone();
    app.token = "private-test-token-never-export".into();
    app.layout_dialog = Some(layout_dialog::LayoutDialog::export(&app.config).unwrap());
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Copy JSON").center(),
    );
    let json = output
        .platform_output
        .commands
        .iter()
        .find_map(|command| match command {
            egui::OutputCommand::CopyText(json) => Some(json),
            _ => None,
        })
        .expect("export produced no clipboard command");
    assert_eq!(config::decode_layout(json).unwrap(), original);
    assert!(!json.contains(&app.token));
    assert_eq!(app.config, original);
    assert!(!app.history.can_undo());
}

#[test]
fn headless_import_is_validated_and_undoable_as_one_edit() {
    let (mut app, _directory) = headless_app();
    let original = app.config.clone();
    let mut imported = original.clone();
    imported.source = CaptureSelection::Desktop { id: None };
    imported.lights[0].route = Route::Wled {
        host: "desk.local".into(),
        start: 0,
        count: 8,
        device_id: "aabbccddeeff".into(),
    };
    imported.lights[0].name = "Imported strip".into();
    imported.brightness = 0.4;
    app.layout_dialog = Some(layout_dialog::LayoutDialog::from_json(
        config::encode_layout(&imported).unwrap(),
    ));
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    draw(
        &mut app,
        &ctx,
        size,
        vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        egui::Modifiers::NONE,
    );
    assert_eq!(app.config, original, "modal allowed background movement");
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Use this configuration").center(),
    );
    assert!(app.layout_dialog.is_none());
    assert_eq!(app.config, imported);
    assert_eq!(app.saved_config, original, "import unexpectedly persisted");
    app.undo(false);
    assert_eq!(app.config, original);
    assert!(!app.history.can_undo());
    app.undo(true);
    assert_eq!(app.config, imported);
}

#[test]
fn headless_invalid_import_keeps_dialog_and_original_layout() {
    let (mut app, _directory) = headless_app();
    let original = app.config.clone();
    let mut json: serde_json::Value =
        serde_json::from_str(&config::encode_layout(&original).unwrap()).unwrap();
    json["access_token"] = "not-a-layout-field".into();
    app.layout_dialog = Some(layout_dialog::LayoutDialog::from_json(json.to_string()));
    let ctx = egui::Context::default();
    let size = Vec2::new(1080.0, 700.0);
    draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    click(
        &mut app,
        &ctx,
        size,
        text_rect(&output, "Use this configuration").center(),
    );
    assert!(app.layout_dialog.is_some());
    assert_eq!(app.config, original);
    assert!(!app.history.can_undo());
    let output = draw(&mut app, &ctx, size, vec![], egui::Modifiers::NONE);
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains("Could not import"))));
}

#[test]
fn reinspection_preserves_verified_mapping_and_rejects_shrunken_controller() {
    let expected = Route::Wled {
        host: "old.local".into(),
        start: 20,
        count: 40,
        device_id: "AA:BB:CC:DD:EE:FF".into(),
    };
    let mut info = outputs::WledInfo {
        name: "Desk".into(),
        device_id: "aabbccddeeff".into(),
        led_count: 120,
        segments: vec![(0, 120)],
    };
    assert_eq!(
        inspected_route(&expected, "new.local".into(), &info).unwrap(),
        Route::Wled {
            host: "new.local".into(),
            start: 20,
            count: 40,
            device_id: info.device_id.clone()
        }
    );
    info.led_count = 50;
    assert!(inspected_route(&expected, "new.local".into(), &info).is_err());
    info.device_id = "001122334455".into();
    assert!(matches!(
        inspected_route(&expected, "other.local".into(), &info).unwrap(),
        Route::Wled {
            start: 0,
            count: 50,
            ..
        }
    ));
}

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
