# Moduvex

**Structure before scale.** A structured backend framework for Rust — an Application Platform Runtime with a full custom stack (runtime → HTTP → framework). Zero 3rd-party async runtime dependencies.

## Overview

Moduvex is a modular backend framework built from scratch in Rust, featuring:

- **Custom async runtime** — epoll/kqueue/IOCP, hybrid threading, no tokio/mio
- **HTTP/1.1 server** — zero-copy parser, router with path params, keep-alive
- **Type-state DI** — compile-time dependency injection, near-zero runtime cost
- **Module system** — lifecycle management, dependency graph, proof-witness pattern
- **Database layer** — PostgreSQL wire protocol, connection pool, migrations
- **Observability** — structured logging, tracing spans, lock-free metrics, health checks
- **Config** — TOML-based, profile overlays, env var overrides, per-module scoping
- **Proc macros** — `#[derive(Module)]`, `#[derive(Component)]`, `#[moduvex::main]`

## Quick Start

```toml
# Cargo.toml
[dependencies]
moduvex-starter-web = "0.1"
```

```rust
use moduvex_starter_web::prelude::*;

#[moduvex::main]
async fn main() {
    info!("Starting server");
    Moduvex::new()
        .module::<HelloModule>()
        .run()
        .await;
}
```

## Workspace Crates

| Crate | Description |
|-------|-------------|
| `moduvex-runtime` | Custom async runtime (executor, reactor, timers, networking, sync primitives) |
| `moduvex-http` | HTTP/1.1 server (parser, router, response builder) |
| `moduvex-core` | Framework core (DI container, module system, lifecycle engine) |
| `moduvex-macros` | Proc macros (`#[derive(Module)]`, `#[moduvex::main]`, etc.) |
| `moduvex-config` | Typed TOML config with profiles and env var overrides |
| `moduvex-db` | PostgreSQL client (wire protocol, connection pool, migrations) |
| `moduvex-observe` | Observability (logging, tracing, metrics, health checks) |
| `moduvex-starter-web` | Web starter — bundles runtime + HTTP + config + observe |
| `moduvex-starter-data` | Data starter — bundles runtime + DB + config |
| `moduvex` | Umbrella crate — re-exports everything with feature flags |

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

## Requirements

- **Rust:** 1.80+ (MSRV)
- **Edition:** 2021
- **Platforms:** Linux (epoll), macOS (kqueue), Windows (IOCP stub)

## Building

```bash
cargo build --workspace
cargo test --workspace
```

## License

MIT OR Apache-2.0
