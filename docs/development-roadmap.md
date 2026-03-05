# Development Roadmap — Path to 1.0

## Current Status: 0.1.0 (MVP)

**Release Date:** Feb 2025
**Maturity Score:** 7/10
**Status:** Feature-complete MVP, working toward 1.0 stability

### What Shipped in 0.1.0

✓ Custom async runtime (epoll/kqueue/IOCP)
✓ HTTP/1.1 server with zero-copy parsing
✓ Type-state dependency injection
✓ Module system with deterministic lifecycle
✓ PostgreSQL wire protocol (MD5 auth)
✓ Connection pooling + migrations
✓ Structured logging + metrics + tracing
✓ TOML config with profiles
✓ Proc macros (Module, Component, DomainError, InfraError)
✓ 5 crates published to crates.io
✓ 373+ tests, ~75% coverage

### Known Issues (0.1.x)

| ID | Issue | Severity | Status |
|----|-------|----------|--------|
| C1 | Unsafe code undocumented | HIGH | ✓ Fixed |
| C2 | Error context chaining missing | MEDIUM | ✓ Fixed |
| C3 | Missing health check exports | MEDIUM | ✓ Fixed |
| C4 | Metrics lock contention | LOW | ✓ Fixed (atomics) |
| C5 | Config validation incomplete | MEDIUM | ✓ Fixed |
| C6 | Module dependency cycle detection | HIGH | ✓ Fixed |
| C7 | SCRAM-SHA-256 auth incomplete | HIGH | In Progress |

---

## Roadmap: Phases A → G

### Phase A: Security & Auth (Current, ETA: Mar 2025)

**Goal:** Complete SCRAM-SHA-256 auth, add TLS support, security audit.

**Requirements:**
- [ ] SCRAM-SHA-256 authentication (MD5 → SHA-256)
  - Add `sha2`, `hmac`, `base64ct` dependencies
  - Implement SCRAM exchange in PgConnection
  - Test with modern Postgres configurations
- [ ] TLS support (optional, feature-gated)
  - Integrate `rustls` or native-tls
  - Add cert validation + ALPN
  - Test with self-signed + CA-signed certs
- [ ] Security audit (internal)
  - Review all unsafe blocks
  - Check for buffer overflows, integer overflow
  - Validate crypto usage (constant-time comparisons)
- [ ] Documentation security guide
  - Best practices for secrets management
  - Recommended TLS configuration

**Deliverables:**
- moduvex-db v0.1.1 (SCRAM-SHA-256)
- moduvex-http v0.1.1 (TLS feature)
- Security documentation

**Success Criteria:**
- All tests pass with SCRAM auth
- TLS tests pass with ample certificate types
- Security audit findings resolved
- No clippy warnings

---

### Phase B: Performance & HTTP/2 (ETA: Apr 2025)

**Goal:** Multi-threaded scaling, HTTP/2 support, benchmarks.

**Requirements:**
- [ ] Work-stealing scheduler
  - Add work-stealing queue to executor
  - Implement per-thread steal attempts
  - Benchmarks: single-thread vs. work-stealing
- [ ] HTTP/2 support
  - Frame parsing (DATA, HEADERS, SETTINGS, GOAWAY)
  - Multiplexing (stream ID routing)
  - Server push (optional)
- [ ] Benchmark suite
  - Throughput (req/sec)
  - Latency percentiles (p50, p95, p99)
  - Comparison with Actix, Rocket, Axum
- [ ] Connection pooling optimization
  - LIFO list validation
  - Memory usage under load
  - Backpressure handling

**Deliverables:**
- moduvex-runtime v0.2.0 (work-stealing)
- moduvex-http v0.2.0 (HTTP/2)
- Benchmark suite + results

**Success Criteria:**
- Work-stealing improves throughput ≥20%
- HTTP/2 passes h2load compliance tests
- Benchmarks published + documented
- No regressions in existing tests

---

### Phase C: Observability (ETA: May 2025)

**Goal:** Enhanced tracing, metrics aggregation, SLO support.

**Requirements:**
- [ ] Distributed tracing enhancements
  - W3C trace context propagation
  - Baggage support (custom fields)
  - Span events (in-span logging)
- [ ] Metrics aggregation
  - Time-series exporter (local buffer)
  - Prometheus remote write
  - Percentile bucket optimization (HDR histogram)
- [ ] SLO framework
  - Error budget tracking
  - Burn-down alerts
  - SLI calculation helpers
- [ ] Health check persistence
  - Historic health status log
  - Degradation analysis

**Deliverables:**
- moduvex-observe v0.2.0 (enhanced)
- SLO documentation + examples

**Success Criteria:**
- All logging macros work with structured fields
- Prometheus export verified with Grafana
- SLO tracking example included
- Coverage maintained ≥80%

---

### Phase D: Reliability & Resilience (ETA: Jun 2025)

**Goal:** Retries, circuit breakers, bulkheads, graceful degradation.

**Requirements:**
- [ ] Retry middleware
  - Exponential backoff
  - Jitter
  - Max retries configurable
- [ ] Circuit breaker
  - Half-open state
  - Failure threshold
  - Timeout recovery
- [ ] Bulkhead pattern (semaphore-limited concurrency)
  - Per-service limits
  - Queue-based backpressure
- [ ] Graceful shutdown
  - Drain in-flight requests
  - Configurable grace period
  - Health probe suppression during shutdown

**Deliverables:**
- moduvex-http v0.3.0 (retry + circuit breaker middleware)
- Resilience documentation + patterns

**Success Criteria:**
- Retry tests pass (verify backoff + jitter)
- Circuit breaker state transitions tested
- Shutdown drains all requests
- Examples for common resilience patterns

---

### Phase E: Ecosystem & Middleware Library (ETA: Jul 2025)

**Goal:** Community middleware, templates, examples.

**Requirements:**
- [ ] Middleware library
  - CORS (origin validation)
  - Rate limiting (token bucket)
  - Authentication (JWT, session)
  - Compression (gzip, brotli)
  - Request ID / correlation
- [ ] Project templates
  - Web service scaffold
  - Data service scaffold
  - Microservice scaffold
- [ ] Example applications
  - Todo API (CRUD + auth)
  - Blog service (static + dynamic content)
  - Chat service (WebSocket ready)

**Deliverables:**
- moduvex-middleware crate (new)
- 3 starter templates
- 3 example apps

**Success Criteria:**
- All middleware pass security review
- Templates scaffold working apps
- Examples run without modification
- Documentation for each middleware

---

### Phase F: Deployment & DevOps (ETA: Aug 2025)

**Goal:** Containerization, orchestration, deployment guides.

**Requirements:**
- [ ] Docker support
  - Dockerfile (multi-stage)
  - Docker Compose setup
  - Health check configuration
- [ ] Kubernetes manifests
  - Deployment template
  - Service + Ingress
  - ConfigMap for config
  - Readiness/liveness probes
- [ ] CI/CD enhancement
  - Release automation (crates.io publishing)
  - Version bumping
  - Changelog generation
- [ ] Deployment guides
  - AWS ECS/EKS
  - Google Cloud Run
  - DigitalOcean Apps

**Deliverables:**
- Docker configuration
- Kubernetes manifests
- CI/CD GitHub Actions workflow
- Deployment guides

**Success Criteria:**
- Docker image builds cleanly
- K8s manifests apply without errors
- CI/CD auto-publishes on tag
- Guides include troubleshooting

---

### Phase G: 1.0 Release (ETA: Sep 2025)

**Goal:** Stabilize API, publish all crates, declare production-ready.

**Requirements:**
- [ ] API stability audit
  - No breaking changes to public API
  - Deprecation warnings for any planned changes
  - Version policy documented (SemVer)
- [ ] All 10 crates published
  - moduvex-observe → crates.io
  - moduvex-db → crates.io
  - moduvex-starter-web → crates.io
  - moduvex-starter-data → crates.io
  - moduvex → crates.io
- [ ] Documentation finalized
  - API docs coverage 100%
  - Getting started guide (20 min)
  - Architecture deep dive
  - Cookbook (common patterns)
- [ ] Production examples
  - 2+ open-source projects using Moduvex
  - Case studies (performance, reliability)
- [ ] Performance baselines locked
  - Throughput, latency, memory documented
  - Performance regression tests added

**Deliverables:**
- v1.0.0 tag + GitHub Release
- All 10 crates on crates.io
- Complete documentation site
- 2+ production examples
- Stability guarantee document

**Success Criteria:**
- All tests pass
- Coverage ≥85%
- Zero warnings (clippy, fmt)
- Production deployments running
- Community uptake ≥ 50 GitHub stars

---

## Timeline

```
Feb 2025  ────── 0.1.0 (MVP)
Mar 2025  ┌──── Phase A (Security, SCRAM, TLS)
          │
Apr 2025  ├──── Phase B (Performance, HTTP/2, Benchmarks)
          │
May 2025  ├──── Phase C (Observability, SLO)
          │
Jun 2025  ├──── Phase D (Reliability, Circuit Breaker)
          │
Jul 2025  ├──── Phase E (Ecosystem, Middleware, Templates)
          │
Aug 2025  ├──── Phase F (Deployment, Kubernetes)
          │
Sep 2025  └──── v1.0.0 (Release)
```

## Success Metrics (1.0 Target)

| Metric | 0.1.0 | 1.0.0 Target |
|--------|-------|--------------|
| Crates on crates.io | 5/10 | 10/10 |
| Test coverage | ~75% | ≥85% |
| Documentation % | ~60% | 100% |
| API stability | MVP | Stable |
| Security audit | Internal | External |
| Production users | 0 | ≥2 |
| GitHub stars | 0 | ≥50 |
| Throughput (4-core) | TBD | >40k req/s |
| Latency p99 | TBD | <10ms |

## Known Limitations (Tracked for Future)

### HTTP Feature Parity
- [ ] HTTP/3 (defer to v1.1+)
- [ ] WebSocket (defer to v1.1+)
- [ ] Server-sent events (defer to v1.1+)
- [ ] Multipart form data (defer to v1.1+)

### Database
- [ ] MySQL/MariaDB support (defer to v1.1+)
- [ ] SQLite (defer to v1.1+)
- [ ] Prepared statements (MVP: simple query only)
- [ ] Transaction savepoints (defer to v1.1+)

### Runtime
- [ ] io_uring support (future optimization)
- [ ] NUMA-aware scheduling (defer)
- [ ] Cgroup integration (defer)

### Framework
- [ ] Sync handlers (async-only by design)
- [ ] Macro DSLs (e.g., routing DSL) (defer)
- [ ] Plugin system (defer)

---

## Contribution Areas (Open to Community)

- **Documentation:** Examples, tutorials, cookbook
- **Middleware:** Custom middleware implementations
- **Performance:** Profiling, optimization
- **Platform support:** Enhanced Windows IOCP
- **Testing:** Benchmarks, stress tests
- **Examples:** Real-world applications

## Breaking Changes Policy

### Pre-1.0 (0.x.y)
- Minor version bumps may include breaking changes
- Deprecation warnings provided

### Post-1.0 (x.y.z)
- Semantic versioning strictly enforced
- Major version bumps for breaking changes
- 2-release deprecation period (where possible)

---

## How to Track Progress

1. **GitHub Issues** — Detailed tracking per phase
2. **GitHub Discussions** — Community roadmap feedback
3. **Changelog** — Merged PRs + releases
4. **Project Boards** — Kanban boards per phase

---

**Last Updated:** Phase 8 (Documentation)
**Next Review:** Phase A completion
