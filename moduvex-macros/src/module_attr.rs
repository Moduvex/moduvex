//! `#[moduvex::module]` — enforces module boundary visibility.
//!
//! Rewrites visibility of items inside an inline `mod { }` block:
//! - `pub` without `#[export]` → `pub(self)` (module-private)
//! - `pub` with `#[export]` → keeps `pub`, strips `#[export]` attr
//! - `pub(crate)`, `pub(super)`, `pub(in ...)` → compile_error!
//! - private items → unchanged
//!
//! **Limitation:** Only works on inline mod blocks, not file-based `mod name;`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ItemMod, Visibility, Item};

/// Expand `#[moduvex::module]` on a `mod` block.
pub fn expand(
    _args: TokenStream,
    item: TokenStream,
) -> syn::Result<TokenStream> {
    let mut module: ItemMod = syn::parse2(item)?;

    // Ensure this is an inline mod (has braces with content)
    let content = match &mut module.content {
        Some((_, items)) => items,
        None => {
            return Err(syn::Error::new_spanned(
                &module.ident,
                "#[moduvex::module] only works on inline `mod name { ... }` blocks, \
                 not file-based `mod name;`. Apply it inside each file instead.",
            ));
        }
    };

    // Process each item in the module
    let mut new_items = Vec::with_capacity(content.len());
    for item in content.drain(..) {
        new_items.push(rewrite_item_visibility(item)?);
    }
    *content = new_items;

    Ok(quote!(#module))
}

/// Rewrite visibility of a single item according to boundary rules.
fn rewrite_item_visibility(mut item: Item) -> syn::Result<Item> {
    let (vis, attrs) = match &mut item {
        Item::Struct(i) => (&mut i.vis, &mut i.attrs),
        Item::Enum(i) => (&mut i.vis, &mut i.attrs),
        Item::Fn(i) => (&mut i.vis, &mut i.attrs),
        Item::Type(i) => (&mut i.vis, &mut i.attrs),
        Item::Const(i) => (&mut i.vis, &mut i.attrs),
        Item::Static(i) => (&mut i.vis, &mut i.attrs),
        Item::Trait(i) => (&mut i.vis, &mut i.attrs),
        Item::Impl(_) | Item::Use(_) | Item::Mod(_) => {
            // impl blocks, use statements, nested mods: pass through unchanged
            return Ok(item);
        }
        _ => return Ok(item),
    };

    match vis {
        Visibility::Public(_) => {
            // Check for #[export] attribute
            let export_idx = attrs.iter().position(|a| a.path().is_ident("export"));
            if let Some(idx) = export_idx {
                // Has #[export] → strip the attribute, keep pub
                attrs.remove(idx);
            } else {
                // No #[export] → rewrite to pub(self) (module-private)
                *vis = Visibility::Inherited;
            }
        }
        Visibility::Restricted(restricted) => {
            // pub(crate), pub(super), pub(in ...) → compile error
            return Err(syn::Error::new_spanned(
                restricted,
                "moduvex module boundary: use `pub` with `#[export]` to expose items \
                 cross-module, or remove the visibility qualifier. \
                 `pub(crate)`, `pub(super)`, and `pub(in ...)` are not allowed.",
            ));
        }
        Visibility::Inherited => {
            // Private — no change needed
        }
    }

    Ok(item)
}
