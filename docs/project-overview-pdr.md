# Project Overview & Product Development Requirements

## Executive Summary

Moduvex is a **structured backend framework for Rust** — an Application Platform Runtime built entirely from scratch with zero external async runtime dependencies. It provides a complete custom stack: async runtime (epoll/kqueue/IOCP) → HTTP/1.1 server → module system with type-state dependency injection → PostgreSQL client.

**Current Version:** 0.1.0 (5 crates published to crates.io)
**License:** MIT OR Apache-2.0
**Repository:** https://github.com/Moduvex/moduvex
**Status:** Functional MVP, working toward 1.0 maturity

## Core Philosophy

- **"Structure before scale"** — modular, type-safe, compile-time validated
- **Zero external async runtime** — custom epoll/kqueue/IOCP eliminates tokio dependency
- **Compile-time safety** — type-state builder ensures valid module dependency graphs
- **Minimal boilerplate** — macros + starters make single-file services feasible

## Functional Requirements

### Framework Capabilities
- Custom async runtime with platform-native I/O multiplexing
- HTTP/1.1 server with zero-copy parsing and path parameter routing
- Type-state dependency injection with compile-time validation
- Module system with deterministic 7-phase lifecycle (Config → Validate → Init → Start → Ready → Stopping → Stopped)
- PostgreSQL wire protocol client with connection pooling
- Structured logging, distributed tracing, and Prometheus metrics
- TOML-based config with profile overlays and env var overrides
- Proc macros for module, component, and error derivation

### Non-Functional Requirements
- **MSRV:** Rust 1.80+
- **Edition:** 2021
- **Platforms:** Linux (epoll), macOS (kqueue), Windows (IOCP stub)
- **Thread Model:** Thread-per-core by default, opt-in work-stealing
- **Target Deployment:** Production web services, data processing pipelines
- **Zero Cost Abstractions:** DI container uses `Arc` clones (no `TypeId` lookup per request)

## Architecture Highlights

### Crate Dependency Layers

**Layer 1 (Foundation — no internal deps)**
- `moduvex-runtime`: Custom async runtime
- `moduvex-macros`: Proc macro definitions
- `moduvex-config`: Typed TOML config

**Layer 2 (Core + Services)**
- `moduvex-core`: DI, module system, lifecycle engine
- `moduvex-http`: HTTP/1.1 server
- `moduvex-observe`: Logging, metrics, health checks

**Layer 3 (Database)**
- `moduvex-db`: PostgreSQL client, pool, migrations

**Layer 4 (Starters)**
- `moduvex-starter-web`: Runtime + HTTP + Config + Observe (one dep for web apps)
- `moduvex-starter-data`: Runtime + DB + Config (one dep for data services)

**Layer 5 (Umbrella)**
- `moduvex`: Re-exports all sub-crates with feature flags

### Request Lifecycle
1. TCP accept on runtime reactor
2. HTTP parser (zero-copy) decodes request
3. Router matches path pattern + method
4. Middleware pipeline processes
5. Handler receives `Request` + extractors (Body, JSON, Path, Query, State)
6. Handler returns `Response` or error
7. Response serialized and sent via TCP
8. Connection kept alive or closed per HTTP semantics

## Acceptance Criteria for v1.0

### Code Quality
- [ ] All crates published to crates.io
- [ ] 373+ tests passing with 80%+ coverage
- [ ] Clippy warnings eliminated (only allowed lint overrides)
- [ ] Unsafe code audited and documented
- [ ] Error handling: all public APIs return `Result<T, ModuvexError>`

### Documentation
- [ ] API docs generated for all public types
- [ ] 6 core docs complete: overview, standards, architecture, summary, roadmap, changelog
- [ ] Examples for each crate
- [ ] Migration guides for breaking changes

### Stability
- [ ] No panic boundaries in hot paths
- [ ] Memory safety verified (miri checks pass)
- [ ] Performance baselines established
- [ ] Known limitations documented

## Known Limitations

### Current Version (0.1.x)
- **HTTP:** HTTP/1.1 only (HTTP/2 deferred to v1.1)
- **Security:** MD5 auth only, no TLS (SCRAM-SHA-256 auth in Phase 7)
- **Database:** PostgreSQL only, simple query protocol (no prepared statements yet)
- **Concurrency:** Single-threaded by default (work-stealing deferred)
- **Configuration:** TOML only (no YAML/JSON overlays)

### Design Constraints
- Request handlers are async (no sync handler support)
- Module dependencies must form a DAG (no cycles)
- Singletons are immutable (`Arc<T>`, no mutation)
- Per-request extraction only (no streaming request bodies)

## Success Metrics

| Metric | Target | Current |
|--------|--------|---------|
| Crates on crates.io | 10/10 | 5/10 |
| Test Coverage | 80%+ | ~75% |
| Benchmark Suite | Yes | Partial |
| Production Examples | 3+ | 0 |
| Community Issues | Avg <1wk response | N/A |
| Documentation % | 100% | ~60% |

## Roadmap Phases (A→G)

- **Phase A:** Security (SCRAM-SHA-256, TLS) — Current
- **Phase B:** Performance (work-stealing, HTTP/2) — Queued
- **Phase C:** Observability (enhanced metrics, spans) — Queued
- **Phase D:** Reliability (retries, circuit breakers) — Queued
- **Phase E:** Ecosystem (middleware library, templates) — Queued
- **Phase F:** Deployment (Docker, Kubernetes) — Queued
- **Phase G:** 1.0 Release — Target date TBD

## Dependencies (External)

### Runtime
- `libc` (Unix) — Platform abstractions
- `windows-sys` (Windows) — IOCP access

### Framework
- `serde`, `toml` — Config deserialization
- `sha2`, `hmac`, `base64ct` — Crypto (SCRAM-SHA-256)
- Proc-macro crates (`proc-macro2`, `quote`, `syn`) — Macro expansion

**Zero:**
- No async runtime (tokio, async-std)
- No HTTP crate (axum, actix-web)
- No database driver (sqlx, tokio-postgres)

## Contact & Support

- **Issues:** GitHub Issues @ https://github.com/Moduvex/moduvex
- **Discussions:** GitHub Discussions (TBD)
- **License Questions:** MIT OR Apache-2.0 (your choice)

---

**Last Updated:** Phase 8 (Documentation)
**Status:** Production-Ready MVP (0.1.0)
