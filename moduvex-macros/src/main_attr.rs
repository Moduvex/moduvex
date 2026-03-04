//! `#[moduvex::main]` — wraps async fn main with runtime bootstrap.
//!
//! Transforms `async fn main() { ... }` into a sync main that creates
//! a moduvex-runtime Runtime and calls `block_on` with the async body.
//! Supports `#[moduvex::main(threads = N)]` for multi-threaded config.
//!
//! # Result handling
//!
//! If the annotated `async fn main()` has a `Result` return type, the
//! generated code propagates errors: on `Err(e)`, it prints the error to
//! stderr and exits with code 1. Unit return types are left as-is.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Expr, ItemFn, Lit, Meta, ReturnType};

/// Expand `#[moduvex::main]` or `#[moduvex::main(threads = N)]`.
pub fn expand(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let func: ItemFn = syn::parse2(item)?;

    // Validate: must be async
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            func.sig.fn_token,
            "#[moduvex::main] requires an async fn",
        ));
    }

    // Validate: must be named "main"
    if func.sig.ident != "main" {
        return Err(syn::Error::new_spanned(
            &func.sig.ident,
            "#[moduvex::main] can only be applied to fn main",
        ));
    }

    // Validate: no arguments
    if !func.sig.inputs.is_empty() {
        return Err(syn::Error::new_spanned(
            &func.sig.inputs,
            "#[moduvex::main] fn main must take no arguments",
        ));
    }

    // Detect whether the return type is `Result<...>` (non-unit).
    let returns_result = match &func.sig.output {
        ReturnType::Default => false,
        ReturnType::Type(_, ty) => is_result_type(ty),
    };

    // Parse optional threads = N from args
    let threads = parse_threads_arg(args)?;
    let body = &func.block;

    let runtime_builder = if let Some(n) = threads {
        quote! {
            ::moduvex_runtime::RuntimeBuilder::new()
                .worker_threads(#n)
                .build()
        }
    } else {
        quote! {
            ::moduvex_runtime::Runtime::new()
        }
    };

    // Generate the block_on call, handling Result returns.
    let block_on_call = if returns_result {
        quote! {
            let result = rt.block_on(async #body);
            if let Err(e) = result {
                eprintln!("Error: {e}");
                ::std::process::exit(1);
            }
        }
    } else {
        quote! {
            rt.block_on(async #body);
        }
    };

    Ok(quote! {
        fn main() {
            let rt = #runtime_builder;
            #block_on_call
        }
    })
}

/// Returns `true` if `ty` looks like a `Result<…>` type.
///
/// Matches bare `Result`, path-qualified `std::result::Result`,
/// `core::result::Result`, and single-segment `Result<…>` generics.
fn is_result_type(ty: &syn::Type) -> bool {
    if let syn::Type::Path(type_path) = ty {
        // Ignore any leading `self::` / `crate::` qualifier for the check.
        if let Some(last) = type_path.path.segments.last() {
            return last.ident == "Result";
        }
    }
    false
}

/// Parse `threads = N` from the attribute argument list.
fn parse_threads_arg(args: TokenStream) -> syn::Result<Option<usize>> {
    if args.is_empty() {
        return Ok(None);
    }

    let meta: Meta = syn::parse2(args)?;
    match meta {
        Meta::NameValue(nv) if nv.path.is_ident("threads") => {
            if let Expr::Lit(expr_lit) = &nv.value {
                if let Lit::Int(i) = &expr_lit.lit {
                    let n = i.base10_parse::<usize>()?;
                    if n == 0 {
                        return Err(syn::Error::new_spanned(i, "threads must be > 0"));
                    }
                    return Ok(Some(n));
                }
            }
            Err(syn::Error::new_spanned(
                &nv.value,
                "expected integer for threads",
            ))
        }
        other => Err(syn::Error::new_spanned(other, "expected `threads = N`")),
    }
}
