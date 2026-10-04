//! One pending native sample, with image samples taking priority over Idle.

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    cell::Cell,
    marker::PhantomData,
    sync::{mpsc, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Idle,
    // A source-state or malformed sample must reach the worker to invalidate
    // cached-image reuse, and later Idle must not overwrite that boundary.
    Invalidate,
}

struct Pending<T> {
    item: T,
    kind: Kind,
}

struct State<T> {
    pending: Option<Pending<T>>,
    senders: usize,
    receiver_alive: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
    #[cfg(test)]
    waiting: AtomicBool,
}

pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    // Match mpsc::Receiver: one consumer can move between threads, but cannot
    // process native samples concurrently through shared references.
    single_consumer: PhantomData<Cell<()>>,
}

pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            pending: None,
            senders: 1,
            receiver_alive: true,
        }),
        ready: Condvar::new(),
        #[cfg(test)]
        waiting: AtomicBool::new(false),
    });
    (
        Sender {
            shared: shared.clone(),
        },
        Receiver {
            shared,
            single_consumer: PhantomData,
        },
    )
}

impl<T> Sender<T> {
    /// Never wait for queue capacity. Images and invalidations replace pending
    /// samples; Idle replaces only Idle, preserving images and state boundaries.
    pub fn send(&self, item: T, kind: Kind) -> Result<(), mpsc::SendError<T>> {
        let mut state = self.shared.state.lock().unwrap();
        if !state.receiver_alive {
            drop(state);
            return Err(mpsc::SendError(item));
        }

        let incoming = Pending { item, kind };
        let displaced = if kind != Kind::Idle
            || state
                .pending
                .as_ref()
                .map_or(true, |pending| pending.kind == Kind::Idle)
        {
            state.pending.replace(incoming)
        } else {
            Some(incoming)
        };
        drop(state);
        self.shared.ready.notify_one();
        // Releasing a native sample can run framework code. Keep that outside
        // the lock, as with releasing the receiver's last pending sample.
        drop(displaced);
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.shared.state.lock().unwrap().senders += 1;
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.senders -= 1;
        let disconnected = state.senders == 0;
        drop(state);
        if disconnected {
            self.shared.ready.notify_all();
        }
    }
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(pending) = state.pending.take() {
                return Ok(pending.item);
            }
            if state.senders == 0 {
                return Err(mpsc::RecvError);
            }
            #[cfg(test)]
            self.shared.waiting.store(true, Ordering::Relaxed);
            state = self.shared.ready.wait(state).unwrap();
            #[cfg(test)]
            self.shared.waiting.store(false, Ordering::Relaxed);
        }
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, mpsc::RecvTimeoutError> {
        let start = Instant::now();
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(pending) = state.pending.take() {
                return Ok(pending.item);
            }
            if state.senders == 0 {
                return Err(mpsc::RecvTimeoutError::Disconnected);
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(mpsc::RecvTimeoutError::Timeout);
            }
            // Recheck the predicate after both notifications and spurious
            // wakeups, without extending the original deadline.
            #[cfg(test)]
            self.shared.waiting.store(true, Ordering::Relaxed);
            state = self.shared.ready.wait_timeout(state, remaining).unwrap().0;
            #[cfg(test)]
            self.shared.waiting.store(false, Ordering::Relaxed);
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.receiver_alive = false;
        let pending = state.pending.take();
        drop(state);
        drop(pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn wait_until_blocked<T>(shared: &Shared<T>) {
        let start = Instant::now();
        while !shared.waiting.load(Ordering::Relaxed) {
            assert!(start.elapsed() < Duration::from_secs(2));
            thread::yield_now();
        }
        // The receiver sets `waiting` while holding the mutex, then Condvar
        // atomically releases it on entering the wait. A sender that observes
        // this flag cannot acquire the mutex and notify before that transition.
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Sample {
        Complete(u64),
        Started(u64),
        Idle(u64),
    }

    fn send(tx: &Sender<Sample>, sample: Sample) {
        let kind = match sample {
            Sample::Complete(_) | Sample::Started(_) => Kind::Image,
            Sample::Idle(_) => Kind::Idle,
        };
        tx.send(sample, kind).unwrap();
    }

    #[test]
    fn queued_idle_then_complete_survives_subsequent_idle() {
        let (tx, rx) = channel();
        send(&tx, Sample::Idle(1));
        send(&tx, Sample::Complete(2));
        send(&tx, Sample::Idle(3));
        send(&tx, Sample::Idle(4));
        assert_eq!(rx.recv().unwrap(), Sample::Complete(2));
        assert_eq!(
            rx.recv_timeout(Duration::ZERO),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    #[test]
    fn latest_image_replaces_older_images_and_idle() {
        let (tx, rx) = channel();
        send(&tx, Sample::Complete(1));
        send(&tx, Sample::Started(2));
        send(&tx, Sample::Idle(3));
        assert_eq!(rx.recv().unwrap(), Sample::Started(2));
        send(&tx, Sample::Idle(4));
        send(&tx, Sample::Started(5));
        send(&tx, Sample::Complete(6));
        assert_eq!(rx.recv().unwrap(), Sample::Complete(6));
    }

    #[test]
    fn idle_replaces_idle_and_can_follow_a_consumed_image() {
        let (tx, rx) = channel();
        send(&tx, Sample::Idle(1));
        send(&tx, Sample::Idle(2));
        assert_eq!(rx.recv().unwrap(), Sample::Idle(2));
        send(&tx, Sample::Complete(3));
        let selected = rx.recv().unwrap();
        send(&tx, Sample::Idle(4));
        assert_eq!(selected, Sample::Complete(3));
        assert_eq!(rx.recv().unwrap(), Sample::Idle(4));
    }

    #[test]
    fn invalidation_replaces_pending_sample_and_survives_idle() {
        let (tx, rx) = channel();
        tx.send(1, Kind::Idle).unwrap();
        tx.send(2, Kind::Image).unwrap();
        tx.send(3, Kind::Invalidate).unwrap();
        tx.send(4, Kind::Idle).unwrap();
        assert_eq!(rx.recv().unwrap(), 3);
        tx.send(5, Kind::Invalidate).unwrap();
        tx.send(6, Kind::Image).unwrap();
        assert_eq!(rx.recv().unwrap(), 6);
    }

    #[test]
    fn empty_mailbox_times_out_then_accepts_a_sample() {
        let (tx, rx) = channel();
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        send(&tx, Sample::Complete(1));
        assert_eq!(
            rx.recv_timeout(Duration::ZERO).unwrap(),
            Sample::Complete(1)
        );
    }

    #[test]
    fn cloned_sender_keeps_receiver_connected_and_pending_item_is_delivered_first() {
        let (tx, rx) = channel();
        let clone = tx.clone();
        drop(tx);
        assert_eq!(
            rx.recv_timeout(Duration::ZERO),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        send(&clone, Sample::Complete(1));
        drop(clone);
        assert_eq!(
            rx.recv_timeout(Duration::ZERO).unwrap(),
            Sample::Complete(1)
        );
        assert_eq!(rx.recv(), Err(mpsc::RecvError));
        assert_eq!(
            rx.recv_timeout(Duration::ZERO),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn blocked_receiver_wakes_for_image() {
        let (tx, rx) = channel();
        let shared = rx.shared.clone();
        let worker = thread::spawn(move || rx.recv_timeout(Duration::from_secs(2)));
        wait_until_blocked(&shared);
        send(&tx, Sample::Complete(1));
        assert_eq!(worker.join().unwrap().unwrap(), Sample::Complete(1));
    }

    #[test]
    fn blocked_receiver_wakes_for_disconnection() {
        let (tx, rx) = channel::<Sample>();
        let shared = rx.shared.clone();
        let worker = thread::spawn(move || rx.recv_timeout(Duration::from_secs(2)));
        wait_until_blocked(&shared);
        drop(tx);
        assert_eq!(
            worker.join().unwrap(),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }

    #[derive(Debug)]
    struct Tracked {
        id: usize,
        dropped: Arc<Mutex<Vec<usize>>>,
        mailbox: std::sync::Weak<Shared<Tracked>>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            if let Some(mailbox) = self.mailbox.upgrade() {
                assert!(mailbox.state.try_lock().is_ok());
            }
            self.dropped.lock().unwrap().push(self.id);
        }
    }

    #[test]
    fn replaced_ignored_and_receiver_dropped_items_are_released_once() {
        let (tx, rx) = channel();
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let sample = |id| Tracked {
            id,
            dropped: dropped.clone(),
            mailbox: Arc::downgrade(&tx.shared),
        };
        tx.send(sample(1), Kind::Idle).unwrap();
        tx.send(sample(2), Kind::Image).unwrap();
        tx.send(sample(3), Kind::Idle).unwrap();
        tx.send(sample(4), Kind::Image).unwrap();
        assert_eq!(*dropped.lock().unwrap(), [1, 3, 2]);
        drop(rx);
        assert_eq!(*dropped.lock().unwrap(), [1, 3, 2, 4]);
        let rejected = tx.send(sample(5), Kind::Image).unwrap_err().0;
        assert_eq!(*dropped.lock().unwrap(), [1, 3, 2, 4]);
        drop(rejected);
        drop(tx);
        assert_eq!(*dropped.lock().unwrap(), [1, 3, 2, 4, 5]);
    }
}
