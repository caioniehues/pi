//! Constrained sampling (`constrained_sampling.rs`): ports the API-independent cases of
//! `constrained-sampling.test.ts`; the cases that convert or stream OpenAI Responses items are C13a's (they need
//! `openai-responses-shared`).

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

use pi_ai::constrained_sampling::{
	ConstrainedSamplingError, GrammarFormat, GrammarToolInputJsonBuffer, append_grammar_tool_input_json_delta,
	close_grammar_tool_input_json, make_strict_json_schema, resolve_grammar_constrained_sampling,
	resolve_json_schema_strict_sampling,
};
use pi_json::JsonValue;
use pi_types::Tool;
use serde_json::json;

fn tool(parameters: JsonValue, constrained: Option<JsonValue>) -> Tool {
	let mut value = json!({ "name": "sample_tool", "description": "Sample tool", "parameters": parameters });
	if let Some(constrained) = constrained {
		value["constrainedSampling"] = constrained;
	}
	common::from_json(value)
}

fn payload_schema() -> JsonValue {
	json!({ "type": "object", "properties": { "payload": { "type": "string" } }, "required": ["payload"], "additionalProperties": false })
}

// TS: constrained-sampling.test.ts
#[test]
fn converts_supported_constraints_and_falls_back_when_unsupported() {
	let prefer = tool(
		payload_schema(),
		Some(json!({ "type": "json_schema", "strict": "prefer" })),
	);
	assert_eq!(resolve_json_schema_strict_sampling(&prefer, true, None), Ok(Some(true)));
	let require = tool(
		payload_schema(),
		Some(json!({ "type": "json_schema", "strict": "require" })),
	);
	assert!(
		resolve_json_schema_strict_sampling(&require, false, None)
			.unwrap_err()
			.to_string()
			.starts_with("Tool \"sample_tool\" requires JSON-schema constrained sampling")
	);
	let grammar = tool(
		payload_schema(),
		Some(json!({ "type": "grammar", "variants": { "openai_lark": "start: /[a-z]+/" } })),
	);
	let resolved = resolve_grammar_constrained_sampling(&grammar, true).unwrap().unwrap();
	assert_eq!(
		(
			resolved.format,
			resolved.definition.as_str(),
			resolved.input_property.as_str()
		),
		(GrammarFormat::Lark, "start: /[a-z]+/", "payload")
	);
	let empty = tool(payload_schema(), Some(json!({ "type": "grammar", "variants": {} })));
	assert_eq!(
		resolve_grammar_constrained_sampling(&empty, true)
			.unwrap_err()
			.to_string(),
		"Tool \"sample_tool\" cannot use grammar constrained sampling: no supported grammar variant was provided."
	);
	assert_eq!(resolve_grammar_constrained_sampling(&grammar, false), Ok(None));
	assert_eq!(
		resolve_json_schema_strict_sampling(&tool(payload_schema(), Some(json!(false))), true, None),
		Ok(None)
	);
}

// TS: constrained-sampling.test.ts
#[test]
fn derives_strict_provider_schemas_without_changing_tool_definitions() {
	let parameters = json!({ "type": "object", "properties": {
		"path": { "type": "string" }, "offset": { "type": "number" },
		"metadata": { "type": "object", "properties": { "enabled": { "type": "boolean" } } },
		"nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
	}, "required": ["path", "metadata"] });
	let strict = JsonValue::Object(make_strict_json_schema(&parameters, None).unwrap());
	assert!(parameters.get("additionalProperties").is_none());
	assert_eq!(parameters["required"], json!(["path", "metadata"]));
	assert_eq!(strict["additionalProperties"], json!(false));
	assert_eq!(strict["required"], json!(["path", "offset", "metadata", "nullable"]));
	assert_eq!(
		strict["properties"]["offset"],
		json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] })
	);
	assert_eq!(strict["properties"]["metadata"]["additionalProperties"], json!(false));
	assert_eq!(strict["properties"]["metadata"]["required"], json!(["enabled"]));
	assert_eq!(
		strict["properties"]["metadata"]["properties"]["enabled"],
		json!({ "anyOf": [{ "type": "boolean" }, { "type": "null" }] })
	);
	assert_eq!(
		strict["properties"]["nullable"],
		json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
	);
}

// TS: constrained-sampling.test.ts
#[test]
fn falls_back_or_rejects_schemas_that_cannot_be_safely_converted() {
	for (parameters, error) in [
		(
			json!({ "type": "object", "properties": { "metadata": { "type": "object", "properties": {}, "additionalProperties": { "type": "string" } } }, "required": ["metadata"] }),
			"additionalProperties is unsupported",
		),
		(
			json!({ "allOf": [{ "type": "object", "properties": { "a": { "type": "string" } } }, { "type": "object", "properties": { "b": { "type": "number" } } }] }),
			"allOf schemas are unsupported",
		),
		(
			json!({ "type": "object", "properties": { "value": { "anyOf": [{ "type": "object", "properties": { "nested": { "type": "string" } } }, { "type": "null" }] } }, "required": ["value"] }),
			"object and array unions are unsupported",
		),
		(
			json!({ "type": "object", "properties": { "child": { "$ref": "https://example.com/child.json" } }, "required": ["child"] }),
			"$ref schemas are unsupported",
		),
	] {
		let Err(ConstrainedSamplingError::UnsupportedStrictSchema(reason)) = make_strict_json_schema(&parameters, None)
		else {
			panic!("{error}")
		};
		assert!(reason.contains(error), "{reason}");
		let prefer = tool(
			parameters.clone(),
			Some(json!({ "type": "json_schema", "strict": "prefer" })),
		);
		assert_eq!(resolve_json_schema_strict_sampling(&prefer, true, None), Ok(None));
		let require = tool(parameters, Some(json!({ "type": "json_schema", "strict": "require" })));
		assert!(
			resolve_json_schema_strict_sampling(&require, true, None)
				.unwrap_err()
				.to_string()
				.contains(error)
		);
	}
}

// TS: constrained-sampling.test.ts
#[test]
fn keeps_grammar_input_json_deltas_append_only() {
	let mut buffer = GrammarToolInputJsonBuffer::default();
	let first = append_grammar_tool_input_json_delta(&mut buffer, "payload", "a\"")
		.unwrap()
		.unwrap();
	let second = close_grammar_tool_input_json(&mut buffer, "payload", Some("a\"\nb"))
		.unwrap()
		.unwrap();
	assert_eq!(
		pi_json::parse(&format!("{first}{second}")).unwrap(),
		json!({ "payload": "a\"\nb" })
	);
	assert_eq!(
		close_grammar_tool_input_json(&mut buffer, "payload", Some("a\"\nb")),
		Ok(None)
	);
	let changed = "grammar tool input for property \"payload\" changed after it was closed";
	assert_eq!(
		close_grammar_tool_input_json(&mut buffer, "payload", Some("changed"))
			.unwrap_err()
			.to_string(),
		changed
	);
	assert_eq!(
		append_grammar_tool_input_json_delta(&mut buffer, "payload", "x")
			.unwrap_err()
			.to_string(),
		changed
	);
}

// TS: constrained-sampling.test.ts
#[test]
fn rejects_a_final_grammar_input_that_does_not_continue_the_streamed_input() {
	let mut buffer = GrammarToolInputJsonBuffer::default();
	append_grammar_tool_input_json_delta(&mut buffer, "payload", "abc").unwrap();
	assert_eq!(
		close_grammar_tool_input_json(&mut buffer, "payload", Some("abd"))
			.unwrap_err()
			.to_string(),
		"grammar tool input for property \"payload\" changed non-monotonically"
	);
}
