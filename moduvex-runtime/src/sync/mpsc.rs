//! Multi-producer single-consumer (MPSC) channel.
//!
//! Provides both bounded and unbounded variants. Senders are `Clone`; the
//! Receiver is unique. Dropping all Senders closes the channel so `recv`
//! returns `None`.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

// ── Shared inner state ────────────────────────────────────────────────────────

struct Inner<T> {
    /// Buffered values awaiting consumption.
    queue: VecDeque<T>,
    /// Maximum number of items allowed when bounded (`None` = unbounded).
    capacity: Option<usize>,
    /// Number of live `Sender` handles (including `UnboundedSender`).
    sender_count: usize,
    /// Set to `true` when the `Receiver` is dropped.
    receiver_dropped: bool,
    /// Waker for the blocked receiver (empty queue).
    recv_waker: Option<Waker>,
    /// Wakers for blocked senders (full bounded queue).
    send_wakers: VecDeque<Waker>,
}

impl<T> Inner<T> {
    fn new(capacity: Option<usize>) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
            sender_count: 1,
            receiver_dropped: false,
            recv_waker: None,
            send_wakers: VecDeque::new(),
        }
    }

    /// True when the channel has room (or is unbounded).
    fn has_capacity(&self) -> bool {
        match self.capacity {
            None => true,
            Some(cap) => self.queue.len() < cap,
        }
    }

    /// True when all senders have been dropped.
    fn senders_closed(&self) -> bool {
        self.sender_count == 0
    }

    /// True when the channel is closed from either direction.
    fn is_closed(&self) -> bool {
        self.sender_count == 0 || self.receiver_dropped
    }
}

// ── Bounded channel ───────────────────────────────────────────────────────────

/// Create a bounded MPSC channel with the given `capacity`.
///
/// `Sender::send` will suspend if the buffer is full; it resumes once the
/// receiver has consumed an item.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Mutex::new(Inner::new(Some(capacity.max(1)))));
    (
        Sender {
            inner: inner.clone(),
        },
        Receiver { inner },
    )
}

/// Sending half of a bounded MPSC channel.
///
/// Cheap to clone; each clone shares the same channel.
pub struct Sender<T> {
    inner: Arc<Mutex<Inner<T>>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.inner.lock().unwrap().sender_count += 1;
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        g.sender_count -= 1;
        if g.sender_count == 0 {
            // Channel closed — wake the receiver so it can return `None`.
            if let Some(w) = g.recv_waker.take() {
                drop(g);
                w.wake();
            }
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        g.receiver_dropped = true;
        // Wake all blocked senders so they can observe the closed channel.
        let wakers: Vec<Waker> = g.send_wakers.drain(..).collect();
        drop(g);
        for w in wakers {
            w.wake();
        }
    }
}

impl<T> Sender<T> {
    /// Send `value` through the channel, waiting if the buffer is full.
    ///
    /// Returns `Err(value)` if the receiver has been dropped.
    pub fn send(&self, value: T) -> SendFuture<'_, T> {
        SendFuture {
            inner: &self.inner,
            value: Some(value),
            registered_waker: None,
        }
    }
}

/// Future returned by [`Sender::send`].
pub struct SendFuture<'a, T> {
    inner: &'a Arc<Mutex<Inner<T>>>,
    /// `None` after the value has been deposited.
    value: Option<T>,
    /// Waker we registered in `send_wakers`, stored for Drop cleanup.
    registered_waker: Option<Waker>,
}

impl<T> Future for SendFuture<'_, T> {
    type Output = Result<(), T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `SendFuture` contains only a shared reference, `Option<T>`,
        // and `Option<Waker>`. No structural pinning on T; safe to get &mut Self.
        let this = unsafe { self.get_unchecked_mut() };
        let mut g = this.inner.lock().unwrap();
        if g.is_closed() {
            this.registered_waker = None;
            return Poll::Ready(Err(this.value.take().unwrap()));
        }
        if g.has_capacity() {
            this.registered_waker = None;
            let val = this.value.take().unwrap();
            g.queue.push_back(val);
            if let Some(w) = g.recv_waker.take() {
                drop(g);
                w.wake();
            }
            Poll::Ready(Ok(()))
        } else {
            let new_waker = cx.waker().clone();
            if let Some(ref existing) = this.registered_waker {
                if !existing.will_wake(&new_waker) {
                    // Replace our stale waker in send_wakers.
                    for w in &mut g.send_wakers {
                        if w.will_wake(existing) {
                            *w = new_waker.clone();
                            break;
                        }
                    }
                    this.registered_waker = Some(new_waker);
                }
            } else {
                g.send_wakers.push_back(new_waker.clone());
                this.registered_waker = Some(new_waker);
            }
            Poll::Pending
        }
    }
}

impl<T> Drop for SendFuture<'_, T> {
    fn drop(&mut self) {
        if let Some(ref waker) = self.registered_waker {
            // Remove our waker from send_wakers to prevent orphaned wake-ups.
            if let Ok(mut g) = self.inner.lock() {
                if let Some(pos) = g.send_wakers.iter().position(|w| w.will_wake(waker)) {
                    g.send_wakers.remove(pos);
                }
            }
        }
    }
}

// ── Unbounded channel ─────────────────────────────────────────────────────────

/// Create an unbounded MPSC channel.
///
/// Sends never block; the buffer grows as needed.
pub fn unbounded<T>() -> (UnboundedSender<T>, Receiver<T>) {
    let inner = Arc::new(Mutex::new(Inner::new(None)));
    (
        UnboundedSender {
            inner: inner.clone(),
        },
        Receiver { inner },
    )
}

/// Sending half of an unbounded MPSC channel.
pub struct UnboundedSender<T> {
    inner: Arc<Mutex<Inner<T>>>,
}

impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        self.inner.lock().unwrap().sender_count += 1;
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for UnboundedSender<T> {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        g.sender_count -= 1;
        if g.sender_count == 0 {
            if let Some(w) = g.recv_waker.take() {
                drop(g);
                w.wake();
            }
        }
    }
}


impl<T> UnboundedSender<T> {
    /// Send `value` immediately (never suspends).
    ///
    /// Returns `Err(value)` if the receiver has been dropped.
    pub fn send(&self, value: T) -> Result<(), T> {
        let mut g = self.inner.lock().unwrap();
        if g.is_closed() {
            return Err(value);
        }
        g.queue.push_back(value);
        if let Some(w) = g.recv_waker.take() {
            drop(g);
            w.wake();
        }
        Ok(())
    }
}

// ── Receiver ──────────────────────────────────────────────────────────────────

/// Receiving half of either channel variant. Not `Clone`.
pub struct Receiver<T> {
    inner: Arc<Mutex<Inner<T>>>,
}

impl<T> Receiver<T> {
    /// Receive the next value, waiting if the buffer is empty.
    ///
    /// Returns `None` when the channel is empty and all senders have been
    /// dropped.
    pub fn recv(&mut self) -> RecvFuture<'_, T> {
        RecvFuture { inner: &self.inner }
    }
}

/// Future returned by [`Receiver::recv`].
pub struct RecvFuture<'a, T> {
    inner: &'a Arc<Mutex<Inner<T>>>,
}

impl<T> Future for RecvFuture<'_, T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut g = self.inner.lock().unwrap();
        if let Some(val) = g.queue.pop_front() {
            // Wake one blocked sender (bounded channel backpressure).
            if let Some(w) = g.send_wakers.pop_front() {
                drop(g);
                w.wake();
            }
            Poll::Ready(Some(val))
        } else if g.senders_closed() {
            Poll::Ready(None)
        } else {
            g.recv_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{block_on, block_on_with_spawn, spawn};

    #[test]
    fn bounded_send_recv_basic() {
        block_on(async {
            let (tx, mut rx) = channel::<u32>(4);
            tx.send(1).await.unwrap();
            tx.send(2).await.unwrap();
            assert_eq!(rx.recv().await, Some(1));
            assert_eq!(rx.recv().await, Some(2));
        });
    }

    #[test]
    fn bounded_channel_close_on_sender_drop() {
        block_on(async {
            let (tx, mut rx) = channel::<u32>(4);
            tx.send(42).await.unwrap();
            drop(tx);
            assert_eq!(rx.recv().await, Some(42));
            assert_eq!(rx.recv().await, None);
        });
    }

    #[test]
    fn unbounded_multi_producer() {
        block_on_with_spawn(async {
            let (tx1, mut rx) = unbounded::<u32>();
            let tx2 = tx1.clone();
            let jh1 = spawn(async move {
                tx1.send(10).unwrap();
            });
            let jh2 = spawn(async move {
                tx2.send(20).unwrap();
            });
            jh1.await.unwrap();
            jh2.await.unwrap();
            let mut vals = vec![rx.recv().await.unwrap(), rx.recv().await.unwrap()];
            vals.sort();
            assert_eq!(vals, vec![10, 20]);
        });
    }

    #[test]
    fn bounded_backpressure_unblocks_when_consumed() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(1);
            // Fill the channel
            tx.send(1).await.unwrap();
            // Spawn a producer that will block until we consume
            let jh = spawn(async move {
                tx.send(2).await.unwrap();
            });
            assert_eq!(rx.recv().await, Some(1));
            jh.await.unwrap();
            assert_eq!(rx.recv().await, Some(2));
        });
    }

    #[test]
    fn unbounded_close_returns_none() {
        block_on(async {
            let (tx, mut rx) = unbounded::<i32>();
            drop(tx);
            assert_eq!(rx.recv().await, None);
        });
    }

    #[test]
    fn bounded_send_to_closed_receiver_returns_err() {
        block_on(async {
            let (tx, rx) = channel::<u32>(4);
            drop(rx);
            // Receiver dropped — sender must get Err immediately.
            let result = tx.send(99).await;
            assert!(result.is_err());
            assert_eq!(result.unwrap_err(), 99);
        });
    }

    #[test]
    fn unbounded_send_to_closed_receiver_returns_err() {
        let (tx, rx) = unbounded::<u32>();
        drop(rx);
        assert_eq!(tx.send(42), Err(42));
    }

    #[test]
    fn bounded_blocked_sender_woken_on_receiver_drop() {
        block_on_with_spawn(async {
            // Channel capacity = 1, fill it, spawn a sender that will block
            let (tx, rx) = channel::<u32>(1);
            tx.send(1).await.unwrap();
            let tx2 = tx.clone();
            let jh = spawn(async move {
                // This send blocks because buffer is full; receiver drop should wake it.
                tx2.send(2).await
            });
            // Drop receiver — should unblock the sender with Err
            drop(tx);
            drop(rx);
            let result = jh.await.unwrap();
            assert!(result.is_err());
        });
    }
}
