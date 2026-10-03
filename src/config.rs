//! Versioned, credential-free configuration with atomic persistence.

pub use crate::core::CaptureSelection;
use crate::core::{Light, Point, Route, Shape, validate_lights};
use anyhow::{Context, Result, ensure};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const CONFIG_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub source: CaptureSelection,
    pub lights: Vec<Light>,
    pub fps: u32,
    pub smoothing_ms: f32,
    pub brightness: f32,
    pub ha_url: String,
    pub ha_interval_ms: u64,
    #[serde(default)]
    pub restore_wled_state: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            source: CaptureSelection::Synthetic,
            lights: vec![Light {
                id: "demo-strip".into(),
                name: "Demo strip".into(),
                shape: Shape::Strip {
                    points: vec![Point { x: 0.15, y: 0.8 }, Point { x: 0.85, y: 0.8 }],
                    radius: 0.06,
                    reverse: false,
                },
                zones: 8,
                route: Route::Mock,
            }],
            fps: 30,
            smoothing_ms: 120.0,
            brightness: 0.7,
            ha_url: "http://homeassistant.local:8123".into(),
            ha_interval_ms: 1000,
            restore_wled_state: false,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == CONFIG_VERSION,
            "unsupported configuration version {}; expected {}",
            self.version,
            CONFIG_VERSION
        );
        ensure!(
            (1..=120).contains(&self.fps),
            "capture FPS must be between 1 and 120"
        );
        ensure!(
            self.smoothing_ms.is_finite() && (0.0..=5000.0).contains(&self.smoothing_ms),
            "smoothing must be between 0 and 5000 ms"
        );
        ensure!(
            self.brightness.is_finite() && (0.0..=1.0).contains(&self.brightness),
            "brightness must be between 0 and 1"
        );
        ensure!(
            (500..=60000).contains(&self.ha_interval_ms),
            "Home Assistant interval must be between 500 and 60000 ms"
        );
        if let CaptureSelection::Desktop { id: Some(id) } = &self.source {
            ensure!(
                !id.is_empty() && id.len() <= 1024,
                "capture source ID must contain 1–1024 bytes"
            );
        }
        ensure!(
            self.ha_url.len() <= 2048 && !self.ha_url.chars().any(char::is_whitespace),
            "invalid Home Assistant URL"
        );
        let address = self
            .ha_url
            .strip_prefix("http://")
            .or_else(|| self.ha_url.strip_prefix("https://"))
            .ok_or_else(|| anyhow::anyhow!("Home Assistant URL must use HTTP or HTTPS"))?;
        ensure!(
            !address.is_empty()
                && !address.starts_with('/')
                && !address.contains('@')
                && !address.contains('?')
                && !address.contains('#')
                && !address.contains('\\'),
            "Home Assistant URL must not contain credentials, query parameters, or fragments"
        );
        validate_lights(&self.lights)
    }
}

pub fn path() -> PathBuf {
    ProjectDirs::from("org", "Lumen", "lumen-desktop")
        .map(|dirs| dirs.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from("lumen-desktop-config.json"))
}

/// Invalid files are preserved verbatim and reported; startup uses safe mock defaults.
pub fn load() -> (Config, Option<String>) {
    load_from(&path())
}

fn load_from(path: &Path) -> (Config, Option<String>) {
    match read_config(path) {
        Ok(config) => (config, None),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            (Config::default(), None)
        }
        Err(error) => (
            Config::default(),
            Some(format!(
                "Could not load {}: {error:#}. Safe demo defaults loaded; the original file was preserved.",
                path.display()
            )),
        ),
    }
}

fn read_config(path: &Path) -> Result<Config> {
    let file = fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    let config: Config = serde_json::from_slice(&bytes).context("invalid configuration JSON")?;
    config.validate()?;
    Ok(config)
}

pub fn save(config: &Config) -> Result<()> {
    save_to(config, &path())
}

fn save_to(config: &Config, path: &Path) -> Result<()> {
    config.validate()?;
    let bytes = serde_json::to_vec_pretty(config).context("serialize configuration")?;
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("create configuration directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create temporary configuration")?;
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    backup_invalid_config(path, parent)?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("atomically replace configuration")?;
    // Best-effort directory fsync is unavailable on some platforms.
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Keep existing invalid files even when the user saves recovered demo defaults.
/// Validation reads at most 1 MiB; oversized originals are streamed to the backup.
fn backup_invalid_config(path: &Path, parent: &Path) -> Result<()> {
    let mut original = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read existing configuration before replacement"),
    };
    let mut bytes = Vec::new();
    (&mut original)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 <= MAX_CONFIG_BYTES
        && serde_json::from_slice::<Config>(&bytes).is_ok_and(|config| config.validate().is_ok())
    {
        return Ok(());
    }
    original.rewind()?;
    let mut backup = tempfile::NamedTempFile::new_in(parent)
        .context("create backup for invalid configuration")?;
    std::io::copy(&mut original, &mut backup).context("copy invalid configuration backup")?;
    backup.as_file().sync_all()?;
    static BACKUP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = BACKUP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let backup_path = parent.join(format!("{stem}.invalid-{timestamp}-{sequence}.json"));
    backup
        .persist_noclobber(&backup_path)
        .map_err(|error| error.error)
        .context("atomically preserve invalid configuration backup")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        };
        assert!((plan.sample(&frame)[0][0][0] - 0.5).abs() < 0.001);
    }
}
