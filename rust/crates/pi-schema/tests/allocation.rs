//! `check()` allocates nothing on the success path: tool schemas of the shapes pi validates (closed objects, tagged
//! unions, `$ref` definitions, recursion, enums, bounds, formats without a regular expression) checked against valid
//! values under a counting allocator. The keywords that do allocate are named in `src/checks.rs` (`pattern` and
//! regex formats through the regex engine, `propertyNames`, `unevaluated*`, `$dynamicRef`).

#![allow(
	clippy::expect_used,
	reason = "test helpers fail the test on a schema that does not compile"
)]

use pi_json::JsonValue;
use pi_schema::{CompileOptions, compile, formats};
use serde_json::json;

/// Each schema with a valid value: tool-call shapes, then the other applicators.
fn cases() -> Vec<(JsonValue, JsonValue)> {
	let mut cases = tool_cases();
	cases.extend(shape_cases());
	cases
}

fn tool_cases() -> Vec<(JsonValue, JsonValue)> {
	let read = json!({
		"type": "object",
		"properties": {
			"path": { "type": "string", "description": "Path to the file", "minLength": 1 },
			"offset": { "type": "integer", "minimum": 1 },
			"limit": { "type": "integer", "minimum": 1, "maximum": 2000 },
			"mode": { "enum": ["text", "binary"] },
		},
		"required": ["path"],
		"additionalProperties": false,
	});
	let blocks = json!({
		"type": "array",
		"items": {
			"anyOf": [
				{
					"type": "object",
					"properties": { "kind": { "const": "text" }, "text": { "type": "string" } },
					"required": ["kind", "text"],
				},
				{
					"type": "object",
					"properties": { "kind": { "const": "image" }, "width": { "type": "integer", "minimum": 0 } },
					"required": ["kind", "width"],
				},
				{
					"type": "object",
					"properties": {
						"kind": { "const": "tool" },
						"args": { "type": "object", "additionalProperties": true },
					},
					"required": ["kind"],
				},
			],
		},
	});
	let mcp = json!({
		"type": "object",
		"properties": {
			"query": { "$ref": "#/$defs/Query" },
			"tags": { "type": "array", "items": { "type": "string" }, "uniqueItems": true, "maxItems": 8 },
			"price": { "type": "number", "multipleOf": 0.01, "exclusiveMinimum": 0 },
			"when": { "type": "string", "format": "date-time" },
			"nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
		},
		"required": ["query"],
		"$defs": {
			"Query": {
				"type": "object",
				"properties": {
					"field": { "type": "string" },
					"op": { "enum": ["eq", "ne", "lt", "gt"] },
					"value": {},
				},
				"required": ["field", "op"],
				"additionalProperties": false,
			},
		},
	});
	vec![
		(
			read,
			json!({ "path": "src/main.rs", "offset": 10, "limit": 100, "mode": "text" }),
		),
		(
			blocks,
			json!([
				{ "kind": "text", "text": "hi" },
				{ "kind": "image", "width": 3 },
				{ "kind": "tool", "args": { "a": 1 } },
			]),
		),
		(
			mcp,
			json!({
				"query": { "field": "name", "op": "eq", "value": [1, 2] },
				"tags": ["a", "b"],
				"price": 19.99,
				"when": "2024-01-01T12:00:00Z",
				"nullable": null,
			}),
		),
	]
}

fn shape_cases() -> Vec<(JsonValue, JsonValue)> {
	let tree = json!({
		"$defs": {
			"Node": {
				"type": "object",
				"properties": {
					"name": { "type": "string" },
					"children": { "type": "array", "items": { "$ref": "#/$defs/Node" } },
				},
				"required": ["name"],
				"additionalProperties": false,
			},
		},
		"$ref": "#/$defs/Node",
	});
	let tuple = json!({
		"type": "array",
		"prefixItems": [{ "type": "string" }, { "type": "number" }],
		"items": false,
		"minItems": 2,
	});
	let conditional = json!({
		"type": "object",
		"if": { "properties": { "kind": { "const": "a" } } },
		"then": { "required": ["a"] },
		"else": { "required": ["b"] },
		"oneOf": [{ "required": ["a"] }, { "required": ["b"] }],
		"not": { "required": ["c"] },
		"minProperties": 2,
		"dependentRequired": { "a": ["kind"] },
	});
	vec![
		(
			tree,
			json!({ "name": "root", "children": [{ "name": "a", "children": [{ "name": "b" }] }, { "name": "c" }] }),
		),
		(tuple, json!(["a", 1.5])),
		(conditional, json!({ "kind": "a", "a": 1 })),
	]
}

#[test]
fn check_allocates_nothing_for_valid_values() {
	for (schema, value) in cases() {
		let options = CompileOptions {
			formats: formats::standard(),
			..CompileOptions::default()
		};
		let validator = compile(&schema, options).expect("the schema compiles");
		assert!(validator.check(&value), "{value} is valid for {schema}");
		let info = allocation_counter::measure(|| {
			for _ in 0..10 {
				assert!(validator.check(std::hint::black_box(&value)));
			}
		});
		assert_eq!(info.count_total, 0, "allocations checking {value} against {schema}");
	}
}
