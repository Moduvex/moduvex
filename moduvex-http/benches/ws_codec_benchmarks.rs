use criterion::{criterion_group, criterion_main, Criterion};
use moduvex_http::websocket::frame::{decode_frame, encode_frame, Frame};

fn bench_encode_text_short(c: &mut Criterion) {
    let frame = Frame::text(b"hello world".to_vec());
    c.bench_function("ws encode: short text (11 bytes)", |b| {
        let mut buf = Vec::with_capacity(64);
        b.iter(|| {
            buf.clear();
            encode_frame(&frame, &mut buf);
        });
    });
}

fn bench_encode_binary_1kb(c: &mut Criterion) {
    let frame = Frame::binary(vec![0xAA; 1024]);
    c.bench_function("ws encode: binary 1KB", |b| {
        let mut buf = Vec::with_capacity(2048);
        b.iter(|| {
            buf.clear();
            encode_frame(&frame, &mut buf);
        });
    });
}

fn bench_decode_text_short(c: &mut Criterion) {
    let frame = Frame::text(b"hello world".to_vec());
    let mut encoded = Vec::new();
    encode_frame(&frame, &mut encoded);
    c.bench_function("ws decode: short text (11 bytes)", |b| {
        b.iter(|| {
            let _ = decode_frame(&encoded);
        });
    });
}

fn bench_decode_binary_1kb(c: &mut Criterion) {
    let frame = Frame::binary(vec![0xAA; 1024]);
    let mut encoded = Vec::new();
    encode_frame(&frame, &mut encoded);
    c.bench_function("ws decode: binary 1KB", |b| {
        b.iter(|| {
            let _ = decode_frame(&encoded);
        });
    });
}

fn bench_roundtrip_text(c: &mut Criterion) {
    let payload = b"The quick brown fox jumps over the lazy dog".to_vec();
    c.bench_function("ws roundtrip: text encode+decode", |b| {
        let mut buf = Vec::with_capacity(128);
        b.iter(|| {
            buf.clear();
            let frame = Frame::text(payload.clone());
            encode_frame(&frame, &mut buf);
            let _ = decode_frame(&buf).unwrap();
        });
    });
}

criterion_group!(
    benches,
    bench_encode_text_short,
    bench_encode_binary_1kb,
    bench_decode_text_short,
    bench_decode_binary_1kb,
    bench_roundtrip_text,
);
criterion_main!(benches);
