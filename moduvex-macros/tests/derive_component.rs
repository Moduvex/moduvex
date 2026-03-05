//! Integration tests for `#[derive(Component)]`.
//!
//! Verifies that the macro correctly generates `Inject` and `Provider`
//! implementations for named structs with various `#[inject]` field patterns.

use std::sync::Arc;

use moduvex_core::{AppContext, Inject, Provider};

// ---------------------------------------------------------------------------
// All-default component (no #[inject] fields)
// ---------------------------------------------------------------------------

/// Component with no injected fields — all fields resolved via Default.
#[derive(moduvex_macros::Component)]
struct AllDefaultComponent {
    counter: u32,
    label: String,
}

#[test]
fn all_default_component_resolves_with_defaults() {
    let ctx = AppContext::new();
    let c = AllDefaultComponent::resolve(&ctx).expect("resolve should succeed");
    assert_eq!(c.counter, 0);
    assert!(c.label.is_empty());
}

#[test]
fn all_default_component_provide_matches_resolve() {
    let ctx = AppContext::new();
    let instance = AllDefaultComponent { counter: 0, label: String::new() };
    let provided = instance.provide(&ctx).expect("provide should succeed");
    assert_eq!(provided.counter, 0);
}

// ---------------------------------------------------------------------------
// Single #[inject] field
// ---------------------------------------------------------------------------

/// Service type stored as a singleton in AppContext.
#[derive(Clone)]
struct DbPool {
    url: String,
}

/// Component that requires a DbPool singleton.
#[derive(moduvex_macros::Component)]
struct RepoComponent {
    #[inject]
    pool: DbPool,
}


#[test]
fn inject_required_field_resolves_when_singleton_present() {
    let ctx = AppContext::new();
    ctx.insert(Arc::new(DbPool { url: "postgres://localhost/test".into() }));
    let repo = RepoComponent::resolve(&ctx).expect("resolve should succeed");
    assert_eq!(repo.pool.url, "postgres://localhost/test");
}

#[test]
fn inject_required_field_errors_when_singleton_missing() {
    let ctx = AppContext::new();
    let result = RepoComponent::resolve(&ctx);
    assert!(result.is_err(), "should fail when DbPool not in context");
}

// ---------------------------------------------------------------------------
// Mixed: #[inject] and non-inject fields
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AuthService {
    secret: String,
}

#[derive(moduvex_macros::Component)]
struct UserService {
    #[inject]
    auth: AuthService,
    request_count: u64, // no #[inject] — uses Default
}

#[test]
fn mixed_component_injects_and_defaults() {
    let ctx = AppContext::new();
    ctx.insert(Arc::new(AuthService { secret: "s3cr3t".into() }));
    let svc = UserService::resolve(&ctx).expect("resolve should succeed");
    assert_eq!(svc.auth.secret, "s3cr3t");
    assert_eq!(svc.request_count, 0); // Default for u64
}

// ---------------------------------------------------------------------------
// Multiple #[inject] fields — two different singletons
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ConfigStore {
    env: String,
}

#[derive(moduvex_macros::Component)]
struct FullService {
    #[inject]
    auth: AuthService,
    #[inject]
    config: ConfigStore,
    call_count: u32,
}

#[test]
fn multi_inject_resolves_all_fields() {
    let ctx = AppContext::new();
    ctx.insert(Arc::new(AuthService { secret: "key".into() }));
    ctx.insert(Arc::new(ConfigStore { env: "prod".into() }));
    let svc = FullService::resolve(&ctx).expect("resolve should succeed");
    assert_eq!(svc.auth.secret, "key");
    assert_eq!(svc.config.env, "prod");
    assert_eq!(svc.call_count, 0);
}

#[test]
fn multi_inject_errors_if_any_singleton_missing() {
    let ctx = AppContext::new();
    // Only one of two required singletons registered
    ctx.insert(Arc::new(AuthService { secret: "key".into() }));
    assert!(FullService::resolve(&ctx).is_err());
}

// ---------------------------------------------------------------------------
// Provider trait — provide() delegates to resolve()
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::Component)]
struct SimpleService {
    value: i32,
}

#[test]
fn provider_output_type_is_self() {
    fn _assert_output<C: Provider<Output = C>>() {}
    _assert_output::<SimpleService>();
}

#[test]
fn provider_provide_returns_fresh_instance() {
    let ctx = AppContext::new();
    let s = SimpleService { value: 0 };
    let out = s.provide(&ctx).expect("provide should succeed");
    assert_eq!(out.value, 0);
}

// ---------------------------------------------------------------------------
// Component is Send + Sync
// ---------------------------------------------------------------------------

#[test]
fn component_impls_are_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<AllDefaultComponent>();
    _assert::<SimpleService>();
}
