//! Stress test: executor under heavy task load.
//!
//! Run with: `cargo test -p moduvex-runtime -- --ignored stress`

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use moduvex_runtime::executor::block_on_multi;
use moduvex_runtime::spawn;

#[test]
#[ignore = "stress test — run with --ignored"]
fn ten_thousand_tasks_all_complete() {
    const N: u64 = 10_000;
    let counter = Arc::new(AtomicU64::new(0));

    block_on_multi(
        {
            let counter = counter.clone();
            async move {
                let mut handles = Vec::with_capacity(N as usize);
                for _ in 0..N {
                    let c = counter.clone();
                    handles.push(spawn(async move {
                        c.fetch_add(1, Ordering::Relaxed);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            }
        },
        8,
    );

    assert_eq!(counter.load(Ordering::SeqCst), N);
}

#[test]
#[ignore = "stress test — run with --ignored"]
fn work_stealing_contention_8_workers() {
    const N: u64 = 5_000;
    let counter = Arc::new(AtomicU64::new(0));

    block_on_multi(
        {
            let counter = counter.clone();
            async move {
                let mut handles = Vec::with_capacity(N as usize);
                for i in 0..N {
                    let c = counter.clone();
                    handles.push(spawn(async move {
                        // Simulate varying work to provoke stealing.
                        let mut acc = 0u64;
                        for j in 0..(i % 100) {
                            acc = acc.wrapping_add(j);
                        }
                        c.fetch_add(1, Ordering::Relaxed);
                        std::hint::black_box(acc);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            }
        },
        8,
    );

    assert_eq!(counter.load(Ordering::SeqCst), N);
}

#[test]
#[ignore = "stress test — run with --ignored"]
fn nested_spawns_deep() {
    const DEPTH: u32 = 100;

    let result = block_on_multi(
        async {
            fn recurse(depth: u32) -> moduvex_runtime::JoinHandle<u32> {
                spawn(async move {
                    if depth == 0 {
                        0
                    } else {
                        let inner = recurse(depth - 1);
                        inner.await.unwrap() + 1
                    }
                })
            }
            recurse(DEPTH).await.unwrap()
        },
        4,
    );

    assert_eq!(result, DEPTH);
}
