//! Schema builders (TS `builders.ts`): short functions that return plain JSON Schema values with the same JSON as the
//! TS builders, ready for a tool definition or `pi_json::to_js_string`, and compiled by `compile()` like any schema.
//!
//! Each builder takes `options`, a JSON object of keywords and annotations (`json!({ "minLength": 1 })`, or
//! `json!({})` for none) merged after the builder's own keywords in their order, as the TS spread does; a non-object
//! `options` adds nothing. TS's `Static<>` has no Rust equivalent: deserialize a validated value with serde.
//! TS's `bytes()` and `bigint()` are not ported (the value model is JSON).

use pi_json::JsonValue;
use serde_json::{Map, json};

/// A string, with optional length bounds (in code points), pattern and format.
pub fn str(options: JsonValue) -> JsonValue {
	with_options(json!({ "type": "string" }), options)
}

/// A number within optional bounds.
pub fn num(options: JsonValue) -> JsonValue {
	with_options(json!({ "type": "number" }), options)
}

/// An integer within optional bounds.
pub fn int(options: JsonValue) -> JsonValue {
	with_options(json!({ "type": "integer" }), options)
}

/// A boolean.
pub fn bool(options: JsonValue) -> JsonValue {
	with_options(json!({ "type": "boolean" }), options)
}

/// `null`.
pub fn nul(options: JsonValue) -> JsonValue {
	with_options(json!({ "type": "null" }), options)
}

/// Any value.
pub fn unknown(options: JsonValue) -> JsonValue {
	with_options(json!({}), options)
}

/// Exactly `value` (a string, number, boolean or null).
pub fn lit(value: JsonValue, options: JsonValue) -> JsonValue {
	with_options(object([("const", value)]), options)
}

/// One of the given values.
pub fn one_of(values: Vec<JsonValue>, options: JsonValue) -> JsonValue {
	with_options(object([("enum", JsonValue::Array(values))]), options)
}

/// Any of the given schemas. A union of objects tagged by a `lit()` property is checked by the tag: only the branch it
/// selects runs, and its errors are the ones reported.
pub fn union(schemas: Vec<JsonValue>, options: JsonValue) -> JsonValue {
	with_options(object([("anyOf", JsonValue::Array(schemas))]), options)
}

/// `schema` or `null`.
pub fn nullable(schema: JsonValue, options: JsonValue) -> JsonValue {
	let branches = JsonValue::Array(vec![schema, json!({ "type": "null" })]);
	with_options(object([("anyOf", branches)]), options)
}

/// An array of `items`, with optional length bounds and unique items.
pub fn arr(items: JsonValue, options: JsonValue) -> JsonValue {
	with_options(object([("type", json!("array")), ("items", items)]), options)
}

/// A fixed-length array whose items match `items` in order.
pub fn tuple(items: Vec<JsonValue>, options: JsonValue) -> JsonValue {
	let length = items.len();
	let schema = object([
		("type", json!("array")),
		("prefixItems", JsonValue::Array(items)),
		("items", json!(false)),
		("minItems", json!(length)),
	]);
	with_options(schema, options)
}

/// An object with string keys and values of `value` (a record).
pub fn map(value: JsonValue, options: JsonValue) -> JsonValue {
	with_options(
		object([("type", json!("object")), ("additionalProperties", value)]),
		options,
	)
}

/// `schema` as it is, for values the builders cannot express (a schema from another system).
pub fn unsafe_(schema: JsonValue) -> JsonValue {
	schema
}

/// A schema that contains itself, as a tree's children are trees: `build` receives a reference to the schema it
/// returns. `id` becomes the schema's `$id` and the target of the reference, so it must be unique among the recursive
/// schemas within one schema.
pub fn recursive(id: &str, build: impl FnOnce(JsonValue) -> JsonValue) -> JsonValue {
	with_options(build(json!({ "$ref": id })), json!({ "$id": id }))
}

/// A property of `obj()`: required, or optional (`opt()`).
#[derive(Clone, Debug, PartialEq)]
pub enum Property {
	/// A required property.
	Required(JsonValue),
	/// An optional property: it may be omitted.
	Optional(JsonValue),
}

impl From<JsonValue> for Property {
	fn from(schema: JsonValue) -> Self {
		Self::Required(schema)
	}
}

/// Marks an `obj()` property as optional.
pub fn opt(schema: JsonValue) -> Property {
	Property::Optional(schema)
}

/// A closed object: the given properties (required unless `opt()`) and no others. `"open": true` in `options` allows
/// other properties, as tool schemas from other systems often do.
pub fn obj(properties: Vec<(&str, Property)>, options: JsonValue) -> JsonValue {
	let mut annotations = match options {
		JsonValue::Object(map) => map,
		_ => Map::new(),
	};
	let open = annotations.shift_remove("open") == Some(JsonValue::Bool(true));
	let mut props = Map::new();
	let mut required = Vec::new();
	for (name, property) in properties {
		match property {
			Property::Optional(schema) => {
				props.insert(name.to_owned(), schema);
			}
			Property::Required(schema) => {
				props.insert(name.to_owned(), schema);
				required.push(JsonValue::String(name.to_owned()));
			}
		}
	}
	let mut schema = object([("type", json!("object")), ("properties", JsonValue::Object(props))]);
	if !required.is_empty() {
		schema = with_options(schema, object([("required", JsonValue::Array(required))]));
	}
	if !open {
		schema = with_options(schema, json!({ "additionalProperties": false }));
	}
	with_options(schema, JsonValue::Object(annotations))
}

/// A JSON object of `entries`, in order, moving the values in.
fn object<const N: usize>(entries: [(&str, JsonValue); N]) -> JsonValue {
	JsonValue::Object(entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// `{ ...schema, ...options }`: a key already present keeps its place and takes the new value; a schema or options
/// that is not an object spreads nothing, as in JS.
fn with_options(schema: JsonValue, options: JsonValue) -> JsonValue {
	let mut map = match schema {
		JsonValue::Object(map) => map,
		_ => Map::new(),
	};
	if let JsonValue::Object(options) = options {
		map.extend(options);
	}
	JsonValue::Object(map)
}
