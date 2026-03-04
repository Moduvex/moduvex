//! Proc macros for the Moduvex framework.
//!
//! Provides derive macros and attribute macros that generate trait
//! implementations for moduvex-core types, transforming verbose
//! manual impls into ergonomic annotations.

mod component_derive;
mod error_derive;
mod main_attr;
mod module_attr;
mod module_derive;
mod utils;

use proc_macro::TokenStream;

/// Derive the `Module` trait for a struct.
///
/// # Attributes
/// - `#[module(depends_on(DepA, DepB))]` — declares module dependencies
/// - `#[module(priority = N)]` — sets boot priority (default: 0)
///
/// # Example
/// ```ignore
/// #[derive(Module)]
/// #[module(depends_on(SharedModule, AuthModule))]
/// struct UserModule;
/// ```
#[proc_macro_derive(Module, attributes(module))]
pub fn derive_module(input: TokenStream) -> TokenStream {
    module_derive::expand(input.into())
        .unwrap_or_else(|e| e.write_errors())
        .into()
}

/// Derive the `Inject` and `Provider` traits for a struct.
///
/// Fields marked with `#[inject]` are resolved from `AppContext`.
/// Fields without `#[inject]` must implement `Default`.
///
/// # Attributes
/// - `#[inject]` — resolve this field from AppContext
/// - `#[inject(optional)]` — field becomes `Option<T>`, returns None if missing
///
/// # Example
/// ```ignore
/// #[derive(Component)]
/// struct UserService {
///     #[inject] repo: Arc<UserRepository>,
///     #[inject] auth: Arc<AuthService>,
/// }
/// ```
#[proc_macro_derive(Component, attributes(inject))]
pub fn derive_component(input: TokenStream) -> TokenStream {
    component_derive::expand(input.into())
        .unwrap_or_else(|e| e.write_errors())
        .into()
}

/// Derive the `DomainError` trait for an enum.
///
/// Each variant must have `#[error(code = "...", status = NNN)]`.
///
/// # Example
/// ```ignore
/// #[derive(DomainError)]
/// enum UserError {
///     #[error(code = "USER_NOT_FOUND", status = 404)]
///     NotFound(UserId),
///     #[error(code = "EMAIL_EXISTS", status = 409)]
///     AlreadyExists(Email),
/// }
/// ```
#[proc_macro_derive(DomainError, attributes(error))]
pub fn derive_domain_error(input: TokenStream) -> TokenStream {
    error_derive::expand_domain(input.into())
        .unwrap_or_else(|e| e.write_errors())
        .into()
}

/// Derive the `InfraError` trait for an enum.
///
/// Each variant can have `#[error(retryable = true/false)]`.
///
/// # Example
/// ```ignore
/// #[derive(InfraError)]
/// enum DbError {
///     #[error(retryable = true)]
///     ConnectionLost(String),
///     #[error(retryable = false)]
///     InvalidQuery(String),
/// }
/// ```
#[proc_macro_derive(InfraError, attributes(error))]
pub fn derive_infra_error(input: TokenStream) -> TokenStream {
    error_derive::expand_infra(input.into())
        .unwrap_or_else(|e| e.write_errors())
        .into()
}

/// Attribute macro that wraps an `async fn main` with runtime bootstrap.
///
/// # Attributes
/// - `#[moduvex::main]` — default single-threaded runtime
/// - `#[moduvex::main(threads = 4)]` — multi-threaded runtime
///
/// # Example
/// ```ignore
/// #[moduvex::main]
/// async fn main() {
///     Moduvex::new()
///         .config("app.toml")
///         .module(UserModule)
///         .run()
///         .await
///         .unwrap();
/// }
/// ```
#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    main_attr::expand(args.into(), item.into())
        .unwrap_or_else(|e| e.into_compile_error())
        .into()
}

/// Attribute macro enforcing module boundary visibility.
///
/// Items with `pub` but no `#[export]` are rewritten to `pub(self)`.
/// Items with `#[export]` keep their `pub` visibility.
/// `pub(crate)`, `pub(super)`, `pub(in ...)` emit compile errors.
///
/// **Limitation:** Only works on inline `mod { }` blocks, not file-based `mod name;`.
///
/// # Example
/// ```ignore
/// #[moduvex::module]
/// mod user {
///     pub struct Internal;           // rewritten to pub(self)
///
///     #[export]
///     pub struct UserServiceApi;     // stays pub
/// }
/// ```
#[proc_macro_attribute]
pub fn module(args: TokenStream, item: TokenStream) -> TokenStream {
    module_attr::expand(args.into(), item.into())
        .unwrap_or_else(|e| e.into_compile_error())
        .into()
}
