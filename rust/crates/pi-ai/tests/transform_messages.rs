//! Transcript preparation for a target model (`transform_messages.rs`): ports
//! `transform-messages-copilot-openai-to-anthropic.test.ts`, `transform-messages-cross-model-thinking.test.ts`,
//! `lax-message-content.test.ts` and the wire-free parts of `cross-provider-handoff.test.ts` and
//! `tool-call-id-normalization.test.ts` (their live handoffs need provider APIs and keys).

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

mod common;

use pi_ai::transform_messages::{split_error_tool_result_images, transform_messages};
use pi_types::{AssistantContentBlock, AssistantMessage, Message, Model};
use serde_json::json;

fn anthropic_normalize(id: &str, _model: &Model, _source: &AssistantMessage) -> String {
	let replaced: String = id
		.chars()
		.map(|character| {
			if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
				character
			} else {
				'_'
			}
		})
		.collect();
	replaced.chars().take(64).collect()
}

fn copilot_claude() -> Model {
	common::model(
		"anthropic-messages",
		"github-copilot",
		"claude-sonnet-4.6",
		json!({ "reasoning": true, "input": ["text", "image"], "baseUrl": "https://api.individual.githubcopilot.com" }),
	)
}

fn gpt_turn(content: serde_json::Value) -> Message {
	Message::Assistant(common::assistant(
		json!({ "content": content, "api": "openai-responses", "provider": "github-copilot", "model": "gpt-5", "stopReason": "toolUse" }),
	))
}

fn tool_result(id: &str, name: &str, text: &str) -> Message {
	common::from_json(
		json!({ "role": "toolResult", "toolCallId": id, "toolName": name, "content": [{ "type": "text", "text": text }], "isError": false, "timestamp": 1 }),
	)
}

// TS: transform-messages-copilot-openai-to-anthropic.test.ts
#[test]
fn removes_thought_signature_from_tool_calls_when_migrating_between_models() {
	let signature = r#"{"type":"reasoning.encrypted","id":"call_123","data":"encrypted"}"#;
	let messages = vec![
		common::user("run a command", 1),
		gpt_turn(
			json!([{ "type": "toolCall", "id": "call_123", "name": "bash", "arguments": { "command": "ls" }, "thoughtSignature": signature }]),
		),
		tool_result("call_123", "bash", "output"),
	];
	let result = transform_messages(&messages, &copilot_claude(), Some(&anthropic_normalize));
	let Message::Assistant(assistant) = &result[1] else {
		panic!("assistant")
	};
	let AssistantContentBlock::ToolCall(call) = &assistant.content[0] else {
		panic!("tool call")
	};
	assert_eq!(call.thought_signature, None);
}

// TS: transform-messages-copilot-openai-to-anthropic.test.ts
#[test]
fn adds_synthetic_tool_results_for_trailing_orphaned_tool_calls() {
	let messages = vec![
		common::user("read the file", 1),
		gpt_turn(
			json!([{ "type": "toolCall", "id": "call_123|fc_123", "name": "read", "arguments": { "path": "README.md" } }]),
		),
	];
	let result = transform_messages(&messages, &copilot_claude(), Some(&anthropic_normalize));
	let last = common::to_json(result.last().unwrap());
	assert_eq!(
		(
			last["role"].clone(),
			last["toolCallId"].clone(),
			last["toolName"].clone(),
			last["isError"].clone()
		),
		(
			json!("toolResult"),
			json!("call_123_fc_123"),
			json!("read"),
			json!(true)
		)
	);
	assert_eq!(
		last["content"],
		json!([{ "type": "text", "text": "No result provided" }])
	);
}

// TS: transform-messages-copilot-openai-to-anthropic.test.ts
#[test]
fn adds_synthetic_results_only_for_trailing_tool_calls_that_are_still_missing_results() {
	let messages = vec![
		common::user("run commands", 1),
		gpt_turn(json!([
			{ "type": "toolCall", "id": "call_1|fc_1", "name": "read", "arguments": { "path": "README.md" } },
			{ "type": "toolCall", "id": "call_2|fc_2", "name": "bash", "arguments": { "command": "pwd" } }
		])),
		tool_result("call_1|fc_1", "read", "done"),
	];
	let result = transform_messages(&messages, &copilot_claude(), Some(&anthropic_normalize));
	let synthetic: Vec<serde_json::Value> = result
		.iter()
		.map(common::to_json)
		.filter(|message| message["role"] == "toolResult" && message["isError"] == true)
		.collect();
	assert_eq!(synthetic.len(), 1);
	assert_eq!(
		(synthetic[0]["toolCallId"].clone(), synthetic[0]["toolName"].clone()),
		(json!("call_2_fc_2"), json!("bash"))
	);
	// The answered call keeps its result, under its normalized id.
	assert!(
		result
			.iter()
			.map(common::to_json)
			.any(|message| message["toolCallId"] == "call_1_fc_1" && message["isError"] == false)
	);
}

fn kimi() -> Model {
	common::model(
		"openai-completions",
		"sference",
		"moonshotai/Kimi-K3",
		json!({ "reasoning": true, "input": ["text", "image"] }),
	)
}

fn sference(model: &str, content: serde_json::Value, stop: &str) -> Message {
	Message::Assistant(common::assistant(
		json!({ "content": content, "api": "openai-completions", "provider": "sference", "model": model, "stopReason": stop }),
	))
}

fn assistant_contents(result: &[Message]) -> Vec<serde_json::Value> {
	result
		.iter()
		.filter_map(|message| match message {
			Message::Assistant(assistant) => Some(common::to_json(&assistant.content)),
			_ => None,
		})
		.collect()
}

// https://github.com/earendil-works/pi/issues/10995
// TS: transform-messages-cross-model-thinking.test.ts
#[test]
fn does_not_send_another_models_readable_thinking_as_text() {
	let messages = vec![
		common::user("Why does curl work but node fails?", 1),
		sference(
			"zai-org/GLM-5.3",
			json!([
				{ "type": "thinking", "thinking": "The user asks about ECONNRESET. Candidates: proxy, IPv6, TLS.", "thinkingSignature": "reasoning_content" },
				{ "type": "text", "text": "Let me check the TLS versions." },
				{ "type": "toolCall", "id": "call_1", "name": "bash", "arguments": { "command": "curl -v" } }
			]),
			"toolUse",
		),
		tool_result("call_1", "bash", "TLSv1.2"),
	];
	assert_eq!(
		assistant_contents(&transform_messages(&messages, &kimi(), None))[0],
		json!([{ "type": "text", "text": "Let me check the TLS versions." }, { "type": "toolCall", "id": "call_1", "name": "bash", "arguments": { "command": "curl -v" } }])
	);
}

// TS: transform-messages-cross-model-thinking.test.ts
#[test]
fn keeps_thinking_only_from_the_target_model_when_models_alternate() {
	let thinking =
		|text: &str| json!({ "type": "thinking", "thinking": text, "thinkingSignature": "reasoning_content" });
	let text = |text: &str| json!({ "type": "text", "text": text });
	let messages = vec![
		common::user("1", 1),
		sference("moonshotai/Kimi-K3", json!([thinking("kimi 1"), text("a")]), "stop"),
		common::user("2", 1),
		sference("zai-org/GLM-5.3", json!([thinking("glm 1"), text("b")]), "stop"),
		common::user("3", 1),
		sference("moonshotai/Kimi-K3", json!([thinking("kimi 2"), text("c")]), "stop"),
		common::user("4", 1),
		sference("zai-org/GLM-5.3", json!([thinking("glm 2"), text("d")]), "stop"),
		common::user("hi", 1),
	];
	assert_eq!(
		assistant_contents(&transform_messages(&messages, &kimi(), None)),
		[
			json!([thinking("kimi 1"), text("a")]),
			json!([text("b")]),
			json!([thinking("kimi 2"), text("c")]),
			json!([text("d")])
		]
	);
}

// TS: lax-message-content.test.ts
#[test]
fn lax_message_content_null_content_never_reaches_transform_messages() {
	// TS normalizes null content of untyped callers; here a message without content does not read as a `Message`,
	// so the typed transcript cannot carry one (the trust boundary is deserialization, issues #6259, #6276).
	for text in [
		r#"{"role":"user","content":null,"timestamp":1}"#,
		r#"{"role":"toolResult","toolCallId":"call_1","toolName":"web_search","isError":false,"timestamp":1}"#,
	] {
		assert!(
			serde_json::from_value::<Message>(pi_json::parse(text).unwrap()).is_err(),
			"{text}"
		);
	}
}

// Rust-only.
#[test]
fn skips_failed_turns_and_downgrades_images_for_text_only_models() {
	let text_only = common::model("openai-completions", "openai", "test-model", json!({}));
	let messages = vec![
		common::from_json(
			json!({ "role": "user", "content": [{ "type": "image", "data": "a", "mimeType": "image/png" }, { "type": "image", "data": "b", "mimeType": "image/png" }], "timestamp": 1 }),
		),
		sference("x", json!([{ "type": "text", "text": "partial" }]), "error"),
	];
	let result = transform_messages(&messages, &text_only, None);
	assert_eq!(result.len(), 1);
	assert_eq!(
		common::to_json(&result[0])["content"],
		json!([{ "type": "text", "text": "(image omitted: model does not support images)" }])
	);
}

// Rust-only.
#[test]
fn splits_images_out_of_error_tool_results() {
	let result: pi_types::ToolResultMessage = common::from_json(
		json!({ "role": "toolResult", "toolCallId": "c1", "toolName": "shot", "isError": true, "timestamp": 1,
		"content": [{ "type": "text", "text": "failed" }, { "type": "image", "data": "a", "mimeType": "image/png" }, { "type": "image", "data": "b", "mimeType": "image/png" }] }),
	);
	let split = split_error_tool_result_images(&result).unwrap();
	assert_eq!(
		common::to_json(&split.content),
		json!([{ "type": "text", "text": "failed" }, { "type": "text", "text": "(image attached after the tool results)" }])
	);
	assert_eq!(split.caption, "Images from the error result of tool call c1 (shot):");
	assert_eq!(split.images.len(), 2);
}
