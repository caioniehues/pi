//! Provider-neutral request options (ports `packages/ai/src/api/simple-options.ts`): output limits that fit the
//! context window, thinking budgets, and the stream options of a simple request.

use crate::estimate::estimate_context_tokens;
use crate::model_operations::clamp_thinking_level;
use crate::options::{SimpleStreamOptions, StreamOptions, ThinkingBudgets};
use crate::transcript::TranscriptContext;
use pi_json::JsonObject;
use pi_types::{Model, ModelThinkingLevel, ThinkingLevel};

const CONTEXT_SAFETY_TOKENS: u64 = 4096;
// OpenRouter counts the input with its own tokenizer and rejects input + max_tokens above the window before
// routing; it counted prose up to 12% above the provider's count (#10497).
const CONTEXT_SAFETY_RATIO: f64 = 0.15;
// The ratio margin is kept only while it leaves at least this much output; otherwise only the fixed margin is kept
// and output is capped at this size.
const MIN_OUTPUT_WITH_RATIO_MARGIN: i64 = 32768;
const MIN_MAX_TOKENS: u64 = 1;

/// `max_tokens` fitted into the context window with room for the input being counted higher than estimated
/// (`clampMaxTokensToContext`).
pub fn clamp_max_tokens_to_context(model: &Model, context: &TranscriptContext, max_tokens: u64) -> u64 {
	if model.context_window == 0 {
		return max_tokens.max(MIN_MAX_TOKENS);
	}
	let input = estimate_context_tokens(context.messages()).tokens;
	#[allow(
		clippy::cast_precision_loss,
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "token counts are far below 2^52; the margin is non-negative"
	)]
	let margin = CONTEXT_SAFETY_TOKENS.max((input as f64 * CONTEXT_SAFETY_RATIO).ceil() as u64);
	let signed = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
	let window = signed(model.context_window);
	let mut room = window - signed(input) - signed(margin);
	if room < MIN_OUTPUT_WITH_RATIO_MARGIN {
		room = MIN_OUTPUT_WITH_RATIO_MARGIN.min(window - signed(input) - signed(CONTEXT_SAFETY_TOKENS));
	}
	let room = u64::try_from(room).unwrap_or(0).max(MIN_MAX_TOKENS);
	max_tokens.min(room)
}

/// The sampling parameters of a request: the model's, then those of the effective thinking level, then the
/// request's (`resolveSamplingParams`); `None` when none is set.
pub fn resolve_sampling_params(
	model: &Model,
	thinking_level: ModelThinkingLevel,
	request: Option<&JsonObject>,
) -> Option<JsonObject> {
	let effective = clamp_thinking_level(model, thinking_level);
	let name = serde_json::to_value(effective)
		.ok()
		.and_then(|value| value.as_str().map(str::to_owned))
		.unwrap_or_default();
	let by_level = model
		.sampling_params_by_thinking_level
		.as_ref()
		.and_then(|levels| levels.get(&name));
	if model.sampling_params.is_none() && by_level.is_none() && request.is_none() {
		return None;
	}
	let mut merged = JsonObject::new();
	for source in [model.sampling_params.as_ref(), by_level, request]
		.into_iter()
		.flatten()
	{
		for (key, value) in source {
			merged.insert(key.clone(), value.clone());
		}
	}
	Some(merged)
}

fn model_level(level: Option<ThinkingLevel>) -> ModelThinkingLevel {
	match level {
		None => ModelThinkingLevel::Off,
		Some(ThinkingLevel::Minimal) => ModelThinkingLevel::Minimal,
		Some(ThinkingLevel::Low) => ModelThinkingLevel::Low,
		Some(ThinkingLevel::Medium) => ModelThinkingLevel::Medium,
		Some(ThinkingLevel::High) => ModelThinkingLevel::High,
		Some(ThinkingLevel::Xhigh) => ModelThinkingLevel::Xhigh,
		Some(ThinkingLevel::Max) => ModelThinkingLevel::Max,
	}
}

/// The stream options of a simple request (`buildBaseOptions`): the caller's, with resolved sampling parameters,
/// `max_tokens` fitted to the context, and `api_key` when the caller has none.
pub fn build_base_options(
	model: &Model,
	context: &TranscriptContext,
	options: &SimpleStreamOptions,
	api_key: Option<&str>,
) -> StreamOptions {
	let stream = &options.stream;
	let mut base = stream.clone();
	base.sampling_params =
		resolve_sampling_params(model, model_level(options.reasoning), stream.sampling_params.as_ref());
	base.max_tokens = Some(clamp_max_tokens_to_context(
		model,
		context,
		stream.max_tokens.unwrap_or(model.max_tokens),
	));
	if let Some(api_key) = api_key.filter(|key| !key.is_empty()) {
		base.request.api_key = Some(api_key.to_owned());
	}
	base.api_options = None;
	base
}

/// Tokens always left for the answer when a thinking budget shares the response ceiling.
pub const MIN_ANSWER_TOKENS: u64 = 1024;

/// The default token budgets per thinking level.
pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = ThinkingBudgets {
	minimal: Some(1024),
	low: Some(2048),
	medium: Some(8192),
	high: Some(16384),
};

/// `xhigh` and `max` as `high` for token-based providers (`clampReasoning`).
pub fn clamp_reasoning(effort: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
	match effort {
		Some(ThinkingLevel::Xhigh | ThinkingLevel::Max) => Some(ThinkingLevel::High),
		other => other,
	}
}

/// The token budget of a level, custom budgets over the defaults (`thinkingBudgetForLevel`).
pub fn thinking_budget_for_level(level: ThinkingLevel, custom: Option<&ThinkingBudgets>) -> u64 {
	let pick = |budgets: &ThinkingBudgets| match clamp_reasoning(Some(level)) {
		Some(ThinkingLevel::Minimal) => budgets.minimal,
		Some(ThinkingLevel::Low) => budgets.low,
		Some(ThinkingLevel::Medium) => budgets.medium,
		_ => budgets.high,
	};
	custom
		.and_then(pick)
		.or_else(|| pick(&DEFAULT_THINKING_BUDGETS))
		.unwrap_or(0)
}

/// A thinking budget capped so at least [`MIN_ANSWER_TOKENS`] remain under the ceiling
/// (`clampThinkingBudgetToAnswerRoom`).
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
	thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

/// A response ceiling with a thinking budget fitted inside it (`adjustMaxTokensForThinking`); `base_max_tokens`
/// `None` means no caller cap.
pub fn adjust_max_tokens_for_thinking(
	base_max_tokens: Option<u64>,
	model_max_tokens: u64,
	level: ThinkingLevel,
	custom: Option<&ThinkingBudgets>,
) -> (u64, u64) {
	let mut thinking_budget = thinking_budget_for_level(level, custom);
	let max_tokens = match base_max_tokens {
		None => model_max_tokens,
		Some(base) => (base + thinking_budget).min(model_max_tokens),
	};
	if max_tokens <= thinking_budget {
		thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
	}
	(max_tokens, thinking_budget)
}
