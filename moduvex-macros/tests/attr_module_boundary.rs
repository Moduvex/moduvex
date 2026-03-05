//! Integration tests for the `#[moduvex::module]` attribute macro.
//!
//! Tests the visibility-rewriting rules:
//! - `pub` without `#[export]` → private (pub(self))
//! - `pub` with `#[export]` → stays pub, attribute stripped
//! - private items → unchanged
//! - impl/use/nested mod → passed through
//!
//! Note: compile-fail cases (pub(crate), pub(super), file-based mod name;)
//! cannot be tested here without trybuild. The positive cases below confirm
//! the happy-path behaviour that users depend on.

// ---------------------------------------------------------------------------
// Basic visibility rewriting — pub without #[export] becomes private
// ---------------------------------------------------------------------------

/// Items that are `pub` but not exported must not be accessible outside
/// the module. We verify the macro does NOT error and that the exported
/// item IS accessible.
#[moduvex_macros::module]
mod boundary_basic {
    /// Not exported — pub rewritten to private inside the module.
    pub struct Internal;

    /// Exported — pub preserved, #[export] stripped.
    #[export]
    pub struct Exported;

    /// Private item — unchanged.
    struct AlsoPrivate;

    // impl and use blocks pass through unchanged.
    impl Exported {
        pub fn hello(&self) -> &'static str {
            "hello"
        }
    }

    impl AlsoPrivate {
        #[allow(dead_code)]
        fn secret() -> u32 {
            42
        }
    }
}

#[test]
fn exported_struct_is_accessible() {
    let e = boundary_basic::Exported;
    assert_eq!(e.hello(), "hello");
}

// ---------------------------------------------------------------------------
// #[export] is stripped from the final item — no attribute remains
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod export_attr_stripped {
    #[export]
    pub struct PublicApi {
        pub value: u32,
    }
}

#[test]
fn exported_struct_has_no_export_attr_at_runtime() {
    // If #[export] were not stripped, unknown-attribute compilation would fail.
    // Reaching here means the attribute was correctly removed.
    let api = export_attr_stripped::PublicApi { value: 7 };
    assert_eq!(api.value, 7);
}

// ---------------------------------------------------------------------------
// Enums inside module boundary
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod enum_boundary {
    #[export]
    pub enum Status {
        Active,
        Inactive,
    }

    /// Not exported — becomes private.
    pub enum InternalState {
        Running,
    }
}

#[test]
fn exported_enum_is_accessible() {
    let s = enum_boundary::Status::Active;
    assert!(matches!(s, enum_boundary::Status::Active));
}

// ---------------------------------------------------------------------------
// Functions inside module boundary
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod fn_boundary {
    #[export]
    pub fn compute(x: u32) -> u32 {
        x * 2
    }

    /// Not exported — becomes private.
    pub fn internal_helper() -> u32 {
        99
    }
}

#[test]
fn exported_fn_is_callable() {
    assert_eq!(fn_boundary::compute(5), 10);
}

// ---------------------------------------------------------------------------
// Constants and statics inside module boundary
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod const_boundary {
    #[export]
    pub const MAX_RETRIES: u32 = 3;

    #[export]
    pub static APP_NAME: &str = "moduvex";

    /// Not exported.
    pub const INTERNAL_LIMIT: u32 = 100;
}

#[test]
fn exported_const_is_accessible() {
    assert_eq!(const_boundary::MAX_RETRIES, 3);
}

#[test]
fn exported_static_is_accessible() {
    assert_eq!(const_boundary::APP_NAME, "moduvex");
}

// ---------------------------------------------------------------------------
// Type aliases inside module boundary
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod type_boundary {
    #[export]
    pub type UserId = u64;

    /// Not exported.
    pub type InternalId = u32;
}

#[test]
fn exported_type_alias_is_usable() {
    let id: type_boundary::UserId = 42;
    assert_eq!(id, 42u64);
}

// ---------------------------------------------------------------------------
// Traits inside module boundary
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod trait_boundary {
    #[export]
    pub trait Greet {
        fn greet(&self) -> &'static str;
    }

    /// Not exported.
    pub trait InternalMarker {}

    /// Exported so the test can construct it.
    #[export]
    pub struct Greeter;

    impl Greet for Greeter {
        fn greet(&self) -> &'static str {
            "hi"
        }
    }

    // impl blocks pass through unchanged
    impl InternalMarker for Greeter {}
}

#[test]
fn exported_trait_impl_works() {
    use trait_boundary::Greet;
    let g = trait_boundary::Greeter;
    assert_eq!(g.greet(), "hi");
}

#[test]
fn exported_struct_satisfies_exported_trait() {
    fn _assert_impl<T: trait_boundary::Greet>() {}
    _assert_impl::<trait_boundary::Greeter>();
}

// ---------------------------------------------------------------------------
// Empty module — no items, should compile fine
// ---------------------------------------------------------------------------

#[moduvex_macros::module]
mod empty_module {}

#[test]
fn empty_module_compiles() {
    // Just reaching here proves the macro handled an empty block correctly.
    let _: () = ();
}
