//! JSON parsing with repair, tool-call argument parsing and lenient parsing of partial JSON (ports
//! `packages/ai/src/utils/json-parse.ts`). Every parse goes through `pi_json::parse`, so a lone surrogate escape reads
//! as U+FFFD (a decided divergence from `JSON.parse`).
//!
//! Decided difference: parse errors carry `serde_json`'s message (`EOF while parsing a string at line 1 column 9`)
//! where TS carries V8's (`Unterminated string in JSON at position 8`); `argumentsError` embeds it.

use crate::partial_json;
use pi_json::{JsonObject, JsonValue};
use std::fmt::Write as _;

/// A JSON text that does not parse, with the parser's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct JsonParseError(pub String);

/// Repairs malformed JSON string literals: escapes raw control characters inside strings and doubles backslashes
/// before invalid escape characters (`repairJson`).
pub fn repair_json(json: &str) -> String {
	let mut repaired = String::with_capacity(json.len());
	let mut in_string = false;
	let mut chars = json.char_indices().peekable();
	while let Some((index, character)) = chars.next() {
		if !in_string {
			repaired.push(character);
			in_string = character == '"';
			continue;
		}
		if character == '"' {
			repaired.push(character);
			in_string = false;
			continue;
		}
		if character == '\\' {
			let rest = &json[index + 1..];
			let Some(next) = rest.chars().next() else {
				repaired.push_str("\\\\");
				continue;
			};
			if next == 'u'
				&& rest
					.get(1..5)
					.is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
			{
				repaired.push_str(&json[index..index + 6]);
				for _ in 0..5 {
					chars.next();
				}
				continue;
			}
			if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
				repaired.push('\\');
				repaired.push(next);
				chars.next();
				continue;
			}
			repaired.push_str("\\\\");
			continue;
		}
		push_escaped_control(character, &mut repaired);
	}
	repaired
}

fn push_escaped_control(character: char, out: &mut String) {
	match character {
		'\u{8}' => out.push_str("\\b"),
		'\u{c}' => out.push_str("\\f"),
		'\n' => out.push_str("\\n"),
		'\r' => out.push_str("\\r"),
		'\t' => out.push_str("\\t"),
		'\u{0}'..='\u{1f}' => {
			// invariant: writing to a String cannot fail.
			let _ = write!(out, "\\u{:04x}", u32::from(character));
		}
		_ => out.push(character),
	}
}

/// Parses JSON; when that fails and repair changes the text, parses the repaired text (`parseJsonWithRepair`).
pub fn parse_json_with_repair(json: &str) -> Result<JsonValue, JsonParseError> {
	match pi_json::parse(json) {
		Ok(value) => Ok(value),
		Err(error) => {
			let repaired = repair_json(json);
			if repaired == json {
				return Err(JsonParseError(error.to_string()));
			}
			pi_json::parse(&repaired).map_err(|error| JsonParseError(error.to_string()))
		}
	}
}

/// The arguments of a finished tool call: an object, or `{}` with the reason the text was not one.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallArguments {
	/// The parsed arguments; empty when `arguments_error` is set.
	pub arguments: JsonObject,
	/// Why the text was not a JSON object, followed by the text the model sent.
	pub arguments_error: Option<String>,
}

/// Parses the model's complete arguments text of a finished tool call (`parseToolCallArguments`): never a partially
/// parsed prefix; malformed text gives `{}` and an error naming the problem and the text.
pub fn parse_tool_call_arguments(json: &str) -> ToolCallArguments {
	if json.trim().is_empty() {
		return ToolCallArguments {
			arguments: JsonObject::new(),
			arguments_error: None,
		};
	}
	let message = match parse_json_with_repair(json) {
		Ok(JsonValue::Object(arguments)) => {
			return ToolCallArguments {
				arguments,
				arguments_error: None,
			};
		}
		Ok(_) => "Tool arguments must be a JSON object".to_owned(),
		Err(error) => error.0,
	};
	ToolCallArguments {
		arguments: JsonObject::new(),
		arguments_error: Some(format!("{message}\n\nReceived arguments:\n{json}")),
	}
}

/// Parses possibly incomplete JSON during streaming; `{}` when nothing parses (`parseStreamingJson`).
pub fn parse_streaming_json(partial: &str) -> JsonValue {
	if partial.trim().is_empty() {
		return JsonValue::Object(JsonObject::new());
	}
	let value = parse_json_with_repair(partial)
		.ok()
		.or_else(|| partial_json::parse(partial).ok())
		.or_else(|| partial_json::parse(&repair_json(partial)).ok());
	match value {
		Some(JsonValue::Null) | None => JsonValue::Object(JsonObject::new()),
		Some(value) => value,
	}
}
