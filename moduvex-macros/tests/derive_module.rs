//! Integration tests for `#[derive(Module)]`.
//!
//! Each test expands the macro in a real compilation unit and asserts on the
//! generated `Module` and `DependsOn` implementations at runtime.

use moduvex_core::{DependsOn, Module};

// ---------------------------------------------------------------------------
// Basic unit-struct derivation
// ---------------------------------------------------------------------------

/// Minimal module — no attributes, unit struct.
#[derive(moduvex_macros::Module)]
struct BasicModule;

#[test]
fn basic_module_name_is_struct_name() {
    assert_eq!(BasicModule.name(), "BasicModule");
}

#[test]
fn basic_module_default_priority_is_zero() {
    assert_eq!(BasicModule.priority(), 0);
}

#[test]
fn basic_module_has_no_deps() {
    // Required = () means the tuple is unit
    fn _assert_unit_deps<M: DependsOn<Required = ()>>() {}
    _assert_unit_deps::<BasicModule>();
}

// ---------------------------------------------------------------------------
// Named-field struct (also valid for #[derive(Module)])
// ---------------------------------------------------------------------------

/// Module with named fields — still valid because darling supports struct_named.
#[derive(moduvex_macros::Module)]
#[module(priority = 5)]
struct NamedFieldModule {
    #[allow(dead_code)]
    label: &'static str,
}

#[test]
fn named_field_module_name() {
    let m = NamedFieldModule { label: "test" };
    assert_eq!(m.name(), "NamedFieldModule");
}

#[test]
fn named_field_module_custom_priority() {
    let m = NamedFieldModule { label: "test" };
    assert_eq!(m.priority(), 5);
}

// ---------------------------------------------------------------------------
// Priority attribute
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::Module)]
#[module(priority = 42)]
struct HighPriorityModule;

#[test]
fn module_with_explicit_priority() {
    assert_eq!(HighPriorityModule.priority(), 42);
}

#[derive(moduvex_macros::Module)]
#[module(priority = -10)]
struct NegativePriorityModule;

#[test]
fn module_with_negative_priority() {
    assert_eq!(NegativePriorityModule.priority(), -10);
}

// ---------------------------------------------------------------------------
// depends_on attribute — single dependency
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::Module)]
struct DepA;

#[derive(moduvex_macros::Module)]
#[module(depends_on(DepA))]
struct SingleDepModule;

#[test]
fn single_dep_module_name() {
    assert_eq!(SingleDepModule.name(), "SingleDepModule");
}

#[test]
fn single_dep_module_required_type_is_nested_tuple() {
    // The macro produces: type Required = (DepA, ());
    fn _check<M: DependsOn<Required = (DepA, ())>>() {}
    _check::<SingleDepModule>();
}

// ---------------------------------------------------------------------------
// depends_on attribute — two dependencies
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::Module)]
struct DepB;

#[derive(moduvex_macros::Module)]
#[module(depends_on(DepA, DepB))]
struct TwoDepModule;

#[test]
fn two_dep_module_required_type_is_doubly_nested() {
    // Generated: type Required = (DepA, (DepB, ()))
    fn _check<M: DependsOn<Required = (DepA, (DepB, ()))>>() {}
    _check::<TwoDepModule>();
}

// ---------------------------------------------------------------------------
// depends_on + priority together
// ---------------------------------------------------------------------------

#[derive(moduvex_macros::Module)]
#[module(depends_on(DepA), priority = 7)]
struct DepAndPriorityModule;

#[test]
fn dep_and_priority_module() {
    assert_eq!(DepAndPriorityModule.name(), "DepAndPriorityModule");
    assert_eq!(DepAndPriorityModule.priority(), 7);
    fn _check<M: DependsOn<Required = (DepA, ())>>() {}
    _check::<DepAndPriorityModule>();
}

// ---------------------------------------------------------------------------
// Module is Send + Sync (required by the trait bound)
// ---------------------------------------------------------------------------

#[test]
fn module_is_send_sync() {
    fn _assert_send_sync<T: Send + Sync>() {}
    _assert_send_sync::<BasicModule>();
    _assert_send_sync::<SingleDepModule>();
    _assert_send_sync::<TwoDepModule>();
}
