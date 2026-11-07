//! Validation of tool call arguments against the tool's JSON Schema (ports `packages/ai/src/utils/validation.ts`),
//! through pi-schema with formats, coercion and dangling `$ref`s rejected only where they are used.
//!
//! TS keeps one compiled validator per schema object in a `WeakMap`; a schema here is a plain value without an
//! identity to key on, so each call compiles it (microseconds for a tool schema; see rust/docs/slices.md, C24).

use pi_json::JsonValue;
use pi_schema::{CompileOptions, UnresolvedRefs, compile, formats};
use pi_types::{Tool, ToolCall};

/// Why a tool call's arguments were rejected; the message is the one the model sees.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ToolValidationError(pub String);

/// Finds the called tool and validates the arguments (`validateToolCall`); the coerced arguments on success.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<JsonValue, ToolValidationError> {
	let tool = tools
		.iter()
		.find(|tool| tool.name == tool_call.name)
		.ok_or_else(|| ToolValidationError(format!("Tool \"{}\" not found", tool_call.name)))?;
	validate_tool_arguments(tool, tool_call)
}

/// Validates arguments against the tool's schema after coercion (`validateToolArguments`): models send `"42"` for
/// numbers, one value for an array, `null` for an omitted optional property.
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<JsonValue, ToolValidationError> {
	if let Some(error) = &tool_call.arguments_error {
		return Err(ToolValidationError(format!(
			"Invalid arguments for tool \"{}\": {error}",
			tool_call.name
		)));
	}
	let options = CompileOptions {
		formats: formats::standard(),
		coerce: true,
		// A $ref that names nothing (a dangling MCP definition) rejects only values at its place.
		unresolved_refs: UnresolvedRefs::Reject,
		..CompileOptions::default()
	};
	let compiled = compile(&tool.parameters, options).map_err(|error| ToolValidationError(error.to_string()))?;
	let Some(validator) = compiled.coercing() else {
		return Err(ToolValidationError("validator compiled without coercion".into()));
	};
	let arguments = JsonValue::Object(tool_call.arguments.clone());
	match validator.parse(&arguments) {
		Ok(value) => Ok(value),
		Err(errors) => {
			let lines: Vec<String> = errors
				.iter()
				.map(|error| format!("  - {}: {}", format_validation_path(&error.path), error.message))
				.collect();
			Err(ToolValidationError(format!(
				"Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
				tool_call.name,
				lines.join("\n"),
				pi_json::to_js_string_pretty(&JsonValue::Object(tool_call.arguments.clone()), 2)
			)))
		}
	}
}

/// A JSON Pointer as the dotted path the model sees: `/edits/0/oldText` is `edits.0.oldText`; the empty pointer is
/// `root`.
fn format_validation_path(pointer: &str) -> String {
	if pointer.is_empty() {
		return "root".into();
	}
	pointer[1..]
		.split('/')
		.map(|segment| segment.replace("~1", "/").replace("~0", "~"))
		.collect::<Vec<_>>()
		.join(".")
}
