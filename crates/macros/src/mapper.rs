// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `#[derive(Mapper)]` — MapStruct's `@Mapper` for Rust.
//!
//! Generates a compile-time, type-checked `From<Source>` for the annotated
//! struct, mapping field-by-field. Unlike the runtime `firefly_data::Mapper`
//! (two serde passes, runtime-checked), this is zero-cost and the compiler
//! verifies every field — the real MapStruct value.
//!
//! ```ignore
//! #[derive(Mapper)]
//! #[firefly(from = "UserEntity")]
//! struct UserDto {
//!     id: u64,
//!     #[firefly(rename = "full_name")] name: String,   // source field is named differently
//!     #[firefly(into)] email: String,                  // source.email.into()
//!     #[firefly(with = "fmt_phone")] phone: String,     // fmt_phone(source.phone)
//!     #[firefly(default)] note: String,                 // Default::default(), no source read
//! }
//! // generates: impl From<UserEntity> for UserDto { fn from(src) -> Self { … } }
//! ```
//!
//! `#[firefly(from = "...")]` may be repeated to map from several sources.

use darling::ast::Data;
use darling::{FromDeriveInput, FromField};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Type};

#[derive(FromDeriveInput)]
#[darling(attributes(firefly), supports(struct_named))]
struct MapperInput {
    ident: syn::Ident,
    generics: syn::Generics,
    /// One or more source types (`#[firefly(from = "Src")]`, repeatable).
    #[darling(multiple, rename = "from")]
    from: Vec<String>,
    data: Data<(), MapperField>,
}

#[derive(FromField)]
#[darling(attributes(firefly))]
struct MapperField {
    ident: Option<syn::Ident>,
    /// Source field name, when it differs from the target field.
    #[darling(default)]
    rename: Option<String>,
    /// Apply `.into()` to the source value.
    #[darling(default)]
    into: bool,
    /// Apply a conversion function: `with(source.field)`.
    #[darling(default)]
    with: Option<String>,
    /// Fill from `Default::default()` (no source field read).
    #[darling(default)]
    default: bool,
    /// Fill from a custom expression (no source field read).
    #[darling(default)]
    default_expr: Option<String>,
}

pub(crate) fn derive_mapper(input: DeriveInput) -> syn::Result<TokenStream> {
    let parsed = MapperInput::from_derive_input(&input).map_err(syn::Error::from)?;
    let ident = &parsed.ident;
    let (impl_g, ty_g, where_g) = parsed.generics.split_for_impl();

    if parsed.from.is_empty() {
        return Err(syn::Error::new_spanned(
            ident,
            "#[derive(Mapper)] needs at least one source: #[firefly(from = \"SourceType\")]",
        ));
    }

    let fields = match &parsed.data {
        Data::Struct(f) => &f.fields,
        Data::Enum(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Mapper)] supports only structs with named fields",
            ))
        }
    };

    // Build the per-field initializer expression (referencing `__src`).
    let mut field_inits = Vec::new();
    for field in fields {
        let fident = field
            .ident
            .as_ref()
            .expect("named struct fields have idents");
        let src_name = field.rename.clone().unwrap_or_else(|| fident.to_string());
        let src_field = syn::Ident::new(&src_name, fident.span());

        let init = match (field.default, &field.default_expr, &field.with, field.into) {
            (true, _, _, _) => quote! { #fident: ::core::default::Default::default() },
            (_, Some(expr_str), _, _) => {
                let expr: syn::Expr = syn::parse_str(expr_str).map_err(|e| {
                    syn::Error::new_spanned(
                        fident,
                        format!("#[firefly(default_expr = \"{expr_str}\")] invalid: {e}"),
                    )
                })?;
                quote! { #fident: #expr }
            }
            (_, _, Some(with_path), _) => {
                let path: syn::Path = syn::parse_str(with_path).map_err(|e| {
                    syn::Error::new_spanned(
                        fident,
                        format!("#[firefly(with = \"{with_path}\")] is not a valid path: {e}"),
                    )
                })?;
                quote! { #fident: #path(__src.#src_field) }
            }
            (_, _, _, true) => quote! { #fident: ::core::convert::Into::into(__src.#src_field) },
            _ => quote! { #fident: __src.#src_field },
        };
        field_inits.push(init);
    }

    // One `From<Source>` impl per declared source.
    let mut impls = Vec::new();
    for src in &parsed.from {
        let src_ty: Type = syn::parse_str(src).map_err(|e| {
            syn::Error::new_spanned(
                ident,
                format!("#[firefly(from = \"{src}\")] is not a valid type: {e}"),
            )
        })?;
        let inits = field_inits.clone();
        impls.push(quote! {
            impl #impl_g ::core::convert::From<#src_ty> for #ident #ty_g #where_g {
                fn from(__src: #src_ty) -> Self {
                    Self { #(#inits),* }
                }
            }
        });
    }

    Ok(quote! { #(#impls)* })
}
