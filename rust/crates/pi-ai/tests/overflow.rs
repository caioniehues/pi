//! Context overflow detection (`overflow.rs`): ports `overflow.test.ts`. `context-overflow.test.ts` runs live
//! requests against providers and is C3b's and later.

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

use pi_ai::overflow::{get_overflow_patterns, is_context_overflow, is_recoverable_length};
use pi_types::{AssistantMessage, StopReason};
use serde_json::json;

fn error_message(error: &str, provider: &str) -> AssistantMessage {
	common::assistant(
		json!({ "api": "openai-completions", "provider": provider, "model": "qwen3.5:35b", "stopReason": "error", "errorMessage": error }),
	)
}

fn error(error: &str) -> AssistantMessage {
	error_message(error, "ollama")
}

// TS: overflow.test.ts
#[test]
fn detects_provider_independent_request_size_overflow() {
	for message in [
		r#"github-copilot API error (413): {"message":"failed to parse request","code":""}"#,
		"custom-provider API error (413): Payload Too Large",
		r#"413 {"error":{"message":"failed to parse request","code":""}}"#,
		"413",
	] {
		assert!(is_context_overflow(&error(message), Some(1_000_000)), "{message}");
		assert!(is_context_overflow(&error(message), None), "{message}");
	}
}

// TS: overflow.test.ts
#[test]
fn does_not_mistake_unrelated_errors_for_request_size_overflow() {
	for message in [
		r#"github-copilot API error (400): {"message":"failed to parse request","code":""}"#,
		"custom-provider API error (429): Too many requests",
		r#"github-copilot API error (400): {"message":"failed to parse request: invalid value 413"}"#,
		"custom-provider API error (400): upstream mentioned OpenAI API error (413): invalid input",
		"custom-provider API error (4130): invalid input",
		"4130 failed to parse request",
	] {
		assert!(!is_context_overflow(&error(message), None), "{message}");
	}
}

// TS: overflow.test.ts
#[test]
fn limits_http_status_matching_to_failed_responses() {
	let mut message = error(r#"OpenAI API error (413): {"message":"failed to parse request"}"#);
	message.stop_reason = StopReason::Aborted;
	assert!(!is_context_overflow(&message, None));
	message.stop_reason = StopReason::Stop;
	assert!(!is_context_overflow(&message, None));
}

// TS: overflow.test.ts
#[test]
fn detects_explicit_ollama_prompt_too_long_errors() {
	assert!(is_context_overflow(
		&error("400 `prompt too long; exceeded max context length by 100918 tokens`"),
		Some(32768)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_z_ai_prompt_too_long_errors() {
	// Regression for #9805.
	assert!(is_context_overflow(
		&error_message(r#"400 {"code":"1261","message":"Prompt too long"}"#, "zai"),
		Some(1_048_576)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_z_ai_cn_endpoint_prompt_exceeds_max_length_errors() {
	// Regression for #10208.
	assert!(is_context_overflow(
		&error_message(r#"400 {"code":"1261","message":"Prompt exceeds max length"}"#, "zai"),
		Some(1_048_576)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_together_ai_context_length_errors() {
	assert!(is_context_overflow(
		&error("400 The input (516368 tokens) is longer than the model's context length (262144 tokens)."),
		Some(262_144)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_litellm_wrapped_openai_maximum_context_length_errors() {
	assert!(is_context_overflow(
		&error(
			"Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens."
		),
		Some(131_072)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_litellm_pre_call_context_window_exceeded_error() {
	// Regression for #11492.
	assert!(is_context_overflow(
		&error(
			"400: {\"message\":\"litellm.ContextWindowExceededError: litellm.BadRequestError: litellm._pre_call_checks: Context Window exceeded for given call. No models have context window large enough for this call.\nModel=Qwen3.8-Flash-Next, Max Input Tokens=262144, Got=265262\nmodel=Qwen3.8-Flash-Next. context_window_fallbacks=None. fallbacks=[].\"}"
		),
		Some(262_144)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_openai_compatible_parenthesized_maximum_context_length_errors() {
	assert!(is_context_overflow(
		&error("Error: 400 Input length (265330) exceeds model's maximum context length (262144)."),
		Some(262_144)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_openrouter_poolside_maximum_allowed_input_length_errors() {
	assert!(is_context_overflow(
		&error(
			"Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens."
		),
		Some(131_072)
	));
}

// TS: overflow.test.ts
#[test]
fn detects_ds4_configured_context_size_errors() {
	assert!(is_context_overflow(
		&error("400 Prompt has 256468 tokens, but the configured context size is 256000 tokens"),
		Some(256_000)
	));
	assert!(is_context_overflow(
		&error("Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens"),
		Some(256_000)
	));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_generic_non_overflow_ollama_errors_as_overflow() {
	assert!(!is_context_overflow(
		&error("500 `model runner crashed unexpectedly`"),
		Some(32768)
	));
}

// TS: overflow.test.ts
#[test]
fn only_treats_bodyless_400_errors_as_overflow_for_cerebras() {
	// Regression for #9482.
	assert!(is_context_overflow(
		&error_message("400 status code (no body)", "cerebras"),
		Some(131_072)
	));
	assert!(!is_context_overflow(
		&error_message("400 status code (no body)", "opencode-go"),
		Some(1_000_000)
	));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_bedrock_throttling_too_many_tokens_as_overflow() {
	assert!(!is_context_overflow(
		&error("Throttling error: Too many tokens, please wait before trying again."),
		Some(200_000)
	));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_bedrock_service_unavailable_as_overflow() {
	assert!(!is_context_overflow(
		&error("Service unavailable: The service is temporarily unavailable."),
		Some(200_000)
	));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_generic_rate_limit_errors_as_overflow() {
	assert!(!is_context_overflow(
		&error("Rate limit exceeded, please retry after 30 seconds."),
		Some(200_000)
	));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_http_429_style_errors_as_overflow() {
	assert!(!is_context_overflow(
		&error("Too many requests. Please slow down."),
		Some(200_000)
	));
}

fn length_stop(input: i64, cache_read: i64, output: i64, cache_write: i64) -> AssistantMessage {
	common::assistant(json!({
		"api": "openai-completions", "provider": "test-provider", "model": "test-model", "stopReason": "length",
		"usage": { "input": input, "output": output, "cacheRead": cache_read, "cacheWrite": cache_write, "totalTokens": input + cache_read + cache_write + output,
			"cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }
	}))
}

// TS: overflow.test.ts
#[test]
fn detects_xiaomi_style_overflow_length_stop_with_zero_output_and_filled_context() {
	assert!(is_context_overflow(&length_stop(58, 1_048_512, 0, 0), Some(1_048_576)));
}

// TS: overflow.test.ts
#[test]
fn treats_a_length_stop_below_the_desired_output_limit_as_recoverable() {
	assert!(is_recoverable_length(&length_stop(3, 253_584, 16, 25_554), 128_000));
}

// TS: overflow.test.ts
#[test]
fn does_not_recover_a_length_stop_that_reached_the_desired_output_limit() {
	assert!(!is_recoverable_length(&length_stop(4062, 0, 1024, 0), 1024));
}

// TS: overflow.test.ts
#[test]
fn treats_zero_output_length_stops_as_recoverable_without_context_metadata() {
	assert!(is_recoverable_length(&length_stop(100, 0, 0, 0), 128_000));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_normal_length_stops_with_output_as_context_overflow() {
	assert!(!is_context_overflow(&length_stop(1000, 0, 4096, 0), Some(200_000)));
}

// TS: overflow.test.ts
#[test]
fn does_not_treat_zero_output_length_stops_far_below_context_as_context_overflow() {
	assert!(!is_context_overflow(&length_stop(100, 0, 0, 0), Some(200_000)));
}

// Rust-only.
#[test]
fn every_overflow_pattern_is_a_valid_ecmascript_pattern() {
	for pattern in get_overflow_patterns() {
		assert!(regress::Regex::with_flags(pattern, "i").is_ok(), "{pattern}");
	}
}
