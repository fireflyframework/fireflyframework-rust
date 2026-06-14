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

//! `#[derive(Builder)]` — Lombok's `@Builder` for Rust.
//!
//! Generates a fluent `T::builder()` returning a `TBuilder` with one setter per
//! field and a `build() -> Result<T, String>`. Required fields (the default)
//! must be set or `build` errors; fields marked `#[builder(default)]` fall back
//! to `Default::default()`, and `#[builder(default = "expr")]` to a custom
//! expression. `#[builder(into)]` makes a setter accept `impl Into<FieldTy>`.
//!
//! Rust's stdlib derives already cover Lombok's `@Data`/`@Value`/`@ToString`/
//! `@EqualsAndHashCode` (`#[derive(Debug, Clone, PartialEq, Default)]` + `pub`
//! fields); this fills the one ergonomic gap they don't: a fluent builder.

use darling::ast::Data;
use darling::{FromDeriveInput, FromField};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, Type};

#[derive(FromDeriveInput)]
#[darling(attributes(builder), supports(struct_named))]
struct BuilderInput {
    ident: syn::Ident,
    generics: syn::Generics,
    data: Data<(), BuilderField>,
}

#[derive(FromField)]
#[darling(attributes(builder))]
struct BuilderField {
    ident: Option<syn::Ident>,
    ty: Type,
    /// Setter accepts `impl Into<FieldTy>`.
    #[darling(default)]
    into: bool,
    /// Unset → `Default::default()`.
    #[darling(default)]
    default: bool,
    /// Unset → this expression (a Rust expression string).
    #[darling(default)]
    default_expr: Option<String>,
}

pub(crate) fn derive_builder(input: DeriveInput) -> syn::Result<TokenStream> {
    let parsed = BuilderInput::from_derive_input(&input).map_err(syn::Error::from)?;
    let ident = &parsed.ident;
    let builder_ident = format_ident!("{}Builder", ident);
    let (impl_g, ty_g, where_g) = parsed.generics.split_for_impl();

    let fields = match &parsed.data {
        Data::Struct(f) => &f.fields,
        Data::Enum(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Builder)] supports only structs with named fields",
            ))
        }
    };

    let mut decls = Vec::new();
    let mut inits = Vec::new();
    let mut setters = Vec::new();
    let mut builds = Vec::new();

    for field in fields {
        let fident = field
            .ident
            .as_ref()
            .expect("named struct fields have idents");
        let fty = &field.ty;
        let name = fident.to_string();

        decls.push(quote! { #fident: ::core::option::Option<#fty> });
        inits.push(quote! { #fident: ::core::option::Option::None });

        // Setter: `into` accepts `impl Into<FieldTy>`.
        if field.into {
            setters.push(quote! {
                #[doc = concat!("Sets `", #name, "`.")]
                pub fn #fident(mut self, value: impl ::core::convert::Into<#fty>) -> Self {
                    self.#fident = ::core::option::Option::Some(value.into());
                    self
                }
            });
        } else {
            setters.push(quote! {
                #[doc = concat!("Sets `", #name, "`.")]
                pub fn #fident(mut self, value: #fty) -> Self {
                    self.#fident = ::core::option::Option::Some(value);
                    self
                }
            });
        }

        // build(): required (error if unset), or default / default_expr fallback.
        let fallback = match (field.default, &field.default_expr) {
            (_, Some(expr_str)) => {
                let expr: syn::Expr = syn::parse_str(expr_str).map_err(|e| {
                    syn::Error::new_spanned(
                        fident,
                        format!(
                            "#[builder(default = \"{expr_str}\")] is not a valid expression: {e}"
                        ),
                    )
                })?;
                quote! { .unwrap_or_else(|| #expr) }
            }
            (true, None) => quote! { .unwrap_or_default() },
            (false, None) => {
                let msg = format!("field `{name}` is required and was not set");
                quote! { .ok_or_else(|| ::std::string::String::from(#msg))? }
            }
        };
        builds.push(quote! { #fident: self.#fident #fallback });
    }

    let builder_doc = format!("Fluent builder for [`{ident}`], from `#[derive(Builder)]`.");

    Ok(quote! {
        impl #impl_g #ident #ty_g #where_g {
            #[doc = concat!("Returns a fluent builder for `", stringify!(#ident), "`.")]
            pub fn builder() -> #builder_ident #ty_g {
                #builder_ident { #(#inits),* }
            }
        }

        #[doc = #builder_doc]
        pub struct #builder_ident #impl_g #where_g {
            #(#decls),*
        }

        impl #impl_g #builder_ident #ty_g #where_g {
            #(#setters)*

            /// Builds the value, returning an error naming the first required
            /// field that was not set.
            pub fn build(self) -> ::core::result::Result<#ident #ty_g, ::std::string::String> {
                ::core::result::Result::Ok(#ident { #(#builds),* })
            }
        }
    })
}
