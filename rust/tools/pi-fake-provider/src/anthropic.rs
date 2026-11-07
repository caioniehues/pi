//! The anthropic-messages SSE renderer (`POST /v1/messages`): one scripted response as the event sequence the
//! Anthropic API streams and `packages/ai/src/api/anthropic-messages.ts` consumes.

use crate::chunking::deltas;
use crate::script::{Block, ScriptResponse, StopReason};
use pi_json::JsonValue;
use serde_json::json;

/// The SSE events of `response`, one string per event: `message_start`, a start/deltas/stop triple per block,
/// `message_delta` with the stop reason and output tokens, `message_stop`.
pub fn render_anthropic(response: &ScriptResponse, message_id: &str, model: &str) -> Vec<String> {
	let usage = response.usage;
	let mut events = vec![event(
		"message_start",
		&json!({
			"type": "message_start",
			"message": {
				"id": message_id,
				"type": "message",
				"role": "assistant",
				"model": model,
				"content": [],
				"stop_reason": null,
				"stop_sequence": null,
				"usage": {
					"input_tokens": usage.input,
					"cache_creation_input_tokens": usage.cache_write,
					"cache_read_input_tokens": usage.cache_read,
					"output_tokens": 0,
				},
			},
		}),
	)];
	for (index, block) in response.content.iter().enumerate() {
		render_block(block, index, response, &mut events);
	}
	events.push(event(
		"message_delta",
		&json!({
			"type": "message_delta",
			"delta": { "stop_reason": stop_reason(response.stop_reason), "stop_sequence": null },
			"usage": { "output_tokens": usage.output },
		}),
	));
	events.push(event("message_stop", &json!({ "type": "message_stop" })));
	events
}

fn render_block(block: &Block, index: usize, response: &ScriptResponse, events: &mut Vec<String>) {
	let (start, text, delta_type, delta_field) = match block {
		Block::Text { text } => (
			json!({ "type": "text", "text": "" }),
			text.clone(),
			"text_delta",
			"text",
		),
		Block::Thinking { thinking, .. } => (
			json!({ "type": "thinking", "thinking": "", "signature": "" }),
			thinking.clone(),
			"thinking_delta",
			"thinking",
		),
		Block::ToolCall { id, name, arguments } => (
			json!({ "type": "tool_use", "id": id, "name": name, "input": {} }),
			pi_json::to_js_string(arguments),
			"input_json_delta",
			"partial_json",
		),
	};
	events.push(event(
		"content_block_start",
		&json!({ "type": "content_block_start", "index": index, "content_block": start }),
	));
	for piece in deltas(&text, response.chunking) {
		events.push(content_delta(index, &json!({ "type": delta_type, delta_field: piece })));
	}
	if let Block::Thinking { signature, .. } = block
		&& !signature.is_empty()
	{
		events.push(content_delta(
			index,
			&json!({ "type": "signature_delta", "signature": signature }),
		));
	}
	events.push(event(
		"content_block_stop",
		&json!({ "type": "content_block_stop", "index": index }),
	));
}

fn content_delta(index: usize, delta: &JsonValue) -> String {
	event(
		"content_block_delta",
		&json!({ "type": "content_block_delta", "index": index, "delta": delta }),
	)
}

fn stop_reason(reason: StopReason) -> &'static str {
	match reason {
		StopReason::Stop => "end_turn",
		StopReason::Length => "max_tokens",
		StopReason::ToolUse => "tool_use",
	}
}

fn event(name: &str, data: &JsonValue) -> String {
	format!("event: {name}\ndata: {}\n\n", pi_json::to_js_string(data))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::chunking::writes;
	use crate::script::{Chunking, Usage};

	fn response(content: Vec<Block>, stop_reason: StopReason, chunking: Option<Chunking>) -> ScriptResponse {
		ScriptResponse {
			content,
			stop_reason,
			usage: Usage {
				input: 10,
				output: 5,
				cache_read: 2,
				cache_write: 1,
			},
			delay_ms: None,
			chunking,
			pause_after: None,
			error: None,
		}
	}

	const START: &str = concat!(
		"event: message_start\n",
		r#"data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":1,"cache_read_input_tokens":2,"output_tokens":0}}}"#,
		"\n\n"
	);

	fn end(stop_reason: &str) -> String {
		format!(
			"event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop_reason}\",\"stop_sequence\":null}},\"usage\":{{\"output_tokens\":5}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
		)
	}

	#[test]
	fn text_block_in_one_delta() {
		let body = render_anthropic(
			&response(vec![Block::Text { text: "Hello".into() }], StopReason::Stop, None),
			"msg_1",
			"m",
		)
		.concat();
		let expected = format!(
			"{START}{}{}",
			concat!(
				"event: content_block_start\n",
				r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
				"\n\nevent: content_block_stop\n",
				r#"data: {"type":"content_block_stop","index":0}"#,
				"\n\n"
			),
			end("end_turn")
		);
		assert_eq!(body, expected);
	}

	#[test]
	fn thinking_block_with_token_chunking_and_signature() {
		let block = Block::Thinking {
			thinking: "abcde".into(),
			signature: "sig".into(),
		};
		let body = render_anthropic(
			&response(vec![block], StopReason::Length, Some(Chunking::Tokens(2))),
			"msg_1",
			"m",
		)
		.concat();
		let expected = format!(
			"{START}{}{}",
			concat!(
				"event: content_block_start\n",
				r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ab"}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"cd"}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"e"}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
				"\n\nevent: content_block_stop\n",
				r#"data: {"type":"content_block_stop","index":0}"#,
				"\n\n"
			),
			end("max_tokens")
		);
		assert_eq!(body, expected);
	}

	#[test]
	fn tool_call_block_streams_argument_json() {
		let block = Block::ToolCall {
			id: "toolu_1".into(),
			name: "bash".into(),
			arguments: json!({ "command": "ls" }),
		};
		let body = render_anthropic(
			&response(vec![block], StopReason::ToolUse, Some(Chunking::Tokens(8))),
			"msg_1",
			"m",
		)
		.concat();
		let expected = format!(
			"{START}{}{}",
			concat!(
				"event: content_block_start\n",
				r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"comman"}}"#,
				"\n\nevent: content_block_delta\n",
				r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"d\":\"ls\"}"}}"#,
				"\n\nevent: content_block_stop\n",
				r#"data: {"type":"content_block_stop","index":0}"#,
				"\n\n"
			),
			end("tool_use")
		);
		assert_eq!(body, expected);
	}

	#[test]
	fn byte_chunking_cuts_the_whole_stream() {
		let rendered = render_anthropic(
			&response(
				vec![Block::Text { text: "Hi".into() }],
				StopReason::Stop,
				Some(Chunking::Bytes(7)),
			),
			"msg_1",
			"m",
		);
		let whole = rendered.concat();
		let pieces = writes(rendered, Some(Chunking::Bytes(7)));
		assert!(pieces[..pieces.len() - 1].iter().all(|piece| piece.len() == 7));
		assert_eq!(pieces.concat(), whole.as_bytes());
		assert_eq!(&pieces[0][..], b"event: ");
	}
}
