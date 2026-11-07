//! Context token estimates and output limits (`estimate.rs`, `simple_options.rs`): ports `context-estimate.test.ts`
//! and `clamp-max-tokens.test.ts`.

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

use pi_ai::estimate::estimate_context_tokens;
use pi_ai::options::SimpleStreamOptions;
use pi_ai::simple_options::{build_base_options, clamp_max_tokens_to_context};
use pi_ai::transcript::{Context, normalize_context};
use pi_types::{Message, Model};
use serde_json::json;

fn assistant(timestamp: u64, total: i64) -> Message {
	Message::Assistant(common::assistant(json!({
		"content": [{ "type": "text", "text": "kept" }], "model": "test-model", "timestamp": timestamp,
		"usage": { "input": total, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": total, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }
	})))
}

fn estimate_model() -> Model {
	common::model(
		"openai-responses",
		"openai",
		"test-model",
		json!({ "contextWindow": 10_000, "maxTokens": 8_000 }),
	)
}

fn context(system_prompt: Option<&str>, messages: Vec<Message>) -> pi_ai::transcript::TranscriptContext {
	normalize_context(Context {
		system_prompt: system_prompt.map(str::to_owned),
		messages,
		tools: None,
	})
}

// TS: context-estimate.test.ts
#[test]
fn reserves_3_5_characters_per_token_for_new_text_when_limiting_output() {
	// Regression for #10497.
	let context = context(None, vec![assistant(100, 2_000), common::user(&"x".repeat(3_500), 200)]);
	let estimate = estimate_context_tokens(context.messages());
	assert_eq!(
		(
			estimate.tokens,
			estimate.usage_tokens,
			estimate.trailing_tokens,
			estimate.last_usage_index
		),
		(3_000, 2_000, 1_000, Some(0))
	);
	assert_eq!(
		build_base_options(&estimate_model(), &context, &SimpleStreamOptions::default(), None).max_tokens,
		Some(2_904)
	);
}

// TS: context-estimate.test.ts
#[test]
fn ignores_stale_assistant_usage_after_a_newer_message_is_inserted_before_it() {
	let context = context(
		Some("system"),
		vec![
			common::user("summary", 200),
			assistant(100, 9_500),
			common::user(&"x".repeat(4_000), 300),
		],
	);
	let estimate = estimate_context_tokens(context.messages());
	assert_eq!(
		(
			estimate.tokens,
			estimate.usage_tokens,
			estimate.trailing_tokens,
			estimate.last_usage_index
		),
		(1_149, 0, 1_149, None)
	);
	assert_eq!(
		build_base_options(&estimate_model(), &context, &SimpleStreamOptions::default(), None).max_tokens,
		Some(4_755)
	);
}

// TS: context-estimate.test.ts
#[test]
fn uses_assistant_usage_again_after_a_response_to_the_inserted_context() {
	let context = context(
		None,
		vec![
			common::user("summary", 200),
			assistant(100, 9_500),
			common::user("new prompt", 300),
			assistant(400, 2_000),
			common::user("tail", 500),
		],
	);
	let estimate = estimate_context_tokens(context.messages());
	assert_eq!(
		(
			estimate.tokens,
			estimate.usage_tokens,
			estimate.trailing_tokens,
			estimate.last_usage_index
		),
		(2_002, 2_000, 2, Some(3))
	);
}

const WINDOW: u64 = 1_048_576;

fn model_with(max_tokens: u64, context_window: u64) -> Model {
	common::model(
		"openai-completions",
		"openai",
		"gpt-4o-mini",
		json!({ "contextWindow": context_window, "maxTokens": max_tokens }),
	)
}

// The last assistant usage anchors the estimate, so the estimated input is exactly `input`.
fn with_input(input: i64) -> pi_ai::transcript::TranscriptContext {
	context(None, vec![common::user("hi", 1), assistant(2, input)])
}

// TS: clamp-max-tokens.test.ts
#[test]
fn leaves_15_percent_of_the_input_unused_so_an_openrouter_overcount_still_fits_10497() {
	let max_tokens = clamp_max_tokens_to_context(&model_with(943_718, WINDOW), &with_input(220_815), 943_718);
	assert_eq!(max_tokens, WINDOW - 220_815 - 33_123);
	assert!(229_747 + max_tokens <= WINDOW);
}

// TS: clamp-max-tokens.test.ts
#[test]
fn leaves_at_least_4096_tokens_unused_for_small_inputs() {
	assert_eq!(
		clamp_max_tokens_to_context(&model_with(WINDOW, WINDOW), &with_input(10_000), WINDOW),
		WINDOW - 10_000 - 4096
	);
}

// TS: clamp-max-tokens.test.ts
#[test]
fn keeps_the_model_output_limit_when_it_already_fits() {
	assert_eq!(
		clamp_max_tokens_to_context(&model_with(64_000, WINDOW), &with_input(100_000), 64_000),
		64_000
	);
}

// TS: clamp-max-tokens.test.ts
#[test]
fn sends_up_to_32768_tokens_when_the_proportional_margin_leaves_no_room() {
	assert_eq!(
		clamp_max_tokens_to_context(&model_with(943_718, WINDOW), &with_input(912_000), 943_718),
		32_768
	);
}

// TS: clamp-max-tokens.test.ts
#[test]
fn uses_the_real_room_minus_4096_when_less_than_32768_tokens_fit() {
	assert_eq!(
		clamp_max_tokens_to_context(&model_with(943_718, WINDOW), &with_input(1_030_000), 943_718),
		WINDOW - 1_030_000 - 4096
	);
}

// TS: clamp-max-tokens.test.ts
#[test]
fn keeps_only_the_fixed_4096_margin_for_small_windows() {
	assert_eq!(
		clamp_max_tokens_to_context(&model_with(32_768, 32_768), &with_input(20_000), 32_768),
		32_768 - 20_000 - 4096
	);
}

// TS: clamp-max-tokens.test.ts
#[test]
fn sends_1_token_when_the_input_already_exceeds_the_window() {
	assert_eq!(
		clamp_max_tokens_to_context(&model_with(943_718, WINDOW), &with_input(1_050_000), 943_718),
		1
	);
}
