use criterion::{criterion_group, criterion_main, Criterion};
use moduvex_http::protocol::h1::parser::{parse_request_head, ParseLimits, ParseStatus};

const SIMPLE_GET: &[u8] = b"GET /users HTTP/1.1\r\nHost: localhost\r\n\r\n";

const COMPLEX_REQUEST: &[u8] = b"POST /api/v1/users HTTP/1.1\r\n\
Host: api.example.com\r\n\
Content-Type: application/json\r\n\
Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9\r\n\
Accept: application/json\r\n\
Accept-Encoding: gzip, deflate, br\r\n\
Accept-Language: en-US,en;q=0.9\r\n\
Cache-Control: no-cache\r\n\
Connection: keep-alive\r\n\
X-Request-Id: 018f3a2c1d4b-0001\r\n\
X-Forwarded-For: 192.168.1.100\r\n\
User-Agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)\r\n\
\r\n";

fn bench_parse_simple_get(c: &mut Criterion) {
    let limits = ParseLimits::default();
    c.bench_function("parse: simple GET", |b| {
        b.iter(|| {
            let result = parse_request_head(SIMPLE_GET, &limits);
            assert!(matches!(result, ParseStatus::Complete(_)));
        });
    });
}

fn bench_parse_complex_headers(c: &mut Criterion) {
    let limits = ParseLimits::default();
    c.bench_function("parse: 12-header POST", |b| {
        b.iter(|| {
            let result = parse_request_head(COMPLEX_REQUEST, &limits);
            assert!(matches!(result, ParseStatus::Complete(_)));
        });
    });
}

fn bench_parse_partial(c: &mut Criterion) {
    let limits = ParseLimits::default();
    let partial = &SIMPLE_GET[..10]; // Incomplete request
    c.bench_function("parse: partial (fast reject)", |b| {
        b.iter(|| {
            let result = parse_request_head(partial, &limits);
            assert!(matches!(result, ParseStatus::Partial));
        });
    });
}

criterion_group!(
    benches,
    bench_parse_simple_get,
    bench_parse_complex_headers,
    bench_parse_partial,
);
criterion_main!(benches);
