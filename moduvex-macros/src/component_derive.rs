//! `#[derive(Component)]` — generates `Inject` + `Provider` trait impls.
//!
//! Fields marked `#[inject]` are resolved from `AppContext`.
//! Fields without `#[inject]` must implement `Default`.
//! `#[inject(optional)]` makes the field `Option<T>` and returns None if missing.

use darling::{FromDeriveInput, FromField};
use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

use crate::utils;

/// Top-level parsed struct for `#[derive(Component)]`.
#[derive(Debug, FromDeriveInput)]
#[darling(supports(struct_named))]
struct ComponentOpts {
    ident: syn::Ident,
    data: darling::ast::Data<(), FieldOpts>,
}

/// Per-field options parsed from `#[inject]` or `#[inject(...)]`.
#[derive(Debug, FromField)]
#[darling(attributes(inject))]
struct FieldOpts {
    ident: Option<syn::Ident>,
    ty: syn::Type,
    /// Whether this field has `#[inject]` at all (presence flag).
    /// darling sets this to true when the attribute is present.
    #[darling(default)]
    optional: bool,
}

impl FieldOpts {
    /// Check if this field has the `#[inject]` attribute.
    fn has_inject_attr(field: &syn::Field) -> bool {
        field.attrs.iter().any(|a| a.path().is_ident("inject"))
    }
}

/// Entry point: parse input and generate Inject + Provider impls.
pub fn expand(input: TokenStream) -> darling::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let opts = ComponentOpts::from_derive_input(&input)?;
    let struct_name = &opts.ident;
    let core = utils::core_path();

    let fields = opts
        .data
        .as_ref()
        .take_struct()
        .ok_or_else(|| darling::Error::unsupported_shape("expected named struct"))?;

    // Build field resolution expressions for Inject::resolve.
    let mut field_inits = Vec::new();
    let original_fields = match &input.data {
        syn::Data::Struct(s) => match &s.fields {
            syn::Fields::Named(n) => &n.named,
            _ => return Err(darling::Error::unsupported_shape("expected named struct")),
        },
        _ => return Err(darling::Error::unsupported_shape("expected struct")),
    };

    for (field_opts, orig_field) in fields.iter().zip(original_fields.iter()) {
        let field_name = field_opts
            .ident
            .as_ref()
            .expect("named struct fields have idents");
        let field_ty = &field_opts.ty;
        let has_inject = FieldOpts::has_inject_attr(orig_field);

        if has_inject {
            if field_opts.optional {
                // Optional injection: try to get, return None if missing.
                // The field type is Option<T>; we look up T in the context.
                // ctx.get::<T>() returns Option<Arc<T>>, map clones to Option<T>.
                field_inits.push(quote! {
                    #field_name: ctx.get::<#field_ty>().map(|arc| (*arc).clone())
                });
            } else {
                // Required injection: fail if missing
                field_inits.push(quote! {
                    #field_name: (*ctx.require::<#field_ty>()?).clone()
                });
            }
        } else {
            // No #[inject] — use Default
            field_inits.push(quote! {
                #field_name: ::std::default::Default::default()
            });
        }
    }

    let inject_impl = quote! {
        impl #core::Inject for #struct_name {
            fn resolve(ctx: &#core::AppContext) -> #core::Result<Self> {
                Ok(Self {
                    #(#field_inits),*
                })
            }
        }
    };

    let provider_impl = quote! {
        impl #core::Provider for #struct_name {
            type Output = Self;
            fn provide(&self, ctx: &#core::AppContext) -> #core::Result<Self::Output> {
                <Self as #core::Inject>::resolve(ctx)
            }
        }
    };

    Ok(quote! {
        #inject_impl
        #provider_impl
    })
}
