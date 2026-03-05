# Project Changelog

All notable changes to Moduvex are documented in this file.
Format: [SemVer](https://semver.org/) | [Keep a Changelog](https://keepachangelog.com/)

---

## [0.1.0] — 2025-02-28

### Initial Release (MVP)

**Status:** Production-ready MVP with custom async runtime, HTTP/1.1 server, module system, and PostgreSQL support.

#### Added

**Framework Core**
- Type-state builder pattern for compile-time dependency validation
- 7-phase deterministic lifecycle (Config → Validate → Init → Start → Ready → Stopping → Stopped)
- `ModuvexError` with 4-variant classification (Domain/Infra/Config/Lifecycle)
- Error context chaining via `.context()` extension trait
- `AppContext` singleton storage (Arc<T> by type)
- Per-request scoped injection via `RequestContext`

**Custom Async Runtime**
- Platform-native I/O multiplexing:
  - Linux: epoll
  - macOS/BSD: kqueue
  - Windows: IOCP (stub implementation)
- Thread-per-core executor (work-stealing deferred)
- Hierarchical timer wheel (O(1) insert/fire)
- Async networking: TCP, UDP sockets
- Sync primitives: Mutex, mpsc, oneshot channels
- Unix signal handling (SIGTERM, SIGINT)
- Task-local storage (`TaskLocal<T>`)

**HTTP/1.1 Server**
- Zero-copy request parsing
- Radix tree routing with path parameter extraction (`:id` pattern)
- Request extractors: `Path<T>`, `Query<T>`, `Json<T>`, `State<T>`
- Response builder with status, headers, body
- `IntoResponse` trait for type-to-response conversion
- Middleware pipeline (pre/post handler)
- Keep-alive connection management
- HTTP/1.1 semantics (Request-Response cycle)

**PostgreSQL Database Client**
- Wire protocol implementation (MD5 authentication)
- Async connection pool with LIFO idle list
- Semaphore-bounded acquire (concurrent limit enforcement)
- Configurable timeouts (connect, idle)
- Health monitor (periodic connectivity check)
- Query builder with fluent API:
  - SELECT with column projection
  - WHERE equality clauses
  - ORDER BY (ASC/DESC)
  - LIMIT
- Parameterized queries (SQL-injection safe)
- Transaction support (BEGIN/COMMIT/ROLLBACK)
- Transaction isolation levels (Serializable, RepeatableRead, ReadCommitted, ReadUncommitted)
- Auto-rollback on Drop (RAII pattern)
- File-based migration engine (up-only, version-tracked)

**Configuration System**
- TOML-based configuration loading
- Profile support (dev, test, prod)
- Environment variable overrides (`MODUVEX__*`)
- Per-module scoped config sections
- Merge priority: env vars > profile overlay > base file > defaults
- Embedded defaults support (for starter crates)

**Observability**
- Structured logging (Event with level, message, key-value fields)
- Log formatters: pretty (human-readable), JSON
- Distributed tracing (TraceId, SpanId, SpanContext)
- Metrics (Counter, Gauge, Histogram — lock-free via atomics)
- Health checks (sync + async, composite status)
- Prometheus exporter (text format)
- Convenience macros: `info!()`, `warn!()`, `error!()`, `debug!()`

**Proc Macros**
- `#[derive(Module)]` — Generate Module + DependsOn traits
- `#[derive(Component)]` — Generate Inject + Provider for DI fields
- `#[derive(DomainError)]` — Generate DomainError with HTTP status mapping
- `#[derive(InfraError)]` — Generate InfraError with retryability
- `#[moduvex::main]` — Entry point macro (runtime setup)

**Starter Crates**
- `moduvex-starter-web` — One-dependency web framework (HTTP + observe)
- `moduvex-starter-data` — One-dependency data service (DB + config)

**Documentation**
- Comprehensive README (overview, quick start)
- API documentation (all public types)
- Per-crate documentation in lib.rs
- Code examples in docstrings

#### Published

- ✓ moduvex-runtime v0.1.0
- ✓ moduvex-macros v0.1.0
- ✓ moduvex-config v0.1.0
- ✓ moduvex-core v0.1.0
- ✓ moduvex-http v0.1.0

#### Tests

- 373+ test cases
- ~75% line coverage
- Unit tests per public function
- Integration tests for cross-module interactions

#### Known Issues (Fixed Post-0.1.0)

| Issue | Severity | Status |
|-------|----------|--------|
| Unsafe code blocks lack SAFETY comments | HIGH | ✓ Fixed in review |
| Error context chaining not exported | MEDIUM | ✓ Fixed in review |
| Health check types not public | MEDIUM | ✓ Fixed in review |
| Config validation incomplete | MEDIUM | ✓ Fixed in review |
| Module circular dependency detection missing | HIGH | ✓ Fixed in review |

#### Known Limitations

- **HTTP:** HTTP/1.1 only (HTTP/2 deferred to Phase B)
- **Database:** PostgreSQL only (MySQL/SQLite deferred)
- **Auth:** MD5 password auth only (SCRAM-SHA-256 deferred to Phase A)
- **TLS:** Not yet supported (deferred to Phase A)
- **Concurrency:** Thread-per-core only (work-stealing deferred to Phase B)
- **Config:** TOML only (YAML/JSON deferred)
- **Handlers:** Async-only (sync blocking not supported)
- **Modules:** No circular dependencies allowed (compile-time DAG validation)

---

## [0.2.0] — TBD (Phase A + Phase B)

### Planned: Security & Performance

#### Planned Additions

**Security (Phase A)**
- SCRAM-SHA-256 authentication (replaces MD5)
- TLS support (feature-gated)
- Security audit (internal)

**Performance (Phase B)**
- Work-stealing scheduler
- HTTP/2 support
- Benchmark suite
- Connection pool optimization

---

## [1.0.0] — TBD (Final Release)

### Planned: Stability & Production

#### Planned Stabilization

- API stability guarantee
- All 10 crates published
- External security audit
- ≥85% test coverage
- Production deployments active
- Community adoption

---

## Version Information

| Version | Release Date | Status | Min Rust |
|---------|--------------|--------|----------|
| 0.1.0 | 2025-02-28 | Current | 1.80+ |
| 0.2.0 | TBD | Planned | 1.80+ |
| 1.0.0 | TBD | Planned | 1.80+ |

---

## Semantic Versioning Policy

### Pre-1.0 (0.x.y)

- **0.x.0** → Minor breaking changes allowed, deprecation warnings provided
- **0.x.y** → Bug fixes, non-breaking enhancements
- Features may be incomplete or experimental

### Post-1.0 (x.y.z)

- **x.0.0** → Major breaking changes (SemVer major)
- **x.y.0** → New features, no breaking changes (SemVer minor)
- **x.y.z** → Bug fixes only (SemVer patch)

---

## Backward Compatibility

### 0.1.x

No backward compatibility guarantee — API may change before 1.0.

### 1.0+

- Stable API
- 2-release deprecation period before removal
- Breaking changes only in major versions

---

## Dependency Changes

### 0.1.0

**Runtime Dependencies:**
- Platform-specific: `libc` (Unix), `windows-sys` (Windows)
- Config: `serde`, `toml`
- Macros: `proc-macro2`, `quote`, `syn`
- **Zero async runtime deps** (custom implementation only)

### Planned Additions

- Phase A: `sha2`, `hmac`, `base64ct` (SCRAM-SHA-256), `rustls` (TLS)
- Phase C: Possible metrics aggregation crate
- Phase E: Middleware ecosystem crates (CORS, rate-limiting, etc.)

---

## Community & Support

- **Issue Tracker:** https://github.com/Moduvex/moduvex/issues
- **Discussions:** https://github.com/Moduvex/moduvex/discussions
- **License:** MIT OR Apache-2.0
- **Code of Conduct:** [Contributor Covenant](https://www.contributor-covenant.org/)

---

## Changelog Entry Template

For future contributors:

```markdown
### Added
- Brief description of new feature

### Changed
- Breaking changes with migration guide

### Fixed
- Bug fix with issue reference (#NNN)

### Deprecated
- API to be removed in future version

### Removed
- Breaking removals (major version only)

### Security
- Security fix with CVE reference (if applicable)
```

---

**Last Updated:** Phase 8 (Documentation)
**Maintained By:** Moduvex Team
**Next Update:** Upon 0.2.0 release or Phase A completion
