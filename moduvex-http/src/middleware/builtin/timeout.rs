//! Timeout middleware — abort handler if it takes too long.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::middleware::{Middleware, Next};
use crate::request::Request;
use crate::response::Response;
use crate::status::StatusCode;

// ── Timeout middleware ───────────────────────────────────────────────────────

/// Returns 408 Request Timeout if the handler exceeds the deadline.
pub struct Timeout {
    duration: Duration,
}

impl Timeout {
    /// Create a timeout middleware with the given duration.
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }

    /// Create from milliseconds.
    pub fn from_millis(ms: u64) -> Self {
        Self::new(Duration::from_millis(ms))
    }
}

impl Middleware for Timeout {
    fn handle(&self, req: Request, next: Next) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let duration = self.duration;
        Box::pin(async move {
            let handler_fut = next.run(req);
            let sleep_fut = moduvex_runtime::time::sleep(duration);

            // Race handler against sleep using a select combinator.
            match race(handler_fut, sleep_fut).await {
                RaceResult::First(resp) => resp,
                RaceResult::Second(()) => {
                    Response::with_body(StatusCode::REQUEST_TIMEOUT, "request timed out")
                        .content_type("text/plain; charset=utf-8")
                }
            }
        })
    }
}

// ── Minimal select/race combinator ───────────────────────────────────────────

enum RaceResult<A, B> {
    First(A),
    Second(B),
}

/// Poll two futures concurrently, returning whichever completes first.
async fn race<A, B>(a: A, b: B) -> RaceResult<A::Output, B::Output>
where
    A: Future,
    B: Future,
{
    use std::pin::pin;

    let mut a = pin!(a);
    let mut b = pin!(b);

    std::future::poll_fn(|cx: &mut Context<'_>| {
        if let Poll::Ready(v) = a.as_mut().poll(cx) {
            return Poll::Ready(RaceResult::First(v));
        }
        if let Poll::Ready(v) = b.as_mut().poll(cx) {
            return Poll::Ready(RaceResult::Second(v));
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_creates_with_millis() {
        let t = Timeout::from_millis(5000);
        assert_eq!(t.duration, Duration::from_millis(5000));
    }
}
