//! Tool argument validation (`validation.rs`): ports `validation.test.ts` and the plain-JSON-schema cases of
//! `validation-probes.test.ts` (its TypeBox-built probe table needs TypeBox to build the schemas; pi-schema's
//! TypeBox conformance in C2 covers those keywords).

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

mod common;

use pi_ai::validation::{validate_tool_arguments, validate_tool_call};
use pi_json::JsonValue;
use pi_types::{Tool, ToolCall};
use serde_json::json;

fn tool(schema: JsonValue) -> Tool {
	common::from_json(json!({ "name": "echo", "description": "Echo tool", "parameters": schema }))
}

fn call(arguments: JsonValue) -> ToolCall {
	common::from_json(json!({ "type": "toolCall", "id": "tool-1", "name": "echo", "arguments": arguments }))
}

fn with_value(schema: JsonValue, value: JsonValue) -> (Tool, ToolCall) {
	(
		tool(json!({ "type": "object", "properties": { "value": schema }, "required": ["value"] })),
		call(json!({ "value": value })),
	)
}

/// The probes' outcome format: `ok <arguments>`, `REJECT <first error line>` or `THROW <first line>`.
fn outcome(schema: JsonValue, arguments: JsonValue) -> String {
	match validate_tool_arguments(&tool(schema), &call(arguments)) {
		Ok(value) => format!("ok {}", pi_json::to_js_string(&value)),
		Err(error) if error.0.starts_with("Validation failed") => {
			format!("REJECT {}", error.0.lines().nth(1).unwrap_or_default().trim())
		}
		Err(error) => format!("THROW {}", error.0.lines().next().unwrap_or_default()),
	}
}

// TS: validation.test.ts
#[test]
fn still_validates_when_function_constructor_is_unavailable() {
	// pi-schema never generates code; the TS case guards a code-generating validator.
	let tool =
		tool(json!({ "type": "object", "properties": { "count": { "type": "number" } }, "required": ["count"] }));
	assert_eq!(
		validate_tool_arguments(&tool, &call(json!({ "count": "42" }))).unwrap(),
		json!({ "count": 42 })
	);
}

// TS: validation.test.ts
#[test]
fn coerces_serialized_plain_json_schemas_like_typebox_schemas() {
	for (schema, input, expected) in [
		(json!({ "type": "number" }), json!("42"), json!(42)),
		(json!({ "type": "number" }), json!(true), json!(1)),
		(json!({ "type": "number" }), JsonValue::Null, json!(0)),
		(json!({ "type": "integer" }), json!("42"), json!(42)),
		(json!({ "type": "boolean" }), json!("true"), json!(true)),
		(json!({ "type": "boolean" }), json!("false"), json!(false)),
		(json!({ "type": "boolean" }), json!(1), json!(true)),
		(json!({ "type": "boolean" }), json!(0), json!(false)),
		(json!({ "type": "string" }), JsonValue::Null, json!("null")),
		(json!({ "type": "string" }), json!(true), json!("true")),
		(json!({ "type": "null" }), json!(""), JsonValue::Null),
		(json!({ "type": "null" }), json!(0), JsonValue::Null),
		(json!({ "type": "null" }), json!(false), JsonValue::Null),
		(json!({ "type": ["number", "string"] }), json!("1"), json!("1")),
		(json!({ "type": ["boolean", "number"] }), json!("1"), json!(1)),
		(json!({ "type": "boolean" }), json!("1"), json!(true)),
		(json!({ "type": "boolean" }), json!("0"), json!(false)),
		(json!({ "type": "null" }), json!("null"), JsonValue::Null),
		(json!({ "type": "integer" }), json!("42.1"), json!(42)),
		(
			json!({ "type": "array", "items": { "type": "number" } }),
			json!("7"),
			json!([7]),
		),
	] {
		let (tool, call) = with_value(schema.clone(), input.clone());
		assert_eq!(
			validate_tool_arguments(&tool, &call).unwrap(),
			json!({ "value": expected }),
			"{schema} {input}"
		);
	}
}

// TS: validation.test.ts
#[test]
fn treats_null_as_omission_for_optional_non_nullable_properties() {
	let schema = json!({ "type": "object", "properties": {
		"path": { "type": "string" }, "offset": { "type": "number" },
		"nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
		"metadata": { "type": "object", "properties": { "enabled": { "type": "boolean" } } }
	}, "required": ["path", "metadata"] });
	let result = validate_tool_arguments(
		&tool(schema),
		&call(json!({ "path": "file.txt", "offset": null, "nullable": null, "metadata": { "enabled": null } })),
	);
	assert_eq!(
		result.unwrap(),
		json!({ "path": "file.txt", "nullable": null, "metadata": {} })
	);
}

// TS: validation.test.ts
#[test]
fn preserves_optional_nulls_whose_referenced_schema_is_nullable() {
	let schema = json!({ "type": "object", "properties": { "value": { "$ref": "#/$defs/value" } }, "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } } });
	assert_eq!(
		validate_tool_arguments(&tool(schema), &call(json!({ "value": null }))).unwrap(),
		json!({ "value": null })
	);
}

// TS: validation.test.ts
#[test]
fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
	let schema = json!({ "type": "object", "properties": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }, "required": ["value"] });
	assert_eq!(
		validate_tool_arguments(&tool(schema), &call(json!({ "value": null }))).unwrap(),
		json!({ "value": null })
	);
}

// TS: validation.test.ts
#[test]
fn preserves_a_value_that_already_matches_a_one_of_nullable_union_arm() {
	let (tool, call) = with_value(
		json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] }),
		JsonValue::Null,
	);
	assert_eq!(validate_tool_arguments(&tool, &call).unwrap(), json!({ "value": null }));
}

// TS: validation.test.ts
#[test]
fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
	let (tool, call) = with_value(
		json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] }),
		json!("42"),
	);
	assert_eq!(validate_tool_arguments(&tool, &call).unwrap(), json!({ "value": 42 }));
}

// TS: validation.test.ts
#[test]
fn accepts_null_for_nullable_array_schemas_with_items() {
	let (tool, call) = with_value(
		json!({ "type": ["array", "null"], "items": { "type": "string" } }),
		JsonValue::Null,
	);
	assert_eq!(validate_tool_arguments(&tool, &call).unwrap(), json!({ "value": null }));
}

// TS: validation.test.ts
#[test]
fn rejects_invalid_coercions_for_serialized_plain_json_schemas() {
	for (schema, input) in [
		(json!({ "type": "boolean" }), json!("yes")),
		(json!({ "type": "number" }), json!("abc")),
		(json!({ "type": "null" }), json!("nil")),
		(json!({ "type": "integer" }), json!("4x")),
	] {
		let (tool, call) = with_value(schema, input);
		assert!(
			validate_tool_arguments(&tool, &call)
				.unwrap_err()
				.0
				.contains("Validation failed")
		);
	}
}

// TS: validation.test.ts
#[test]
fn lists_pi_schemas_errors_then_the_arguments_it_received() {
	let (tool, call) = with_value(json!({ "type": "number" }), json!("many"));
	assert_eq!(
		validate_tool_arguments(&tool, &call).unwrap_err().0,
		"Validation failed for tool \"echo\":\n  - value: expected number, got string\n\nReceived arguments:\n{\n  \"value\": \"many\"\n}"
	);
}

// TS: validation.test.ts
#[test]
fn reports_a_missing_required_property_at_the_property() {
	let (tool, _) = with_value(json!({ "type": "string" }), json!("x"));
	assert!(
		validate_tool_arguments(&tool, &call(json!({})))
			.unwrap_err()
			.0
			.contains("  - value: missing")
	);
}

// TS: validation.test.ts
#[test]
fn returns_a_copy_of_the_arguments_also_when_nothing_is_coerced() {
	let (tool, call) = with_value(json!({ "type": "string" }), json!("x"));
	assert_eq!(validate_tool_arguments(&tool, &call).unwrap(), json!({ "value": "x" }));
}

// #10897
// TS: validation.test.ts
#[test]
fn reports_malformed_arguments_json_instead_of_validating_empty_arguments() {
	let mut call = call(json!({}));
	call.arguments_error = Some("Unexpected token".into());
	let tool = tool(json!({ "type": "object", "properties": { "value": { "type": "string" } } }));
	assert_eq!(
		validate_tool_arguments(&tool, &call).unwrap_err().0,
		"Invalid arguments for tool \"echo\": Unexpected token"
	);
}

// Rust-only.
#[test]
fn validate_tool_call_finds_the_tool_by_name() {
	assert_eq!(
		validate_tool_call(&[], &call(json!({}))).unwrap_err().0,
		"Tool \"echo\" not found"
	);
}

// TS: validation-probes.test.ts
#[test]
fn scalar_or_array_that_refers_to_itself() {
	let schema = json!({ "type": "object", "properties": { "value": { "anyOf": [{ "type": "number" }, { "type": "array", "items": { "$ref": "#/properties/value" } }] } }, "required": ["value"] });
	assert_eq!(outcome(schema, json!({ "value": "42" })), r#"ok {"value":42}"#);
}

// TS: validation-probes.test.ts
#[test]
fn string_enum_with_an_empty_member_from_null() {
	let schema = json!({ "type": "object", "properties": { "mode": { "type": "string", "enum": ["", "x"] } }, "required": ["mode"] });
	assert_eq!(outcome(schema, json!({ "mode": null })), r#"ok {"mode":""}"#);
}

// TS: validation-probes.test.ts
#[test]
fn recursive_union_with_a_value_no_branch_converts() {
	let schema = json!({ "type": "object", "properties": { "t": { "$ref": "#/$defs/Tree" } }, "$defs": { "Tree": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "$ref": "#/$defs/Tree" } }] } } });
	assert_eq!(
		outcome(schema, json!({ "t": {} })),
		"REJECT - t: expected string, got object"
	);
}

// TS: validation-probes.test.ts
#[test]
fn accepts_the_last_member_of_a_10000_value_enum() {
	let values: Vec<String> = (0..10_000).map(|index| format!("member{index}")).collect();
	let schema = json!({ "type": "object", "properties": { "choice": { "type": "string", "enum": values } }, "required": ["choice"] });
	assert_eq!(
		outcome(schema.clone(), json!({ "choice": "member9999" })),
		r#"ok {"choice":"member9999"}"#
	);
	let first: String = pi_json::to_js_string(&json!(values[..20]));
	assert_eq!(
		outcome(schema, json!({ "choice": "x" })),
		format!(
			"REJECT - choice: expected one of {}, ... and 9980 more]",
			&first[..first.len() - 1]
		)
	);
}

// TS: validation-probes.test.ts
#[test]
fn drops_an_optional_null_inside_property_values() {
	let item = json!({ "type": "object", "properties": { "n": { "type": "integer", "minimum": 1 } } });
	for schema in [
		json!({ "type": "object", "properties": { "entry": item } }),
		json!({ "type": "object", "additionalProperties": item }),
		json!({ "type": "object", "patternProperties": { "^entry$": item }, "additionalProperties": false }),
	] {
		assert_eq!(outcome(schema, json!({ "entry": { "n": null } })), r#"ok {"entry":{}}"#);
	}
}

// TS: validation-probes.test.ts
#[test]
fn converts_a_branch_together_with_the_nodes_sibling_properties() {
	for keyword in ["anyOf", "oneOf"] {
		let mut schema = json!({ "type": "object", "properties": { "c": { "type": "integer" } }, "required": ["c"] });
		schema[keyword] = json!([
			{ "type": "object", "properties": { "a": { "type": "array", "items": { "type": "integer" } } }, "required": ["a"] },
			{ "type": "object", "properties": { "b": { "type": "boolean" } }, "required": ["b"] }
		]);
		assert_eq!(
			outcome(schema, json!({ "a": "3", "c": "2" })),
			r#"ok {"a":[3],"c":2}"#,
			"{keyword}"
		);
	}
}

// TS: validation-probes.test.ts
#[test]
fn rejects_a_deep_value_every_branch_of_a_recursive_object_union_rejects_without_exponential_work() {
	let s = json!({ "anyOf": [
		{ "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["a"] },
		{ "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["b"] }
	] });
	let schema = json!({ "type": "object", "properties": { "s": { "$ref": "#/$defs/S" } }, "$defs": { "S": s } });
	let mut value = json!({ "x": 1 });
	for _ in 0..40 {
		value = json!({ "c": value });
	}
	assert_eq!(outcome(schema, json!({ "s": value })), "REJECT - s.a: missing");
}
