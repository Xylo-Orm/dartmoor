//! Metadata and cached-image reuse policy shared by the native worker and tests.

use std::sync::atomic::{AtomicBool, Ordering};

use super::mac_mailbox::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStatus {
    Complete,
    Started,
    Idle,
    Invalid,
}

impl FrameStatus {
    pub fn from_raw(status: Option<i64>) -> Self {
        // The native SCFrameStatus enum uses Complete = 0, Idle = 1, Started = 4.
        // Blank, Suspended, Stopped, absent and unknown values cannot prove that
        // a cached image still describes the source.
        match status {
            Some(0) => Self::Complete,
            Some(1) => Self::Idle,
            Some(4) => Self::Started,
            _ => Self::Invalid,
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::Complete | Self::Started => Kind::Image,
            Self::Idle => Kind::Idle,
            Self::Invalid => Kind::Invalidate,
        }
    }
}

#[derive(Default)]
pub struct ImageState {
    idle_image_valid: AtomicBool,
}

impl ImageState {
    pub fn invalidate(&self) {
        self.idle_image_valid.store(false, Ordering::Relaxed);
    }

    pub fn process<T, F>(
        &self,
        status: FrameStatus,
        sample: T,
        make_image: impl FnOnce(T) -> Option<F>,
        make_idle: impl FnOnce(T) -> Option<F>,
    ) -> Option<F> {
        match status {
            FrameStatus::Complete | FrameStatus::Started => {
                let frame = make_image(sample);
                // A changed image that failed conversion breaks the relation
                // between later Idle samples and the consumer's cached image.
                self.idle_image_valid
                    .store(frame.is_some(), Ordering::Relaxed);
                frame
            }
            FrameStatus::Idle if self.idle_image_valid.load(Ordering::Relaxed) => make_idle(sample),
            FrameStatus::Idle => None,
            FrameStatus::Invalid => {
                self.invalidate();
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capturer::engine::mac_mailbox;
    use std::time::Duration;

    fn process(state: &ImageState, status: FrameStatus, image: Option<u64>) -> Option<u64> {
        state.process(status, image, |image| image, |_| Some(0))
    }

    #[test]
    fn idle_requires_a_valid_image_and_recovers_after_failed_conversion() {
        let state = ImageState::default();
        assert_eq!(process(&state, FrameStatus::Idle, None), None);
        assert_eq!(process(&state, FrameStatus::Complete, Some(1)), Some(1));
        assert_eq!(process(&state, FrameStatus::Idle, None), Some(0));
        assert_eq!(process(&state, FrameStatus::Complete, None), None);
        assert_eq!(process(&state, FrameStatus::Idle, None), None);
        assert_eq!(process(&state, FrameStatus::Started, None), None);
        assert_eq!(process(&state, FrameStatus::Idle, None), None);
        assert_eq!(process(&state, FrameStatus::Complete, Some(2)), Some(2));
        assert_eq!(process(&state, FrameStatus::Idle, None), Some(0));
    }

    #[test]
    fn only_explicit_known_statuses_are_accepted() {
        assert_eq!(FrameStatus::from_raw(Some(0)), FrameStatus::Complete);
        assert_eq!(FrameStatus::from_raw(Some(1)), FrameStatus::Idle);
        assert_eq!(FrameStatus::from_raw(Some(4)), FrameStatus::Started);
        for raw in [
            None,
            Some(-1),
            Some(2),
            Some(3),
            Some(5),
            Some(6),
            Some(i64::MAX),
        ] {
            let status = FrameStatus::from_raw(raw);
            assert_eq!(status, FrameStatus::Invalid);
            assert_eq!(status.kind(), Kind::Invalidate);
            let state = ImageState::default();
            assert_eq!(process(&state, FrameStatus::Complete, Some(1)), Some(1));
            assert_eq!(process(&state, status, None), None);
            assert_eq!(process(&state, FrameStatus::Idle, None), None);
        }
    }

    #[test]
    fn backend_error_invalidates_idle_reuse() {
        let state = ImageState::default();
        assert_eq!(process(&state, FrameStatus::Complete, Some(1)), Some(1));
        assert_eq!(process(&state, FrameStatus::Idle, None), Some(0));
        state.invalidate();
        assert_eq!(process(&state, FrameStatus::Idle, None), None);
    }

    #[test]
    fn mailbox_delivers_invalidation_before_idle_can_refresh_cached_image() {
        let (tx, rx) = mac_mailbox::channel();
        let state = ImageState::default();
        assert_eq!(process(&state, FrameStatus::Complete, Some(1)), Some(1));
        let invalid = FrameStatus::from_raw(None);
        tx.send((invalid, None), invalid.kind()).unwrap();
        tx.send((FrameStatus::Idle, None), FrameStatus::Idle.kind())
            .unwrap();
        let (status, image) = rx.recv_timeout(Duration::ZERO).unwrap();
        assert_eq!(process(&state, status, image), None);
        tx.send((FrameStatus::Idle, None), FrameStatus::Idle.kind())
            .unwrap();
        let (status, image) = rx.recv_timeout(Duration::ZERO).unwrap();
        assert_eq!(process(&state, status, image), None);
        tx.send((FrameStatus::Started, Some(2)), FrameStatus::Started.kind())
            .unwrap();
        let (status, image) = rx.recv_timeout(Duration::ZERO).unwrap();
        assert_eq!(process(&state, status, image), Some(2));
        tx.send((FrameStatus::Idle, None), FrameStatus::Idle.kind())
            .unwrap();
        let (status, image) = rx.recv_timeout(Duration::ZERO).unwrap();
        assert_eq!(process(&state, status, image), Some(0));
    }
}
