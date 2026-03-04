//! Shared helpers for proc macro code generation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

/// Returns the token path `::moduvex_core` for fully-qualified references
/// in generated code.
pub fn core_path() -> TokenStream {
    quote!(::moduvex_core)
}

/// Build a right-nested tuple type-list from a slice of types.
///
/// `[A, B, C]` becomes `(A, (B, (C, ())))`.
/// `[]` becomes `()`.
pub fn build_nested_tuple(types: &[Type]) -> TokenStream {
    let mut result = quote!(());
    for ty in types.iter().rev() {
        result = quote!((#ty, #result));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn nested_tuple_empty() {
        let result = build_nested_tuple(&[]);
        assert_eq!(result.to_string(), "()");
    }

    #[test]
    fn nested_tuple_single() {
        let types: Vec<Type> = vec![parse_quote!(Foo)];
        let result = build_nested_tuple(&types);
        assert_eq!(result.to_string(), "(Foo , ())");
    }

    #[test]
    fn nested_tuple_multiple() {
        let types: Vec<Type> = vec![parse_quote!(A), parse_quote!(B), parse_quote!(C)];
        let result = build_nested_tuple(&types);
        assert_eq!(result.to_string(), "(A , (B , (C , ())))");
    }
}
