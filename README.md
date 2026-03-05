# Moduvex

**Structure before scale.** A structured backend framework for Rust — an Application Platform Runtime with a full custom stack (runtime → HTTP → framework). Zero 3rd-party async runtime dependencies.

[![crates.io](https://img.shields.io/crates/v/moduvex.svg)](https://crates.io/crates/moduvex)
[![docs.rs](https://docs.rs/moduvex/badge.svg)](https://docs.rs/moduvex)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.80-orange.svg)](https://www.rust-lang.org)

## Overview

Moduvex is a modular backend framework built from scratch in Rust, featuring:

- **Custom async runtime** — epoll/kqueue/WSAPoll, work-stealing executor, timers, signals
- **HTTP/1.1 + HTTP/2** (RFC 9113) with h2c cleartext support
- **TLS/HTTPS** via rustls (feature-gated, no OpenSSL)
- **Radix-tree router** with middleware pipeline, path params, wildcards
- **WebSocket** (RFC 6455) with frame fragmentation reassembly
- **Type-state DI** — compile-time dependency injection, near-zero runtime cost
- **Module system** — lifecycle management, dependency graph, proof-witness pattern
- **PostgreSQL** wire protocol, SCRAM-SHA-256 auth, connection pool, migrations
- **Observability** — structured logging (JSON), tracing spans, lock-free metrics, health checks
- **Distributed tracing** — W3C `traceparent` propagation
- **Config** — TOML-based, profile overlays, env var overrides, per-module scoping
- **Proc macros** — `#[derive(Module)]`, `#[derive(Component)]`, `#[moduvex::main]`

## Install

```bash
cargo add moduvex
```

Or in `Cargo.toml`:

```toml
[dependencies]
moduvex = "1.0"
```

## Quick Start

```rust
use moduvex::prelude::*;

#[moduvex::main]
async fn main() {
    info!("Starting server");
    Moduvex::new()
        .module::<HelloModule>()
        .run()
        .await;
}
```

## Feature Flags

| Flag | Description |
|------|-------------|
| `default` | `web` + `data` — everything included |
| `web` | HTTP server, router, middleware, WebSocket, static files |
| `data` | PostgreSQL connection pool, query builder |

## Workspace Crates

| Crate | Description |
|-------|-------------|
| [`moduvex`](https://crates.io/crates/moduvex) | Umbrella — one dep to rule them all |
| [`moduvex-runtime`](https://crates.io/crates/moduvex-runtime) | Custom async runtime (executor, reactor, timers, networking, sync primitives) |
| [`moduvex-http`](https://crates.io/crates/moduvex-http) | HTTP/1.1+2 server, radix router, WebSocket, TLS, static files |
| [`moduvex-core`](https://crates.io/crates/moduvex-core) | Framework core (DI container, module system, lifecycle engine) |
| [`moduvex-macros`](https://crates.io/crates/moduvex-macros) | Proc macros (`#[derive(Module)]`, `#[moduvex::main]`, etc.) |
| [`moduvex-config`](https://crates.io/crates/moduvex-config) | Typed TOML config with profiles and env var overrides |
| [`moduvex-db`](https://crates.io/crates/moduvex-db) | PostgreSQL client (wire protocol, connection pool, migrations) |
| [`moduvex-observe`](https://crates.io/crates/moduvex-observe) | Observability (logging, tracing, metrics, health checks) |
| [`moduvex-starter-web`](https://crates.io/crates/moduvex-starter-web) | Web starter (CORS, tracing middleware) |
| [`moduvex-starter-data`](https://crates.io/crates/moduvex-starter-data) | Data starter (DB integration) |

## Architecture

```
moduvex (umbrella)
├── moduvex-starter-web ─── moduvex-http ──┐
│                                             ├── moduvex-runtime
├── moduvex-starter-data ── moduvex-db ────┘
│                                             ├── moduvex-core ── moduvex-macros
├── moduvex-config                           │
└── moduvex-observe ────────────────────────┘
```

## Stats

- **~40K** lines of Rust
- **1,500+** tests passing
- **10** focused crates on [crates.io](https://crates.io/crates/moduvex)
- **0** async runtime dependencies

## Requirements

- **Rust:** 1.80+ (MSRV)
- **Edition:** 2021
- **Platforms:** Linux (epoll), macOS (kqueue), Windows (WSAPoll)

## Building

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## License

MIT OR Apache-2.0
