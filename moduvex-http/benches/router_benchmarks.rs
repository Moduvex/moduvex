use criterion::{criterion_group, criterion_main, Criterion};
use moduvex_http::request::Request;
use moduvex_http::response::Response;
use moduvex_http::routing::method::Method;
use moduvex_http::routing::router::Router;
use moduvex_http::status::StatusCode;

async fn noop(_req: Request) -> Response {
    Response::new(StatusCode::OK)
}

fn make_router() -> Router {
    Router::new()
        .get("/", noop)
        .get("/users", noop)
        .get("/users/:id", noop)
        .get("/users/:id/posts", noop)
        .get("/users/:id/posts/:post_id", noop)
        .post("/users", noop)
        .get("/api/v1/health", noop)
        .get("/static/*path", noop)
}

fn bench_static_route_lookup(c: &mut Criterion) {
    let router = make_router();
    c.bench_function("router: static /users", |b| {
        b.iter(|| router.lookup(Method::GET, "/users"));
    });
}

fn bench_param_route_lookup(c: &mut Criterion) {
    let router = make_router();
    c.bench_function("router: param /users/:id", |b| {
        b.iter(|| router.lookup(Method::GET, "/users/42"));
    });
}

fn bench_nested_param_lookup(c: &mut Criterion) {
    let router = make_router();
    c.bench_function("router: nested /users/:id/posts/:post_id", |b| {
        b.iter(|| router.lookup(Method::GET, "/users/42/posts/99"));
    });
}

fn bench_wildcard_lookup(c: &mut Criterion) {
    let router = make_router();
    c.bench_function("router: wildcard /static/*path", |b| {
        b.iter(|| router.lookup(Method::GET, "/static/css/app.css"));
    });
}

fn bench_miss_lookup(c: &mut Criterion) {
    let router = make_router();
    c.bench_function("router: miss /not-found", |b| {
        b.iter(|| router.lookup(Method::GET, "/not-found"));
    });
}

criterion_group!(
    benches,
    bench_static_route_lookup,
    bench_param_route_lookup,
    bench_nested_param_lookup,
    bench_wildcard_lookup,
    bench_miss_lookup,
);
criterion_main!(benches);
