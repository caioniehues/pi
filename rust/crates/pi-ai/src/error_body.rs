//! Normalization of provider HTTP errors (ports `packages/ai/src/utils/error-body.ts`): a non-2xx response behind a
//! proxy may carry a body the SDK did not fold into its message; these helpers find the status and body in the
//! SDK-shaped fields and compose the display string providers surface.
//!
//! TS probes untyped SDK error objects. Here the error is described by [`SdkError`], one field per shape TS probes,
//! in its probe order; C3b and later fill it from their own HTTP failures.

use crate::utf16::{utf16_len, utf16_prefix};
use pi_json::JsonValue;

/// The most characters of a body kept in an error.
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// What a provider failure threw: an error, or a non-`Error` value (`unknown` in TS).
#[derive(Debug, Clone, PartialEq)]
pub enum ThrownValue {
	/// An error with SDK fields.
	Error(SdkError),
	/// Any other thrown value, as JSON.
	Value(JsonValue),
}

/// An error with the SDK fields `normalizeProviderError` probes, each `None` when absent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SdkError {
	/// `error.message`.
	pub message: String,
	/// `statusCode` (Mistral).
	pub status_code: Option<u16>,
	/// `status` (`openai`, `@google/genai`).
	pub status: Option<u16>,
	/// `$metadata.httpStatusCode` (Bedrock).
	pub metadata_http_status_code: Option<u16>,
	/// `$response.statusCode` (Bedrock).
	pub response_status_code: Option<u16>,
	/// `body` when it is a string (Mistral).
	pub body: Option<String>,
	/// `error` (the `openai` SDK's parsed body).
	pub error: Option<SdkBodyField>,
	/// `$response.body` (Bedrock).
	pub response_body: Option<SdkBodyField>,
	/// The `cause` chain, outermost first.
	pub causes: Vec<ErrorCause>,
}

/// The value of an SDK body field.
#[derive(Debug, Clone, PartialEq)]
pub enum SdkBodyField {
	/// A string.
	Text(String),
	/// A plain object or other JSON (a parsed body).
	Json(JsonValue),
	/// An unread response stream (an object with `pipe`).
	Stream,
	/// A class instance that is not a parsed body.
	Instance,
}

/// One error of a `cause` chain.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ErrorCause {
	/// Its message.
	pub message: String,
	/// Its name, such as `TypeError`.
	pub name: String,
	/// Its `code`, such as `ECONNREFUSED`.
	pub code: Option<String>,
}

/// The status, body and message of a provider error (`NormalizedProviderError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderError {
	/// The HTTP status, when one was found.
	pub status: Option<u16>,
	/// The trimmed, truncated body, when one was found.
	pub body: Option<String>,
	/// The message, or the JSON of a non-error value.
	pub message: String,
	/// Whether `message` already contains the body.
	pub message_carries_body: bool,
}

/// Finds the status and body of a provider error (`normalizeProviderError`).
pub fn normalize_provider_error(thrown: &ThrownValue) -> NormalizedProviderError {
	let error = match thrown {
		ThrownValue::Value(value) => {
			return NormalizedProviderError {
				status: None,
				body: None,
				message: pi_json::to_js_string(value),
				message_carries_body: false,
			};
		}
		ThrownValue::Error(error) => error,
	};
	let status = error
		.status_code
		.or(error.status)
		.or(error.metadata_http_status_code)
		.or(error.response_status_code);
	let body = extract_body(error);
	let message_carries_body = body.as_ref().is_none_or(|body| error.message.contains(body.as_str()));
	let message = if status.is_none() && body.is_none() {
		with_cause_detail(&error.message, &error.causes)
	} else {
		error.message.clone()
	};
	NormalizedProviderError {
		status,
		body,
		message,
		message_carries_body,
	}
}

/// The most `cause` levels examined.
const MAX_CAUSE_DEPTH: usize = 5;

/// The message with the deepest cause's reason (and code) appended (`withCauseDetail`): connection failures carry the
/// real reason two levels down (`fetch failed` wrapping `connect ECONNREFUSED ...`).
pub fn with_cause_detail(message: &str, causes: &[ErrorCause]) -> String {
	let Some(deepest) = causes.iter().take(MAX_CAUSE_DEPTH).next_back() else {
		return message.to_owned();
	};
	let detail = if deepest.message.is_empty() {
		deepest.name.clone()
	} else {
		deepest.message.clone()
	};
	let mut detail = detail.trim().to_owned();
	if let Some(code) = &deepest.code
		&& !detail.contains(code.as_str())
	{
		detail = if detail.is_empty() {
			code.clone()
		} else {
			format!("{detail} ({code})")
		};
	}
	if detail.is_empty() || message.contains(&detail) {
		return message.to_owned();
	}
	if message.is_empty() {
		detail
	} else {
		format!("{message}: {detail}")
	}
}

/// Probes the body in SDK-field order: `body` string, a non-empty plain `error` object, `$response.body`.
fn extract_body(error: &SdkError) -> Option<String> {
	let text = pick_body_text(error)?;
	let trimmed = text.trim();
	if trimmed.is_empty() {
		return None;
	}
	Some(truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

fn pick_body_text(error: &SdkError) -> Option<String> {
	if let Some(body) = &error.body {
		return Some(body.clone());
	}
	if let Some(SdkBodyField::Json(value)) = &error.error
		&& is_plain_non_empty_object(value)
	{
		return Some(pi_json::to_js_string(value));
	}
	match &error.response_body {
		Some(SdkBodyField::Text(text)) => Some(text.clone()),
		Some(SdkBodyField::Json(value)) if is_plain_non_empty_object(value) => Some(pi_json::to_js_string(value)),
		_ => None,
	}
}

fn is_plain_non_empty_object(value: &JsonValue) -> bool {
	value.as_object().is_some_and(|object| !object.is_empty())
}

/// The display string of a normalized error (`formatProviderError`): `<status>: <body>`, or
/// `<prefix> (<status>): <body>`; the message when it already carries the body or nothing was found.
pub fn format_provider_error(normalized: &NormalizedProviderError, prefix: Option<&str>) -> String {
	match (normalized.status, &normalized.body, prefix) {
		(Some(status), Some(body), prefix) if !normalized.message_carries_body => match prefix {
			Some(prefix) => format!("{prefix} ({status}): {body}"),
			None => format!("{status}: {body}"),
		},
		(Some(status), _, Some(prefix)) => format!("{prefix} ({status}): {}", normalized.message),
		_ => normalized.message.clone(),
	}
}

/// `text` cut to `max_chars` UTF-16 units with a note of how many were dropped (`truncateErrorText`). The excerpt is
/// a fresh string, so it never keeps a large body alive.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
	let length = utf16_len(text);
	if length <= max_chars {
		return text.to_owned();
	}
	format!(
		"{}... [truncated {} chars]",
		utf16_prefix(text, max_chars),
		length - max_chars
	)
}
