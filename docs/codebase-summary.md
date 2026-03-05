# Codebase Summary — All 10 Crates

## Overview

Moduvex workspace contains **10 crates** organized in 5 layers. Total: ~20K LOC, 373+ tests, 0 external async runtime dependencies.

| Crate | Layer | Type | LOC | Tests | Published |
|-------|-------|------|-----|-------|-----------|
| moduvex-runtime | 1 | Library | ~2500 | 60+ | ✓ 0.1.0 |
| moduvex-macros | 1 | Proc Macro | ~800 | 20+ | ✓ 0.1.0 |
| moduvex-config | 1 | Library | ~1200 | 40+ | ✓ 0.1.0 |
| moduvex-core | 2 | Library | ~3500 | 80+ | ✓ 0.1.0 |
| moduvex-http | 2 | Library | ~4200 | 100+ | ✓ 0.1.0 |
| moduvex-observe | 2 | Library | ~2800 | 50+ | Pending |
| moduvex-db | 3 | Library | ~3000 | 30+ | Pending |
| moduvex-starter-web | 4 | Library | ~200 | 5+ | Pending |
| moduvex-starter-data | 4 | Library | ~200 | 5+ | Pending |
| moduvex | 5 | Umbrella | ~300 | 10+ | Pending |

## Crate Details

### Layer 1: Foundation Crates

#### moduvex-runtime
**Purpose:** Custom async runtime without tokio/mio dependencies.

**Key Modules:**
- `executor::task` — Task scheduler, JoinHandle, spawning
- `executor::task_local` — Thread-local + async task-local storage
- `reactor` — Event loop selection (epoll/kqueue/IOCP)
  - `epoll` — Linux I/O multiplexing (Level 2 syscall)
  - `kqueue` — macOS/BSD I/O multiplexing
  - `iocp` — Windows I/O completion ports (stub)
- `net` — Async networking primitives
  - `tcp_listener` — Accept incoming connections
  - `tcp_stream` — Bidirectional TCP socket
  - `udp_socket` — Connectionless UDP socket
  - `sockaddr` — Socket address wrapper
  - `async_read/write` — Traits for async I/O
- `time::interval` — Recurring timer
- `time::sleep` — One-shot timer
- `sync` — Synchronization primitives
  - `Mutex` — Async-friendly mutex (no poisoning)
  - `mpsc` — Multi-producer, single-consumer channel
  - `oneshot` — One-way async signal
- `signal` — Unix signal handling (SIGTERM, SIGINT, etc.)
- `platform` — OS-specific code (cfg[unix]/cfg[windows])

**Public API Highlights:**
```rust
pub fn block_on<F: Future>(future: F) -> F::Output
pub fn spawn<F: Future + 'static>(future: F) -> JoinHandle<F::Output>
pub async fn sleep(duration: Duration)
pub fn interval(period: Duration) -> Interval
pub struct TcpListener { ... }
pub struct TcpStream { ... }
pub struct Mutex<T> { ... }
```

**Design:** Thread-per-core by default (one executor per thread). Reactor runs event loop in background via `reactor::Poller`. Timers use hierarchical wheel (O(1) insert/fire).

---

#### moduvex-macros
**Purpose:** Proc macros for zero-boilerplate trait derivation.

**Key Macros:**

1. **`#[derive(Module)]`** + `#[module(...)]` attr
   - Generates `Module` impl with `name()` method
   - Generates `DependsOn` impl with `type Required`
   - Attributes:
     - `#[module(depends_on(ModA, ModB))]` — list deps
     - `#[module(priority = N)]` — boot order (higher first)
   - Example output:
   ```rust
   pub struct UserModule;
   // impl Module { fn name(&self) -> &'static str { "user" } }
   // impl DependsOn { type Required = (ConfigModule, AuthModule); }
   ```

2. **`#[derive(Component)]`** + `#[inject(...)]` attrs
   - Generates `Inject` + `Provider` impls for DI fields
   - Resolves `#[inject]` fields from `AppContext`
   - Fields without `#[inject]` must impl `Default`
   - Optional injection: `#[inject(optional)]` → `Option<T>`
   - Example:
   ```rust
   #[derive(Component)]
   struct UserService {
       #[inject] repo: Arc<UserRepository>,
       #[inject(optional)] cache: Option<Arc<Cache>>,
   }
   ```

3. **`#[derive(DomainError)]`** + `#[error(...)]` attrs
   - Generates `DomainError` trait impl
   - Maps variants → HTTP status codes + error codes
   - Attributes: `#[error(code = "...", status = NNN)]`
   - Example:
   ```rust
   #[derive(DomainError)]
   enum UserError {
       #[error(code = "USER_NOT_FOUND", status = 404)]
       NotFound(UserId),
       #[error(code = "EMAIL_EXISTS", status = 409)]
       AlreadyExists,
   }
   ```

4. **`#[derive(InfraError)]`** + `#[error(...)]` attrs
   - Generates `InfraError` trait impl
   - Marks variants retryable or not
   - Example:
   ```rust
   #[derive(InfraError)]
   enum DbError {
       #[error(retryable = true)]
       ConnectionLost(String),
       #[error(retryable = false)]
       InvalidQuery(String),
   }
   ```

5. **`#[moduvex::main]`** attr
   - Replaces `#[tokio::main]` for Moduvex
   - Sets up runtime + block_on_with_spawn
   - Example:
   ```rust
   #[moduvex::main]
   async fn main() { ... }
   ```

**Implementation:** Uses `syn` for AST parsing, `quote` for code generation, `proc-macro2` for hygiene.

---

#### moduvex-config
**Purpose:** Typed TOML config with profile overlays and env var merging.

**Key Types:**
- `ConfigLoader` — Main entry point
  - `load(name, dir)` — Load base + profile + env
  - `load_with_defaults(defaults_str, name, dir)` — Embedded defaults fallback
  - `scope::<T>(section_name)` → `Result<Arc<T>>` — Extract typed config section
- `Profile` — dev | test | prod (from `MODUVEX_PROFILE` env var, default: dev)
- `ConfigError` — Wraps parse/missing/validation errors
- `Validate` trait + `ValidationError` — Custom validators

**Merge Order (highest precedence first):**
1. `MODUVEX__*` env vars (e.g., `MODUVEX__SERVER__PORT=3000`)
2. `{name}-{profile}.toml` file overlay
3. `{name}.toml` base file
4. Embedded defaults (from starters)

**Example File Structure:**
```
app.toml
app-dev.toml
app-prod.toml

[server]
port = 8080
host = "0.0.0.0"

[database]
url = "postgres://localhost/mydb"
pool_size = 10
```

**Access Pattern:**
```rust
#[derive(Deserialize)]
struct ServerConfig { port: u16, host: String }

let config = ConfigLoader::load("app", Path::new("."))?;
let server: Arc<ServerConfig> = config.scope("server")?;
println!("Listening on {}:{}", server.host, server.port);
```

---

### Layer 2: Framework Core

#### moduvex-core
**Purpose:** DI container, module system, lifecycle engine, error handling.

**Key Modules:**

1. **`app`** — Application builder + contexts
   - `Moduvex<State>` type-state builder (Unconfigured → Configured → Ready)
   - `AppContext` — Singleton container (Arc<T> by type)
   - `RequestContext` — Per-request scope
   - `AppBuilder` — Fluent API for `.config()` + `.module::<M>()`

2. **`di`** — Dependency injection
   - `TypeMap` — HashMap<TypeId, Box<dyn Any>> for type-safe storage
   - `Inject<T>` — Resolve T from AppContext
   - `Provider<T>` — Factory to create instances
   - `Singleton<T>` — Arc wrapper for singletons
   - `RequestScoped<T>` — Per-request factory pattern

3. **`module`** — Module trait family
   - `Module` — `fn name(&self) -> &'static str`
   - `DependsOn` — `type Required = (...)`
   - `ModuleLifecycle` — Hooks: config, validate, init, start, ready, stopping, stopped
   - `ModuleRoutes` — Register HTTP routes
   - Dependency proof-witnesses: `Here`, `There`, `ContainsModule`

4. **`lifecycle`** — Boot sequence + phases
   - `LifecycleEngine` — Orchestrates 7 phases with rollback on error
   - `Phase` enum — Config, Validate, Init, Start, Ready, Stopping, Stopped
   - `LifecycleHook` — Closure registered per phase
   - `ShutdownHandle` — Signal shutdown, wait for completion
   - `ShutdownConfig` — Grace period, signal handling

5. **`error`** — Error system
   - `ModuvexError` — 4-variant enum:
     - `Domain(Box<dyn DomainError>)` — Business logic errors
     - `Infra(Box<dyn InfraError>)` — Infrastructure errors
     - `Config(ConfigError)` — Configuration errors
     - `Lifecycle(LifecycleError)` — Framework errors
   - `Result<T> = std::result::Result<T, ModuvexError>`
   - `.context(msg)` — Chain errors with context

6. **`tx`** — Transaction boundary (stub, implemented by moduvex-db)
   - `TransactionBoundary` trait — Begin/commit/rollback

**Type-State Pattern:**
```rust
Moduvex::new()                // State: Unconfigured
    .config(loader)          // State: Configured
    .module::<UserModule>()  // Type-check deps
    .module::<AuthModule>()  // Type-check deps
    .run()                   // State: Ready → AppContext
```

Compiler errors if module deps are missing or circular.

**Lifecycle Example:**
```rust
impl ModuleLifecycle for UserModule {
    async fn config(&mut self, loader: &ConfigLoader) -> Result<()> {
        self.config = loader.scope("user")?;
        Ok(())
    }

    async fn init(&self, ctx: &mut ProviderContext) -> Result<()> {
        let repo = Arc::new(UserRepository::new(self.config.clone()));
        ctx.insert(repo);
        Ok(())
    }

    async fn start(&self) -> Result<()> {
        info!("UserModule started");
        Ok(())
    }
}
```

---

#### moduvex-http
**Purpose:** Custom HTTP/1.1 server (zero external HTTP crate deps).

**Key Modules:**

1. **`server`** — HTTP server orchestrator
   - `HttpServer` — Builder + listen/serve
   - `ConnConfig` — Keep-alive settings, buffer sizes
   - `Connection` — Single client connection handler

2. **`protocol`** — Raw HTTP parsing
   - `parse_request_line` — "GET /path HTTP/1.1" → (Method, Path, Version)
   - `parse_headers` — Raw bytes → HeaderMap
   - Zero-copy parsing (no allocations for validation)

3. **`routing`** — Path matching
   - `Router` — Radix tree of routes
   - `Pattern` — `/users/:id` pattern matching (extracts `:id`)
   - `Method` enum — GET, POST, PUT, DELETE, PATCH, etc.

4. **`request`** — Request container
   - `Request` — Immutable snapshot: method, path, headers, body
   - `Extensions` — Per-request data store (type-indexed)
   - `HttpVersion` — HTTP/1.0, HTTP/1.1

5. **`response`** — Response builder
   - `Response` — Status, headers, body
   - `IntoResponse` trait — Type → Response (String, JSON, etc.)
   - `StatusCode` enum — 2xx, 3xx, 4xx, 5xx variants

6. **`extract`** — Request extractors
   - `FromRequest<'a>` trait — Extract typed values from request
   - `Path<T>` — Deserialize path params (`:id` → T)
   - `Query<T>` — Deserialize query string
   - `Json<T>` — Deserialize request body as JSON
   - `State<T>` — Inject app state (AppContext)
   - `IntoHandler<T>` — Type-to-handler conversion (auto-extract deps)

7. **`middleware`** — Middleware pipeline
   - `Middleware` trait — Wrap handler logic
   - `Next` — Call next middleware or handler

8. **`body`** — Request/response bodies
   - `Body` — Async byte stream
   - `BodySender` — Write to response
   - `BodyReceiver` — Read request body

9. **`header`** — HTTP headers
   - `HeaderMap` — Case-insensitive header storage
   - Common headers: Content-Type, Content-Length, etc.

10. **`status`** — HTTP status codes
    - Enum: OK (200), Created (201), BadRequest (400), NotFound (404), etc.

**Handler Example:**
```rust
async fn get_user(
    Path(UserId(id)): Path<UserId>,
    State(ctx): State<AppContext>,
) -> Result<Json<User>> {
    let repo = ctx.require::<Arc<UserRepo>>()?;
    let user = repo.find(id).await?;
    Ok(Json(user))
}

fn main() {
    HttpServer::bind("0.0.0.0:8080")
        .get("/users/:id", get_user)
        .post("/users", create_user)
        .serve()
        .unwrap();
}
```

---

#### moduvex-observe
**Purpose:** Observability: structured logging, tracing, metrics, health checks.

**Key Modules:**

1. **`log`** — Structured logging
   - `Event` — Log event with timestamp, level, message, fields
   - `Level` — Error, Warn, Info, Debug, Trace
   - `Subscriber` — Listen to events (global dispatch)
   - `JsonFormatter` — JSON output
   - `PrettyFormatter` — Human-readable output
   - Macros: `error!()`, `warn!()`, `info!()`, `debug!()`, `trace!()`

2. **`trace`** — Distributed tracing
   - `Span` — Named operation context
   - `SpanContext` — Metadata (trace ID, span ID, parent span ID)
   - `TraceId`, `SpanId` — 128-bit and 64-bit identifiers
   - Baggage propagation (W3C trace context compatible)

3. **`metrics`** — Lock-free metrics (atomic-based)
   - `Counter` — Increment-only counter
   - `Gauge` — Set/increment/decrement gauge
   - `Histogram` — Distribution bucket recording
   - `MetricsRegistry` — Central metric store

4. **`health`** — Health check system
   - `HealthCheck` — Sync check (fn() -> Status)
   - `AsyncHealthCheck` — Async check (async fn() -> Status)
   - `HealthRegistry` — Composite checks
   - `HealthStatus` — Healthy, Degraded, Unhealthy

5. **`export`** — Metrics export
   - `Exporter` trait — Custom exporters
   - `PrometheusExporter` — Prometheus text format
   - `StdoutExporter` — Print to stdout

**Example Usage:**
```rust
use moduvex_observe::prelude::*;

info!("request received", method = "GET", path = "/users");

let span = Span::new("db_query");
let result = query.execute().await;
span.end();

let counter = Counter::new("requests_total", "Total requests");
counter.inc();
```

---

### Layer 3: Database

#### moduvex-db
**Purpose:** PostgreSQL async driver (wire protocol + pool + migrations).

**Key Modules:**

1. **`protocol::postgres`** — PostgreSQL wire protocol
   - `PgConnection` — Low-level TCP connection to PG server
   - `PgRow` — Single result row (byte representation)
   - `PgRowSet` — Result set from query
   - `PgColumn` — Column metadata (name, type OID, etc.)
   - MD5 auth (MD5-hashed password + salt exchange)
   - SCRAM-SHA-256 auth (in Phase 7 work)

2. **`pool`** — Async connection pool
   - `ConnectionPool` — Main pool manager
   - `PoolConfig` — URL, max_connections, timeouts
   - LIFO idle list (recently-used → hot cache)
   - Semaphore-bounded acquire (blocking semantics)
   - `health::HealthMonitor` — Periodic connectivity check

3. **`query`** — Query builder + typed accessors
   - `QueryBuilder` — Fluent builder for SELECT queries
     - `.select(table)` → `.columns([...])` → `.where_eq(...)` → `.order_by(...)` → `.limit(N)` → `.build_inlined()`
   - `Row<T>` — Typed row accessor (deserialization)
   - `RowSet<T>` — Iterator of typed rows
   - `FromRow` trait — Deserialize row → struct
   - `Param`, `ToParam` — Type-safe parameter binding
   - `Order` enum — Asc, Desc

4. **`tx`** — Transactions
   - `Transaction<'pool>` — Scoped transaction
   - `IsolationLevel` — Serializable, RepeatableRead, ReadCommitted, ReadUncommitted
   - `PoolTransactionBoundary` — Implements core `TransactionBoundary`
   - Auto-rollback on Drop (RAII)

5. **`migrate`** — Migration engine
   - `MigrationEngine` — Load + apply migrations
   - `Migration` — Single migration (version + SQL)
   - `load_migrations(dir)` → Vec<Migration>
   - Version tracking in `schema_versions` table
   - Up-only (no rollback support)

6. **`error`** — Database errors
   - `DbError` — Connection, query, parse, auth errors
   - `Result<T> = std::result::Result<T, DbError>`

**Example Usage:**
```rust
let cfg = PoolConfig::new("postgres://localhost/mydb");
let pool = ConnectionPool::new(cfg);

let mut conn = pool.acquire().await?;
let sql = QueryBuilder::select("users")?
    .columns(&["id", "name"])?
    .where_eq("active", true)?
    .build_inlined()?;
let rows = conn.query(&sql).await?;
for row in rows.iter() {
    let id: i32 = row.get("id")?;
    let name: String = row.get("name")?;
    println!("{}: {}", id, name);
}
pool.release(conn).await;
```

---

### Layer 4: Starters

#### moduvex-starter-web
**Purpose:** One-dependency web framework (runtime + HTTP + config + observe).

**Contents:**
- Re-exports: `moduvex_runtime`, `moduvex_http`, `moduvex_core`, `moduvex_config`, `moduvex_observe`
- `WEB_DEFAULTS` — Embedded config (port 8080, log level info, etc.)
- `load_config(name, dir)` — Load with defaults
- `default_config()` — Defaults only, no file needed
- `prelude::*` — Convenient glob import

**File Layout:**
```
src/
├── lib.rs        # Re-exports, constants, prelude
└── tests/
    └── tests.rs  # Smoke test for default_config
```

**Example:**
```rust
use moduvex_starter_web::prelude::*;

#[moduvex::main]
async fn main() {
    let config = default_config().unwrap();
    Moduvex::new()
        .config(config)
        .module::<UserModule>()
        .run()
        .await;
}
```

---

#### moduvex-starter-data
**Purpose:** One-dependency data service (runtime + DB + config).

**Contents:**
- Re-exports: `moduvex_runtime`, `moduvex_db`, `moduvex_core`, `moduvex_config`
- `DATA_DEFAULTS` — Embedded pool config (max 10 conns, min idle 2)
- `load_config(name, dir)` — Load with defaults
- `default_config()` — Defaults only, no file needed

**Example:**
```rust
use moduvex_starter_data::prelude::*;

#[moduvex::main]
async fn main() {
    let config = default_config().unwrap();
    let pool_config: Arc<PoolConfig> = config.scope("pool")?;
    let pool = ConnectionPool::new((*pool_config).clone());
    // ... use pool ...
}
```

---

### Layer 5: Umbrella

#### moduvex
**Purpose:** Single-dependency convenience crate with feature-gated re-exports.

**Features:**
- **default:** config, core, observe, runtime
- **web:** +http, +starter-web
- **data:** +db, +starter-data
- **full:** all of the above

**Usage:**
```toml
[dependencies]
# Just the framework core
moduvex = "0.1"

# Web app convenience
moduvex = { version = "0.1", features = ["web"] }

# Data service convenience
moduvex = { version = "0.1", features = ["data"] }

# All features
moduvex = { version = "0.1", features = ["full"] }
```

**Prelude:**
```rust
use moduvex::prelude::*;

// Available:
// - ConfigLoader, Profile
// - Moduvex, Module, DependsOn, ModuleLifecycle
// - ModuvexError, Result
// - info!, warn!, error!() macros
// - Counter, Gauge, Histogram
// - (web only) HttpServer, Request, Response, Router
// - (data only) ConnectionPool, RowSet, Transaction
```

---

## Summary Statistics

| Metric | Value |
|--------|-------|
| Total crates | 10 |
| Total LOC (code + comments) | ~20,000 |
| Total test cases | 373+ |
| Unsafe blocks | ~15 (all documented) |
| Proc macros | 5 |
| External deps | ~25 (no async runtime) |
| Published crates | 5/10 (0.1.0) |

## Feature Flags by Crate

| Crate | Features | Default |
|-------|----------|---------|
| moduvex-runtime | (none) | — |
| moduvex-macros | (none) | — |
| moduvex-config | (none) | — |
| moduvex-core | (none) | — |
| moduvex-http | tls | Disabled |
| moduvex-observe | (none) | — |
| moduvex-db | (none) | — |
| moduvex-starter-web | (none) | — |
| moduvex-starter-data | observe | Enabled |
| moduvex | web, data, full | Default: core only |

---

**Last Updated:** Phase 8 (Documentation)
**Total Coverage:** All 10 crates documented
