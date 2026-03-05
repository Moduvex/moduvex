use criterion::{criterion_group, criterion_main, Criterion};
use moduvex_runtime::{block_on, block_on_with_spawn, spawn};

fn bench_block_on_baseline(c: &mut Criterion) {
    c.bench_function("block_on(async { 42 })", |b| {
        b.iter(|| block_on(async { 42u32 }));
    });
}

fn bench_spawn_join_10(c: &mut Criterion) {
    c.bench_function("spawn+join 10 tasks", |b| {
        b.iter(|| {
            block_on_with_spawn(async {
                let mut handles = Vec::with_capacity(10);
                for i in 0..10u32 {
                    handles.push(spawn(async move { i * i }));
                }
                for h in handles {
                    let _ = h.await;
                }
            });
        });
    });
}

fn bench_spawn_join_1000(c: &mut Criterion) {
    c.bench_function("spawn+join 1000 tasks", |b| {
        b.iter(|| {
            block_on_with_spawn(async {
                let mut handles = Vec::with_capacity(1000);
                for i in 0..1000u32 {
                    handles.push(spawn(async move { i * i }));
                }
                for h in handles {
                    let _ = h.await;
                }
            });
        });
    });
}

fn bench_multi_thread_spawn_join(c: &mut Criterion) {
    c.bench_function("multi-thread 4 workers, 100 tasks", |b| {
        b.iter(|| {
            moduvex_runtime::executor::block_on_multi(
                async {
                    let mut handles = Vec::with_capacity(100);
                    for i in 0..100u32 {
                        handles.push(spawn(async move { i * i }));
                    }
                    for h in handles {
                        let _ = h.await;
                    }
                },
                4,
            );
        });
    });
}

criterion_group!(
    benches,
    bench_block_on_baseline,
    bench_spawn_join_10,
    bench_spawn_join_1000,
    bench_multi_thread_spawn_join,
);
criterion_main!(benches);
