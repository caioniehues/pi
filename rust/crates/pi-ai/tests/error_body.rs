//! Provider error normalization (`error_body.rs`): ports `error-body.test.ts`. The SDK shapes are described by
//! `SdkError` fields; the case that drives a real `openai` SDK connection is C3b's (it needs the wire client).

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

use pi_ai::error_body::{
	ErrorCause, MAX_PROVIDER_ERROR_BODY_CHARS, SdkBodyField, SdkError, ThrownValue, format_provider_error,
	normalize_provider_error, truncate_error_text, with_cause_detail,
};
use serde_json::json;

fn error(message: &str) -> SdkError {
	SdkError {
		message: message.into(),
		..SdkError::default()
	}
}

fn normalize(error: SdkError) -> pi_ai::error_body::NormalizedProviderError {
	normalize_provider_error(&ThrownValue::Error(error))
}

// TS: error-body.test.ts
#[test]
fn extracts_status_and_body_from_a_mistral_shaped_error() {
	let norm = normalize(SdkError {
		status_code: Some(403),
		body: Some(r#"{"error":"blocked by gateway WAF"}"#.into()),
		..error("Mistral request failed")
	});
	assert_eq!(
		(norm.status, norm.body.as_deref(), norm.message_carries_body),
		(Some(403), Some(r#"{"error":"blocked by gateway WAF"}"#), false)
	);
}

// TS: error-body.test.ts
#[test]
fn reads_the_parsed_body_off_an_openai_api_error_when_the_message_is_opaque() {
	let norm = normalize(SdkError {
		status: Some(403),
		error: Some(SdkBodyField::Json(json!({ "error": "blocked by gateway WAF" }))),
		..error("403 status code (no body)")
	});
	assert_eq!(
		(norm.status, norm.body.as_deref(), norm.message_carries_body),
		(Some(403), Some(r#"{"error":"blocked by gateway WAF"}"#), false)
	);
}

// TS: error-body.test.ts
#[test]
fn preserves_the_message_when_google_genai_already_folds_the_body_into_it() {
	let body = r#"{"error":{"code":403,"message":"Permission denied"}}"#;
	let norm = normalize(SdkError {
		status: Some(403),
		..error(body)
	});
	assert_eq!(
		(norm.status, norm.message_carries_body, norm.message.as_str()),
		(Some(403), true, body)
	);
}

// TS: error-body.test.ts
#[test]
fn extracts_status_and_body_from_a_bedrock_shaped_service_exception() {
	let norm = normalize(SdkError {
		metadata_http_status_code: Some(403),
		response_status_code: Some(403),
		response_body: Some(SdkBodyField::Text(r#"{"message":"blocked by gateway WAF"}"#.into())),
		..error("UnknownError")
	});
	assert_eq!(
		(norm.status, norm.body.as_deref(), norm.message_carries_body),
		(Some(403), Some(r#"{"message":"blocked by gateway WAF"}"#), false)
	);
}

// TS: error-body.test.ts
#[test]
fn ignores_a_bedrock_response_stream_instead_of_serializing_its_internals() {
	let norm = normalize(SdkError {
		metadata_http_status_code: Some(400),
		response_status_code: Some(400),
		response_body: Some(SdkBodyField::Stream),
		..error("Invocation of model ID anthropic.claude-opus-5 with on-demand throughput isn't supported.")
	});
	assert_eq!(
		(norm.status, norm.body, norm.message_carries_body),
		(Some(400), None, true)
	);
	assert!(norm.message.contains("on-demand throughput isn't supported"));
}

// TS: error-body.test.ts
#[test]
fn ignores_a_class_instance_response_body_without_a_pipe_method_instead_of_serializing_it() {
	let norm = normalize(SdkError {
		metadata_http_status_code: Some(400),
		response_status_code: Some(400),
		response_body: Some(SdkBodyField::Instance),
		..error("Input is too long for requested model.")
	});
	assert_eq!(
		(norm.status, norm.body, norm.message_carries_body),
		(Some(400), None, true)
	);
	assert!(norm.message.contains("Input is too long"));
}

// TS: error-body.test.ts
#[test]
fn ignores_a_class_instance_error_field_instead_of_serializing_it() {
	let norm = normalize(SdkError {
		status: Some(502),
		error: Some(SdkBodyField::Instance),
		..error("TLS handshake failed")
	});
	assert_eq!(
		(norm.body, norm.message.as_str(), norm.message_carries_body),
		(None, "TLS handshake failed", true)
	);
}

// TS: error-body.test.ts
#[test]
fn still_surfaces_a_plain_parsed_json_body_object() {
	let norm = normalize(SdkError {
		status: Some(400),
		error: Some(SdkBodyField::Json(json!({ "message": "bad input" }))),
		..error("400 status code (no body)")
	});
	assert_eq!(
		(norm.body.as_deref(), norm.message_carries_body),
		(Some(r#"{"message":"bad input"}"#), false)
	);
}

// TS: error-body.test.ts
#[test]
fn json_stringifies_a_non_error_thrown_value() {
	let norm = normalize_provider_error(&ThrownValue::Value(json!({ "reason": "boom" })));
	assert_eq!(
		(norm.message.as_str(), norm.message_carries_body, norm.status, norm.body),
		(r#"{"reason":"boom"}"#, false, None, None)
	);
}

// TS: error-body.test.ts
#[test]
fn treats_an_empty_parsed_body_object_as_no_body() {
	let norm = normalize(SdkError {
		status: Some(403),
		error: Some(SdkBodyField::Json(json!({}))),
		..error("403 status code (no body)")
	});
	assert_eq!((norm.body, norm.message_carries_body), (None, true));
}

// TS: error-body.test.ts
#[test]
fn truncates_the_body_at_the_cap() {
	let long = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
	let norm = normalize(SdkError {
		status_code: Some(500),
		body: Some(long.clone()),
		..error("failed")
	});
	let body = norm.body.unwrap();
	assert!(body.contains("... [truncated 50 chars]") && body.len() < long.len());
}

// TS: error-body.test.ts
#[test]
fn sets_message_carries_body_when_the_message_already_contains_the_extracted_body() {
	let norm = normalize(SdkError {
		status_code: Some(500),
		body: Some("upstream exploded".into()),
		..error("500: upstream exploded")
	});
	assert!(norm.message_carries_body);
}

fn opaque_403() -> pi_ai::error_body::NormalizedProviderError {
	normalize(SdkError {
		status: Some(403),
		error: Some(SdkBodyField::Json(json!({ "error": "blocked by gateway WAF" }))),
		..error("403 status code (no body)")
	})
}

// TS: error-body.test.ts
#[test]
fn surfaces_status_and_body_without_a_prefix() {
	let formatted = format_provider_error(&opaque_403(), None);
	assert!(
		formatted.contains("403")
			&& formatted.contains("blocked by gateway WAF")
			&& formatted != "403 status code (no body)"
	);
}

// TS: error-body.test.ts
#[test]
fn applies_a_provider_prefix_with_status_and_body() {
	assert_eq!(
		format_provider_error(&opaque_403(), Some("OpenAI API error")),
		r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#
	);
}

// TS: error-body.test.ts
#[test]
fn preserves_the_message_with_prefix_and_status_when_it_already_carries_the_body() {
	let body = r#"{"error":{"message":"Permission denied"}}"#;
	assert_eq!(
		format_provider_error(
			&normalize(SdkError {
				status: Some(403),
				..error(body)
			}),
			Some("OpenAI API error")
		),
		format!("OpenAI API error (403): {body}")
	);
}

// TS: error-body.test.ts
#[test]
fn returns_the_bare_message_for_a_non_error_value() {
	assert_eq!(
		format_provider_error(
			&normalize_provider_error(&ThrownValue::Value(json!({ "reason": "boom" }))),
			None
		),
		r#"{"reason":"boom"}"#
	);
}

// TS: error-body.test.ts
#[test]
fn keeps_an_excerpt_that_does_not_hold_the_whole_body_alive() {
	// #9265: the excerpt is a fresh string of its own size.
	let body = "<html>".repeat(1_000_000);
	let kept = truncate_error_text(&body, MAX_PROVIDER_ERROR_BODY_CHARS);
	drop(body);
	assert!(kept.starts_with("<html><html>"));
	assert!(kept.ends_with(&format!(
		"[truncated {} chars]",
		6_000_000 - MAX_PROVIDER_ERROR_BODY_CHARS
	)));
	assert!(kept.capacity() < 10_000);
}

fn cause(message: &str, name: &str, code: Option<&str>) -> ErrorCause {
	ErrorCause {
		message: message.into(),
		name: name.into(),
		code: code.map(str::to_owned),
	}
}

// TS: error-body.test.ts
#[test]
fn appends_the_deepest_cause_of_a_connection_error_with_no_status_or_body() {
	let norm = normalize(SdkError {
		causes: vec![
			cause("fetch failed", "TypeError", None),
			cause("connect ECONNREFUSED 127.0.0.1:8080", "Error", Some("ECONNREFUSED")),
		],
		..error("Connection error.")
	});
	assert_eq!(
		format_provider_error(&norm, None),
		"Connection error.: connect ECONNREFUSED 127.0.0.1:8080"
	);
}

// TS: error-body.test.ts
#[test]
fn adds_the_error_code_when_the_cause_message_lacks_it() {
	assert_eq!(
		with_cause_detail(
			"Connection error.",
			&[cause("other side closed", "Error", Some("UND_ERR_SOCKET"))]
		),
		"Connection error.: other side closed (UND_ERR_SOCKET)"
	);
}

// TS: error-body.test.ts
#[test]
fn leaves_errors_without_a_cause_unchanged() {
	assert_eq!(with_cause_detail("Connection error.", &[]), "Connection error.");
}
