//! Context overflow detection (ports `packages/ai/src/utils/overflow.ts`; the TS file lists the provider messages
//! each pattern matches).

use crate::js_regex::JsRegexSet;
use pi_types::{AssistantMessage, StopReason};

/// The overflow patterns, in TS order, case-insensitive.
const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
	r"prompt (?:is )?too long",
	r"prompt exceeds max length",
	r"request_too_large",
	r"input is too long for requested model",
	r"exceeds the context window",
	r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))",
	r"context window exceeded",
	r"input token count.*exceeds the maximum",
	r"maximum prompt length is \d+",
	r"reduce the length of the messages",
	r"maximum context length is \d+ tokens",
	r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?",
	r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)",
	r"exceeds the limit of \d+",
	r"exceeds the available context size",
	r"greater than the context length",
	r"context window exceeds limit",
	r"exceeded model token limit",
	r"too large for model with \d+ maximum context length",
	r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?",
	r"model_context_window_exceeded",
	r"prompt too long; exceeded (?:max )?context length",
	r"range of input length should be",
	r"context[_ ]length[_ ]exceeded",
	r"too many tokens",
	r"token limit exceeded",
	// HTTP 413 means request-size overflow regardless of provider or body text.
	r"^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))",
];

static OVERFLOW_PATTERNS: JsRegexSet = JsRegexSet::new(OVERFLOW_PATTERN_SOURCES, "i");

static CEREBRAS_BODYLESS_OVERFLOW: JsRegexSet =
	JsRegexSet::new(&[r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)"], "i");

/// Errors that are not overflow even when an overflow pattern matches them.
static NON_OVERFLOW_PATTERNS: JsRegexSet = JsRegexSet::new(
	&[
		r"^(Throttling error|Service unavailable):",
		r"rate limit",
		r"too many requests",
	],
	"i",
);

/// Whether a response is a context overflow (`isContextOverflow`): an overflow error message, a successful response
/// whose input exceeds `context_window` (z.ai), or a zero-output length stop that filled it (`MiMo` of Xiaomi).
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
	if message.stop_reason == StopReason::Error
		&& let Some(error) = message.error_message.as_deref().filter(|error| !error.is_empty())
		&& !NON_OVERFLOW_PATTERNS.test(error)
	{
		if OVERFLOW_PATTERNS.test(error) {
			return true;
		}
		if message.provider == "cerebras" && CEREBRAS_BODYLESS_OVERFLOW.test(error) {
			return true;
		}
	}
	let Some(window) = context_window.filter(|window| *window > 0) else {
		return false;
	};
	#[allow(
		clippy::cast_precision_loss,
		reason = "token counts are far below 2^52, as JS numbers"
	)]
	let input = (message.usage.input + message.usage.cache_read) as f64;
	#[allow(
		clippy::cast_precision_loss,
		reason = "token counts are far below 2^52, as JS numbers"
	)]
	let window = window as f64;
	match message.stop_reason {
		StopReason::Stop => input > window,
		StopReason::Length => message.usage.output == 0 && input >= window * 0.99,
		_ => false,
	}
}

/// Whether a length stop ended below the intended output limit, so one compact-and-retry may help
/// (`isRecoverableLength`).
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
	message.stop_reason == StopReason::Length
		&& desired_max_output > 0
		&& u64::try_from(message.usage.output).is_ok_and(|output| output < desired_max_output)
}

/// The overflow patterns' sources, for testing (`getOverflowPatterns`).
pub fn get_overflow_patterns() -> Vec<&'static str> {
	OVERFLOW_PATTERN_SOURCES.to_vec()
}
