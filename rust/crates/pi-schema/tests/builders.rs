//! The builders (TS `test/builders.test.ts`): the JSON Schema they produce and how they compile. TS's static-type
//! cases have no Rust equivalent (validated values are deserialized with serde), and `bytes()`/`bigint()` are not
//! ported: the value model is JSON.

#![allow(
	clippy::expect_used,
	reason = "test helpers fail the test on a schema that does not compile"
)]

use pi_json::JsonValue;
use pi_schema::builders::{
	arr, bool, int, lit, map, nul, nullable, num, obj, one_of, opt, recursive, str, tuple, union, unknown, unsafe_,
};
use pi_schema::{CompileOptions, Compiled, ValidationError, compile};
use serde_json::json;

fn plain(schema: &JsonValue) -> Compiled {
	compile(schema, CompileOptions::default()).expect("the schema compiles")
}

fn none() -> JsonValue {
	json!({})
}

#[test]
fn produce_plain_json_schema() {
	let schema = obj(
		vec![
			(
				"path",
				str(json!({ "description": "File path", "minLength": 1 })).into(),
			),
			("offset", opt(int(json!({ "minimum": 1 })))),
			("mode", one_of(vec![json!("read"), json!("write")], none()).into()),
		],
		none(),
	);
	assert_eq!(
		pi_json::to_js_string(&schema),
		pi_json::to_js_string(&json!({
			"type": "object",
			"properties": {
				"path": { "type": "string", "description": "File path", "minLength": 1 },
				"offset": { "type": "integer", "minimum": 1 },
				"mode": { "enum": ["read", "write"] },
			},
			"required": ["path", "mode"],
			"additionalProperties": false,
		}))
	);
}

#[test]
fn produce_every_leaf() {
	assert_eq!(
		[
			str(none()),
			num(none()),
			int(none()),
			bool(none()),
			nul(none()),
			unknown(none()),
			lit(json!(3), none())
		],
		[
			json!({ "type": "string" }),
			json!({ "type": "number" }),
			json!({ "type": "integer" }),
			json!({ "type": "boolean" }),
			json!({ "type": "null" }),
			json!({}),
			json!({ "const": 3 }),
		]
	);
	assert_eq!(
		arr(str(none()), json!({ "maxItems": 2 })),
		json!({ "type": "array", "items": { "type": "string" }, "maxItems": 2 })
	);
	assert_eq!(
		map(num(none()), none()),
		json!({ "type": "object", "additionalProperties": { "type": "number" } })
	);
	assert_eq!(
		pi_json::to_js_string(&tuple(vec![str(none()), num(none())], none())),
		r#"{"type":"array","prefixItems":[{"type":"string"},{"type":"number"}],"items":false,"minItems":2}"#
	);
	assert_eq!(
		nullable(str(none()), none()),
		json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
	);
	assert_eq!(
		obj(vec![("a", str(none()).into())], json!({ "open": true })),
		json!({ "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a"] })
	);
}

#[test]
fn type_a_schema_as_given_with_unsafe() {
	let usage = unsafe_(none());
	let level = unsafe_(json!({ "type": "string", "enum": ["low", "high"] }));
	assert_eq!(usage, json!({}));
	assert_eq!(level, json!({ "type": "string", "enum": ["low", "high"] }));
	// Nothing checks the given type: an unconstrained schema accepts any value.
	assert!(plain(&usage).check(&json!("not usage")));
	assert!(!plain(&level).check(&json!("medium")));
}

#[test]
fn build_a_recursive_schema_that_checks_every_level() {
	let tree = recursive("Tree", |tree| {
		obj(
			vec![("name", str(none()).into()), ("children", arr(tree, none()).into())],
			none(),
		)
	});
	assert_eq!(
		pi_json::to_js_string(&tree),
		pi_json::to_js_string(&json!({
			"type": "object",
			"properties": {
				"name": { "type": "string" },
				"children": { "type": "array", "items": { "$ref": "Tree" } },
			},
			"required": ["name", "children"],
			"additionalProperties": false,
			"$id": "Tree",
		}))
	);
	let validator = plain(&tree);
	assert!(validator.check(&json!({ "name": "a", "children": [{ "name": "b", "children": [] }] })));
	let deep = json!({ "name": "a", "children": [{ "name": "b", "children": [{ "name": 1, "children": [] }] }] });
	assert_eq!(
		validator.errors(&deep),
		[ValidationError::new(
			"/children/0/children/0/name",
			"type",
			"expected string, got integer"
		)]
	);
}

#[test]
fn nest_recursive_schemas_with_distinct_ids_inside_other_schemas() {
	let tree = recursive("Tree", |tree| {
		obj(
			vec![("name", str(none()).into()), ("children", arr(tree, none()).into())],
			none(),
		)
	});
	let list = recursive("List", |list| {
		obj(
			vec![("value", num(none()).into()), ("next", nullable(list, none()).into())],
			none(),
		)
	});
	let both = plain(&obj(vec![("tree", tree.into()), ("list", opt(list))], none()));
	let root = json!({ "name": "a", "children": [{ "name": "b", "children": [] }] });
	assert!(both.check(&json!({ "tree": root, "list": { "value": 1, "next": { "value": 2, "next": null } } })));
	assert!(!both.check(&json!({ "tree": root, "list": { "value": 1, "next": { "value": "2", "next": null } } })));
	assert!(
		!both.check(&json!({ "tree": { "name": "a", "children": [{ "name": "b", "children": [{ "name": 3 }] }] } }))
	);
}

#[test]
fn compile_into_checks_with_the_declared_semantics() {
	let validator = plain(&obj(
		vec![
			("name", str(json!({ "minLength": 1 })).into()),
			("tags", opt(arr(str(none()), json!({ "uniqueItems": true })))),
		],
		none(),
	));
	assert!(validator.check(&json!({ "name": "a" })));
	assert!(validator.check(&json!({ "name": "a", "tags": ["x", "y"] })));
	assert_eq!(
		validator.errors(&json!({ "name": "", "tags": ["x", "x"], "other": 1 })),
		[
			ValidationError::new("/name", "minLength", "expected at least 1 characters"),
			ValidationError::new("/tags", "uniqueItems", "expected unique items"),
			ValidationError::new("/other", "additionalProperties", "unknown field"),
		]
	);
}

// TS: "carry the static type of valid values", on the same union; here what it accepts and how it reports.
#[test]
fn carry_the_tag_of_a_union_of_objects() {
	let message = union(
		vec![
			obj(
				vec![
					("kind", lit(json!("text"), none()).into()),
					("text", str(none()).into()),
				],
				none(),
			),
			obj(
				vec![
					("kind", lit(json!("image"), none()).into()),
					("url", str(none()).into()),
					("alt", opt(str(none()))),
				],
				none(),
			),
		],
		none(),
	);
	let validator = plain(&message);
	assert!(validator.check(&json!({ "kind": "image", "url": "u" })));
	assert_eq!(
		validator.errors(&json!({ "kind": "video" })),
		[ValidationError::new(
			"/kind",
			"anyOf",
			r#"expected one of ["text","image"]"#
		)]
	);
}

// TS: "narrow values through a compiled check", which also checks the narrowed static type; here a checked value
// is read as JSON (or deserialized with serde).
#[test]
fn narrow_values_through_a_compiled_check() {
	let point = obj(
		vec![
			("x", num(none()).into()),
			("y", num(none()).into()),
			("label", opt(str(none()))),
		],
		none(),
	);
	let value = json!({ "x": 1, "y": 2 });
	assert!(plain(&point).check(&value));
	assert_eq!(
		value["x"].as_f64().zip(value["y"].as_f64()).map(|(x, y)| x + y),
		Some(3.0)
	);
}
