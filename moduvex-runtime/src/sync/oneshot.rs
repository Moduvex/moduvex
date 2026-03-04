//! One-shot channel — send exactly one value from producer to consumer.
//!
//! `Sender` consumes itself on `send`; `Receiver` implements `Future` and
//! resolves to `Result<T, RecvError>`. Dropping the `Sender` before sending
//! causes the `Receiver` to resolve with `RecvError::Closed`.

use std::cell::UnsafeCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

// ── State constants ───────────────────────────────────────────────────────────

/// No value has been sent; sender still alive.
const EMPTY: u8 = 0;
/// Value has been written into the cell; receiver may take it.
const SENT: u8 = 1;
/// Sender was dropped without sending (channel closed).
const CLOSED: u8 = 2;

// ── Inner shared state ────────────────────────────────────────────────────────

struct Inner<T> {
    /// Current channel state: EMPTY | SENT | CLOSED.
    state: AtomicU8,
    /// Storage for the transmitted value. Written exactly once (EMPTY → SENT).
    ///
    /// `UnsafeCell` is required because we write through a shared `Arc`.
    /// Access is guarded by the `state` atomic: the sender writes while
    /// `state == EMPTY` (exclusive via CAS), the receiver reads only after
    /// observing `state == SENT`.
    value: UnsafeCell<Option<T>>,
    /// Waker for the blocked receiver (stored while state == EMPTY).
    waker: Mutex<Option<Waker>>,
}

// SAFETY: `Inner<T>` is shared across threads via `Arc`. The `UnsafeCell`
// holding the value is accessed in a sequenced, non-concurrent fashion:
// the sender writes once (EMPTY → SENT CAS), the receiver reads once
// (after observing SENT). The `Mutex<Option<Waker>>` guards the waker.
unsafe impl<T: Send> Send for Inner<T> {}
unsafe impl<T: Send> Sync for Inner<T> {}

impl<T> Inner<T> {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(EMPTY),
            value: UnsafeCell::new(None),
            waker: Mutex::new(None),
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Error returned when a `Receiver` future resolves without a value.
#[derive(Debug, PartialEq, Eq)]
pub enum RecvError {
    /// The `Sender` was dropped without calling `send`.
    Closed,
}

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvError::Closed => f.write_str("oneshot channel closed without a value"),
        }
    }
}

impl std::error::Error for RecvError {}

/// Create a new one-shot channel, returning `(Sender, Receiver)`.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Inner::new());
    (Sender { inner: inner.clone(), sent: false }, Receiver { inner })
}

// ── Sender ────────────────────────────────────────────────────────────────────

/// Sending half of a one-shot channel. Consumed on `send`.
pub struct Sender<T> {
    inner: Arc<Inner<T>>,
    /// Guards against accidental double-send through raw pointer tricks.
    sent: bool,
}

impl<T> Sender<T> {
    /// Send `value` to the receiver. Consumes `self`.
    ///
    /// Returns `Err(value)` if the receiver has already been dropped.
    pub fn send(mut self, value: T) -> Result<(), T> {
        // Write the value before transitioning state so the receiver always
        // sees a fully initialized `Option<T>` when it observes `SENT`.
        //
        // SAFETY: We hold exclusive write rights while state == EMPTY.
        // The CAS below succeeds only once; no other thread writes here.
        unsafe { *self.inner.value.get() = Some(value) };

        match self.inner.state.compare_exchange(
            EMPTY,
            SENT,
            Ordering::Release,  // publish the write above
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                self.sent = true;
                // Wake the receiver if it registered a waker.
                if let Some(w) = self.inner.waker.lock().unwrap().take() {
                    w.wake();
                }
                Ok(())
            }
            Err(_) => {
                // Receiver already dropped (state == CLOSED) — reclaim value.
                // SAFETY: We just wrote it above and the CAS failed, so
                // the receiver will never read it.
                let val = unsafe { (*self.inner.value.get()).take() }.unwrap();
                Err(val)
            }
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.sent {
            return; // value already transferred
        }
        // Signal the receiver that no value is coming.
        let prev = self.inner.state.swap(CLOSED, Ordering::Release);
        if prev == EMPTY {
            if let Some(w) = self.inner.waker.lock().unwrap().take() {
                w.wake();
            }
        }
    }
}

// ── Receiver ──────────────────────────────────────────────────────────────────

/// Receiving half of a one-shot channel. Implements `Future`.
pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, RecvError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let state = self.inner.state.load(Ordering::Acquire);
        match state {
            SENT => {
                // SAFETY: state == SENT guarantees the sender wrote the value
                // and will not write again. We are the sole reader (Receiver
                // is not Clone), so `take` is safe.
                let val = unsafe { (*self.inner.value.get()).take() }
                    .expect("oneshot: SENT state but value is None (logic error)");
                Poll::Ready(Ok(val))
            }
            CLOSED => Poll::Ready(Err(RecvError::Closed)),
            _ => {
                // EMPTY — register waker and yield.
                *self.inner.waker.lock().unwrap() = Some(cx.waker().clone());
                // Re-check state after registering to avoid lost wake.
                let state2 = self.inner.state.load(Ordering::Acquire);
                if state2 == SENT {
                    // SAFETY: same as above — SENT, sole reader.
                    let val = unsafe { (*self.inner.value.get()).take() }
                        .expect("oneshot: SENT but value None after re-check");
                    Poll::Ready(Ok(val))
                } else if state2 == CLOSED {
                    Poll::Ready(Err(RecvError::Closed))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Inform the sender (if still alive) that nobody will read the value.
        // CAS EMPTY → CLOSED; if already SENT we just leave the value to be
        // dropped when `inner` is freed.
        let _ = self.inner.state.compare_exchange(
            EMPTY,
            CLOSED,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{block_on, block_on_with_spawn, spawn};

    #[test]
    fn send_then_recv() {
        let result = block_on(async {
            let (tx, rx) = channel::<u32>();
            tx.send(42).unwrap();
            rx.await
        });
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn recv_then_send_via_spawn() {
        let result = block_on_with_spawn(async {
            let (tx, rx) = channel::<String>();
            let jh = spawn(async move {
                tx.send("hello".to_string()).unwrap();
            });
            let val = rx.await.unwrap();
            jh.await.unwrap();
            val
        });
        assert_eq!(result, "hello");
    }

    #[test]
    fn sender_drop_closes_channel() {
        let result = block_on(async {
            let (tx, rx) = channel::<u32>();
            drop(tx);
            rx.await
        });
        assert_eq!(result, Err(RecvError::Closed));
    }

    #[test]
    fn send_after_receiver_drop_returns_err() {
        let (tx, rx) = channel::<u32>();
        drop(rx);
        assert!(tx.send(1).is_err());
    }

    #[test]
    fn value_types_roundtrip() {
        block_on(async {
            let (tx, rx) = channel::<Vec<u8>>();
            tx.send(vec![1, 2, 3]).unwrap();
            assert_eq!(rx.await.unwrap(), vec![1, 2, 3]);
        });
    }
}
