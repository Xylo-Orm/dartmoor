use super::*;

fn layout_with_retired_lights() -> String {
    let mut document = serde_json::to_value(Config::default()).unwrap();
    for (id, route) in [
        (
            "old-mqtt",
            serde_json::json!({"kind":"mqtt", "broker":"mqtt://localhost:1883", "username":"", "topic":"bulb/set", "format":"json", "interval_ms":1000}),
        ),
        (
            "old-tapo",
            serde_json::json!({"kind":"tapo", "ip":"192.0.2.1", "username":"owner@example.com", "interval_ms":1000}),
        ),
        (
            "old-matter",
            serde_json::json!({"kind":"matter", "fabric_id":1, "node_id":2, "endpoint":1, "color_mode":"hue_saturation", "fps":30}),
        ),
    ] {
        let mut light = document["lights"][0].clone();
        light["id"] = serde_json::json!(id);
        light["route"] = route;
        light["zones"] = serde_json::json!(1);
        document["lights"].as_array_mut().unwrap().push(light);
        document["music"]["light_overrides"][id] =
            serde_json::to_value(crate::music::LightMusicOverride::default()).unwrap();
    }
    document["music"]["alternating_order"] =
        serde_json::json!(["old-mqtt", "old-tapo", "demo-strip", "old-matter"]);
    document.to_string()
}

#[test]
fn retired_outputs_load_without_losing_supported_devices_and_backup_on_save() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let original = layout_with_retired_lights();
    fs::write(&path, &original).unwrap();
    let (config, warning) = load_from(&path);
    let mut expected = Config::default();
    expected.music.alternating_order = vec!["demo-strip".into()];
    assert_eq!(config, expected);
    assert!(warning.unwrap().contains("Removed 3"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    // Interchange stays strict; only startup migrates old saved layouts.
    assert!(decode_layout(&original).is_err());
    save_to(&config, &path).unwrap();
    let backups: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry != &path)
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(&backups[0]).unwrap(), original);
    assert_eq!(load_from(&path), (expected, None));
}

#[test]
fn retired_output_migration_keeps_strict_validation() {
    let original = layout_with_retired_lights();
    for invalid in [
        original.replacen("\"fps\":30", "\"fps\":30,\"fps\":30", 1),
        original.replacen("\"zones\":8", "\"zones\":8,\"zones\":8", 1),
        original.replacen("\"version\":2", "\"version\":999", 1),
        original.replacen("\"kind\":\"mock\"", "\"kind\":\"unknown\"", 1),
        original.replacen(
            "\"kind\":\"mock\"",
            "\"kind\":\"mock\",\"password\":\"secret\"",
            1,
        ),
        original.replacen(
            "\"demo-strip\",\"old-matter\"",
            "\"missing-light\",\"old-matter\"",
            1,
        ),
    ] {
        assert_ne!(invalid, original, "test must mutate the fixture");
        assert!(migrate_retired_lights(invalid.as_bytes()).is_err());
    }
    let mut only_retired: serde_json::Value = serde_json::from_str(&original).unwrap();
    only_retired["lights"].as_array_mut().unwrap().remove(0);
    only_retired["music"]["alternating_order"] =
        serde_json::json!(["old-mqtt", "old-tapo", "old-matter"]);
    let (config, count) = migrate_retired_lights(only_retired.to_string().as_bytes()).unwrap();
    assert_eq!(count, 3);
    assert!(config.lights.is_empty());
    assert!(config.music.alternating_order.is_empty());
    assert!(config.music.light_overrides.is_empty());
}

#[test]
fn dark_and_music_settings_default_validate_and_roundtrip() {
    let mut config = Config::default();
    let mut old = serde_json::to_value(&config).unwrap();
    for field in ["dark_zones", "mode", "music"] {
        old.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(decode_layout(&old.to_string()).unwrap(), config);
    config.dark_zones = crate::dark_zones::DarkZones {
        enabled: true,
        threshold: 24,
    };
    config.mode = crate::music::SyncMode::Music;
    config.music.input = crate::music::AudioInput::Demo;
    config.music.color = [10, 40, 240];
    config.music.decay_ms = 500.0;
    let json = encode_layout(&config).unwrap();
    assert_eq!(decode_layout(&json).unwrap(), config);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    save_to(&config, &path).unwrap();
    assert_eq!(read_config(&path).unwrap(), config);
    for bad in [
        "\"dark_zones\": {\"enabled\": true, \"threshold\": 65}",
        "\"dark_zones\": {\"unknown\": 1}",
        "\"dark_zones\": {\"threshold\": 8, \"threshold\": 9}",
    ] {
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value.as_object_mut().unwrap().remove("dark_zones");
        let document = value.to_string();
        let document = format!("{},{} }}", &document[..document.len() - 1], bad);
        assert!(decode_layout(&document).is_err());
    }
    for field in 0..4 {
        for value in [f32::NAN, f32::INFINITY, -1.0, 3000.0] {
            let mut invalid = config.clone();
            match field {
                0 => invalid.music.sensitivity = value,
                1 => invalid.music.attack_ms = value,
                2 => invalid.music.decay_ms = value,
                _ => invalid.music.brightness = value,
            }
            assert!(invalid.validate().is_err());
        }
    }
    let mut invalid = serde_json::to_value(&config).unwrap();
    invalid["music"]["unknown"] = serde_json::json!(true);
    assert!(decode_layout(&invalid.to_string()).is_err());
}
#[test]
fn advanced_music_defaults_and_controls_are_strict_and_persistent() {
    use crate::music::MusicSettings;
    let old = r#"{"input":"playback","device":"","sensitivity":0.8,"attack_ms":20,"decay_ms":240,"brightness":0.7,"color":[100,170,255]}"#;
    let decoded: MusicSettings = serde_json::from_str(old).unwrap();
    assert_eq!(decoded.sensitivity, 0.8);
    assert_eq!(decoded.subbass_weight, 0.2);
    assert_eq!(decoded.sparkle_amount, 0.35);
    assert!(decoded.follow_beat);
    for json in [
        r#"{"unknown":true}"#,
        r#"{"follow_beat":false,"follow_beat":true}"#,
    ] {
        assert!(serde_json::from_str::<MusicSettings>(json).is_err());
    }
    for value in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(
            MusicSettings {
                subbass_weight: value,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            MusicSettings {
                sparkle_amount: value,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    let mut config = Config::default();
    config.music.subbass_weight = 0.7;
    config.music.sparkle_amount = 0.9;
    config.music.follow_beat = false;
    assert_eq!(
        decode_layout(&encode_layout(&config).unwrap()).unwrap(),
        config
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("advanced.json");
    save_to(&config, &path).unwrap();
    assert_eq!(read_config(&path).unwrap(), config);
}
#[test]
fn retired_music_settings_load_without_affecting_active_controls() {
    let light_id = Config::default().lights[0].id.clone();
    let mut old = serde_json::to_value(Config::default()).unwrap();
    old["music"]["detector"] = serde_json::json!("classic");
    old["music"]["preset"] = serde_json::json!("energetic");
    old["music"]["wled_timing_ms"] = serde_json::json!(-80.0);
    old["music"]["ha_timing_ms"] = serde_json::json!(200.0);
    old["music"]["light_overrides"] = serde_json::json!({
        (light_id.clone()): {"gain": 0.5, "timing_ms": 100.0}
    });
    let loaded = decode_layout(&old.to_string()).unwrap();
    assert_eq!(loaded.music.effect, crate::music::MusicEffect::Pulse);
    assert_eq!(loaded.music.brightness, Config::default().music.brightness);
    assert_eq!(loaded.music.light_overrides[&light_id].gain, 0.5);
    let saved: serde_json::Value = serde_json::from_str(&encode_layout(&loaded).unwrap()).unwrap();
    for key in ["detector", "preset", "wled_timing_ms", "ha_timing_ms"] {
        assert!(saved["music"].get(key).is_none());
    }
    assert!(
        saved["music"]["light_overrides"][&light_id]
            .get("timing_ms")
            .is_none()
    );
    assert_eq!(decode_layout(&saved.to_string()).unwrap(), loaded);
}
#[test]
fn removed_music_effects_migrate_to_pulse() {
    for effect in ["traveling_wave", "spectrum", "ambient"] {
        let mut old = serde_json::to_value(Config::default()).unwrap();
        let light_id = Config::default().lights[0].id.clone();
        old["music"]["effect"] = serde_json::json!(effect);
        old["music"]["wave_width"] = serde_json::json!(0.3);
        old["music"]["wave_travel_beats"] = serde_json::json!(2.0);
        old["music"]["spectrum_contrast"] = serde_json::json!(0.8);
        old["music"]["ambient_response_ms"] = serde_json::json!(1200.0);
        old["music"]["light_overrides"] = serde_json::json!({
            (light_id.clone()): {"effect": effect, "phase_offset": 0.5, "gain": 0.6}
        });
        let loaded = decode_layout(&old.to_string()).unwrap();
        assert_eq!(loaded.music.effect, crate::music::MusicEffect::Pulse);
        assert_eq!(
            loaded.music.light_overrides[&light_id].effect,
            Some(crate::music::MusicEffect::Pulse)
        );
        assert_eq!(loaded.music.light_overrides[&light_id].gain, 0.6);
        let saved = serde_json::to_value(&loaded).unwrap();
        assert_eq!(saved["music"]["effect"], "pulse");
        assert_eq!(
            saved["music"]["light_overrides"][&light_id]["effect"],
            "pulse"
        );
        for key in [
            "wave_width",
            "wave_travel_beats",
            "spectrum_contrast",
            "ambient_response_ms",
        ] {
            assert!(saved["music"].get(key).is_none());
        }
        assert!(
            saved["music"]["light_overrides"][&light_id]
                .get("phase_offset")
                .is_none()
        );
        assert_eq!(decode_layout(&saved.to_string()).unwrap(), loaded);
    }
    assert!(
        serde_json::from_str::<crate::music::MusicSettings>(r#"{"effect":"unknown"}"#).is_err()
    );
}
#[test]
fn soft_ambience_defaults_strict_validation_and_persistence() {
    let mut config = Config::default();
    let mut previous = serde_json::to_value(&config).unwrap();
    previous["version"] = serde_json::json!(1);
    assert!(decode_layout(&previous.to_string()).is_err());
    let mut old = serde_json::to_value(&config).unwrap();
    old.as_object_mut().unwrap().remove("soft_ambience");
    assert_eq!(
        decode_layout(&old.to_string()).unwrap().soft_ambience,
        SoftAmbience::default()
    );
    config.soft_ambience = SoftAmbience {
        enabled: true,
        color_strength: 0.75,
        response_ms: 400.0,
    };
    assert_eq!(
        decode_layout(&encode_layout(&config).unwrap()).unwrap(),
        config
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    save_to(&config, &path).unwrap();
    assert_eq!(read_config(&path).unwrap(), config);
    for value in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
        let mut invalid = config.clone();
        invalid.soft_ambience.color_strength = value;
        assert!(invalid.validate().is_err());
    }
    config.soft_ambience.response_ms = 0.0;
    assert_eq!(
        decode_layout(&encode_layout(&config).unwrap()).unwrap(),
        config
    );
    for value in [-0.1, 1501.0, f32::NAN, f32::INFINITY] {
        let mut invalid = config.clone();
        invalid.soft_ambience.response_ms = value;
        assert!(invalid.validate().is_err());
    }
    let json = encode_layout(&config)
        .unwrap()
        .replace("\"enabled\": true", "\"unknown\": true");
    assert!(decode_layout(&json).is_err());
}

#[test]
fn segment_counts_survive_save_export_and_import() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let mut config = Config::default();
    config.lights[0].shape = Shape::Strip {
        points: crate::editor::strip_preset_points(crate::editor::StripPreset::Perimeter, 0.04),
        radius: 0.04,
        reverse: true,
        segment_leds: vec![24, 40, 25, 38],
    };
    config.lights[0].route = Route::Wled {
        host: "desk.local".into(),
        start: 10,
        count: 127,
        device_id: "aabbccddeeff".into(),
    };
    save_to(&config, &path).unwrap();
    assert_eq!(load_from(&path), (config.clone(), None));
    let exported = encode_layout(&config).unwrap();
    assert!(exported.contains("segment_leds"));
    assert_eq!(decode_layout(&exported).unwrap(), config);
}

#[test]
fn layout_interchange_roundtrips_unicode_and_pretty_json() {
    let mut config = Config::default();
    config.lights[0].name = "Schreibtisch 🌈 東京".into();
    config.source = CaptureSelection::Desktop {
        id: Some("Anzeige-東京".into()),
    };
    config.restore_wled_state = true;
    config.black_bar_detection = true;
    let json = encode_layout(&config).unwrap();
    assert!(json.contains("Schreibtisch 🌈 東京"));
    assert!(json.contains("\n  \"version\""));
    assert_eq!(decode_layout(&json).unwrap(), config);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    fs::write(&path, &json).unwrap();
    assert_eq!(read_config(&path).unwrap(), config);
    save_to(&config, &path).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), format!("{json}\n"));
}

#[test]
fn layout_export_rejects_invalid_current_configuration() {
    let mut config = Config {
        fps: 0,
        ..Config::default()
    };
    assert!(encode_layout(&config).is_err());
    config.fps = 30;
    config.brightness = f32::NAN;
    assert!(encode_layout(&config).is_err());
    config.brightness = 0.7;
    config.ha_url = "https://user:secret@example.com".into();
    assert!(encode_layout(&config).is_err());
}

#[test]
fn layouts_without_black_bar_setting_default_to_enabled() {
    let mut json = serde_json::to_value(Config::default()).unwrap();
    json.as_object_mut().unwrap().remove("black_bar_detection");
    assert!(
        decode_layout(&json.to_string())
            .unwrap()
            .black_bar_detection
    );
}

#[test]
fn layout_import_rejects_unknown_credentials_and_unsupported_versions() {
    let valid = serde_json::to_value(Config::default()).unwrap();
    for secret_field in ["ha_token", "password", "credentials"] {
        let mut json = valid.clone();
        json[secret_field] = serde_json::json!("secret");
        assert!(decode_layout(&json.to_string()).is_err());
    }
    let mut nested_secret = valid.clone();
    nested_secret["lights"][0]["route"]["token"] = serde_json::json!("secret");
    assert!(decode_layout(&nested_secret.to_string()).is_err());
    let mut synthetic_secret = valid.clone();
    synthetic_secret["source"]["token"] = serde_json::json!("secret");
    assert!(decode_layout(&synthetic_secret.to_string()).is_err());
    for version in [0, CONFIG_VERSION + 1] {
        let mut json = valid.clone();
        json["version"] = serde_json::json!(version);
        let error = decode_layout(&json.to_string()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported configuration version")
        );
    }
    let mut embedded_credentials = valid;
    embedded_credentials["ha_url"] = serde_json::json!("https://user:secret@example.com");
    assert!(decode_layout(&embedded_credentials.to_string()).is_err());
}

#[test]
fn layout_import_validates_routes_before_returning_a_configuration() {
    let mut config = Config::default();
    config.lights[0].route = Route::HomeAssistant {
        entity_id: "light.desk".into(),
    };
    assert!(decode_layout(&serde_json::to_string(&config).unwrap()).is_err());
    config.lights[0].zones = 1;
    assert!(decode_layout(&serde_json::to_string(&config).unwrap()).is_ok());
    config.lights[0].route = Route::Wled {
        host: "desk.local".into(),
        start: 0,
        count: 20,
        device_id: "AA:BB:CC:DD:EE:FF".into(),
    };
    let mut duplicate = config.lights[0].clone();
    duplicate.id = "duplicate-route".into();
    duplicate.route = Route::Wled {
        host: "192.168.1.2".into(),
        start: 19,
        count: 20,
        device_id: "aabbccddeeff".into(),
    };
    config.lights.push(duplicate);
    assert!(decode_layout(&serde_json::to_string(&config).unwrap()).is_err());
}

#[test]
fn layout_import_enforces_byte_limit_before_parsing() {
    let mut exactly_at_limit = encode_layout(&Config::default()).unwrap();
    exactly_at_limit.push_str(&" ".repeat(MAX_CONFIG_BYTES as usize - exactly_at_limit.len()));
    assert_eq!(decode_layout(&exactly_at_limit).unwrap(), Config::default());
    exactly_at_limit.push(' ');
    assert_eq!(
        decode_layout(&exactly_at_limit).unwrap_err().to_string(),
        "configuration exceeds 1 MiB"
    );
    let malformed = "{".repeat(MAX_CONFIG_BYTES as usize + 1);
    assert_eq!(
        decode_layout(&malformed).unwrap_err().to_string(),
        "configuration exceeds 1 MiB"
    );
    let multibyte = "🌈".repeat(MAX_CONFIG_BYTES as usize / 4 + 1);
    assert!(multibyte.chars().count() < MAX_CONFIG_BYTES as usize);
    assert_eq!(
        decode_layout(&multibyte).unwrap_err().to_string(),
        "configuration exceeds 1 MiB"
    );
}

#[test]
fn config_roundtrip_and_invalid_preservation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let config = Config::default();
    save_to(&config, &path).unwrap();
    assert_eq!(load_from(&path), (config.clone(), None));
    let invalid = b"{ definitely invalid JSON";
    fs::write(&path, invalid).unwrap();
    let (fallback, warning) = load_from(&path);
    assert_eq!(fallback, Config::default());
    assert!(warning.unwrap().contains("preserved"));
    assert_eq!(fs::read(&path).unwrap(), invalid);
    let mut invalid_config = config;
    invalid_config.fps = 0;
    assert!(save_to(&invalid_config, &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), invalid);
    save_to(&fallback, &path).unwrap();
    assert_eq!(load_from(&path), (fallback, None));
    let backups: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".invalid-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), invalid);
    save_to(&Config::default(), &path).unwrap();
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}
#[test]
fn future_version_and_credentials_are_rejected() {
    let mut config = Config::default();
    config.version += 1;
    assert!(config.validate().is_err());
    config.version = CONFIG_VERSION;
    config.ha_url = "https://user:secret@example.com".into();
    assert!(config.validate().is_err());
    let json = serde_json::to_value(Config::default()).unwrap();
    let mut object = json.as_object().unwrap().clone();
    object.insert("ha_token".into(), serde_json::json!("secret"));
    assert!(serde_json::from_value::<Config>(serde_json::Value::Object(object)).is_err());
}
#[test]
fn future_and_oversized_files_survive_recovery_save() {
    for oversized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let original = if oversized {
            vec![b' '; MAX_CONFIG_BYTES as usize + 1]
        } else {
            let mut future = Config::default();
            future.version += 1;
            serde_json::to_vec(&future).unwrap()
        };
        fs::write(&path, &original).unwrap();
        let (fallback, warning) = load_from(&path);
        assert!(warning.is_some());
        save_to(&fallback, &path).unwrap();
        let backup = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|candidate| candidate != &path)
            .unwrap();
        assert_eq!(fs::read(backup).unwrap(), original);
    }
}
#[test]
fn legacy_configs_default_to_no_wled_restoration() {
    let mut json = serde_json::to_value(Config::default()).unwrap();
    json.as_object_mut().unwrap().remove("restore_wled_state");
    let config: Config = serde_json::from_value(json).unwrap();
    assert!(!config.restore_wled_state);
    assert!(config.validate().is_ok());
    let mut too_fast = config;
    too_fast.ha_interval_ms = 499;
    assert!(too_fast.validate().is_err());
}
#[test]
fn overlapping_routes_are_rejected_but_adjacent_ranges_are_valid() {
    let mut config = Config::default();
    config.lights[0].route = Route::Wled {
        host: "192.168.1.2".into(),
        start: 0,
        count: 20,
        device_id: "wled-1".into(),
    };
    let mut second = config.lights[0].clone();
    second.id = "second".into();
    second.route = Route::Wled {
        host: "192.168.1.2".into(),
        start: 20,
        count: 20,
        device_id: "wled-1".into(),
    };
    config.lights.push(second);
    assert!(config.validate().is_ok());
    if let Route::Wled { start, .. } = &mut config.lights[1].route {
        *start = 19;
    }
    assert!(config.validate().is_err());
}

#[test]
fn mac_spellings_and_host_aliases_share_overlap_validation() {
    for (second_host, second_mac) in [
        ("desk.local", "aabbccddeeff"),
        ("192.168.1.2", "aa-bb-cc-dd-ee-ff"),
    ] {
        let mut config = Config::default();
        config.lights[0].route = Route::Wled {
            host: "Desk.local".into(),
            start: 0,
            count: 20,
            device_id: "AA:BB:CC:DD:EE:FF".into(),
        };
        let mut second = config.lights[0].clone();
        second.id = "second".into();
        second.route = Route::Wled {
            host: second_host.into(),
            start: 19,
            count: 20,
            device_id: second_mac.into(),
        };
        config.lights.push(second);
        assert!(config.validate().is_err());
        if let Route::Wled { start, .. } = &mut config.lights[1].route {
            *start = 20;
        }
        assert!(config.validate().is_ok());
    }
    assert_eq!(
        crate::core::canonical_device_id("AA:BB:CC:DD:EE:FF"),
        "aabbccddeeff"
    );
    assert_eq!(crate::core::canonical_host("Desk.LOCAL"), "desk.local");
}

#[test]
fn same_host_cannot_claim_conflicting_controller_identities() {
    let mut config = Config::default();
    config.lights[0].route = Route::Wled {
        host: "Desk.local".into(),
        start: 0,
        count: 20,
        device_id: "AA:BB:CC:DD:EE:FF".into(),
    };
    let mut second = config.lights[0].clone();
    second.id = "second".into();
    second.route = Route::Wled {
        host: "desk.LOCAL".into(),
        start: 20,
        count: 20,
        device_id: "11:22:33:44:55:66".into(),
    };
    config.lights.push(second);
    assert!(config.validate().is_err());
}

#[test]
fn loaded_ha_strips_must_sample_one_zone_covering_the_entire_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.json");
    let mut config = Config::default();
    config.lights[0].route = Route::HomeAssistant {
        entity_id: "light.desk".into(),
    };
    // The default strip has eight zones, which must not silently use zone zero.
    assert!(config.validate().is_err());
    let invalid = serde_json::to_vec(&config).unwrap();
    fs::write(&path, &invalid).unwrap();
    let (fallback, warning) = load_from(&path);
    assert_eq!(fallback, Config::default());
    assert!(warning.unwrap().contains("one zone"));
    assert_eq!(fs::read(&path).unwrap(), invalid);
    config.lights[0].zones = 1;
    config.lights[0].shape = Shape::Strip {
        points: vec![Point { x: 0.0, y: 0.5 }, Point { x: 1.0, y: 0.5 }],
        radius: 0.2,
        segment_leds: vec![],
        reverse: false,
    };
    save_to(&config, &path).unwrap();
    assert_eq!(load_from(&path), (config.clone(), None));
    let plan = crate::core::SamplingPlan::compile(&config.lights, 10, 2).unwrap();
    let frame = crate::core::Frame {
        width: 10,
        height: 2,
        pixels: (0..20)
            .map(|index| if index % 10 < 5 { [0; 3] } else { [255; 3] })
            .collect(),

        taps: None,
    };
    assert!((plan.sample(&frame)[0][0][0] - 0.5).abs() < 0.001);
}
