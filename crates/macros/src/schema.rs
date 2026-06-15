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

//! `#[derive(Schema)]` — registers a type's OpenAPI component schema so the
//! generator emits `#/components/schemas/{Type}` for it, the Rust analog of
//! springdoc reflecting over a `@Schema` model class.
//!
//! Rust has no runtime reflection, so the schema is computed **at macro
//! expansion time** by walking the struct's fields and mapping each Rust type
//! to a JSON Schema fragment (`String` → `{"type":"string"}`, `Option<T>` →
//! `T` made non-required, `Vec<T>` → `array`, a `Uuid`/`DateTime` → a typed
//! `format`, an unknown named type → a `$ref` to its own component schema). The
//! result is emitted as a compile-time JSON string in an `inventory::submit!`
//! of a [`firefly_container::SchemaDescriptor`], which the OpenAPI `Builder`
//! collects into the document's `components.schemas`.

use darling::FromDeriveInput;
use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, GenericArgument, Lit, Meta, PathArguments, Token,
    Type,
};

use crate::common::facade_from_override;

/// `#[derive(Schema)]`'s `#[firefly(...)]` options — only the facade override.
#[derive(FromDeriveInput, Default)]
#[darling(attributes(firefly), supports(struct_named), default)]
struct SchemaOpts {
    /// Facade override (`#[firefly(crate = "...")]`).
    #[darling(rename = "crate")]
    krate: Option<String>,
}

/// Expands `#[derive(Schema)]` for a named-field struct into an `inventory`
/// submission of its OpenAPI component schema.
pub(crate) fn derive_schema(input: DeriveInput) -> syn::Result<TokenStream> {
    let ident = &input.ident;
    let name = ident.to_string();

    // Resolve the `#[firefly(crate = "...")]` facade override (if any) so the
    // generated path reaches `firefly_container` through the facade contract.
    let opts = SchemaOpts::from_derive_input(&input).map_err(syn::Error::from)?;
    let facade = facade_from_override(&opts.krate)?;
    let container = facade.container();

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            "#[derive(Schema)] supports only structs with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            ident,
            "#[derive(Schema)] supports only structs with named fields",
        ));
    };

    // The struct-level `#[serde(rename_all = "...")]` convention, if any, so the
    // emitted property names match the JSON wire shape (springdoc/Jackson parity).
    let rename_all = serde_rename_all(&input.attrs);

    // Build `"name": <fragment>` property entries and the required list, honouring
    // each field's serde renaming and skipping `#[serde(skip)]` fields.
    let mut properties: Vec<String> = Vec::new();
    let mut required: Vec<String> = Vec::new();
    for field in &fields.named {
        let Some(field_ident) = &field.ident else {
            continue;
        };
        let serde = field_serde(&field.attrs);
        if serde.skip {
            continue;
        }
        let json_name = serde
            .rename
            .unwrap_or_else(|| apply_rename_all(&field_ident.to_string(), rename_all.as_deref()));
        if !is_option(&field.ty) {
            required.push(json_string(&json_name));
        }
        properties.push(format!(
            "{}:{}",
            json_string(&json_name),
            json_schema_fragment(&field.ty)
        ));
    }

    let mut schema = format!(
        "{{\"type\":\"object\",\"properties\":{{{}}}",
        properties.join(",")
    );
    if !required.is_empty() {
        schema.push_str(&format!(",\"required\":[{}]", required.join(",")));
    }
    schema.push('}');

    Ok(quote! {
        #container::inventory::submit! {
            #container::SchemaDescriptor {
                name: #name,
                schema: #schema,
            }
        }
    })
}

/// Whether a field type is `Option<…>` (an optional, non-required property).
fn is_option(ty: &Type) -> bool {
    outer_ident(ty).as_deref() == Some("Option")
}

/// The last path-segment identifier of a type (`std::string::String` →
/// `"String"`), or `None` for non-path types (references, tuples, …).
fn outer_ident(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        Type::Reference(r) => outer_ident(&r.elem),
        _ => None,
    }
}

/// The generic type arguments of a path type (`HashMap<K, V>` → `[K, V]`).
fn generic_args(ty: &Type) -> Vec<&Type> {
    let Type::Path(p) = ty else {
        return Vec::new();
    };
    let Some(segment) = p.path.segments.last() else {
        return Vec::new();
    };
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Vec::new();
    };
    args.args
        .iter()
        .filter_map(|a| match a {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// Maps a Rust field type to its JSON Schema fragment (a JSON object string).
///
/// Primitives map to their JSON type; common library types (`Uuid`, chrono /
/// time date-times) gain a `format`; `Option`/`Box`/`Arc`/`Rc` are transparent
/// wrappers; sequences become `array`; maps become an open `object`; and any
/// other named type is referenced as `$ref:#/components/schemas/{Type}` (so a
/// nested DTO that also `#[derive(Schema)]`s is linked, not inlined).
fn json_schema_fragment(ty: &Type) -> String {
    let ident = match outer_ident(ty) {
        Some(i) => i,
        // References to primitives are handled by `outer_ident`; anything else
        // (tuples, fn pointers, …) becomes the permissive empty schema.
        None => return "{}".to_string(),
    };
    match ident.as_str() {
        "String" | "str" | "Cow" | "PathBuf" | "Path" => r#"{"type":"string"}"#.to_string(),
        "char" => r#"{"type":"string"}"#.to_string(),
        "bool" => r#"{"type":"boolean"}"#.to_string(),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" | "NonZeroU8" | "NonZeroU16" | "NonZeroU32" | "NonZeroU64" | "NonZeroUsize"
        | "NonZeroI8" | "NonZeroI16" | "NonZeroI32" | "NonZeroI64" => {
            r#"{"type":"integer"}"#.to_string()
        }
        "f32" | "f64" => r#"{"type":"number"}"#.to_string(),
        "Uuid" => r#"{"type":"string","format":"uuid"}"#.to_string(),
        "DateTime" | "NaiveDateTime" | "OffsetDateTime" | "PrimitiveDateTime" | "SystemTime"
        | "Instant" => r#"{"type":"string","format":"date-time"}"#.to_string(),
        "NaiveDate" | "Date" => r#"{"type":"string","format":"date"}"#.to_string(),
        "NaiveTime" | "Time" => r#"{"type":"string","format":"time"}"#.to_string(),
        "Decimal" | "BigDecimal" => r#"{"type":"string"}"#.to_string(),
        // Transparent wrappers — describe the inner type.
        "Option" | "Box" | "Arc" | "Rc" => generic_args(ty)
            .first()
            .map(|inner| json_schema_fragment(inner))
            .unwrap_or_else(|| "{}".to_string()),
        // Sequences become arrays of the element schema.
        "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "LinkedList" => {
            let items = generic_args(ty)
                .first()
                .map(|inner| json_schema_fragment(inner))
                .unwrap_or_else(|| "{}".to_string());
            format!(r#"{{"type":"array","items":{items}}}"#)
        }
        // Maps become an open object keyed by string with typed values.
        "HashMap" | "BTreeMap" => {
            let value = generic_args(ty)
                .get(1)
                .map(|inner| json_schema_fragment(inner))
                .unwrap_or_else(|| "{}".to_string());
            format!(r#"{{"type":"object","additionalProperties":{value}}}"#)
        }
        // Any other named type is assumed to be a sibling DTO that also derives
        // `Schema`; reference it so the document links the two component schemas.
        other => format!(r##"{{"$ref":"#/components/schemas/{other}"}}"##),
    }
}

/// JSON-encodes an identifier-shaped string (field / type name). Field and type
/// names are Rust identifiers (or serde renames, also simple tokens), so only
/// the surrounding quotes are needed.
fn json_string(s: &str) -> String {
    format!("\"{s}\"")
}

/// The struct-level `#[serde(rename_all = "...")]` convention, if present.
fn serde_rename_all(attrs: &[Attribute]) -> Option<String> {
    for meta in serde_metas(attrs) {
        if let Meta::NameValue(nv) = &meta {
            if nv.path.is_ident("rename_all") {
                if let Some(value) = lit_str_value(&nv.value) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// The serde naming/skip directives on a field.
#[derive(Default)]
struct FieldSerde {
    /// `#[serde(skip)]` / `#[serde(skip_serializing)]` — omit from the schema.
    skip: bool,
    /// `#[serde(rename = "...")]` — the explicit JSON property name.
    rename: Option<String>,
}

/// Parses a field's `#[serde(...)]` directives (rename + skip).
fn field_serde(attrs: &[Attribute]) -> FieldSerde {
    let mut out = FieldSerde::default();
    for meta in serde_metas(attrs) {
        match &meta {
            Meta::Path(p) if p.is_ident("skip") || p.is_ident("skip_serializing") => {
                out.skip = true;
            }
            Meta::NameValue(nv) if nv.path.is_ident("rename") => {
                if let Some(value) = lit_str_value(&nv.value) {
                    out.rename = Some(value);
                }
            }
            _ => {}
        }
    }
    out
}

/// Flattens every `#[serde(...)]` attribute's comma-separated items into a list
/// of [`Meta`], ignoring malformed attributes.
fn serde_metas(attrs: &[Attribute]) -> Vec<Meta> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        if let Ok(items) = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) {
            out.extend(items);
        }
    }
    out
}

/// Extracts a string literal value from an attribute expression
/// (`rename = "x"` → `Some("x")`).
fn lit_str_value(expr: &Expr) -> Option<String> {
    if let Expr::Lit(lit) = expr {
        if let Lit::Str(s) = &lit.lit {
            return Some(s.value());
        }
    }
    None
}

/// Applies a serde `rename_all` convention to a (snake_case) Rust field name.
/// Supports the conventions a Firefly DTO realistically uses; an unrecognised
/// convention leaves the name unchanged.
fn apply_rename_all(name: &str, convention: Option<&str>) -> String {
    let Some(convention) = convention else {
        return name.to_string();
    };
    let words: Vec<&str> = name.split('_').filter(|w| !w.is_empty()).collect();
    match convention {
        "lowercase" => name.replace('_', "").to_lowercase(),
        "UPPERCASE" => name.replace('_', "").to_uppercase(),
        "snake_case" => name.to_string(),
        "SCREAMING_SNAKE_CASE" => name.to_uppercase(),
        "kebab-case" => name.replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => name.replace('_', "-").to_uppercase(),
        "camelCase" => words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.to_string() } else { capitalize(w) })
            .collect(),
        "PascalCase" => words.iter().map(|w| capitalize(w)).collect(),
        _ => name.to_string(),
    }
}

/// Upper-cases the first character of `word`, leaving the rest unchanged.
fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
