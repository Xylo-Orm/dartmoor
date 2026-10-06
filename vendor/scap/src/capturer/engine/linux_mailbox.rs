//! Reserve the single queue slot before allocating or processing pixels.
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

pub struct Sender<T> {
    tx: mpsc::SyncSender<T>,
    available: Arc<AtomicBool>,
    receiver_alive: Arc<AtomicBool>,
}
pub struct Receiver<T> {
    rx: mpsc::Receiver<T>,
    available: Arc<AtomicBool>,
    receiver_alive: Arc<AtomicBool>,
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel(1);
    let available = Arc::new(AtomicBool::new(true));
    let receiver_alive = Arc::new(AtomicBool::new(true));
    (
        Sender {
            tx,
            available: available.clone(),
            receiver_alive: receiver_alive.clone(),
        },
        Receiver {
            rx,
            available,
            receiver_alive,
        },
    )
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            available: self.available.clone(),
            receiver_alive: self.receiver_alive.clone(),
        }
    }
}

impl<T> Sender<T> {
    pub fn try_send_with(&self, make: impl FnOnce() -> Option<T>) -> bool {
        if !self.receiver_alive.load(Ordering::Acquire)
            || !self.available.swap(false, Ordering::AcqRel)
        {
            return false;
        }
        let Some(item) = make() else {
            self.available.store(true, Ordering::Release);
            return false;
        };
        match self.tx.try_send(item) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
            Err(mpsc::TrySendError::Full(_)) => unreachable!("queue slot was reserved"),
        }
    }
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        let item = self.rx.recv()?;
        self.available.store(true, Ordering::Release);
        Ok(item)
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let item = self.rx.recv_timeout(timeout)?;
        self.available.store(true, Ordering::Release);
        Ok(item)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.receiver_alive.store(false, Ordering::Release);
        self.available.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_queue_skips_pixel_processing_and_resumes_after_receive() {
        let (tx, rx) = channel();
        assert!(tx.try_send_with(|| Some(1)));
        assert!(!tx.try_send_with(|| panic!("must not copy a dropped frame")));
        assert_eq!(rx.recv().unwrap(), 1);
        assert!(tx.try_send_with(|| Some(2)));
        assert_eq!(rx.recv_timeout(Duration::ZERO).unwrap(), 2);
        assert!(!tx.try_send_with(|| None));
        assert!(tx.try_send_with(|| Some(3)));
        drop(rx);
        assert!(!tx.try_send_with(|| panic!("receiver is gone")));
    }
    #[test]
    fn empty_disconnected_queue_wakes_receiver() {
        let (tx, rx) = channel::<u8>();
        drop(tx);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn receiver_drop_during_rejected_conversion_does_not_reopen_slot() {
        let (tx, rx) = channel::<u8>();
        assert!(!tx.try_send_with(|| {
            drop(rx);
            None
        }));
        assert!(!tx.try_send_with(|| panic!("receiver is gone")));
    }
}
