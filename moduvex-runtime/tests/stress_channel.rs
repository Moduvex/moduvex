//! Stress test: MPSC channel under high contention.
//!
//! Run with: `cargo test -p moduvex-runtime -- --ignored stress`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use moduvex_runtime::executor::block_on_multi;
use moduvex_runtime::spawn;
use moduvex_runtime::sync::mpsc;

#[test]
#[ignore = "stress test — run with --ignored"]
fn eight_senders_high_throughput() {
    const SENDERS: u64 = 8;
    const MSGS_PER_SENDER: u64 = 12_500;
    const TOTAL: u64 = SENDERS * MSGS_PER_SENDER;

    let sum = block_on_multi(
        async {
            let (tx, mut rx) = mpsc::unbounded::<u64>();
            for s in 0..SENDERS {
                let tx = tx.clone();
                spawn(async move {
                    for i in 0..MSGS_PER_SENDER {
                        tx.send(s * MSGS_PER_SENDER + i).unwrap();
                    }
                });
            }
            drop(tx);

            let mut count = 0u64;
            let mut sum = 0u64;
            while let Some(v) = rx.recv().await {
                sum = sum.wrapping_add(v);
                count += 1;
            }
            assert_eq!(count, TOTAL);
            sum
        },
        4,
    );

    // Sum of 0..TOTAL
    let expected: u64 = (0..TOTAL).fold(0u64, |a, b| a.wrapping_add(b));
    assert_eq!(sum, expected);
}

#[test]
#[ignore = "stress test — run with --ignored"]
fn bounded_backpressure() {
    const CAP: usize = 16;
    const MSGS: u64 = 10_000;

    let counter = Arc::new(AtomicU64::new(0));

    block_on_multi(
        {
            let counter = counter.clone();
            async move {
                let (tx, mut rx) = mpsc::channel::<u64>(CAP);

                let c = counter.clone();
                let receiver = spawn(async move {
                    while let Some(_v) = rx.recv().await {
                        c.fetch_add(1, Ordering::Relaxed);
                    }
                });

                // Send all messages; tx is dropped at end closing the channel.
                for i in 0..MSGS {
                    assert!(tx.send(i).await.is_ok());
                }
                drop(tx);

                receiver.await.unwrap();
            }
        },
        2,
    );

    assert_eq!(counter.load(Ordering::SeqCst), MSGS);
}

#[test]
#[ignore = "stress test — run with --ignored"]
fn many_oneshot_channels() {
    const N: usize = 5_000;

    block_on_multi(
        async {
            let mut handles = Vec::with_capacity(N);
            for i in 0..N {
                let (tx, rx) = moduvex_runtime::sync::oneshot::channel::<usize>();
                handles.push(spawn(async move {
                    tx.send(i).unwrap();
                }));
                handles.push(spawn(async move {
                    let v = rx.await.unwrap();
                    assert_eq!(v, i);
                }));
            }
            for h in handles {
                h.await.unwrap();
            }
        },
        4,
    );
}
