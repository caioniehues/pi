//! The scenario script the fake provider serves: the responses it returns, in request order, and how each one is
//! cut into writes. API-neutral: the renderers turn one response into the Anthropic or OpenAI wire form.

use pi_json::JsonValue;
use serde::Deserialize;
use std::path::Path;

/// A scenario script: `{ "delayMs"?: n, "responses": [...] }`; request `n` (from zero) gets `responses[n]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Script {
	/// The delay before each write of every response that sets none.
	#[serde(default)]
	pub delay_ms: u64,
	/// The responses, in the order requests arrive.
	pub responses: Vec<ScriptResponse>,
}

/// One scripted response: a streamed assistant message, or an HTTP error when `error` is set.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScriptResponse {
	/// The content blocks, streamed in order.
	#[serde(default)]
	pub content: Vec<Block>,
	/// Why the message stopped.
	#[serde(default)]
	pub stop_reason: StopReason,
	/// Token counts reported for the message; zero when absent.
	#[serde(default)]
	pub usage: Usage,
	/// Milliseconds to wait before each write of the stream; the script's `delayMs` when absent.
	pub delay_ms: Option<u64>,
	/// How deltas and writes are cut; one delta per block and one write per event when absent.
	pub chunking: Option<Chunking>,
	/// After this many writes the stream stops until the driver releases it (`ServerHandle::release`).
	pub pause_after: Option<usize>,
	/// Answer with this HTTP error instead of a stream.
	pub error: Option<ErrorReply>,
}

/// A content block of a scripted message.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Block {
	/// Visible text.
	Text {
		/// The text.
		text: String,
	},
	/// Reasoning text; the signature is sent by the Anthropic renderer only.
	Thinking {
		/// The reasoning text.
		thinking: String,
		/// The opaque signature Anthropic attaches; empty when absent.
		#[serde(default)]
		signature: String,
	},
	/// A tool call.
	ToolCall {
		/// The tool call id, sent as is.
		id: String,
		/// The tool name.
		name: String,
		/// The arguments object, streamed as its `JSON.stringify` text.
		arguments: JsonValue,
	},
}

/// Why a scripted message stopped, in pi's vocabulary; each renderer maps it to its wire value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
	/// A normal end of turn.
	#[default]
	Stop,
	/// The output limit was reached.
	Length,
	/// The message ends with tool calls.
	ToolUse,
}

/// Token counts of a scripted message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
	/// Uncached input tokens.
	#[serde(default)]
	pub input: u64,
	/// Output tokens.
	#[serde(default)]
	pub output: u64,
	/// Input tokens read from the cache.
	#[serde(default)]
	pub cache_read: u64,
	/// Input tokens written to the cache.
	#[serde(default)]
	pub cache_write: u64,
}

/// How a response is cut: into deltas of `tokens` characters each, or into network writes of `bytes` bytes each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum Chunking {
	/// Every text, thinking and argument delta carries this many characters (Unicode scalar values).
	Tokens(usize),
	/// The rendered stream is written in pieces of this many bytes, cutting through events and UTF-8 sequences.
	Bytes(usize),
}

/// An HTTP error answer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorReply {
	/// The HTTP status, an error status (400 to 599).
	pub status: u16,
	/// The body: a string is sent as plain text, anything else as JSON.
	pub body: JsonValue,
	/// The `retry-after` header value, in seconds.
	pub retry_after: Option<u64>,
}

/// Why a script could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
	/// The file could not be read.
	#[error("cannot read script {path}: {source}")]
	Read {
		/// The script path.
		path: String,
		/// The I/O error.
		source: std::io::Error,
	},
	/// The file is not a valid script.
	#[error("invalid script {path}: {source}")]
	Parse {
		/// The script path.
		path: String,
		/// The JSON or shape error.
		source: serde_json::Error,
	},
	/// A chunking size of zero would never make progress.
	#[error("invalid script {path}: response {index} has a chunking size of 0")]
	ZeroChunk {
		/// The script path.
		path: String,
		/// The response index, from zero.
		index: usize,
	},
	/// An error reply needs an error status.
	#[error("invalid script {path}: response {index} has error status {status}; use 400 to 599")]
	ErrorStatus {
		/// The script path.
		path: String,
		/// The response index, from zero.
		index: usize,
		/// The status found.
		status: u16,
	},
}

/// Reads and checks a script file (a trust boundary: the file is written by hand).
pub fn load_script(path: &Path) -> Result<Script, ScriptError> {
	let display = path.display().to_string();
	let text = std::fs::read_to_string(path).map_err(|source| ScriptError::Read {
		path: display.clone(),
		source,
	})?;
	let parsed = pi_json::parse(&text).and_then(serde_json::from_value::<Script>);
	let script = parsed.map_err(|source| ScriptError::Parse {
		path: display.clone(),
		source,
	})?;
	for (index, response) in script.responses.iter().enumerate() {
		if matches!(response.chunking, Some(Chunking::Tokens(0) | Chunking::Bytes(0))) {
			return Err(ScriptError::ZeroChunk { path: display, index });
		}
		if let Some(error) = &response.error
			&& !(400..=599).contains(&error.status)
		{
			return Err(ScriptError::ErrorStatus {
				path: display,
				index,
				status: error.status,
			});
		}
	}
	Ok(script)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn load(name: &str, text: &str) -> Result<Script, ScriptError> {
		let path = std::env::temp_dir().join(format!("pi-fake-provider-script-{}-{name}.json", std::process::id()));
		std::fs::write(&path, text).unwrap();
		let result = load_script(&path);
		std::fs::remove_file(&path).unwrap();
		result
	}

	#[test]
	fn reads_every_field_with_defaults() {
		let script = load(
			"full",
			r#"{"responses":[{"content":[{"type":"thinking","thinking":"t"},{"type":"toolCall","id":"c","name":"n","arguments":{}}],"stopReason":"toolUse","usage":{"input":1,"cacheRead":2},"delayMs":3,"chunking":{"bytes":4},"pauseAfter":5},{"error":{"status":429,"body":"x","retryAfter":1}}]}"#,
		)
		.unwrap();
		let first = &script.responses[0];
		assert_eq!(
			first.content[0],
			Block::Thinking {
				thinking: "t".into(),
				signature: String::new()
			}
		);
		assert_eq!(first.stop_reason, StopReason::ToolUse);
		assert_eq!(
			first.usage,
			Usage {
				input: 1,
				output: 0,
				cache_read: 2,
				cache_write: 0
			}
		);
		assert_eq!(
			(first.delay_ms, first.chunking, first.pause_after),
			(Some(3), Some(Chunking::Bytes(4)), Some(5))
		);
		let second = &script.responses[1];
		assert_eq!(second.stop_reason, StopReason::Stop);
		assert_eq!(
			second.error.as_ref().map(|error| (error.status, error.retry_after)),
			Some((429, Some(1)))
		);
	}

	#[test]
	fn rejects_unknown_fields_and_zero_chunks() {
		assert!(matches!(
			load("unknown", r#"{"responses":[{"stop":"x"}]}"#),
			Err(ScriptError::Parse { .. })
		));
		assert!(matches!(
			load("zero", r#"{"responses":[{},{"chunking":{"tokens":0}}]}"#),
			Err(ScriptError::ZeroChunk { index: 1, .. })
		));
		for status in ["200", "99", "1000"] {
			let text = format!(r#"{{"responses":[{{"error":{{"status":{status},"body":"x"}}}}]}}"#);
			assert!(
				matches!(load(status, &text), Err(ScriptError::ErrorStatus { index: 0, .. })),
				"{status}"
			);
		}
	}
}
