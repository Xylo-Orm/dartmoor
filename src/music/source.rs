//! Capture contract. Empty negotiated idle and timeout have different freshness semantics.
use super::{AudioInput, MusicSettings, demo::Demo};
use anyhow::Result;
use std::time::Duration;

/// Capture payload. `Idle` means the stream is connected but paused; `Timeout`
/// means no data arrived within the polling interval.
pub(crate) enum AudioBlock {
    Samples(Vec<f32>),
    Idle,
    Timeout,
}

pub(crate) struct AudioRead {
    pub block: AudioBlock,
    pub discontinuity: bool,
    /// Approximate age of queued samples when drained; not a device timestamp.
    pub estimated_queue_age: Option<Duration>,
}

pub(crate) trait AudioSource {
    /// Interleaved stereo F32 at 48 kHz, bounded to MAX_SAMPLES per block.
    fn next_samples(&mut self, timeout: Duration) -> Result<Option<Vec<f32>>>;
    /// A dropped or renegotiated block invalidates spectral/tempo continuity.
    fn take_discontinuity(&mut self) -> bool {
        false
    }
    fn estimated_queue_age(&self) -> Option<Duration> {
        None
    }
    fn read(&mut self, timeout: Duration) -> Result<AudioRead> {
        let block = match self.next_samples(timeout)? {
            Some(samples) if samples.is_empty() => AudioBlock::Idle,
            Some(samples) => AudioBlock::Samples(samples),
            None => AudioBlock::Timeout,
        };
        Ok(AudioRead {
            block,
            discontinuity: self.take_discontinuity(),
            estimated_queue_age: self.estimated_queue_age(),
        })
    }
}
pub(crate) fn open(settings: &MusicSettings) -> Result<Box<dyn AudioSource>> {
    if settings.input == AudioInput::Demo {
        return Ok(Box::new(Demo::new(settings.demo_scenario)));
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(super::linux::Playback::open(&settings.device)?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!(
            "System playback capture is currently available on Linux. Choose Demo pulses to try music mode."
        )
    }
}
