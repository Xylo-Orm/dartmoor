//! Versioned, credential-free configuration with atomic persistence.

pub use crate::core::CaptureSelection;
use crate::core::SoftAmbience;
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

pub const CONFIG_VERSION: u32 = 2;
fn default_true() -> bool {
    true
}
pub(crate) const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub source: CaptureSelection,
    pub lights: Vec<Light>,
    pub fps: u32,
    pub smoothing_ms: f32,
    pub brightness: f32,
    #[serde(default = "default_true")]
    pub black_bar_detection: bool,
    #[serde(default)]
    pub soft_ambience: SoftAmbience,
    #[serde(default)]
    pub dark_zones: crate::dark_zones::DarkZones,
    #[serde(default)]
    pub mode: crate::music::SyncMode,
    #[serde(default)]
    pub music: crate::music::MusicSettings,
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
                video_response_ms: None,
                id: "demo-strip".into(),
                name: "Demo strip".into(),
                shape: Shape::Strip {
                    points: vec![Point { x: 0.15, y: 0.8 }, Point { x: 0.85, y: 0.8 }],
                    radius: 0.06,
                    segment_leds: vec![],
                    reverse: false,
                },
                zones: 8,
                route: Route::Mock,
            }],
            fps: 30,
            smoothing_ms: 120.0,
            brightness: 1.0,
            black_bar_detection: true,
            soft_ambience: SoftAmbience::default(),
            dark_zones: crate::dark_zones::DarkZones::default(),
            mode: crate::music::SyncMode::default(),
            music: crate::music::MusicSettings::default(),
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
        self.soft_ambience.validate()?;
        self.dark_zones.validate()?;
        self.music.validate()?;
        for id in self.music.light_overrides.keys() {
            ensure!(
                self.lights.iter().any(|light| light.id == *id),
                "music override references unknown light: {id}"
            );
        }
        for id in &self.music.alternating_order {
            ensure!(
                self.lights.iter().any(|light| light.id == *id),
                "alternating order references unknown light: {id}"
            );
        }
        validate_lights(&self.lights)
    }
}

/// Clipboard/file interchange uses the same credential-free schema as persistence.
pub(crate) fn encode_layout(config: &Config) -> Result<String> {
    config.validate()?;
    let json = serde_json::to_string_pretty(config).context("serialize configuration")?;
    ensure!(
        json.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    Ok(json)
}

pub(crate) fn decode_layout(json: &str) -> Result<Config> {
    decode_layout_bytes(json.as_bytes())
}

fn decode_layout_bytes(bytes: &[u8]) -> Result<Config> {
    let config = parse_layout_bytes(bytes)?;
    config.validate()?;
    Ok(config)
}

fn parse_layout_bytes(bytes: &[u8]) -> Result<Config> {
    // Check bytes before invoking serde, including malformed and multibyte input.
    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    let config: Config = serde_json::from_slice(bytes).context("invalid configuration JSON")?;
    // Serde's tagged unit variants ignore extra fields even with deny_unknown_fields.
    // Keep the strict struct deserialization above (including duplicate-field checks),
    // then inspect these two unit variants so imports cannot silently discard secrets.
    let json: serde_json::Value = serde_json::from_slice(bytes)?;
    if matches!(config.source, CaptureSelection::Synthetic) {
        ensure!(
            json["source"]
                .as_object()
                .is_some_and(|source| source.len() == 1),
            "invalid configuration JSON: unknown fields in synthetic source"
        );
    }
    for (index, light) in config.lights.iter().enumerate() {
        if matches!(light.route, Route::Mock) {
            ensure!(
                json["lights"][index]["route"]
                    .as_object()
                    .is_some_and(|route| route.len() == 1),
                "invalid configuration JSON: unknown fields in mock route"
            );
        }
    }
    Ok(config)
}

pub fn path() -> PathBuf {
    ProjectDirs::from("org", "Lumen", "lumen-desktop")
        .map(|dirs| dirs.config_dir().join("config.json"))
        .unwrap_or_else(|| PathBuf::from("lumen-desktop-config.json"))
}

/// Invalid files are preserved verbatim and reported; the editor sanitizes the fallback.
pub fn load() -> (Config, Option<String>) {
    load_from(&path())
}

fn load_from(path: &Path) -> (Config, Option<String>) {
    match read_saved_config(path) {
        Ok((config, removed)) => (
            config,
            (removed > 0).then(|| format!(
                "Removed {removed} retired MQTT/Matter/Tapo light(s) from the loaded layout. Other devices and settings were retained; the original file will be backed up when saving."
            )),
        ),
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
                "Could not load {}: {error:#}. Safe defaults loaded; the original file was preserved.",
                path.display()
            )),
        ),
    }
}

#[cfg(test)]
fn read_config(path: &Path) -> Result<Config> {
    read_saved_config(path).map(|(config, _)| config)
}

fn read_saved_config(path: &Path) -> Result<(Config, usize)> {
    let file = fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    match decode_layout_bytes(&bytes) {
        Ok(config) => Ok((config, 0)),
        Err(error) => migrate_retired_lights(&bytes).or(Err(error)),
    }
}

/// Load-only compatibility for retired outputs. Preserve the raw JSON for all
/// surviving fields so normal strict decoding still catches duplicate fields,
/// unknown keys and invalid settings. Imports keep requiring the current schema.
fn migrate_retired_lights(bytes: &[u8]) -> Result<(Config, usize)> {
    use serde_json::value::RawValue;

    ensure!(
        bytes.len() as u64 <= MAX_CONFIG_BYTES,
        "configuration exceeds 1 MiB"
    );
    #[derive(Deserialize)]
    struct SavedLayout<'a> {
        #[serde(borrow)]
        lights: &'a RawValue,
    }
    #[derive(Deserialize)]
    struct SavedLight {
        id: String,
        route: SavedRoute,
    }
    #[derive(Deserialize)]
    struct SavedRoute {
        kind: String,
    }
    let layout: SavedLayout<'_> = serde_json::from_slice(bytes)?;
    let lights: Vec<&RawValue> = serde_json::from_str(layout.lights.get())?;
    let mut retired = std::collections::HashSet::new();
    let mut kept = Vec::new();
    let mut removed = 0;
    for raw in lights {
        let light: SavedLight = serde_json::from_str(raw.get())?;
        if matches!(light.route.kind.as_str(), "mqtt" | "matter" | "tapo") {
            retired.insert(light.id);
            removed += 1;
        } else {
            kept.push(raw.get());
        }
    }
    ensure!(removed > 0, "no retired outputs in saved configuration");
    // RawValue borrows the original input, giving the exact array span without
    // reserializing the rest of the document (which would erase duplicate keys).
    let start = layout.lights.get().as_ptr() as usize - bytes.as_ptr() as usize;
    let end = start + layout.lights.get().len();
    let replacement = format!("[{}]", kept.join(","));
    let mut filtered = Vec::with_capacity(bytes.len());
    filtered.extend_from_slice(&bytes[..start]);
    filtered.extend_from_slice(replacement.as_bytes());
    filtered.extend_from_slice(&bytes[end..]);
    let mut config = parse_layout_bytes(&filtered)?;
    // A reused ID still belonging to a supported light must keep its settings.
    retired.retain(|id| !config.lights.iter().any(|light| &light.id == id));
    config
        .music
        .light_overrides
        .retain(|id, _| !retired.contains(id));
    config
        .music
        .alternating_order
        .retain(|id| !retired.contains(id));
    config.validate()?;
    Ok((config, removed))
}

pub fn save(config: &Config) -> Result<()> {
    save_to(config, &path())
}

pub(crate) fn save_to(config: &Config, path: &Path) -> Result<()> {
    let json = encode_layout(config)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("create configuration directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create temporary configuration")?;
    temporary.write_all(json.as_bytes())?;
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
/// Validation uses a bounded read; oversized originals are streamed to the backup.
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
    if decode_layout_bytes(&bytes).is_ok() {
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
mod tests;
