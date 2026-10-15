//! Bounded, sample-timed facts emitted by the analyzer.
use super::TrackingState;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct AnalysisFrame {
    pub time_seconds: f64,
    pub bands: [f32; 4],
    pub novelty: [f32; 4],
    pub rms: f32,
    pub peak: f32,
    pub audible: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnsetKind {
    Main,
    Sparkle,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct OnsetEvent {
    pub sequence: u64,
    pub time_seconds: f64,
    pub kind: OnsetKind,
    pub strength: f32,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct BeatEvent {
    pub identity: u64,
    pub time_seconds: f64,
    pub observed: bool,
    /// A confirming attack may have the same identity as an earlier prediction.
    pub starts_visual: bool,
    pub strength: f32,
    pub confidence: f32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AnalysisEvent {
    Onset(OnsetEvent),
    Beat(BeatEvent),
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct BeatState {
    pub bpm: Option<f32>,
    pub confidence: f32,
    pub tracking: TrackingState,
}
