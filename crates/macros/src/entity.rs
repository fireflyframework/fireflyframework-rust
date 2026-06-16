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

//! `#[derive(Entity)]` — generates the `SqlxEntity` mapping (`@Table` / `@Id` /
//! `@Version` / `@Column`) from a struct's fields, so a persistence entity is
//! just annotated fields — the JPA `@Entity` experience.
//!
//! Container: `#[firefly(table = "wallets")]` (the table name; `crate = "..."`
//! overrides the facade). Fields:
//! - `#[firefly(id)]` — the primary key (its type becomes `SqlxEntity::Id`)
//! - `#[firefly(version)]` — the `@Version` optimistic-lock column
//! - `#[firefly(column = "name")]` — a column rename (default: the field name)
//! - `#[firefly(with(read = "path", write = "path"))]` — a custom converter for a
//!   non-scalar field (e.g. an enum): `read` is `fn(&str) -> Field`, `write` is
//!   `fn(&Field) -> impl Into<Value>`
//!
//! Scalar fields map automatically: `String`, `i64`/`i32`, `bool`, `f64`,
//! `Uuid` (text), and `DateTime<Utc>` (text, via `parse_timestamp`).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Path, Type};

use crate::common::facade_from_override;

/// One mapped field.
struct FieldSpec {
    ident: syn::Ident,
    ty: Type,
    column: String,
    is_id: bool,
    is_version: bool,
    read_with: Option<Path>,
    write_with: Option<Path>,
}

/// Expands `#[derive(Entity)]`.
pub(crate) fn derive_entity(input: DeriveInput) -> syn::Result<TokenStream> {
    let self_ty = &input.ident;

    // Container options.
    let mut table: Option<String> = None;
    let mut krate: Option<String> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("firefly") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else {
                return Err(meta.error("#[derive(Entity)] container accepts `table` and `crate`"));
            }
            Ok(())
        })?;
    }
    let table = table.ok_or_else(|| {
        syn::Error::new_spanned(
            self_ty,
            "#[derive(Entity)] needs `#[firefly(table = \"...\")]`",
        )
    })?;

    let facade = facade_from_override(&krate)?;
    let rt = facade.rt();
    let sqlx = quote!(#rt::firefly_data_sqlx);
    let kernel = quote!(#rt::firefly_kernel);

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            self_ty,
            "#[derive(Entity)] supports only structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            self_ty,
            "#[derive(Entity)] needs named fields",
        ));
    };

    let mut specs: Vec<FieldSpec> = Vec::new();
    for field in &fields.named {
        let ident = field.ident.clone().unwrap();
        let mut spec = FieldSpec {
            column: ident.to_string(),
            ident,
            ty: field.ty.clone(),
            is_id: false,
            is_version: false,
            read_with: None,
            write_with: None,
        };
        for attr in &field.attrs {
            if !attr.path().is_ident("firefly") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("id") {
                    spec.is_id = true;
                } else if meta.path.is_ident("version") {
                    spec.is_version = true;
                } else if meta.path.is_ident("column") {
                    spec.column = meta.value()?.parse::<syn::LitStr>()?.value();
                } else if meta.path.is_ident("with") {
                    meta.parse_nested_meta(|w| {
                        if w.path.is_ident("read") {
                            spec.read_with = Some(w.value()?.parse::<syn::LitStr>()?.parse()?);
                        } else if w.path.is_ident("write") {
                            spec.write_with = Some(w.value()?.parse::<syn::LitStr>()?.parse()?);
                        } else {
                            return Err(w.error("`with` accepts `read` and `write`"));
                        }
                        Ok(())
                    })?;
                } else {
                    return Err(meta.error("field accepts `id`, `version`, `column`, `with(...)`"));
                }
                Ok(())
            })?;
        }
        specs.push(spec);
    }

    let id = specs.iter().find(|s| s.is_id).ok_or_else(|| {
        syn::Error::new_spanned(
            self_ty,
            "#[derive(Entity)] needs one `#[firefly(id)]` field",
        )
    })?;
    let id_ty = &id.ty;
    let id_col = &id.column;
    let version_col = specs
        .iter()
        .find(|s| s.is_version)
        .map(|s| s.column.clone());
    let version_tokens = match version_col {
        Some(c) => quote!(::core::option::Option::Some(#c)),
        None => quote!(::core::option::Option::None),
    };

    let columns: Vec<&str> = specs.iter().map(|s| s.column.as_str()).collect();

    // Per-field read + write expressions.
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for s in &specs {
        let f = &s.ident;
        let col = &s.column;
        let read = read_expr(s, &sqlx, &kernel)?;
        reads.push(quote!(#f: #read));
        let write = write_expr(s, &sqlx)?;
        writes.push(write);
        let _ = col;
    }

    Ok(quote! {
        impl #sqlx::SqlxEntity for #self_ty {
            type Id = #id_ty;

            fn table() -> &'static str { #table }
            fn id_column() -> &'static str { #id_col }
            fn columns() -> &'static [&'static str] { &[#(#columns),*] }
            fn version_column() -> ::core::option::Option<&'static str> { #version_tokens }

            fn read_row(row: & #sqlx::AnyRow<'_>) -> ::core::result::Result<Self, #kernel::FireflyError> {
                ::core::result::Result::Ok(Self { #(#reads),* })
            }

            fn write_row(&self) -> ::std::vec::Vec<#sqlx::ColumnValue> {
                ::std::vec![#(#writes),*]
            }
        }
    })
}

/// The read expression for a field (a value of the field's type from `row`).
fn read_expr(s: &FieldSpec, sqlx: &TokenStream, kernel: &TokenStream) -> syn::Result<TokenStream> {
    let col = &s.column;
    if let Some(read) = &s.read_with {
        return Ok(quote!(#read(&row.get_str(#col)?)));
    }
    Ok(match type_name(&s.ty).as_deref() {
        Some("String") => quote!(row.get_str(#col)?),
        Some("i64") => quote!(row.get_i64(#col)?),
        Some("i32") => quote!(row.get_i32(#col)?),
        Some("bool") => quote!(row.get_bool(#col)?),
        Some("f64") => quote!(row.get_f64(#col)?),
        Some("Uuid") => {
            let ty = &s.ty;
            quote!(<#ty>::parse_str(&row.get_str(#col)?)
                .map_err(|e| #kernel::FireflyError::internal(format!("bad uuid in '{}': {}", #col, e)))?)
        }
        Some("DateTime") => quote!(#sqlx::parse_timestamp(&row.get_str(#col)?)?),
        other => {
            return Err(syn::Error::new_spanned(
                &s.ty,
                format!(
                    "#[derive(Entity)]: unsupported field type `{}` — add #[firefly(with(read = \"...\", write = \"...\"))]",
                    other.unwrap_or("?")
                ),
            ))
        }
    })
}

/// The write expression for a field (a `ColumnValue`).
fn write_expr(s: &FieldSpec, sqlx: &TokenStream) -> syn::Result<TokenStream> {
    let col = &s.column;
    let f = &s.ident;
    let cv = quote!(#sqlx::ColumnValue);
    if let Some(write) = &s.write_with {
        return Ok(quote!(#cv::new(#col, #write(&self.#f))));
    }
    Ok(match type_name(&s.ty).as_deref() {
        Some("String") => quote!(#cv::new(#col, self.#f.clone())),
        Some("i64") | Some("i32") | Some("bool") | Some("f64") => quote!(#cv::new(#col, self.#f)),
        Some("Uuid") => quote!(#cv::new(#col, self.#f.to_string())),
        Some("DateTime") => quote!(#cv::new(#col, self.#f.to_rfc3339())),
        other => {
            return Err(syn::Error::new_spanned(
                &s.ty,
                format!(
                    "#[derive(Entity)]: unsupported field type `{}` — add #[firefly(with(read = \"...\", write = \"...\"))]",
                    other.unwrap_or("?")
                ),
            ))
        }
    })
}

/// The last path-segment identifier of a type (`chrono::DateTime<Utc>` → `DateTime`).
fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}
