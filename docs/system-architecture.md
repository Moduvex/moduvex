# System Architecture & Design

## Architectural Overview

Moduvex is a **layered, modular backend framework** with zero external async runtime dependencies. The architecture is built on 4 core principles:

1. **Compile-time safety** via type-state pattern and type-aware DI
2. **Platform-native I/O** (epoll/kqueue/IOCP) managed by custom runtime
3. **Deterministic lifecycle** with 7-phase boot sequence and rollback semantics
4. **Immutable singletons** via `Arc<T>` for thread-safe sharing

## Crate Dependency Graph

```
┌─────────────────────────────────────────────────────┐
│             moduvex (umbrella)                      │
│  Feature-gated re-exports + convenience prelude    │
└──────────────────────────┬──────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        │                  │                  │
        ▼                  ▼                  ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│moduvex-      │  │moduvex-      │  │moduvex-      │
│starter-web   │  │starter-data  │  │core          │
└──────┬───────┘  └──────┬───────┘  └──────┬───────┘
       │                 │                 │
       ├─ moduvex-http ──┼─ moduvex-db ───┤
       │                 │                 │
       └─────────┬───────┴─────────┬───────┘
                 │                 │
        ┌────────┴──────┬──────────┴────────┐
        │               │                   │
        ▼               ▼                   ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│moduvex-      │  │moduvex-      │  │moduvex-      │
│runtime       │  │macros        │  │config        │
└──────────────┘  └──────────────┘  └──────────────┘
        │
        ▼
┌──────────────┐
│moduvex-      │
│observe       │
└──────────────┘
```

## Layer Breakdown

### Layer 1: Foundation (No Internal Dependencies)

#### moduvex-runtime
**Purpose:** Custom async runtime with platform-native I/O multiplexing (zero tokio dependency).

**Key Modules:**
- `executor` — Task scheduler, spawn, block_on, task-local storage
- `reactor` — Event loop with platform-native multiplexing:
  - **Linux:** epoll (Level 2 syscall, O(1) per event)
  - **macOS/BSD:** kqueue (Level 1 syscall, supports files + sockets)
  - **Windows:** WSAPoll (replaces IOCP stub in Wave 9)
- `net` — Async TCP/UDP sockets built on reactor
- `time` — Hierarchical timer wheel, sleep, intervals
- `sync` — Mutex, mpsc, oneshot channels (lock-free where possible)
- `signal` — Unix signal handling (SIGTERM, SIGINT, etc.)
- `platform` — OS-specific abstractions

**Design Rationale:**
- **Zero tokio dependency** reduces transitive deps and binary size
- **Thread-per-core** default improves cache locality (vs. work-stealing overhead)
- **Hierarchical timer wheel** (vs. heap) reduces GC pressure
- **Async-only** (no sync blocking) enforces non-blocking semantics

**Thread Model:**
```
Thread 0        Thread 1        Thread N
  │               │               │
  ├─ Executor      ├─ Executor    ├─ Executor
  │   ├─ Tasks     │   ├─ Tasks   │   ├─ Tasks
  │   └─ TimerQ    │   └─ TimerQ  │   └─ TimerQ
  │               │               │
  ├─ Reactor       ├─ Reactor     ├─ Reactor
  │   └─ Epoll     │   └─ Epoll   │   └─ Epoll
  └─────────────────────────────────────
        Work-stealing queue (optional)
```

#### moduvex-macros
**Purpose:** Procedural macros for zero-boilerplate trait derivation.

**Provided Macros:**
- `#[derive(Module)]` — Implements `Module + DependsOn`
- `#[derive(Component)]` — Implements `Inject + Provider` for DI fields
- `#[derive(DomainError)]` — Implements `DomainError` trait
- `#[derive(InfraError)]` — Implements `InfraError` trait
- `#[moduvex::main]` — Entry point macro (sets up runtime + lifecycle)

#### moduvex-config
**Purpose:** Typed TOML configuration with profiles, env overrides, and per-module scoping.

**Load Priority (highest to lowest):**
1. Environment variables (`MODUVEX__*` or `MODUVEX_PROFILE`)
2. Profile overlay (`app-{profile}.toml`)
3. Base config (`app.toml`)
4. Embedded defaults (from starter crates)

**Example:**
```toml
# app.toml
[server]
port = 8080
host = "0.0.0.0"

[database]
url = "postgres://localhost/mydb"

# app-prod.toml overlay
[server]
port = 3000
host = "0.0.0.0"
```

Accessed via `ConfigLoader::scope::<T>("section")`.

### Layer 2: Framework Core

#### moduvex-core
**Purpose:** Type-state dependency injection, module system, and lifecycle engine.

**Key Types:**
- `Moduvex<State>` — Type-state builder (compile-time validation)
- `Module` — Trait for modules with `DependsOn` for compile-time dep graph
- `AppContext` — Shared singletons (`Arc<T>` by type)
- `RequestContext` — Per-request scope (request-local factories)
- `LifecycleEngine` — 7-phase boot with rollback on failure
- `ModuvexError` — 4-variant error type (Domain/Infra/Config/Lifecycle)

**Type-State Builder Pattern:**
```rust
Moduvex::new()          // State: Unconfigured
    .config(loader)    // State: Configured
    .module::<M1>()    // Type-check: M1::DependsOn = ()
    .module::<M2>()    // Type-check: M2::DependsOn contains M1 ✓
    .run()             // Execute lifecycle, return AppContext
```

Compiler rejects at type-check time if dependencies are missing or circular.

**Lifecycle Phases:**
```
Phase 1: Config      Load + validate config for each module
         ↓
Phase 2: Validate    Check invariants (e.g., port range, URL format)
         ↓
Phase 3: Init        Create singletons, register services in AppContext
         ↓
Phase 4: Start       Bind listeners, open DB pools
         ↓
Phase 5: Ready       Signal readiness (e.g., liveness probe)
         ↓ (running)
Phase 6: Stopping    Shutdown requested (signal, API, etc.)
         ↓
Phase 7: Stopped     Close pools, listeners, flush logs
```

On error in any phase → auto-rollback to Stopped (deterministic).

#### moduvex-http
**Purpose:** Custom HTTP/1.1 + HTTP/2 server built entirely on moduvex-runtime.

**Protocol Detection & Handling:**
```
TCP Accept
    │
    ├─ TLS Handshake (if configured)
    │   ├─ ALPN negotiation (h2 or http/1.1)
    │   └─ Certificate validation
    │
    ├─ Protocol Detection (h2c via preface)
    │   └─ HTTP/2 Preface: "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
    │
    ├─ HTTP/1.1 Request Pipeline
    │   ├─ HttpParser::parse_request (zero-copy)
    │   ├─ Router::match(path, method)
    │   ├─ Middleware chain (pre-handler)
    │   ├─ Handler invocation + response generation
    │   ├─ Middleware chain (post-handler)
    │   └─ TCP write + keep-alive
    │
    └─ HTTP/2 Multiplexing Pipeline
        ├─ Frame codec (RFC 9113)
        │   └─ DATA, HEADERS, SETTINGS, GOAWAY, etc.
        ├─ HPACK decompression (RFC 7541)
        ├─ Stream state machine (per stream)
        │   ├─ Idle → Open → Reserved → Closed
        │   └─ Flow control (window-based)
        ├─ Per-stream handler invocation (concurrent)
        │   └─ Each stream → own Request/Response cycle
        ├─ Response encoding (HPACK compression)
        └─ Multiplexed frame transmission
```

**Key Components:**
- `Request` — Immutable HTTP request snapshot
- `Response` — Builder for status, headers, body
- `Router` — Radix tree matching for O(path_len) route lookup
- `Middleware` — Async closures that wrap handlers
- `Extractors` — `FromRequest` trait for type-safe param extraction
- `H2FrameCodec` — RFC 9113 frame parsing/encoding
- `HpackEncoder/Decoder` — RFC 7541 header compression
- `H2Stream` — Per-stream state machine
- `H2Connection` — Multiplexer for concurrent streams

**Protocol Modules:**
- `protocol/h1/` — HTTP/1.1 parser, encoder, chunked transfer
- `protocol/h2/` — HTTP/2 frame codec, HPACK, flow control, stream machine

**Handler Signature (Protocol Agnostic):**
```rust
async fn handler(Path(id): Path<UserId>, State(ctx): State<AppContext>) -> Response {
    // Same handler works for HTTP/1.1 and HTTP/2
}
```

**Middleware Highlights:**
- Request ID middleware (correlation across protocols)
- W3C traceparent tracing (distributed tracing)
- CORS (origin validation)
- Static file serving
- WebSocket upgrade (HTTP/1.1 + HTTP/2)
- Form/multipart parsing

#### moduvex-observe
**Purpose:** Structured logging, distributed tracing, lock-free metrics, health checks.

**Subsystems:**
- `log` — Structured events with key-value fields, pretty + JSON formatters
- `trace` — Distributed tracing (SpanContext, TraceId, SpanId)
- `metrics` — Counter, Gauge, Histogram (all lock-free via atomics)
- `health` — Pluggable health checks, composite status
- `export` — Prometheus text format, stdout exporter

**Macro Convenience:**
```rust
info!("request handled", status = 200, path = "/users");
let span = Span::new("user_creation");
// ... work ...
span.end();
```

### Layer 3: Database

#### moduvex-db
**Purpose:** PostgreSQL wire protocol client with async connection pool and migrations.

**Connection Pool:**
- LIFO idle list (cache-friendly)
- Semaphore-bounded (max connections, blocking on acquire)
- Health monitor (periodic connectivity check)
- Configurable timeouts (connect, idle)

**Query Builder:**
```rust
QueryBuilder::select("users")?
    .columns(&["id", "name"])?
    .where_eq("active", true)?
    .order_by("id", Order::Asc)?
    .limit(10)
    .build_inlined()?
    // Returns: SELECT id, name FROM users WHERE active=true ORDER BY id ASC LIMIT 10
```

**Transaction Support:**
- `BEGIN` / `COMMIT` / `ROLLBACK`
- Auto-rollback on `Drop` (RAII pattern)
- Isolation levels: Serializable, RepeatableRead, ReadCommitted, ReadUncommitted

**Migration Engine:**
- File-based: `migrations/001_create_users.sql`
- Version-tracked in `schema_versions` table
- Up-only (no rollback), deterministic order

### Layer 4: Starters

#### moduvex-starter-web
**One-dependency web framework.** Bundles: runtime, HTTP, config, observe.

**Embedded Defaults:**
```toml
[server]
port = 8080
host = "0.0.0.0"

[observe.log]
level = "info"
format = "pretty"
```

**Prelude re-exports:** `Moduvex`, `HttpServer`, `Router`, `Request`, `Response`, `info!`, etc.

#### moduvex-starter-data
**One-dependency data service.** Bundles: runtime, DB, config.

**Embedded Defaults:**
```toml
[pool]
max_connections = 10
min_idle = 2
```

### Layer 5: Umbrella

#### moduvex
**Purpose:** Single-dependency convenience crate with feature-gated re-exports.

**Features:**
- Default: `config`, `core`, `observe`, `runtime`
- `web` — Adds HTTP, starters-web
- `data` — Adds DB, starters-data
- `full` — All of the above

**Usage:**
```toml
[dependencies]
moduvex = { version = "0.1", features = ["web"] }

# OR start with a starter
moduvex-starter-web = "0.1"
```

## Data Flow: Complete Request Lifecycle

### Scenario: HTTP GET /users/42 with DI

```
1. TCP Accept
   └─ Accept new socket from listener (runtime reactor)

2. Request Parsing
   └─ HttpParser reads bytes, produces Request{
      method: GET,
      path: "/users/42",
      headers: HeaderMap,
      body: BodyReceiver
   }

3. Route Matching
   └─ Router pattern "/users/:id" matches → extract "42"

4. Extractor Resolution
   └─ Path<UserId> extractor parses "42" → UserId(42)
   └─ State<AppContext> extractor provides &AppContext
   └─ Dependency tree: UserId + AppContext available to handler

5. Handler Invocation
   async fn get_user(Path(id): Path<UserId>, State(ctx): State<AppContext>) -> Response {
       let user_service = ctx.require::<Arc<UserService>>()?;
       // user_service is singleton from Init phase
       user_service.find_by_id(id).await
   }

6. Service Layer (DI)
   └─ UserService is in AppContext (inserted during Init phase)
   └─ UserService holds Arc<UserRepository>
   └─ UserRepository holds pool: Arc<ConnectionPool>

7. Database Query
   └─ repository.find_by_id(id)
   └─ Acquires connection from pool
   └─ Executes query: SELECT * FROM users WHERE id = $1
   └─ Parses response, releases connection

8. Response Serialization
   └─ Handler returns User struct
   └─ IntoResponse trait: struct → JSON serialization
   └─ Status 200 set automatically

9. Middleware (post-handler)
   └─ Add CORS headers
   └─ Log request metrics

10. TCP Write
    └─ Serialize Response{status, headers, body}
    └─ Write to socket
    └─ Keep-alive: yes (HTTP/1.1 default)

11. Next Request
    └─ Reuse same connection
```

## Key Design Decisions

### Why Custom Runtime?
- **Dependency elimination** — tokio adds 50+ transitive crates
- **Customization** — tuned for thread-per-core, minimal work-stealing
- **Learning** — developers understand async foundations

### Why Type-State DI?
- **Compile-time safety** — missing dependencies detected at type-check
- **Zero runtime cost** — DI graph erased after monomorphization
- **Proof-witness pattern** — `DependsOn` acts as proof of dependency satisfaction

### Why Immutable Singletons?
- **No mutation** → no locks on hot path
- **Arc clones** → cheap reference sharing
- **Predictable performance** → no contention

### HTTP/1.1 + HTTP/2 Dual Support
- **HTTP/1.1** — Foundation protocol, proven, simplest semantics
- **HTTP/2** — Multiplexing, header compression (HPACK), flow control
- **Protocol Selection:**
  - TLS: ALPN negotiation (h2 preferred, fallback h1)
  - Plain TCP: HTTP/2 preface detection (h2c), fallback h1
- **Handler Agnostic** — Same business logic works for both protocols
- **Backward Compatible** — Existing HTTP/1.1 code unchanged

## Performance Model

### Latency
```
Request accept → parse → route → extract → handler → response → send
   <1µs      +  50µs  + 10µs + 5µs    + Xms     + 10µs    + 50µs

   Hot path (no alloc): ~125µs overhead per request
   Handler time: variable (DB, logic, etc.)
```

### Throughput
- **Single thread:** ~10k req/s on Ryzen 5 (typical)
- **Thread-per-core (4 cores):** ~35k req/s
- **Bottleneck:** Handler logic, DB round-trips (not framework)

### Memory
- **Per connection:** ~4 KB (buffers)
- **Per request:** ~2 KB stack (async task)
- **Per singleton:** 1 allocation (via AppContext::insert)

## Extensibility Points

### Custom Modules
```rust
#[derive(Module)]
#[module(depends_on(ConfigModule, RuntimeModule))]
struct MyModule;

impl ModuleLifecycle for MyModule {
    async fn initialize(ctx: &mut ProviderContext) -> Result<()> {
        let config = ctx.require_config::<MyConfig>()?;
        let service = Arc::new(MyService::new(config));
        ctx.insert(service);
        Ok(())
    }
}
```

### Custom Middleware
```rust
struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    async fn handle(&self, mut req: Request, next: Next) -> Response {
        info!("request", method = %req.method, path = %req.path);
        let resp = next.call(req).await;
        info!("response", status = resp.status);
        resp
    }
}
```

### Custom Extractors
```rust
impl<'a> FromRequest<'a> for MyCustomType {
    async fn from_request(req: &'a Request) -> Result<Self> {
        // Validation + conversion logic
    }
}
```

---

**Status:** Wave 9 Complete — HTTP/2 + Windows WSAPoll reactor fully integrated.

**Last Updated:** Waves 7-9 complete (Mar 2026)
**Audience:** Developers, architects
