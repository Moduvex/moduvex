//! `#[derive(DomainError)]` and `#[derive(InfraError)]` — generate error trait impls.
//!
//! DomainError: parses `#[error(code = "...", status = NNN)]` per variant.
//! InfraError: parses `#[error(retryable = true/false)]` per variant.
//! Both generate Display, std::error::Error, and From<Self> for ModuvexError.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Expr, Fields, Lit, Meta};

use crate::utils;

/// Parsed per-variant attributes for DomainError.
struct DomainVariantAttrs {
    code: String,
    status: u16,
}

/// Parsed per-variant attributes for InfraError.
struct InfraVariantAttrs {
    retryable: bool,
}

// ---------------------------------------------------------------------------
// DomainError
// ---------------------------------------------------------------------------

/// Expand `#[derive(DomainError)]` on an enum.
pub fn expand_domain(input: TokenStream) -> darling::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let enum_name = &input.ident;
    let core = utils::core_path();

    let variants = match &input.data {
        syn::Data::Enum(e) => &e.variants,
        _ => {
            return Err(darling::Error::unsupported_shape(
                "DomainError can only be derived on enums",
            ))
        }
    };

    let mut display_arms = Vec::new();
    let mut code_arms = Vec::new();
    let mut status_arms = Vec::new();

    for variant in variants {
        let vname = &variant.ident;
        let attrs = parse_domain_attrs(variant)?;

        // Validate HTTP status range
        if !(100..=599).contains(&attrs.status) {
            return Err(darling::Error::custom(format!(
                "invalid HTTP status {}: must be 100-599",
                attrs.status
            ))
            .with_span(variant));
        }

        let code_str = &attrs.code;
        let status_val = attrs.status;
        let pattern = variant_match_pattern(vname, &variant.fields);

        display_arms.push(quote! {
            Self::#pattern => write!(f, "{}", #code_str)
        });
        code_arms.push(quote! {
            Self::#pattern => #code_str
        });
        status_arms.push(quote! {
            Self::#pattern => #status_val
        });
    }

    Ok(quote! {
        impl ::std::fmt::Display for #enum_name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match self {
                    #(#display_arms),*
                }
            }
        }

        impl ::std::error::Error for #enum_name {}

        impl #core::DomainError for #enum_name {
            fn error_code(&self) -> &str {
                match self {
                    #(#code_arms),*
                }
            }
            fn http_status(&self) -> u16 {
                match self {
                    #(#status_arms),*
                }
            }
        }

        impl From<#enum_name> for #core::ModuvexError {
            fn from(e: #enum_name) -> Self {
                #core::ModuvexError::Domain(Box::new(e))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// InfraError
// ---------------------------------------------------------------------------

/// Expand `#[derive(InfraError)]` on an enum.
pub fn expand_infra(input: TokenStream) -> darling::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let enum_name = &input.ident;
    let core = utils::core_path();

    let variants = match &input.data {
        syn::Data::Enum(e) => &e.variants,
        _ => {
            return Err(darling::Error::unsupported_shape(
                "InfraError can only be derived on enums",
            ))
        }
    };

    let mut display_arms = Vec::new();
    let mut retryable_arms = Vec::new();

    for variant in variants {
        let vname = &variant.ident;
        let attrs = parse_infra_attrs(variant)?;
        let retryable_val = attrs.retryable;
        let variant_str = vname.to_string();
        let pattern = variant_match_pattern(vname, &variant.fields);

        display_arms.push(quote! {
            Self::#pattern => write!(f, "{}", #variant_str)
        });
        retryable_arms.push(quote! {
            Self::#pattern => #retryable_val
        });
    }

    Ok(quote! {
        impl ::std::fmt::Display for #enum_name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                match self {
                    #(#display_arms),*
                }
            }
        }

        impl ::std::error::Error for #enum_name {}

        impl #core::InfraError for #enum_name {
            fn is_retryable(&self) -> bool {
                match self {
                    #(#retryable_arms),*
                }
            }
        }

        impl From<#enum_name> for #core::ModuvexError {
            fn from(e: #enum_name) -> Self {
                #core::ModuvexError::Infra(Box::new(e))
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Generate a match pattern for a variant, ignoring field bindings.
/// Unit: `Variant`, Unnamed: `Variant(..)`, Named: `Variant { .. }`.
fn variant_match_pattern(vname: &syn::Ident, fields: &Fields) -> TokenStream {
    match fields {
        Fields::Unit => quote!(#vname),
        Fields::Unnamed(_) => quote!(#vname(..)),
        Fields::Named(_) => quote!(#vname { .. }),
    }
}

/// Parse `#[error(code = "...", status = NNN)]` from a variant's attributes.
fn parse_domain_attrs(variant: &syn::Variant) -> darling::Result<DomainVariantAttrs> {
    let mut code: Option<String> = None;
    let mut status: Option<u16> = None;

    for attr in &variant.attrs {
        if !attr.path().is_ident("error") {
            continue;
        }
        let nested = attr
            .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            .map_err(|e| darling::Error::custom(e.to_string()).with_span(attr))?;

        for meta in &nested {
            match meta {
                Meta::NameValue(nv) if nv.path.is_ident("code") => {
                    if let Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Str(s) = &expr_lit.lit {
                            code = Some(s.value());
                        }
                    }
                }
                Meta::NameValue(nv) if nv.path.is_ident("status") => {
                    if let Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Int(i) = &expr_lit.lit {
                            status =
                                Some(i.base10_parse::<u16>().map_err(|e| {
                                    darling::Error::custom(e.to_string()).with_span(i)
                                })?);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let code = code.ok_or_else(|| darling::Error::missing_field("code").with_span(variant))?;
    let status =
        status.ok_or_else(|| darling::Error::missing_field("status").with_span(variant))?;

    Ok(DomainVariantAttrs { code, status })
}

/// Parse `#[error(retryable = true/false)]` from a variant's attributes.
fn parse_infra_attrs(variant: &syn::Variant) -> darling::Result<InfraVariantAttrs> {
    let mut retryable: Option<bool> = None;

    for attr in &variant.attrs {
        if !attr.path().is_ident("error") {
            continue;
        }
        let nested = attr
            .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            .map_err(|e| darling::Error::custom(e.to_string()).with_span(attr))?;

        for meta in &nested {
            if let Meta::NameValue(nv) = meta {
                if nv.path.is_ident("retryable") {
                    if let Expr::Lit(expr_lit) = &nv.value {
                        if let Lit::Bool(b) = &expr_lit.lit {
                            retryable = Some(b.value());
                        }
                    }
                }
            }
        }
    }

    Ok(InfraVariantAttrs {
        retryable: retryable.unwrap_or(false),
    })
}
