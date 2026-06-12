//! A runtime object-to-object [`Mapper`] — the Rust port of pyfly's
//! `data.mapper` (itself the runtime equivalent of MapStruct).
//!
//! Where pyfly inspects dataclass / Pydantic field names by reflection,
//! Rust has no runtime field reflection, so the idiomatic equivalent is
//! to bridge through `serde_json`: the source is serialised to a JSON
//! object, the configured transformations (field renaming, value
//! transformers, field exclusion) are applied to that object, and the
//! result is deserialised into the destination type. Because the bridge
//! is JSON, **nested models and collections of models are recursed into
//! automatically** by serde — matching pyfly's nested-recursion
//! behaviour without any per-type registration.
//!
//! The mapper exposes the same three operations as pyfly:
//!
//! - [`Mapper::map`] — name-matched conversion between two types, with
//!   optional renaming / transformers / exclusion registered via
//!   [`Mapper::add_mapping`];
//! - [`Mapper::map_list`] — map a slice of sources in one call;
//! - [`Mapper::project`] — map a source to a (typically smaller)
//!   projection type, with optional computed fields registered via
//!   [`Mapper::register_projection`].
//!
//! Field renaming is **source → destination**, exactly like pyfly's
//! `field_map`. Transformers are keyed by **destination** field name and
//! receive (and return) the field's `serde_json::Value`. Projection
//! transforms are keyed by destination field name and receive the
//! *entire* source object as a `serde_json::Value`.
//!
//! # Quick start
//!
//! ```
//! use firefly_data::{Mapper, Mapping};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize)]
//! struct UserEntity {
//!     id: u64,
//!     username: String,
//!     email: String,
//!     active: bool,
//! }
//!
//! #[derive(Deserialize, PartialEq, Debug)]
//! struct UserDto {
//!     name: String,
//!     email: String,
//! }
//!
//! let mut mapper = Mapper::new();
//! // rename source `username` -> dest `name`
//! mapper.add_mapping::<UserEntity, UserDto>(Mapping::new().rename("username", "name"));
//!
//! let entity = UserEntity {
//!     id: 1,
//!     username: "alice".into(),
//!     email: "a@b.com".into(),
//!     active: true,
//! };
//! let dto: UserDto = mapper.map(&entity).unwrap();
//! assert_eq!(dto, UserDto { name: "alice".into(), email: "a@b.com".into() });
//! ```

use std::any::TypeId;
use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

/// A boxed value transformer: takes a field's JSON value and returns the
/// transformed value.
type Transformer = Box<dyn Fn(Value) -> Value + Send + Sync>;

/// A boxed projection transform: takes the whole source object's JSON
/// value and produces the destination field's value.
type ProjectionTransform = Box<dyn Fn(&Value) -> Value + Send + Sync>;

/// The error type returned by mapping operations.
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    /// The source value did not serialise to a JSON **object** (it was a
    /// scalar, array, or null). The mapper can only map object-shaped
    /// types.
    #[error("firefly/data: mapper source is not an object")]
    SourceNotObject,
    /// Serialising the source failed.
    #[error("firefly/data: mapper serialise failed: {0}")]
    Serialize(String),
    /// Deserialising into the destination type failed (typically a
    /// missing required field with no default).
    #[error("firefly/data: mapper deserialise failed: {0}")]
    Deserialize(String),
}

/// A custom mapping configuration, built fluently and handed to
/// [`Mapper::add_mapping`].
///
/// Port of pyfly's `MappingConfig` (`field_map` / `transformers` /
/// `exclude`). Built with the [`Mapping::rename`] / [`Mapping::transform`]
/// / [`Mapping::exclude`] chain so an empty configuration needs no turbofish:
///
/// ```
/// use firefly_data::Mapping;
/// use serde_json::json;
///
/// let mapping = Mapping::new()
///     .rename("username", "name")
///     .transform("name", |v| json!(v.as_str().unwrap().to_uppercase()))
///     .exclude("is_active");
/// ```
#[derive(Default)]
pub struct Mapping {
    /// Source-field → destination-field renames.
    field_map: HashMap<String, String>,
    /// Destination-field → value transformer.
    transformers: HashMap<String, Transformer>,
    /// Destination fields to exclude from the mapping.
    exclude: Vec<String>,
}

impl Mapping {
    /// Creates an empty mapping (plain name-matched conversion).
    pub fn new() -> Self {
        Mapping::default()
    }

    /// Renames a **source** field to a **destination** field (pyfly's
    /// `field_map` entry).
    pub fn rename(mut self, source: impl Into<String>, dest: impl Into<String>) -> Self {
        self.field_map.insert(source.into(), dest.into());
        self
    }

    /// Registers a value transformer keyed by **destination** field name
    /// (pyfly's `transformers` entry). The transformer receives the
    /// field's `serde_json::Value` after renaming and returns its
    /// replacement.
    pub fn transform(
        mut self,
        dest: impl Into<String>,
        f: impl Fn(Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.transformers.insert(dest.into(), Box::new(f));
        self
    }

    /// Excludes a **destination** field so it keeps its own serde default
    /// (pyfly's `exclude` entry).
    pub fn exclude(mut self, dest: impl Into<String>) -> Self {
        self.exclude.push(dest.into());
        self
    }
}

/// A projection configuration, built fluently and handed to
/// [`Mapper::register_projection`].
///
/// Port of pyfly's projection `transforms`: destination-field → computed
/// transform over the *whole* source object.
///
/// ```
/// use firefly_data::Projection;
/// use serde_json::json;
///
/// let projection = Projection::new()
///     .computed("total", |src| json!(src["quantity"].as_f64().unwrap() * src["unit_price"].as_f64().unwrap()));
/// ```
#[derive(Default)]
pub struct Projection {
    transforms: HashMap<String, ProjectionTransform>,
}

impl Projection {
    /// Creates an empty projection (plain name-matched subset).
    pub fn new() -> Self {
        Projection::default()
    }

    /// Registers a computed field keyed by **destination** field name.
    /// The callable receives the *entire* source object as a
    /// `serde_json::Value` and produces the field's value (a computed
    /// field *overrides* any same-named source field).
    pub fn computed(
        mut self,
        dest: impl Into<String>,
        f: impl Fn(&Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.transforms.insert(dest.into(), Box::new(f));
        self
    }
}

/// A runtime object-to-object mapper.
///
/// Register custom mappings with [`Mapper::add_mapping`] and projections
/// with [`Mapper::register_projection`]; both are keyed by the
/// `(source, destination)` type pair so the same mapper can hold many
/// independent mappings. With no registration, [`Mapper::map`] and
/// [`Mapper::project`] fall back to plain name-matched conversion.
#[derive(Default)]
pub struct Mapper {
    mappings: HashMap<(TypeId, TypeId), Mapping>,
    projections: HashMap<(TypeId, TypeId), Projection>,
}

impl Mapper {
    /// Creates an empty mapper.
    pub fn new() -> Self {
        Mapper::default()
    }

    /// Registers a custom [`Mapping`] between source type `S` and
    /// destination type `D` (pyfly's `add_mapping`). Build the mapping
    /// fluently with [`Mapping::rename`] / [`Mapping::transform`] /
    /// [`Mapping::exclude`].
    pub fn add_mapping<S, D>(&mut self, mapping: Mapping)
    where
        S: 'static,
        D: 'static,
    {
        self.mappings
            .insert((TypeId::of::<S>(), TypeId::of::<D>()), mapping);
    }

    /// Maps `source` (type `S`) to destination type `D`.
    ///
    /// Strategy, matching pyfly's `Mapper.map`: the source is serialised
    /// to a JSON object; for each source field, the registered rename
    /// (if any) is applied; excluded destination fields are dropped; a
    /// destination transformer (if registered) is applied to the value;
    /// and the resulting object is deserialised into `D` (so nested
    /// models and collections recurse through serde, and absent fields
    /// fall back to `D`'s defaults).
    pub fn map<S, D>(&self, source: &S) -> Result<D, MapError>
    where
        S: Serialize + 'static,
        D: DeserializeOwned + 'static,
    {
        let value = serde_json::to_value(source).map_err(|e| MapError::Serialize(e.to_string()))?;
        let Value::Object(src) = value else {
            return Err(MapError::SourceNotObject);
        };
        let config = self.mappings.get(&(TypeId::of::<S>(), TypeId::of::<D>()));
        let out = self.transform_object(src, config);
        serde_json::from_value(Value::Object(out)).map_err(|e| MapError::Deserialize(e.to_string()))
    }

    /// Maps a slice of sources to a `Vec<D>` (pyfly's `map_list`). Stops
    /// at the first error.
    pub fn map_list<S, D>(&self, sources: &[S]) -> Result<Vec<D>, MapError>
    where
        S: Serialize + 'static,
        D: DeserializeOwned + 'static,
    {
        sources.iter().map(|s| self.map(s)).collect()
    }

    /// Registers a [`Projection`] from source type `S` to projection
    /// type `D` (pyfly's `register_projection`). Build the projection
    /// fluently with [`Projection::computed`].
    pub fn register_projection<S, D>(&mut self, projection: Projection)
    where
        S: 'static,
        D: 'static,
    {
        self.projections
            .insert((TypeId::of::<S>(), TypeId::of::<D>()), projection);
    }

    /// Projects `source` (type `S`) onto projection type `D` (pyfly's
    /// `project`).
    ///
    /// For each registered transform, the destination field is computed
    /// from the whole source object; every other field is name-matched
    /// from the source (a transform thus *overrides* a same-named source
    /// field). Fields absent from both the source and the transforms
    /// fall back to `D`'s defaults via serde.
    pub fn project<S, D>(&self, source: &S) -> Result<D, MapError>
    where
        S: Serialize + 'static,
        D: DeserializeOwned + 'static,
    {
        let value = serde_json::to_value(source).map_err(|e| MapError::Serialize(e.to_string()))?;
        let Value::Object(ref src) = value else {
            return Err(MapError::SourceNotObject);
        };
        let mut out = src.clone();
        if let Some(config) = self
            .projections
            .get(&(TypeId::of::<S>(), TypeId::of::<D>()))
        {
            for (field, transform) in &config.transforms {
                out.insert(field.clone(), transform(&value));
            }
        }
        serde_json::from_value(Value::Object(out)).map_err(|e| MapError::Deserialize(e.to_string()))
    }

    /// Applies a mapping config's renames, exclusions, and transformers
    /// to a source JSON object, producing the destination object.
    fn transform_object(
        &self,
        src: Map<String, Value>,
        config: Option<&Mapping>,
    ) -> Map<String, Value> {
        let mut out = Map::new();
        for (key, value) in src {
            // Apply source -> dest rename (field_map is source-keyed).
            let dest_key = config
                .and_then(|c| c.field_map.get(&key))
                .cloned()
                .unwrap_or(key);

            if let Some(c) = config {
                if c.exclude.iter().any(|e| e == &dest_key) {
                    continue;
                }
                if let Some(transformer) = c.transformers.get(&dest_key) {
                    out.insert(dest_key, transformer(value));
                    continue;
                }
            }
            out.insert(dest_key, value);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    // ---- Test types (port of tests/data/test_mapper.py) --------------

    #[derive(Serialize)]
    struct UserEntity {
        id: u64,
        username: String,
        email: String,
        active: bool,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct UserDto {
        username: String,
        email: String,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct UserResponse {
        name: String,
        email: String,
        #[serde(default = "default_true")]
        is_active: bool,
    }

    fn default_true() -> bool {
        true
    }

    #[derive(Serialize)]
    struct UserDtoSrc {
        username: String,
        email: String,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct ProfileDto {
        username: String,
        email: String,
        #[serde(default)]
        bio: String,
    }

    fn entity() -> UserEntity {
        UserEntity {
            id: 1,
            username: "alice".into(),
            email: "alice@example.com".into(),
            active: true,
        }
    }

    // ---- TestBasicAutoMapping ----------------------------------------

    #[test]
    fn test_matching_fields_are_mapped() {
        let mapper = Mapper::new();
        let dto: UserDto = mapper.map(&entity()).unwrap();
        assert_eq!(dto.username, "alice");
        assert_eq!(dto.email, "alice@example.com");
    }

    // ---- TestEntityToDTO: extra source fields dropped ----------------

    #[test]
    fn test_entity_extra_fields_are_ignored() {
        let e = UserEntity {
            id: 42,
            username: "bob".into(),
            email: "bob@test.com".into(),
            active: false,
        };
        let mapper = Mapper::new();
        let dto: UserDto = mapper.map(&e).unwrap();
        assert_eq!(dto.username, "bob");
        assert_eq!(dto.email, "bob@test.com");
    }

    // ---- TestDTOToDTO ------------------------------------------------

    #[test]
    fn test_dto_to_dto_matching_fields() {
        let source = UserDtoSrc {
            username: "carol".into(),
            email: "carol@test.com".into(),
        };
        let mapper = Mapper::new();
        let profile: ProfileDto = mapper.map(&source).unwrap();
        assert_eq!(profile.username, "carol");
        assert_eq!(profile.email, "carol@test.com");
        assert_eq!(profile.bio, ""); // default kept
    }

    // ---- TestCustomFieldMap ------------------------------------------

    #[test]
    fn test_field_map_renames_field() {
        let mut mapper = Mapper::new();
        mapper.add_mapping::<UserEntity, UserResponse>(
            Mapping::new()
                .rename("username", "name")
                .rename("active", "is_active"),
        );
        let resp: UserResponse = mapper.map(&entity()).unwrap();
        assert_eq!(resp.name, "alice");
        assert_eq!(resp.email, "alice@example.com");
        assert!(resp.is_active);
    }

    #[test]
    fn test_field_map_without_registration_fails_when_name_unmatched() {
        // UserResponse needs `name`, but the source only has `username`.
        let mapper = Mapper::new();
        let result: Result<UserResponse, _> = mapper.map(&entity());
        assert!(matches!(result, Err(MapError::Deserialize(_))));
    }

    // ---- TestTransformers --------------------------------------------

    #[test]
    fn test_transformer_applied_to_field() {
        let mut mapper = Mapper::new();
        mapper.add_mapping::<UserEntity, UserDto>(
            Mapping::new().transform("username", |v| json!(v.as_str().unwrap().to_uppercase())),
        );
        let dto: UserDto = mapper.map(&entity()).unwrap();
        assert_eq!(dto.username, "ALICE");
        assert_eq!(dto.email, "alice@example.com");
    }

    #[test]
    fn test_transformer_with_field_map() {
        // Transformer is keyed by *dest* field name and works with rename.
        let mut mapper = Mapper::new();
        mapper.add_mapping::<UserEntity, UserResponse>(
            Mapping::new()
                .rename("username", "name")
                .rename("active", "is_active")
                .transform("name", |v| json!(v.as_str().unwrap().to_uppercase())),
        );
        let e = UserEntity {
            id: 1,
            username: "bob".into(),
            email: "b@c.com".into(),
            active: true,
        };
        let resp: UserResponse = mapper.map(&e).unwrap();
        assert_eq!(resp.name, "BOB");
        assert!(resp.is_active);
    }

    // ---- TestExclude -------------------------------------------------

    #[test]
    fn test_excluded_field_uses_dest_default() {
        let mut mapper = Mapper::new();
        mapper.add_mapping::<UserEntity, UserResponse>(
            Mapping::new()
                .rename("username", "name")
                .rename("active", "is_active")
                .exclude("is_active"),
        );
        let e = UserEntity {
            id: 1,
            username: "alice".into(),
            email: "a@b.com".into(),
            active: false, // would map to is_active=false, but excluded
        };
        let resp: UserResponse = mapper.map(&e).unwrap();
        assert_eq!(resp.name, "alice");
        assert_eq!(resp.email, "a@b.com");
        // is_active keeps its serde default (true), not the source's false.
        assert!(resp.is_active);
    }

    // ---- TestMapList -------------------------------------------------

    #[test]
    fn test_map_list_returns_list_of_dest_type() {
        let entities = vec![
            UserEntity {
                id: 1,
                username: "alice".into(),
                email: "a@b.com".into(),
                active: true,
            },
            UserEntity {
                id: 2,
                username: "bob".into(),
                email: "b@c.com".into(),
                active: true,
            },
        ];
        let mapper = Mapper::new();
        let dtos: Vec<UserDto> = mapper.map_list(&entities).unwrap();
        assert_eq!(dtos.len(), 2);
        assert_eq!(dtos[0].username, "alice");
        assert_eq!(dtos[1].username, "bob");
    }

    #[test]
    fn test_map_list_empty() {
        let mapper = Mapper::new();
        let dtos: Vec<UserDto> = mapper.map_list::<UserEntity, UserDto>(&[]).unwrap();
        assert!(dtos.is_empty());
    }

    // ---- Nested recursion (port of test_mapper_nested.py) ------------

    #[derive(Serialize)]
    struct AddressEntity {
        street: String,
        city: String,
    }

    #[derive(Serialize)]
    struct UserWithAddressEntity {
        username: String,
        address: AddressEntity,
        tags: Vec<String>,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct AddressDto {
        street: String,
        city: String,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct UserWithAddressDto {
        username: String,
        address: AddressDto,
        #[serde(default)]
        tags: Vec<String>,
    }

    #[test]
    fn test_nested_model_is_recursively_mapped() {
        let src = UserWithAddressEntity {
            username: "ada".into(),
            address: AddressEntity {
                street: "1 Main".into(),
                city: "London".into(),
            },
            tags: vec!["x".into()],
        };
        let mapper = Mapper::new();
        let dto: UserWithAddressDto = mapper.map(&src).unwrap();
        assert_eq!(dto.username, "ada");
        assert_eq!(dto.address.city, "London");
        assert_eq!(dto.tags, vec!["x".to_string()]);
    }

    #[derive(Serialize)]
    struct TeamEntity {
        name: String,
        members: Vec<UserWithAddressEntity>,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct TeamDto {
        name: String,
        members: Vec<UserWithAddressDto>,
    }

    #[test]
    fn test_collection_of_models_is_recursively_mapped() {
        let team = TeamEntity {
            name: "A".into(),
            members: vec![UserWithAddressEntity {
                username: "ada".into(),
                address: AddressEntity {
                    street: "s".into(),
                    city: "c".into(),
                },
                tags: vec![],
            }],
        };
        let mapper = Mapper::new();
        let dto: TeamDto = mapper.map(&team).unwrap();
        assert_eq!(dto.members.len(), 1);
        assert_eq!(dto.members[0].address.city, "c");
    }

    // ---- Projection (port of test_mapper_projection.py) --------------

    #[derive(Serialize)]
    struct OrderEntity {
        id: String,
        customer: String,
        quantity: u32,
        unit_price: f64,
        status: String,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct OrderSummary {
        id: String,
        status: String,
    }

    #[derive(Deserialize, PartialEq, Debug)]
    struct OrderWithTotal {
        id: String,
        total: f64,
    }

    fn order() -> OrderEntity {
        OrderEntity {
            id: "1".into(),
            customer: "alice".into(),
            quantity: 3,
            unit_price: 10.0,
            status: "shipped".into(),
        }
    }

    #[test]
    fn test_project_subset_fields() {
        let mapper = Mapper::new();
        let summary: OrderSummary = mapper.project(&order()).unwrap();
        assert_eq!(summary.id, "1");
        assert_eq!(summary.status, "shipped");
    }

    #[test]
    fn test_project_with_no_registration_uses_name_match() {
        let o = OrderEntity {
            id: "2".into(),
            customer: "bob".into(),
            quantity: 1,
            unit_price: 5.0,
            status: "pending".into(),
        };
        let mapper = Mapper::new();
        let summary: OrderSummary = mapper.project(&o).unwrap();
        assert_eq!(summary.id, "2");
        assert_eq!(summary.status, "pending");
    }

    #[test]
    fn test_computed_field_via_transform() {
        let mut mapper = Mapper::new();
        mapper.register_projection::<OrderEntity, OrderWithTotal>(Projection::new().computed(
            "total",
            |src: &Value| {
                let q = src["quantity"].as_f64().unwrap();
                let p = src["unit_price"].as_f64().unwrap();
                json!(q * p)
            },
        ));
        let result: OrderWithTotal = mapper.project(&order()).unwrap();
        assert_eq!(result.id, "1");
        assert_eq!(result.total, 30.0);
    }

    #[test]
    fn test_transform_overrides_source_field() {
        let mut mapper = Mapper::new();
        mapper.register_projection::<OrderEntity, OrderSummary>(
            Projection::new().computed("status", |src: &Value| {
                json!(src["status"].as_str().unwrap().to_uppercase())
            }),
        );
        let result: OrderSummary = mapper.project(&order()).unwrap();
        assert_eq!(result.id, "1");
        assert_eq!(result.status, "SHIPPED");
    }

    // ---- Rust-specific: source-not-object error ----------------------

    #[test]
    fn test_source_not_object_errors() {
        let mapper = Mapper::new();
        let result: Result<OrderSummary, _> = mapper.map(&vec![1, 2, 3]);
        assert!(matches!(result, Err(MapError::SourceNotObject)));
    }
}
