//! Builders shared by the pi-ai tests: models, messages and tools as the TS tests write them inline.

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
#![allow(dead_code, reason = "each test file uses its own subset")]

use pi_json::{JsonObject, JsonValue};
use pi_types::{AssistantMessage, Message, Model, Tool};

/// Reads JSON text into a typed value through `pi_json::parse`.
pub fn read<T: serde::de::DeserializeOwned>(text: &str) -> T {
	serde_json::from_value(pi_json::parse(text).expect("valid JSON")).expect("typed value")
}

/// A value from a `serde_json::json!` literal.
pub fn from_json<T: serde::de::DeserializeOwned>(value: JsonValue) -> T {
	serde_json::from_value(value).expect("typed value")
}

/// JSON of a typed value.
pub fn to_json<T: serde::Serialize>(value: &T) -> JsonValue {
	serde_json::to_value(value).expect("serializable")
}

/// A chat model with zero cost; `extra` fields of the JSON override the defaults.
pub fn model(api: &str, provider: &str, id: &str, extra: JsonValue) -> Model {
	let mut value = serde_json::json!({
		"id": id, "name": id, "api": api, "provider": provider, "baseUrl": "https://example.test/v1",
		"reasoning": false, "input": ["text"],
		"cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
		"contextWindow": 128000, "maxTokens": 4096
	});
	if let (Some(base), JsonValue::Object(extra)) = (value.as_object_mut(), extra) {
		for (key, item) in extra {
			base.insert(key, item);
		}
	}
	from_json(value)
}

/// A user message.
pub fn user(text: &str, timestamp: u64) -> Message {
	from_json(serde_json::json!({ "role": "user", "content": text, "timestamp": timestamp }))
}

/// An assistant message from JSON fields over a zero-usage default.
pub fn assistant(fields: JsonValue) -> AssistantMessage {
	let mut value = serde_json::json!({
		"role": "assistant", "content": [], "api": "openai-responses", "provider": "openai", "model": "m",
		"usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
			"cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
		"stopReason": "stop", "timestamp": 0
	});
	if let (Some(base), JsonValue::Object(fields)) = (value.as_object_mut(), fields) {
		for (key, item) in fields {
			base.insert(key, item);
		}
	}
	from_json(value)
}

/// A tool with an empty object schema (`Type.Object({})`).
pub fn tool(name: &str, description: &str) -> Tool {
	from_json(serde_json::json!({
		"name": name, "description": description, "parameters": { "type": "object", "properties": {} }
	}))
}

/// An object from a `json!` literal.
pub fn object(value: JsonValue) -> JsonObject {
	match value {
		JsonValue::Object(object) => object,
		_ => panic!("expected an object"),
	}
}
