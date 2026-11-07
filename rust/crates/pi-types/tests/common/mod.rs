//! Round-trip helpers shared by the test files: JSON text in, typed value, JSON text out through `to_js_string`.

// Each test binary includes this module and uses a subset of it.
#![allow(dead_code)]

use pi_json::to_js_string;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Serializes a value the way every writer of the port does.
pub fn write<T: Serialize>(value: &T) -> String {
	to_js_string(&serde_json::to_value(value).unwrap_or_else(|error| panic!("not serializable: {error}")))
}

/// Reads JSON text as `T` the way every reader of the port does (`pi_json::parse`, lenient surrogates).
pub fn read<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
	serde_json::from_value(pi_json::parse(text)?)
}

/// Parses `text` as `T` and asserts it writes back byte for byte; returns the value for further checks.
#[track_caller]
pub fn round_trip<T: Serialize + DeserializeOwned>(text: &str) -> T {
	let value: T = read(text).unwrap_or_else(|error| panic!("{error}: {text}"));
	assert_eq!(write(&value), text);
	value
}

/// A usage object in writer order.
pub const USAGE: &str = r#"{"input":10,"output":5,"cacheRead":2,"cacheWrite":1,"totalTokens":18,"cost":{"input":0.001,"output":0.0025,"cacheRead":0.0001,"cacheWrite":0.00005,"total":0.00365}}"#;

/// An assistant message with a text block and a tool call, in writer order.
pub fn assistant() -> String {
	format!(
		r#"{{"role":"assistant","content":[{{"type":"text","text":"Reading."}},{{"type":"toolCall","id":"call_1","name":"read","arguments":{{"path":"a.txt"}}}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{USAGE},"stopReason":"toolUse","timestamp":1733234401000}}"#
	)
}
