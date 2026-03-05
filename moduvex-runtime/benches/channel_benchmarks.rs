use criterion::{criterion_group, criterion_main, Criterion};
use moduvex_runtime::block_on_with_spawn;
use moduvex_runtime::sync::mpsc;

fn bench_mpsc_unbounded_send_recv(c: &mut Criterion) {
    c.bench_function("mpsc unbounded 1000 send+recv", |b| {
        b.iter(|| {
            block_on_with_spawn(async {
                let (tx, mut rx) = mpsc::unbounded::<u32>();
                for i in 0..1000 {
                    tx.send(i).unwrap();
                }
                drop(tx);
                while rx.recv().await.is_some() {}
            });
        });
    });
}

fn bench_mpsc_unbounded_multi_sender(c: &mut Criterion) {
    c.bench_function("mpsc unbounded 4 senders × 250 msgs", |b| {
        b.iter(|| {
            block_on_with_spawn(async {
                let (tx, mut rx) = mpsc::unbounded::<u32>();
                for _ in 0..4 {
                    let tx = tx.clone();
                    moduvex_runtime::spawn(async move {
                        for i in 0..250 {
                            let _ = tx.send(i);
                        }
                    });
                }
                drop(tx);
                while rx.recv().await.is_some() {}
            });
        });
    });
}

fn bench_oneshot(c: &mut Criterion) {
    c.bench_function("oneshot send+recv", |b| {
        b.iter(|| {
            block_on_with_spawn(async {
                let (tx, rx) = moduvex_runtime::sync::oneshot::channel::<u32>();
                moduvex_runtime::spawn(async move {
                    let _ = tx.send(42);
                });
                let _ = rx.await;
            });
        });
    });
}

criterion_group!(
    benches,
    bench_mpsc_unbounded_send_recv,
    bench_mpsc_unbounded_multi_sender,
    bench_oneshot,
);
criterion_main!(benches);
