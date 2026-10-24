// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::Duration,
};

use super::WorkerPoolError;

struct State<T> {
    values: VecDeque<T>,
    senders: usize,
    receiver_open: bool,
    failed: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.failed = true;
                state
            }
        }
    }
}

pub(super) struct ReplySender<T> {
    shared: Arc<Shared<T>>,
    counted: bool,
}

pub(super) struct ReplyReceiver<T> {
    shared: Arc<Shared<T>>,
}

pub(super) fn channel<T>() -> (ReplySender<T>, ReplyReceiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            values: VecDeque::new(),
            senders: 1,
            receiver_open: true,
            failed: false,
        }),
        ready: Condvar::new(),
    });
    (
        ReplySender {
            shared: Arc::clone(&shared),
            counted: true,
        },
        ReplyReceiver { shared },
    )
}

impl<T> Clone for ReplySender<T> {
    fn clone(&self) -> Self {
        let mut state = self.shared.lock();
        let counted = match state.senders.checked_add(1) {
            Some(count) => {
                state.senders = count;
                true
            }
            None => {
                state.failed = true;
                false
            }
        };
        let failed = state.failed;
        drop(state);
        if failed {
            self.shared.ready.notify_all();
        }
        Self {
            shared: Arc::clone(&self.shared),
            counted,
        }
    }
}

impl<T> ReplySender<T> {
    pub(super) fn send(&self, value: T) -> Result<(), T> {
        self.send_notifying(value, || self.shared.ready.notify_one())
    }

    fn send_notifying(&self, value: T, notify: impl FnOnce()) -> Result<(), T> {
        let mut state = self.shared.lock();
        if state.failed || !state.receiver_open {
            let failed = state.failed;
            drop(state);
            if failed {
                self.shared.ready.notify_all();
            }
            return Err(value);
        }
        state.values.push_back(value);
        drop(state);
        notify();
        Ok(())
    }
}

impl<T> Drop for ReplySender<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        if self.counted {
            if let Some(count) = state.senders.checked_sub(1) {
                state.senders = count;
            } else {
                state.failed = true;
            }
        }
        let wake = state.senders == 0 || state.failed;
        drop(state);
        if wake {
            self.shared.ready.notify_all();
        }
    }
}

impl<T> ReplyReceiver<T> {
    pub(super) fn reserve(&self, capacity: usize) -> Result<(), WorkerPoolError> {
        let mut state = self.shared.lock();
        if state.failed {
            drop(state);
            self.shared.ready.notify_all();
            return Err(WorkerPoolError::WorkersExited);
        }
        let additional = capacity.saturating_sub(state.values.len());
        state
            .values
            .try_reserve(additional)
            .map_err(|_| WorkerPoolError::QueueCapacity)
    }

    pub(super) fn try_receive(&self) -> Result<Option<T>, WorkerPoolError> {
        let mut state = self.shared.lock();
        if state.failed {
            drop(state);
            self.shared.ready.notify_all();
            return Err(WorkerPoolError::WorkersExited);
        }
        let value = state.values.pop_front();
        if value.is_none() && state.senders == 0 {
            return Err(WorkerPoolError::RepliesClosed);
        }
        Ok(value)
    }

    #[cfg(test)]
    pub(super) fn receive(&self) -> Result<T, WorkerPoolError> {
        self.receive_waiting(None, || {})?
            .ok_or(WorkerPoolError::RepliesClosed)
    }

    pub(super) fn receive_timeout(&self, timeout: Duration) -> Result<Option<T>, WorkerPoolError> {
        self.receive_waiting(Some(timeout), || {})
    }

    #[allow(clippy::disallowed_methods)]
    fn receive_waiting(
        &self,
        timeout: Option<Duration>,
        mut before_wait: impl FnMut(),
    ) -> Result<Option<T>, WorkerPoolError> {
        let started = std::time::Instant::now();
        let mut state = self.shared.lock();
        loop {
            if state.failed {
                drop(state);
                self.shared.ready.notify_all();
                return Err(WorkerPoolError::WorkersExited);
            }
            if let Some(value) = state.values.pop_front() {
                return Ok(Some(value));
            }
            if state.senders == 0 {
                return Err(WorkerPoolError::RepliesClosed);
            }
            state = if let Some(timeout) = timeout {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Ok(None);
                }
                before_wait();
                match self.shared.ready.wait_timeout(state, remaining) {
                    Ok((state, _)) => state,
                    Err(error) => {
                        let (mut state, _) = error.into_inner();
                        state.failed = true;
                        state
                    }
                }
            } else {
                before_wait();
                match self.shared.ready.wait(state) {
                    Ok(state) => state,
                    Err(error) => {
                        let mut state = error.into_inner();
                        state.failed = true;
                        state
                    }
                }
            };
        }
    }
}

impl<T> Drop for ReplyReceiver<T> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.receiver_open = false;
        let values = std::mem::take(&mut state.values);
        drop(state);
        self.shared.ready.notify_all();
        drop(values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
    };

    #[test]
    fn reply_queue_wait_registration_cannot_lose_a_ready_reply() {
        let (sender, receiver) = channel();
        let (waiting, entered) = mpsc::channel();
        thread::scope(|scope| {
            let result = scope.spawn(|| {
                receiver.receive_waiting(Some(Duration::from_secs(5)), || {
                    waiting.send(()).unwrap();
                })
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            sender.send(17).unwrap();
            assert_eq!(result.join().unwrap().unwrap(), Some(17));
        });
    }

    #[test]
    fn reply_queue_notification_follows_unlock_and_allows_receiver_progress() {
        let (sender, receiver) = channel();
        let (published, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        thread::scope(|scope| {
            let sending = scope.spawn(move || {
                sender.send_notifying(9, || {
                    assert!(sender.shared.state.try_lock().is_ok());
                    published.send(()).unwrap();
                    blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                    sender.shared.ready.notify_one();
                })
            });
            ready.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(receiver.try_receive().unwrap(), Some(9));
            release.send(()).unwrap();
            sending.join().unwrap().unwrap();
        });
    }

    #[test]
    fn reply_queue_timeout_rechecks_after_spurious_wakes_without_resetting_deadline() {
        let (_sender, receiver) = channel::<u8>();
        let (waiting, entered) = mpsc::channel();
        let waits = AtomicUsize::new(0);
        thread::scope(|scope| {
            let result = scope.spawn(|| {
                receiver.receive_waiting(Some(Duration::from_millis(10)), || {
                    waits.fetch_add(1, Ordering::SeqCst);
                    waiting.send(()).unwrap();
                })
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let guard = receiver.shared.lock();
            thread::sleep(Duration::from_millis(20));
            drop(guard);
            receiver.shared.ready.notify_one();
            assert_eq!(result.join().unwrap().unwrap(), None);
        });
        assert_eq!(waits.load(Ordering::SeqCst), 1);
        assert_eq!(receiver.try_receive().unwrap(), None);
    }

    #[test]
    fn reply_queue_drains_buffered_values_before_last_sender_disconnect() {
        let (sender, receiver) = channel();
        let other = sender.clone();
        sender.send(1).unwrap();
        other.send(2).unwrap();
        drop(sender);
        drop(other);
        assert_eq!(receiver.receive_timeout(Duration::ZERO).unwrap(), Some(1));
        assert_eq!(receiver.try_receive().unwrap(), Some(2));
        assert_eq!(receiver.try_receive(), Err(WorkerPoolError::RepliesClosed));
        assert_eq!(receiver.receive(), Err(WorkerPoolError::RepliesClosed));
    }

    #[test]
    fn reply_queue_last_sender_panic_wakes_the_blocked_receiver() {
        let (sender, receiver) = channel::<u8>();
        let (waiting, entered) = mpsc::channel();
        thread::scope(|scope| {
            let result = scope.spawn(|| {
                receiver.receive_waiting(Some(Duration::from_secs(5)), || {
                    waiting.send(()).unwrap();
                })
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(
                thread::spawn(move || {
                    let _sender = sender;
                    panic!("sender unwound");
                })
                .join()
                .is_err()
            );
            assert_eq!(result.join().unwrap(), Err(WorkerPoolError::RepliesClosed));
        });
    }

    struct Released {
        shared: Arc<Shared<Released>>,
        dropped: Arc<AtomicUsize>,
        panic_on_drop: bool,
    }

    impl Drop for Released {
        fn drop(&mut self) {
            assert!(self.shared.state.try_lock().is_ok());
            self.dropped.fetch_add(1, Ordering::SeqCst);
            assert!(!self.panic_on_drop, "payload drop failed");
        }
    }

    #[test]
    fn reply_queue_receiver_close_and_rejected_send_drop_values_outside_lock() {
        let (sender, receiver) = channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            assert!(
                sender
                    .send(Released {
                        shared: Arc::clone(&sender.shared),
                        dropped: Arc::clone(&dropped),
                        panic_on_drop: false,
                    })
                    .is_ok()
            );
        }
        drop(receiver);
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
        let returned = sender
            .send(Released {
                shared: Arc::clone(&sender.shared),
                dropped: Arc::clone(&dropped),
                panic_on_drop: false,
            })
            .err()
            .unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
        drop(returned);
        assert_eq!(dropped.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn reply_queue_payload_drop_panic_does_not_poison_transport() {
        let (sender, receiver) = channel();
        let dropped = Arc::new(AtomicUsize::new(0));
        assert!(
            sender
                .send(Released {
                    shared: Arc::clone(&sender.shared),
                    dropped: Arc::clone(&dropped),
                    panic_on_drop: true,
                })
                .is_ok()
        );
        assert!(std::panic::catch_unwind(|| drop(receiver)).is_err());
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert!(!sender.shared.state.is_poisoned());
        assert!(!sender.shared.lock().receiver_open);
    }

    #[test]
    fn reply_queue_poison_fails_closed_and_wakes_waiters_without_drop_panic() {
        let (sender, receiver) = channel::<u8>();
        let (waiting, entered) = mpsc::channel();
        thread::scope(|scope| {
            let result = scope.spawn(|| {
                receiver.receive_waiting(Some(Duration::from_secs(5)), || {
                    waiting.send(()).unwrap();
                })
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let shared = Arc::clone(&sender.shared);
            assert!(
                thread::spawn(move || {
                    let _guard = shared.state.lock().unwrap();
                    panic!("queue lock poisoned");
                })
                .join()
                .is_err()
            );
            drop(sender);
            assert_eq!(result.join().unwrap(), Err(WorkerPoolError::WorkersExited));
        });
        assert_eq!(receiver.try_receive(), Err(WorkerPoolError::WorkersExited));
    }

    #[test]
    fn reply_queue_window_reserve_reuses_storage_and_capacity_failure_keeps_values() {
        let (sender, receiver) = channel();
        sender.send(7).unwrap();
        receiver.reserve(16).unwrap();
        let capacity = receiver.shared.lock().values.capacity();
        assert!(capacity >= 16);
        assert_eq!(
            receiver.reserve(usize::MAX),
            Err(WorkerPoolError::QueueCapacity)
        );
        assert_eq!(receiver.try_receive().unwrap(), Some(7));
        for _ in 0..4 {
            for i in 0..16 {
                sender.send(i).unwrap();
            }
            for i in 0..16 {
                assert_eq!(receiver.try_receive().unwrap(), Some(i));
            }
            assert_eq!(receiver.shared.lock().values.capacity(), capacity);
        }
    }
}
