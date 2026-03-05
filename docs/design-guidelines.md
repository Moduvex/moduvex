# Design Guidelines — How to Use Moduvex

This guide shows how to use Moduvex for common development scenarios: creating modules, registering services, handling HTTP requests, managing config, and querying databases.

## Module System

### Creating a Custom Module

A module encapsulates configuration, services, and lifecycle hooks for a cohesive feature (e.g., authentication, user management).

**Basic Module Structure:**

```rust
use moduvex_core::prelude::*;
use std::sync::Arc;

#[derive(Module)]
struct UserModule;

// Declare dependencies (compile-time checked)
impl DependsOn for UserModule {
    type Required = ();  // No dependencies
}

// Implement lifecycle hooks
impl ModuleLifecycle for UserModule {
    async fn config(&mut self, loader: &ConfigLoader) -> Result<()> {
        // Load module-specific config from [user] section
        Ok(())
    }

    async fn validate(&self) -> Result<()> {
        // Validate invariants (e.g., port range, URLs valid)
        Ok(())
    }

    async fn init(&self, ctx: &mut ProviderContext) -> Result<()> {
        // Create and register singletons
        let service = Arc::new(UserService::new());
        ctx.insert(service);
        Ok(())
    }

    async fn start(&self) -> Result<()> {
        info!("UserModule started");
        Ok(())
    }

    // Optional: on_ready(), on_stopping(), on_stopped()
}
```

**Config (app.toml):**
```toml
[user]
enable_signup = true
max_users = 10000
```

**Usage in Application:**

```rust
#[moduvex::main]
async fn main() {
    let config = ConfigLoader::load("app", Path::new(".")).unwrap();
    Moduvex::new()
        .config(config)
        .module::<UserModule>()
        .run()
        .await;
}
```

### Module Dependencies (Type-State Pattern)

Define module dependencies in `DependsOn::Required`. The compiler enforces that all required modules are registered.

**Module with Dependencies:**

```rust
use moduvex_core::prelude::*;

#[derive(Module)]
#[module(depends_on(ConfigModule, DatabaseModule))]
struct UserModule;

// Compiler generates:
// impl DependsOn for UserModule {
//     type Required = (ConfigModule, DatabaseModule);
// }
```

**Application Builder (type-safe):**

```rust
Moduvex::new()
    .config(loader)
    .module::<ConfigModule>()      // ✓ Registered
    .module::<DatabaseModule>()    // ✓ Registered
    .module::<UserModule>()        // ✓ Compiles (both deps present)
    .run()
    .await;

// This would NOT compile:
Moduvex::new()
    .config(loader)
    .module::<UserModule>()        // ✗ Compiler error: missing DatabaseModule
    .run()
    .await;
```

### Proof-Witness Pattern

Dependencies form a proof at compile time. The type-state builder encodes which modules are registered.

```rust
// State: Moduvex<(ConfigModule, DatabaseModule)>
let app = Moduvex::new()
    .config(loader)
    .module::<ConfigModule>()
    .module::<DatabaseModule>();

// Now we can add UserModule (compiler knows deps are satisfied)
let app = app.module::<UserModule>();

// State: Moduvex<(ConfigModule, DatabaseModule, UserModule)>
// Type-state prevents invalid orderings at compile time
```

---

## Dependency Injection (DI)

### Registering Singletons

Singletons are services created once during Init phase, then shared via `Arc<T>` clones.

**In Module::init():**

```rust
impl ModuleLifecycle for UserModule {
    async fn init(&self, ctx: &mut ProviderContext) -> Result<()> {
        // Create singleton
        let repo = Arc::new(UserRepository::new());
        ctx.insert(repo);

        let service = Arc::new(UserService::new());
        ctx.insert(service);

        Ok(())
    }
}

pub struct UserService {
    repo: Arc<UserRepository>,
}

impl UserService {
    pub fn new() -> Self {
        Self { repo: Arc::new(UserRepository::new()) }
    }
}
```

### Injecting into HTTP Handlers

Use `State<T>` extractor to inject `AppContext`, then retrieve services.

**Handler with DI:**

```rust
use moduvex_http::prelude::*;

async fn create_user(
    Json(req): Json<CreateUserRequest>,
    State(ctx): State<AppContext>,
) -> Result<Json<User>> {
    // Retrieve singleton from context
    let service = ctx.require::<Arc<UserService>>()?;

    // Use service
    let user = service.create(req).await?;
    Ok(Json(user))
}
```

### Using Components with Derived Injection

Use `#[derive(Component)]` + `#[inject]` attrs for automatic field injection.

**Component Definition:**

```rust
use moduvex_macros::Component;

#[derive(Component)]
pub struct UserService {
    #[inject]
    repo: Arc<UserRepository>,

    #[inject(optional)]
    cache: Option<Arc<Cache>>,

    config: ServiceConfig,  // No #[inject] → uses Default
}

impl UserService {
    pub async fn create(&self, req: CreateUserRequest) -> Result<User> {
        // repo and cache are auto-resolved from AppContext
        self.repo.insert(&req).await?
    }
}
```

**In Module::init():**

```rust
impl ModuleLifecycle for UserModule {
    async fn init(&self, ctx: &mut ProviderContext) -> Result<()> {
        ctx.insert(Arc::new(UserRepository::new()));

        // Component auto-resolves #[inject] fields
        let service = Arc::new(UserService::default());
        ctx.insert(service);

        Ok(())
    }
}
```

---

## HTTP Handlers & Extractors

### Handler Signatures

Handlers are async functions that accept extracted values and return a response.

**Basic Patterns:**

```rust
// Minimal handler
async fn hello(_req: Request) -> Response {
    Response::text("Hello, World!")
}

// With extractors
async fn get_user(Path(UserId(id)): Path<UserId>) -> Json<User> {
    Json(User { id, name: "Alice".into() })
}

// With multiple extractors
async fn create_user(
    Json(req): Json<CreateUserRequest>,
    State(ctx): State<AppContext>,
    Query(q): Query<QueryParams>,
) -> Result<Json<User>, MyError> {
    let service = ctx.require::<Arc<UserService>>()?;
    service.create(req).await
}

// With error handling
async fn delete_user(
    Path(UserId(id)): Path<UserId>,
    State(ctx): State<AppContext>,
) -> Result<StatusCode, UserError> {
    let service = ctx.require::<Arc<UserService>>()?;
    service.delete(id).await?;
    Ok(StatusCode::NoContent)
}
```

### Extractors

Extractors parse request data into typed values.

**Built-in Extractors:**

| Extractor | Extracts | Errors |
|-----------|----------|--------|
| `Path<T>` | URL path params | Parse error (400) |
| `Query<T>` | Query string params | Parse error (400) |
| `Json<T>` | Request body (JSON) | JSON parse error (400) |
| `State<T>` | AppContext | Never (always present) |
| `Request` | Raw request | Never |
| `Extensions` | Per-request data | Never |
| `Body` | Async byte stream | I/O error |

**Example Implementations:**

```rust
use moduvex_http::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct UserId(i32);

#[derive(Deserialize)]
struct CreateUserRequest {
    name: String,
    email: String,
}

#[derive(Serialize)]
struct User {
    id: i32,
    name: String,
    email: String,
}

// GET /users/:id
async fn get_user(Path(UserId(id)): Path<UserId>) -> Json<User> {
    Json(User {
        id,
        name: "Alice".into(),
        email: "alice@example.com".into(),
    })
}

// POST /users?sendmail=true
#[derive(Deserialize)]
struct SignupQuery {
    sendmail: bool,
}

async fn create_user(
    Json(req): Json<CreateUserRequest>,
    Query(q): Query<SignupQuery>,
) -> Json<User> {
    // Use req and q...
    Json(User {
        id: 1,
        name: req.name,
        email: req.email,
    })
}
```

### Custom Extractors

Implement `FromRequest<'a>` for custom extraction logic.

```rust
use moduvex_http::FromRequest;

pub struct UserId(i32);

impl<'a> FromRequest<'a> for UserId {
    async fn from_request(req: &'a Request) -> Result<Self> {
        let id_str = req.path_param("id")?;
        let id: i32 = id_str.parse()?;
        if id <= 0 {
            return Err(ModuvexError::Domain(UserError::InvalidId));
        }
        Ok(UserId(id))
    }
}

// Now use in handler:
async fn get_user(Path(id): Path<UserId>) -> Json<User> {
    // id.0 is already validated
    Json(User { id: id.0, name: "Alice".into() })
}
```

---

## Configuration Management

### Loading Configuration

Configuration is loaded once at startup and scoped by module.

**Basic Loading:**

```rust
use moduvex_config::ConfigLoader;
use std::path::Path;

let config = ConfigLoader::load("app", Path::new(".")).unwrap();
```

**Load Priority:**
1. Environment variables: `MODUVEX__SERVER__PORT=3000`
2. Profile overlay: `app-prod.toml`
3. Base file: `app.toml`
4. Embedded defaults (from starters)

**Configuration Files:**

```toml
# app.toml (base)
[server]
port = 8080
host = "0.0.0.0"

[database]
url = "postgres://localhost/mydb"
pool_size = 10

# app-prod.toml (overlay for prod profile)
[server]
port = 80
host = "0.0.0.0"

[database]
pool_size = 50
```

### Scoped Configuration

Extract typed config sections per module.

```rust
use serde::Deserialize;

#[derive(Deserialize, Clone)]
struct ServerConfig {
    port: u16,
    host: String,
}

#[derive(Deserialize, Clone)]
struct DatabaseConfig {
    url: String,
    pool_size: u32,
}

// In Module::config():
impl ModuleLifecycle for ServerModule {
    async fn config(&mut self, loader: &ConfigLoader) -> Result<()> {
        self.server_config = loader.scope::<ServerConfig>("server")?;
        Ok(())
    }
}
```

### Environment Variable Overrides

Environment variables override file config using `MODUVEX__` prefix.

```bash
# Override server.port
export MODUVEX__SERVER__PORT=3000

# Override database.url
export MODUVEX__DATABASE__URL="postgres://prod-db:5432/mydb"
```

---

## Database Access

### Connection Pool Setup

Create a pool once during Init phase, then share via DI.

```rust
use moduvex_db::{ConnectionPool, PoolConfig};
use std::sync::Arc;

impl ModuleLifecycle for DatabaseModule {
    async fn init(&self, ctx: &mut ProviderContext) -> Result<()> {
        let cfg = PoolConfig::new("postgres://localhost/mydb");
        let pool = Arc::new(ConnectionPool::new(cfg));
        ctx.insert(pool);
        Ok(())
    }
}
```

### Query Building

Use `QueryBuilder` for type-safe SQL construction.

```rust
use moduvex_db::{QueryBuilder, Order};

async fn find_users(pool: Arc<ConnectionPool>) -> Result<Vec<User>> {
    let mut conn = pool.acquire().await?;

    let sql = QueryBuilder::select("users")?
        .columns(&["id", "name", "email"])?
        .where_eq("active", true)?
        .order_by("name", Order::Asc)?
        .limit(100)
        .build_inlined()?;

    let rows = conn.query(&sql).await?;

    let mut users = Vec::new();
    for row in rows.iter() {
        let id: i32 = row.get("id")?;
        let name: String = row.get("name")?;
        let email: String = row.get("email")?;
        users.push(User { id, name, email });
    }

    pool.release(conn).await;
    Ok(users)
}
```

### Transactions

Use transactions for multi-statement operations with atomicity.

```rust
use moduvex_db::IsolationLevel;

async fn transfer_funds(
    pool: Arc<ConnectionPool>,
    from_id: i32,
    to_id: i32,
    amount: f64,
) -> Result<()> {
    let mut tx = pool.transaction(IsolationLevel::Serializable).await?;

    // Debit source
    tx.execute(&format!(
        "UPDATE accounts SET balance = balance - {} WHERE id = {}",
        amount, from_id
    )).await?;

    // Credit destination
    tx.execute(&format!(
        "UPDATE accounts SET balance = balance + {} WHERE id = {}",
        amount, to_id
    )).await?;

    tx.commit().await?;
    Ok(())
}

// Auto-rollback on Drop if not committed
```

### Parameterized Queries

Use parameter binding to prevent SQL injection.

```rust
use moduvex_db::Param;

async fn insert_user(
    pool: Arc<ConnectionPool>,
    name: &str,
    email: &str,
) -> Result<()> {
    let mut conn = pool.acquire().await?;

    let sql = format!(
        "INSERT INTO users (name, email) VALUES ({}, {})",
        Param::new(name).placeholder(),
        Param::new(email).placeholder()
    );

    let params = vec![
        Param::new(name),
        Param::new(email),
    ];

    conn.execute_with_params(&sql, params).await?;
    pool.release(conn).await;
    Ok(())
}
```

---

## Error Handling

### Domain Errors (Business Logic)

Business logic errors map to HTTP status codes.

```rust
use moduvex_macros::DomainError;

#[derive(DomainError)]
enum UserError {
    #[error(code = "USER_NOT_FOUND", status = 404)]
    NotFound(i32),

    #[error(code = "EMAIL_EXISTS", status = 409)]
    EmailAlreadyExists(String),

    #[error(code = "INVALID_EMAIL", status = 400)]
    InvalidEmail(String),
}

// Handler returns domain error → automatic HTTP response
async fn create_user(Json(req): Json<CreateUserRequest>) -> Result<Json<User>, UserError> {
    if !req.email.contains('@') {
        return Err(UserError::InvalidEmail(req.email));
    }
    Ok(Json(User { ... }))
}

// UserError::InvalidEmail("bad") → HTTP 400 with error code "INVALID_EMAIL"
```

### Infrastructure Errors (System Issues)

Infrastructure errors indicate retryability.

```rust
use moduvex_macros::InfraError;

#[derive(InfraError)]
enum DbError {
    #[error(retryable = true)]
    ConnectionLost(String),

    #[error(retryable = false)]
    InvalidQuery(String),
}

// Handler can catch and retry:
let result = match db_operation().await {
    Err(DbError::ConnectionLost(msg)) => {
        // Retry logic here
        db_operation().await
    }
    Err(DbError::InvalidQuery(msg)) => {
        // Don't retry, fail fast
        Err(ModuvexError::Infra(DbError::InvalidQuery(msg)))
    }
    Ok(val) => Ok(val),
};
```

### Error Chaining

Chain errors with context for debugging.

```rust
use moduvex_core::ErrorContext;

async fn complex_operation() -> Result<User> {
    let config = load_config()
        .context("failed to load configuration")?;

    let repo = UserRepository::new(&config)
        .context("failed to initialize repository")?;

    let user = repo.find(1)
        .await
        .context("failed to find user")?;

    Ok(user)
}

// Error chain on failure:
// "failed to find user (caused by: failed to initialize repository (caused by: failed to load configuration))"
```

---

## Logging & Observability

### Structured Logging

Emit structured log events with key-value fields.

```rust
use moduvex_observe::prelude::*;

info!("user created", user_id = 123, email = "alice@example.com");
warn!("slow query", duration_ms = 500, query = "SELECT ...");
error!("request failed", status = 500, error = "internal error");

// Pretty format output:
// [INFO] user created user_id=123 email=alice@example.com
// [WARN] slow query duration_ms=500 query=SELECT ...
// [ERROR] request failed status=500 error=internal error
```

### Spans & Tracing

Trace operation execution with spans.

```rust
use moduvex_observe::Span;

async fn process_order(order_id: i32) -> Result<()> {
    let span = Span::new("process_order");
    span.set_attribute("order_id", order_id);

    // ... processing ...

    span.end();
    Ok(())
}

// Distributed trace:
// trace_id: 1234567890abcdef
// span_id: 0987654321fedcba
// parent_span_id: abcdef1234567890
```

### Metrics

Record application metrics.

```rust
use moduvex_observe::{Counter, Gauge, Histogram};

let requests = Counter::new("http_requests_total", "Total HTTP requests");
requests.inc();

let active = Gauge::new("http_requests_active", "Active HTTP requests");
active.inc();
// ... handle request ...
active.dec();

let latency = Histogram::new("http_request_duration_ms", "Request latency");
latency.record(42.0);
```

---

## Testing

### Unit Testing

Test individual components in isolation.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_user_valid() {
        let req = CreateUserRequest {
            name: "Alice".into(),
            email: "alice@example.com".into(),
        };

        let result = CreateUserRequest::validate(&req);
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_user_invalid_email() {
        let req = CreateUserRequest {
            name: "Alice".into(),
            email: "not-an-email".into(),
        };

        let result = CreateUserRequest::validate(&req);
        assert!(result.is_err());
    }
}
```

### Integration Testing

Test module integration and lifecycle.

```rust
#[moduvex::main]
async fn test_user_module_integration() {
    let config = ConfigLoader::from_defaults("...").unwrap();
    let ctx = Moduvex::new()
        .config(config)
        .module::<UserModule>()
        .run()
        .await;

    let service = ctx.require::<Arc<UserService>>().unwrap();
    let user = service.create(...).await.unwrap();
    assert_eq!(user.id, 1);
}
```

---

## Best Practices

1. **Keep modules focused** — One responsibility per module
2. **Use DI for services** — Share via Arc<T>, not globals
3. **Validate at boundaries** — Check user input early
4. **Chain errors** — Use `.context()` for debugging
5. **Log structure** — Use key-value fields, not string formatting
6. **Type-safe config** — Deserialize to typed structs
7. **Transactions for multi-step** — Ensure atomicity
8. **Test async code** — Use #[moduvex::main] for runtime
9. **Document extractors** — Explain validation rules
10. **Monitor metrics** — Counter/Gauge for observability

---

**Last Updated:** Phase 8 (Documentation)
**Audience:** Developers using Moduvex
