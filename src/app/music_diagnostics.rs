//! Export the bounded music feature/event buffers without audio samples.
use super::*;

impl App {
    pub(super) fn export_music_json(&mut self, telemetry: crate::music::Telemetry) {
        let path = self.config_path.with_file_name("music-diagnostics.json");
        let result = serde_json::to_vec_pretty(&serde_json::json!({
            "settings": self.config.music,
            "telemetry": telemetry,
            "features": self.music_frames,
            "events": self.music_events,
            "audio_included": false,
        }))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| std::fs::write(&path, bytes).map_err(anyhow::Error::from));
        self.notice = match result {
            Ok(()) => format!("Music diagnostics saved to {}", path.display()),
            Err(error) => format!("Could not export music diagnostics: {error}"),
        };
    }
    pub(super) fn export_music_csv(&mut self) {
        let feature_path = self.config_path.with_file_name("music-features.csv");
        let event_path = self.config_path.with_file_name("music-events.csv");
        let settings_path = self.config_path.with_file_name("music-settings.json");
        let mut features = String::from(
            "time_seconds,rms,peak,sub,midbass,body,air,novelty_sub,novelty_midbass,novelty_body,novelty_air,audible\n",
        );
        for frame in &self.music_frames {
            features.push_str(&format!(
                "{:.3},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{}\n",
                frame.time_seconds,
                frame.rms,
                frame.peak,
                frame.bands[0],
                frame.bands[1],
                frame.bands[2],
                frame.bands[3],
                frame.novelty[0],
                frame.novelty[1],
                frame.novelty[2],
                frame.novelty[3],
                frame.audible
            ));
        }
        let mut events = String::from(
            "time_seconds,event,kind_or_identity,observed,starts_visual,strength,confidence\n",
        );
        for event in &self.music_events {
            match event {
                crate::music::AnalysisEvent::Onset(onset) => events.push_str(&format!(
                    "{:.3},onset,{:?},,,{:.4},\n",
                    onset.time_seconds, onset.kind, onset.strength
                )),
                crate::music::AnalysisEvent::Beat(beat) => events.push_str(&format!(
                    "{:.3},beat,{},{},{},{:.4},{:.4}\n",
                    beat.time_seconds,
                    beat.identity,
                    beat.observed,
                    beat.starts_visual,
                    beat.strength,
                    beat.confidence
                )),
            }
        }
        let result = (|| -> anyhow::Result<()> {
            std::fs::write(&feature_path, features)?;
            std::fs::write(&event_path, events)?;
            std::fs::write(
                &settings_path,
                serde_json::to_vec_pretty(&self.config.music)?,
            )?;
            Ok(())
        })();
        self.notice = match result {
            Ok(()) => format!(
                "Music CSV and settings saved beside {}",
                feature_path.display()
            ),
            Err(error) => format!("Could not export music diagnostics: {error}"),
        };
    }
}
