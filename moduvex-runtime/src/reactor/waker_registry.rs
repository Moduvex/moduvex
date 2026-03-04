//! Waker registry — maps reactor tokens to task wakers for I/O readiness.
//!
//! When the reactor fires an event for token T, this registry looks up the
//! stored waker and calls `wake()` to re-schedule the waiting task.
//!
//! Read-wakers and write-wakers are tracked separately because a single fd
//! may have independent read and write waiters (e.g. a TcpStream awaiting
//! both `readable()` and `writable()` concurrently from different tasks).

use std::collections::HashMap;
use std::task::Waker;

/// Registry mapping (token, direction) → Waker.
pub(crate) struct WakerRegistry {
    /// Wakers waiting for read-readiness, keyed by reactor token.
    read_wakers: HashMap<usize, Waker>,
    /// Wakers waiting for write-readiness, keyed by reactor token.
    write_wakers: HashMap<usize, Waker>,
}

impl WakerRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self {
            read_wakers: HashMap::new(),
            write_wakers: HashMap::new(),
        }
    }

    /// Store a waker for read-readiness on `token`.
    ///
    /// Replaces any previously stored read waker for the same token.
    pub(crate) fn set_read_waker(&mut self, token: usize, waker: Waker) {
        self.read_wakers.insert(token, waker);
    }

    /// Store a waker for write-readiness on `token`.
    ///
    /// Replaces any previously stored write waker for the same token.
    pub(crate) fn set_write_waker(&mut self, token: usize, waker: Waker) {
        self.write_wakers.insert(token, waker);
    }

    /// Remove and return the read waker for `token`, if any.
    pub(crate) fn take_read_waker(&mut self, token: usize) -> Option<Waker> {
        self.read_wakers.remove(&token)
    }

    /// Remove and return the write waker for `token`, if any.
    pub(crate) fn take_write_waker(&mut self, token: usize) -> Option<Waker> {
        self.write_wakers.remove(&token)
    }

    /// Remove all wakers for `token` (both read and write).
    ///
    /// Called when an `IoSource` is deregistered/dropped.
    pub(crate) fn remove_token(&mut self, token: usize) {
        self.read_wakers.remove(&token);
        self.write_wakers.remove(&token);
    }

    /// Wake all tasks registered for events on `token`.
    ///
    /// `readable` / `writable` flags come directly from the reactor event.
    /// Wakers are removed from the registry before being fired (one-shot).
    /// The caller is responsible for re-registering wakers after each fire.
    pub(crate) fn wake_token(&mut self, token: usize, readable: bool, writable: bool) {
        if readable {
            if let Some(w) = self.read_wakers.remove(&token) {
                w.wake();
            }
        }
        if writable {
            if let Some(w) = self.write_wakers.remove(&token) {
                w.wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::task::{RawWaker, RawWakerVTable, Waker};

    /// Build a trivial counting waker for testing.
    fn make_counting_waker(count: Arc<Mutex<usize>>) -> Waker {
        let data = Arc::into_raw(count) as *const ();

        unsafe fn clone_waker(ptr: *const ()) -> RawWaker {
            // Re-increment refcount.
            let arc = Arc::from_raw(ptr as *const Mutex<usize>);
            let _ = Arc::clone(&arc);
            let _ = Arc::into_raw(arc); // don't drop
            RawWaker::new(ptr, &VTABLE)
        }
        unsafe fn wake(ptr: *const ()) {
            let arc = Arc::from_raw(ptr as *const Mutex<usize>);
            *arc.lock().unwrap() += 1;
            // arc drops → refcount decrements
        }
        unsafe fn wake_by_ref(ptr: *const ()) {
            let arc = Arc::from_raw(ptr as *const Mutex<usize>);
            *arc.lock().unwrap() += 1;
            let _ = Arc::into_raw(arc); // keep alive
        }
        unsafe fn drop_waker(ptr: *const ()) {
            drop(Arc::from_raw(ptr as *const Mutex<usize>));
        }

        static VTABLE: RawWakerVTable =
            RawWakerVTable::new(clone_waker, wake, wake_by_ref, drop_waker);

        // SAFETY: vtable correctly implements the waker contract.
        unsafe { Waker::from_raw(RawWaker::new(data, &VTABLE)) }
    }

    #[test]
    fn set_and_wake_read_waker() {
        let count = Arc::new(Mutex::new(0usize));
        let waker = make_counting_waker(Arc::clone(&count));

        let mut reg = WakerRegistry::new();
        reg.set_read_waker(42, waker);
        reg.wake_token(42, true, false);

        assert_eq!(*count.lock().unwrap(), 1, "read waker must fire once");
    }

    #[test]
    fn wake_removes_waker_one_shot() {
        let count = Arc::new(Mutex::new(0usize));
        let waker = make_counting_waker(Arc::clone(&count));

        let mut reg = WakerRegistry::new();
        reg.set_read_waker(1, waker);
        reg.wake_token(1, true, false);
        // Second wake should be a no-op (waker already removed).
        reg.wake_token(1, true, false);

        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[test]
    fn remove_token_clears_both_directions() {
        let c1 = Arc::new(Mutex::new(0usize));
        let c2 = Arc::new(Mutex::new(0usize));

        let mut reg = WakerRegistry::new();
        reg.set_read_waker(5, make_counting_waker(Arc::clone(&c1)));
        reg.set_write_waker(5, make_counting_waker(Arc::clone(&c2)));
        reg.remove_token(5);

        reg.wake_token(5, true, true); // should be no-op now
        assert_eq!(*c1.lock().unwrap(), 0);
        assert_eq!(*c2.lock().unwrap(), 0);
    }
}
