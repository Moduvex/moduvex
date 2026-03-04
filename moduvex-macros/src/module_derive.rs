//! `#[derive(Module)]` — generates `Module` + `DependsOn` trait impls.
//!
//! Parses `#[module(depends_on(A, B), priority = N)]` attributes on a struct
//! and emits the corresponding trait implementations for moduvex-core.

use darling::{FromDeriveInput, FromMeta};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Type};

use crate::utils;

/// Parsed representation of `#[module(...)]` attributes.
#[derive(Debug, FromDeriveInput)]
#[darling(attributes(module), supports(struct_unit, struct_named))]
struct ModuleOpts {
    ident: syn::Ident,
    /// List of dependency module types: `depends_on(A, B)`
    #[darling(default)]
    depends_on: Option<DependsList>,
    /// Boot priority (lower = earlier). Default: 0
    #[darling(default)]
    priority: Option<i32>,
}

/// Wrapper to parse `depends_on(TypeA, TypeB)` as a list of types.
#[derive(Debug, Clone)]
struct DependsList(Vec<Type>);

impl FromMeta for DependsList {
    fn from_list(items: &[darling::ast::NestedMeta]) -> darling::Result<Self> {
        let mut types = Vec::new();
        for item in items {
            match item {
                darling::ast::NestedMeta::Meta(meta) => {
                    // Each item is a path like `SharedModule`
                    let path = match meta {
                        syn::Meta::Path(p) => p.clone(),
                        other => {
                            return Err(
                                darling::Error::unexpected_type("path").with_span(other),
                            );
                        }
                    };
                    types.push(Type::Path(syn::TypePath {
                        qself: None,
                        path,
                    }));
                }
                darling::ast::NestedMeta::Lit(lit) => {
                    return Err(
                        darling::Error::unexpected_lit_type(lit),
                    );
                }
            }
        }
        Ok(DependsList(types))
    }
}

/// Entry point: parse input and generate Module + DependsOn impls.
pub fn expand(input: TokenStream) -> darling::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let opts = ModuleOpts::from_derive_input(&input)?;
    let struct_name = &opts.ident;
    let name_str = struct_name.to_string();
    let priority = opts.priority.unwrap_or(0);
    let core = utils::core_path();

    let deps: Vec<Type> = opts
        .depends_on
        .map(|d| d.0)
        .unwrap_or_default();
    let required_type = utils::build_nested_tuple(&deps);

    let module_impl = quote! {
        impl #core::Module for #struct_name {
            fn name(&self) -> &'static str {
                #name_str
            }
            fn priority(&self) -> i32 {
                #priority
            }
        }
    };

    let depends_on_impl = quote! {
        impl #core::DependsOn for #struct_name {
            type Required = #required_type;
        }
    };

    Ok(quote! {
        #module_impl
        #depends_on_impl
    })
}
