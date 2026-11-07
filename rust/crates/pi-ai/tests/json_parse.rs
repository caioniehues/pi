//! Repairing and lenient JSON parsing (`json_parse.rs`): ports `json-parse.test.ts`.

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

use pi_ai::json_parse::{parse_streaming_json, parse_tool_call_arguments};
use serde_json::json;

// #10868
// TS: json-parse.test.ts
#[test]
fn keeps_invalid_u_escapes_as_literal_text_without_dropping_fields() {
	assert_eq!(
		parse_streaming_json(r#"{"text":"\url{x}"}"#),
		json!({ "text": "\\url{x}" })
	);
	assert_eq!(
		parse_streaming_json(r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#),
		json!({ "path": "a.tex", "content": "\\usepackage{x}", "mode": "w" })
	);
}

// TS: json-parse.test.ts
#[test]
fn decodes_valid_u_escapes() {
	assert_eq!(
		parse_streaming_json(r#"{"text":"café \url"}"#),
		json!({ "text": "café \\url" })
	);
}

// TS: json-parse.test.ts
#[test]
fn omits_a_u_escape_that_is_cut_off_mid_stream() {
	assert_eq!(parse_streaming_json(r#"{"text":"ab\u00"#), json!({ "text": "ab" }));
}

// #10897
// TS: json-parse.test.ts
#[test]
fn parses_complete_json_and_repairs_invalid_escapes() {
	let parsed = parse_tool_call_arguments(r#"{"kind":"seek","note":"\url"}"#);
	assert_eq!(
		serde_json::Value::Object(parsed.arguments),
		json!({ "kind": "seek", "note": "\\url" })
	);
	assert_eq!(parsed.arguments_error, None);
}

// TS: json-parse.test.ts
#[test]
fn uses_empty_arguments_for_empty_text() {
	let parsed = parse_tool_call_arguments("");
	assert!(parsed.arguments.is_empty() && parsed.arguments_error.is_none());
}

// TS: json-parse.test.ts
#[test]
fn does_not_keep_the_valid_prefix_of_malformed_json() {
	let raw = r#"{"action":"apply","actions":[{"kind":"seek","time":.5}]}"#;
	let parsed = parse_tool_call_arguments(raw);
	assert!(parsed.arguments.is_empty());
	let error = parsed.arguments_error.unwrap();
	assert!(error.contains("\n\nReceived arguments:\n") && error.ends_with(raw));
}

// TS: json-parse.test.ts
#[test]
fn does_not_complete_cut_off_json() {
	let raw = r#"{"kind":"seek","clipId":"clip-fl"#;
	let parsed = parse_tool_call_arguments(raw);
	assert!(parsed.arguments.is_empty());
	assert!(parsed.arguments_error.unwrap().ends_with(raw));
}

// TS: json-parse.test.ts
#[test]
fn rejects_json_that_is_not_an_object() {
	let parsed = parse_tool_call_arguments("[1,2]");
	assert!(parsed.arguments.is_empty());
	assert_eq!(
		parsed.arguments_error.as_deref(),
		Some("Tool arguments must be a JSON object\n\nReceived arguments:\n[1,2]")
	);
}
