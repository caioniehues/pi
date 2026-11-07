//! Redacted diagnostics attached to assistant messages (ports `packages/ai/src/utils/diagnostics.ts`).

use pi_json::JsonObject;
use pi_types::{AssistantMessage, AssistantMessageDiagnostic, DiagnosticErrorInfo};
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

/// The text of a failure (`formatThrownValue`): its message.
pub fn format_thrown_value(error: &dyn Error) -> String {
	error.to_string()
}

/// The parts of an error a diagnostic keeps (`extractDiagnosticError`). Rust errors have no name, stack or code.
pub fn extract_diagnostic_error(error: &dyn Error) -> DiagnosticErrorInfo {
	let message = error.to_string();
	DiagnosticErrorInfo {
		// TS parity: an `Error` named "Error"; an empty message falls back to the name.
		name: Some("Error".into()),
		message: if message.is_empty() { "Error".into() } else { message },
		stack: None,
		code: None,
		extra: JsonObject::new(),
	}
}

/// A diagnostic of `kind` for `error`, stamped now (`createAssistantMessageDiagnostic`).
pub fn create_assistant_message_diagnostic(
	kind: &str,
	error: &dyn Error,
	details: Option<JsonObject>,
) -> AssistantMessageDiagnostic {
	AssistantMessageDiagnostic {
		kind: kind.to_owned(),
		timestamp: now_ms(),
		error: Some(extract_diagnostic_error(error)),
		details,
		extra: JsonObject::new(),
	}
}

/// Appends `diagnostic` to the message's diagnostics (`appendAssistantMessageDiagnostic`).
pub fn append_assistant_message_diagnostic(message: &mut AssistantMessage, diagnostic: AssistantMessageDiagnostic) {
	message.diagnostics.get_or_insert_with(Vec::new).push(diagnostic);
}

/// Milliseconds since the epoch (`Date.now()`).
pub fn now_ms() -> u64 {
	let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
	u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}
