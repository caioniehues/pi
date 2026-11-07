//! The openai-completions SSE renderer (`POST /v1/chat/completions`): one scripted response as the chunk sequence
//! the Chat Completions API streams and `packages/ai/src/api/openai-completions.ts` consumes.

use crate::chunking::deltas;
use crate::script::{Block, ScriptResponse, StopReason, Usage};
use pi_json::JsonValue;
use serde_json::json;

/// The SSE events of `response`, one string per event: a role chunk, `reasoning_content`, `content` and
/// `tool_calls` delta chunks in block order, a `finish_reason` chunk, a usage chunk, then `[DONE]`.
pub fn render_openai_completions(response: &ScriptResponse, completion_id: &str, model: &str) -> Vec<String> {
	let chunk = |choices: JsonValue| {
		json!({
			"id": completion_id,
			"object": "chat.completion.chunk",
			"created": 0,
			"model": model,
			"choices": choices,
		})
	};
	let delta = |delta: JsonValue, finish_reason: JsonValue| {
		event(&chunk(
			json!([{ "index": 0, "delta": delta, "finish_reason": finish_reason }]),
		))
	};
	let mut events = vec![delta(json!({ "role": "assistant", "content": "" }), JsonValue::Null)];
	let mut tool_index = 0;
	for block in &response.content {
		match block {
			Block::Text { text } => {
				for piece in deltas(text, response.chunking) {
					events.push(delta(json!({ "content": piece }), JsonValue::Null));
				}
			}
			Block::Thinking { thinking, .. } => {
				for piece in deltas(thinking, response.chunking) {
					events.push(delta(json!({ "reasoning_content": piece }), JsonValue::Null));
				}
			}
			Block::ToolCall { id, name, arguments } => {
				let function = json!({ "name": name, "arguments": "" });
				let head = json!({ "index": tool_index, "id": id, "type": "function", "function": function });
				events.push(delta(json!({ "tool_calls": [head] }), JsonValue::Null));
				for piece in deltas(&pi_json::to_js_string(arguments), response.chunking) {
					let part = json!({ "index": tool_index, "function": { "arguments": piece } });
					events.push(delta(json!({ "tool_calls": [part] }), JsonValue::Null));
				}
				tool_index += 1;
			}
		}
	}
	events.push(delta(json!({}), json!(finish_reason(response.stop_reason))));
	let mut usage_chunk = chunk(json!([]));
	usage_chunk["usage"] = usage_json(response.usage);
	events.push(event(&usage_chunk));
	events.push("data: [DONE]\n\n".to_owned());
	events
}

fn finish_reason(reason: StopReason) -> &'static str {
	match reason {
		StopReason::Stop => "stop",
		StopReason::Length => "length",
		StopReason::ToolUse => "tool_calls",
	}
}

/// OpenAI counts cached input inside `prompt_tokens`; `cache_write_tokens` is the OpenRouter-style extension the
/// TS client reads, sent only when there are any.
fn usage_json(usage: Usage) -> JsonValue {
	let prompt = usage.input + usage.cache_read + usage.cache_write;
	let mut details = json!({ "cached_tokens": usage.cache_read });
	if usage.cache_write > 0 {
		details["cache_write_tokens"] = json!(usage.cache_write);
	}
	json!({
		"prompt_tokens": prompt,
		"completion_tokens": usage.output,
		"total_tokens": prompt + usage.output,
		"prompt_tokens_details": details,
	})
}

fn event(data: &JsonValue) -> String {
	format!("data: {}\n\n", pi_json::to_js_string(data))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::chunking::writes;
	use crate::script::Chunking;

	fn response(content: Vec<Block>, stop_reason: StopReason, chunking: Option<Chunking>) -> ScriptResponse {
		ScriptResponse {
			content,
			stop_reason,
			usage: Usage {
				input: 10,
				output: 5,
				cache_read: 2,
				cache_write: 0,
			},
			delay_ms: None,
			chunking,
			pause_after: None,
			error: None,
		}
	}

	const HEAD: &str = r#"data: {"id":"c1","object":"chat.completion.chunk","created":0,"model":"m","choices":"#;

	fn line(choices: &str) -> String {
		format!("{HEAD}{choices}}}\n\n")
	}

	fn tail(finish_reason: &str) -> String {
		format!(
			"{}{HEAD}[],\"usage\":{{\"prompt_tokens\":12,\"completion_tokens\":5,\"total_tokens\":17,\"prompt_tokens_details\":{{\"cached_tokens\":2}}}}}}\n\ndata: [DONE]\n\n",
			line(&format!(
				r#"[{{"index":0,"delta":{{}},"finish_reason":"{finish_reason}"}}]"#
			))
		)
	}

	fn role() -> String {
		line(r#"[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]"#)
	}

	#[test]
	fn text_block_in_one_delta() {
		let body = render_openai_completions(
			&response(vec![Block::Text { text: "Hello".into() }], StopReason::Stop, None),
			"c1",
			"m",
		)
		.concat();
		let expected = format!(
			"{}{}{}",
			role(),
			line(r#"[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]"#),
			tail("stop")
		);
		assert_eq!(body, expected);
	}

	#[test]
	fn thinking_block_streams_reasoning_content_by_tokens() {
		let block = Block::Thinking {
			thinking: "abc".into(),
			signature: "ignored".into(),
		};
		let body = render_openai_completions(
			&response(vec![block], StopReason::Length, Some(Chunking::Tokens(2))),
			"c1",
			"m",
		)
		.concat();
		let expected = format!(
			"{}{}{}{}",
			role(),
			line(r#"[{"index":0,"delta":{"reasoning_content":"ab"},"finish_reason":null}]"#),
			line(r#"[{"index":0,"delta":{"reasoning_content":"c"},"finish_reason":null}]"#),
			tail("length")
		);
		assert_eq!(body, expected);
	}

	#[test]
	fn tool_call_blocks_get_their_own_indexes() {
		let first = Block::ToolCall {
			id: "call_1".into(),
			name: "read".into(),
			arguments: json!({ "path": "a" }),
		};
		let second = Block::ToolCall {
			id: "call_2".into(),
			name: "ls".into(),
			arguments: json!({}),
		};
		let body = render_openai_completions(
			&response(
				vec![Block::Text { text: "x".into() }, first, second],
				StopReason::ToolUse,
				None,
			),
			"c1",
			"m",
		)
		.concat();
		let expected = format!(
			"{}{}{}{}{}{}{}",
			role(),
			line(r#"[{"index":0,"delta":{"content":"x"},"finish_reason":null}]"#),
			line(
				r#"[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]},"finish_reason":null}]"#
			),
			line(
				r#"[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"a\"}"}}]},"finish_reason":null}]"#
			),
			line(
				r#"[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_2","type":"function","function":{"name":"ls","arguments":""}}]},"finish_reason":null}]"#
			),
			line(
				r#"[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{}"}}]},"finish_reason":null}]"#
			),
			tail("tool_calls")
		);
		assert_eq!(body, expected);
	}

	#[test]
	fn usage_reports_cache_writes_only_when_present() {
		let usage = usage_json(Usage {
			input: 1,
			output: 2,
			cache_read: 3,
			cache_write: 4,
		});
		assert_eq!(
			pi_json::to_js_string(&usage),
			r#"{"prompt_tokens":8,"completion_tokens":2,"total_tokens":10,"prompt_tokens_details":{"cached_tokens":3,"cache_write_tokens":4}}"#
		);
	}

	#[test]
	fn byte_chunking_cuts_the_whole_stream() {
		let rendered = render_openai_completions(
			&response(
				vec![Block::Text { text: "é".into() }],
				StopReason::Stop,
				Some(Chunking::Bytes(5)),
			),
			"c1",
			"m",
		);
		let whole = rendered.concat();
		let pieces = writes(rendered, Some(Chunking::Bytes(5)));
		assert_eq!(pieces.len(), whole.len().div_ceil(5));
		assert_eq!(pieces.concat(), whole.as_bytes());
		assert!(whole.ends_with("data: [DONE]\n\n"));
	}
}
