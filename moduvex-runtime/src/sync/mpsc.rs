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

    // ── Additional MPSC tests ──────────────────────────────────────────────

    #[test]
    fn bounded_capacity_1_sequential_sends() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(1);
            for i in 0..5u32 {
                tx.send(i).await.unwrap();
                assert_eq!(rx.recv().await, Some(i));
            }
        });
    }

    #[test]
    fn bounded_clone_increments_sender_count() {
        block_on(async {
            let (tx, mut rx) = channel::<u32>(4);
            let tx2 = tx.clone();
            tx.send(1).await.unwrap();
            tx2.send(2).await.unwrap();
            drop(tx);
            assert_eq!(rx.recv().await, Some(1));
            assert_eq!(rx.recv().await, Some(2));
            // tx2 still alive — channel not yet closed
            drop(tx2);
            assert_eq!(rx.recv().await, None); // now closed
        });
    }

    #[test]
    fn unbounded_stress_100_msgs() {
        block_on_with_spawn(async {
            let (tx, mut rx) = unbounded::<u32>();
            let jh = spawn(async move {
                for i in 0..100u32 {
                    tx.send(i).unwrap();
                }
            });
            jh.await.unwrap();
            let mut count = 0u32;
            while let Some(v) = rx.recv().await {
                assert_eq!(v, count);
                count += 1;
            }
            assert_eq!(count, 100);
        });
    }

    #[test]
    fn bounded_send_future_drop_cleans_waker() {
        block_on(async {
            let (tx, rx) = channel::<u32>(1);
            tx.send(1).await.unwrap(); // fill
            // Create send future but drop it without polling
            let fut = tx.send(2);
            drop(fut); // must not panic or corrupt waker list
            drop(rx);
        });
    }

    #[test]
    fn bounded_multiple_senders_all_items_received() {
        block_on_with_spawn(async {
            let (tx1, mut rx) = channel::<u32>(16);
            let tx2 = tx1.clone();
            let tx3 = tx2.clone();
            let jh1 = spawn(async move {
                for i in 0..3u32 {
                    tx1.send(i).await.unwrap();
                }
            });
            let jh2 = spawn(async move {
                for i in 10..13u32 {
                    tx2.send(i).await.unwrap();
                }
            });
            let jh3 = spawn(async move {
                for i in 20..23u32 {
                    tx3.send(i).await.unwrap();
                }
            });
            jh1.await.unwrap();
            jh2.await.unwrap();
            jh3.await.unwrap();
            // Collect exactly 9 items (3 per sender × 3 senders)
            let mut vals: Vec<u32> = Vec::new();
            for _ in 0..9 {
                if let Some(v) = rx.recv().await {
                    vals.push(v);
                }
            }
            vals.sort();
            assert_eq!(vals, vec![0, 1, 2, 10, 11, 12, 20, 21, 22]);
        });
    }

    #[test]
    fn unbounded_capacity_is_unlimited() {
        block_on(async {
            let (tx, mut rx) = unbounded::<u32>();
            // Send 500 items without consuming — should never block
            for i in 0..500u32 {
                tx.send(i).unwrap();
            }
            for i in 0..500u32 {
                assert_eq!(rx.recv().await, Some(i));
            }
        });
    }

    #[test]
    fn bounded_receiver_drop_mid_queue() {
        block_on(async {
            let (tx, rx) = channel::<u32>(4);
            tx.send(1).await.unwrap();
            tx.send(2).await.unwrap();
            drop(rx); // items queued but receiver dropped
            // Subsequent send must return Err immediately
            let result = tx.send(3).await;
            assert!(result.is_err());
        });
    }

    #[test]
    fn bounded_channel_capacity_max_1_enforced() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(1);
            tx.send(10).await.unwrap();
            let tx2 = tx.clone();
            let jh = spawn(async move {
                // This should block until rx consumes the first item
                tx2.send(20).await.unwrap();
            });
            // Consume first
            let v = rx.recv().await.unwrap();
            assert_eq!(v, 10);
            jh.await.unwrap();
            let v2 = rx.recv().await.unwrap();
            assert_eq!(v2, 20);
        });
    }

    #[test]
    fn bounded_channel_string_type() {
        block_on(async {
            let (tx, mut rx) = channel::<String>(4);
            tx.send("hello".to_string()).await.unwrap();
            tx.send("world".to_string()).await.unwrap();
            drop(tx);
            assert_eq!(rx.recv().await, Some("hello".to_string()));
            assert_eq!(rx.recv().await, Some("world".to_string()));
            assert_eq!(rx.recv().await, None);
        });
    }

    #[test]
    fn unbounded_clone_sender_count() {
        block_on(async {
            let (tx, mut rx) = unbounded::<u32>();
            let tx2 = tx.clone();
            let tx3 = tx2.clone();
            tx.send(1).unwrap();
            tx2.send(2).unwrap();
            tx3.send(3).unwrap();
            drop(tx);
            drop(tx2);
            assert_eq!(rx.recv().await, Some(1));
            assert_eq!(rx.recv().await, Some(2));
            assert_eq!(rx.recv().await, Some(3));
            // tx3 still alive
            drop(tx3);
            assert_eq!(rx.recv().await, None);
        });
    }

    #[test]
    fn bounded_capacity_2_allows_2_sends_before_blocking() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(2);
            // Both of these should succeed immediately without blocking
            tx.send(1).await.unwrap();
            tx.send(2).await.unwrap();
            // These should be buffered
            assert_eq!(rx.recv().await, Some(1));
            assert_eq!(rx.recv().await, Some(2));
        });
    }

    #[test]
    fn unbounded_receiver_close_mid_batch() {
        block_on(async {
            let (tx, rx) = unbounded::<u32>();
            // Send several items
            for i in 0..5 {
                tx.send(i).unwrap();
            }
            drop(rx);
            // Subsequent sends must fail
            assert!(tx.send(99).is_err());
        });
    }

    #[test]
    fn bounded_channel_capacity_10_fills_before_block() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(10);
            // Fill all 10 slots
            for i in 0..10u32 {
                tx.send(i).await.unwrap();
            }
            // Drain all
            for i in 0..10u32 {
                assert_eq!(rx.recv().await, Some(i));
            }
        });
    }

    #[test]
    fn bounded_single_item_channel_send_recv_alternating() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(1);
            for i in 0..10u32 {
                tx.send(i * 2).await.unwrap();
                let v = rx.recv().await.unwrap();
                assert_eq!(v, i * 2);
            }
        });
    }

    #[test]
    fn unbounded_send_err_value_preserves_original() {
        let (tx, rx) = unbounded::<String>();
        drop(rx);
        let original = "test_value".to_string();
        let result = tx.send(original.clone());
        assert_eq!(result, Err(original));
    }

    #[test]
    fn bounded_send_err_value_preserves_original() {
        block_on(async {
            let (tx, rx) = channel::<String>(4);
            drop(rx);
            let original = "test".to_string();
            let result = tx.send(original.clone()).await;
            assert_eq!(result, Err(original));
        });
    }

    #[test]
    fn bounded_three_senders_one_receiver_pipelining() {
        block_on_with_spawn(async {
            let (tx, mut rx) = channel::<u32>(3);
            let tx2 = tx.clone();
            let tx3 = tx.clone();
            // Send 1 each from 3 senders (within capacity)
            tx.send(100).await.unwrap();
            tx2.send(200).await.unwrap();
            tx3.send(300).await.unwrap();
            let mut results = vec![
                rx.recv().await.unwrap(),
                rx.recv().await.unwrap(),
                rx.recv().await.unwrap(),
            ];
            results.sort();
            assert_eq!(results, vec![100, 200, 300]);
        });
    }

    #[test]
    fn bounded_channel_preserves_ordering() {
        block_on(async {
            let (tx, mut rx) = channel::<u32>(5);
            for i in 0..5u32 {
                tx.send(i * 10).await.unwrap();
            }
            for i in 0..5u32 {
                assert_eq!(rx.recv().await, Some(i * 10));
            }
        });
    }

    #[test]
    fn unbounded_immediately_closed_channel() {
        block_on(async {
            let (tx, rx) = unbounded::<u32>();
            drop(tx);
            drop(rx);
            // Just verifies no panic on immediate close
        });
    }

    #[test]
    fn bounded_immediately_closed_channel() {
        block_on(async {
            let (tx, rx) = channel::<u32>(1);
            drop(tx);
            drop(rx);
            // Just verifies no panic on immediate close
        });
    }

    #[test]
    fn unbounded_send_option_type() {
        block_on(async {
            let (tx, mut rx) = unbounded::<Option<u32>>();
            tx.send(Some(42)).unwrap();
            tx.send(None).unwrap();
            drop(tx);
            assert_eq!(rx.recv().await, Some(Some(42)));
            assert_eq!(rx.recv().await, Some(None));
            assert_eq!(rx.recv().await, None);
        });
    }
}
