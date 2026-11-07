//! Transcript replay (`transcript.rs`, `text.rs`): ports `system-message-replay.test.ts`, `text.test.ts` and the
//! transport-free case of `transcript-tool-changes.test.ts` ("resolves the top-level tool boundary per transport").

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

use common::{from_json, to_json, tool, user};
use pi_ai::text::{blocks_text, get_system_message_text, render_system_message_update};
use pi_ai::transcript::{
	Context, TranscriptToolSupport, collapse_system_messages, declarations_equal, get_current_system_message,
	get_current_system_prompt, get_tool_state_changes, has_non_additive_tool_changes, has_tool_redefinitions,
	normalize_context, resolve_transcript_tools, transcript_from_messages,
};
use pi_types::{AssistantContentBlock, ContentBlock, Message, Tool};
use serde_json::json;

fn tool_of(name: &str) -> Tool {
	tool(name, &format!("{name} tool"))
}

fn system(value: serde_json::Value) -> Message {
	from_json(value)
}

fn transcript() -> Vec<Message> {
	vec![
		system(
			json!({ "role": "system", "content": "base", "sections": { "a": "<a>1</a>", "b": "<b>1</b>" }, "toolsAdded": [to_json(&tool_of("first"))], "timestamp": 10 }),
		),
		user("hello", 11),
		system(json!({ "role": "system", "content": "also do this", "timestamp": 12 })),
		Message::Assistant(common::assistant(
			json!({ "content": [{ "type": "text", "text": "ok" }], "timestamp": 13 }),
		)),
		system(
			json!({ "role": "system", "content": "", "sections": { "a": "<a>2</a>", "b": null, "c": "<c>1</c>" }, "toolsRemoved": [{ "name": "first" }], "toolsAdded": [to_json(&tool_of("second"))], "timestamp": 14 }),
		),
	]
}

// TS: system-message-replay.test.ts
#[test]
fn replays_content_sections_and_tools_into_one_leading_message() {
	let current = get_current_system_message(&transcript()).unwrap();
	assert_eq!(
		to_json(&current),
		json!({ "role": "system", "content": "base\n\nalso do this", "sections": { "a": "<a>2</a>", "c": "<c>1</c>" }, "toolsAdded": [to_json(&tool_of("second"))], "timestamp": 10 })
	);
	assert_eq!(
		get_current_system_prompt(&transcript()),
		"base\n\nalso do this\n\n<a>2</a>\n\n<c>1</c>"
	);
}

// TS: system-message-replay.test.ts
#[test]
fn collapse_keeps_only_non_system_messages_after_the_replayed_head() {
	let collapsed = collapse_system_messages(&transcript_from_messages(transcript()));
	let roles: Vec<&str> = collapsed
		.messages()
		.iter()
		.map(|message| match message {
			Message::System(_) => "system",
			Message::User(_) => "user",
			Message::Assistant(_) => "assistant",
			Message::ToolResult(_) => "toolResult",
		})
		.collect();
	assert_eq!(roles, ["system", "user", "assistant"]);
	assert_eq!(collapse_system_messages(&collapsed), collapsed);
}

// TS: system-message-replay.test.ts
#[test]
fn replay_of_a_transcript_without_system_messages_is_empty() {
	let context = normalize_context(Context {
		messages: vec![user("hi", 1)],
		..Context::default()
	});
	assert_eq!(get_current_system_message(context.messages()), None);
	assert_eq!(get_current_system_prompt(context.messages()), "");
	assert_eq!(collapse_system_messages(&context).messages(), context.messages());
}

// TS: system-message-replay.test.ts
#[test]
fn a_late_full_patch_on_a_transcript_without_a_leading_message_replays_as_the_prompt() {
	let messages = vec![
		user("old session", 1),
		system(
			json!({ "role": "system", "content": "", "sections": { "preamble": "You are pi." }, "toolsAdded": [to_json(&tool_of("x"))], "timestamp": 2 }),
		),
	];
	assert_eq!(get_current_system_prompt(&messages), "You are pi.");
	let collapsed = collapse_system_messages(&transcript_from_messages(messages));
	let Message::System(head) = &collapsed.messages()[0] else {
		panic!("system head")
	};
	assert_eq!(head.tools_added.as_deref(), Some(&[tool_of("x")][..]));
}

// TS: system-message-replay.test.ts
#[test]
fn renders_complete_prompts_and_framed_updates() {
	let messages = transcript();
	let (Message::System(leading), Message::System(update)) = (&messages[0], &messages[4]) else {
		panic!("system messages")
	};
	assert_eq!(get_system_message_text(leading), "base\n\n<a>1</a>\n\n<b>1</b>");
	assert_eq!(
		render_system_message_update(update),
		[
			"Updated system prompt section \"a\":\n\n<a>2</a>",
			"Removed system prompt section \"b\".",
			"Updated system prompt section \"c\":\n\n<c>1</c>"
		]
		.join("\n\n")
	);
}

// TS: system-message-replay.test.ts
#[test]
fn normalizes_the_legacy_prompt_and_tool_fields_into_a_leading_system_message() {
	let messages = vec![user("hi", 1)];
	assert_eq!(
		normalize_context(Context {
			messages: messages.clone(),
			..Context::default()
		})
		.messages(),
		messages
	);
	let empty = Context {
		system_prompt: Some(String::new()),
		tools: Some(Vec::new()),
		messages: messages.clone(),
	};
	assert_eq!(normalize_context(empty).messages(), messages);
	let full = Context {
		system_prompt: Some("be brief".into()),
		tools: Some(vec![tool_of("a")]),
		messages: messages.clone(),
	};
	let normalized = normalize_context(full);
	assert_eq!(
		to_json(&normalized.messages()[0]),
		json!({ "role": "system", "content": "be brief", "toolsAdded": [to_json(&tool_of("a"))], "timestamp": 0 })
	);
	assert_eq!(&normalized.messages()[1..], messages);
}

// TS: system-message-replay.test.ts
#[test]
fn compares_tool_declarations_without_executable_or_undefined_fields() {
	let mut executable = tool_of("a");
	executable.extra.insert("execute".into(), json!("fn"));
	assert!(declarations_equal(&executable, &tool_of("a")));
	assert!(!declarations_equal(&tool_of("a"), &tool("a", "changed")));
	let constrained: Tool = from_json(
		json!({ "name": "a", "description": "a tool", "parameters": { "type": "object", "properties": {} }, "constrainedSampling": false }),
	);
	assert!(!declarations_equal(&tool_of("a"), &constrained));
}

// TS: system-message-replay.test.ts
#[test]
fn tool_state_changes_treat_changed_definitions_as_removal_plus_addition() {
	let changes = get_tool_state_changes(&[tool_of("a"), tool_of("b")], &[tool("b", "changed"), tool_of("c")]);
	assert_eq!(changes.tools_added, [tool("b", "changed"), tool_of("c")]);
	assert_eq!(
		changes
			.tools_removed
			.iter()
			.map(|tool| tool.name.as_str())
			.collect::<Vec<_>>(),
		["a", "b"]
	);
	let unchanged = get_tool_state_changes(&[tool_of("a")], &[tool_of("a")]);
	assert!(unchanged.tools_added.is_empty() && unchanged.tools_removed.is_empty());
}

// TS: system-message-replay.test.ts
#[test]
fn detects_non_additive_tool_history_and_redefinitions() {
	assert!(has_non_additive_tool_changes(&transcript()));
	assert!(!has_tool_redefinitions(&transcript()));
	let added = |name: &str, description: &str, timestamp: u64| {
		system(
			json!({ "role": "system", "content": "", "toolsAdded": [to_json(&tool(name, description))], "timestamp": timestamp }),
		)
	};
	let additive = vec![added("a", "a tool", 1), added("b", "b tool", 2)];
	assert!(!has_non_additive_tool_changes(&additive));
	let redeclared = vec![added("a", "a tool", 1), added("a", "changed", 2)];
	assert!(has_non_additive_tool_changes(&redeclared));
	assert!(has_tool_redefinitions(&redeclared));
}

// TS: transcript-tool-changes.test.ts
#[test]
fn resolves_the_top_level_tool_boundary_per_transport() {
	let (base, drop, late) = (tool_of("base_tool"), tool_of("drop_tool"), |description: &str| {
		tool("late_tool", description)
	});
	let messages = vec![
		system(
			json!({ "role": "system", "content": "base prompt", "toolsAdded": [to_json(&base), to_json(&drop)], "timestamp": 0 }),
		),
		system(json!({ "role": "system", "content": "", "toolsRemoved": [{ "name": "drop_tool" }], "timestamp": 1 })),
		system(json!({ "role": "system", "content": "", "toolsAdded": [to_json(&drop)], "timestamp": 2 })),
		system(json!({ "role": "system", "content": "", "toolsAdded": [to_json(&late("x"))], "timestamp": 3 })),
		system(json!({ "role": "system", "content": "", "toolsAdded": [to_json(&late("y"))], "timestamp": 4 })),
	];
	let resolved = resolve_transcript_tools(&messages[..2], TranscriptToolSupport::Additions);
	assert_eq!(
		(resolved.request_tools, resolved.folded_system_messages),
		(vec![base.clone()], 2)
	);
	// Kimi K3 ignores a removed tool that a later tool message declares again, so the re-added drop_tool moves the
	// boundary; the new late_tool after it stays anchored.
	let resolved = resolve_transcript_tools(&messages[..4], TranscriptToolSupport::Additions);
	assert_eq!(
		(resolved.request_tools, resolved.folded_system_messages),
		(vec![base.clone(), drop.clone()], 3)
	);
	let all = vec![base, drop, late("y")];
	let resolved = resolve_transcript_tools(&messages, TranscriptToolSupport::Additions);
	assert_eq!(
		(resolved.request_tools, resolved.folded_system_messages),
		(all.clone(), 5)
	);
	let resolved = resolve_transcript_tools(&messages, TranscriptToolSupport::Changes);
	assert_eq!(
		(resolved.request_tools, resolved.folded_system_messages),
		(Vec::new(), 0)
	);
	let resolved = resolve_transcript_tools(&messages, TranscriptToolSupport::None);
	assert_eq!((resolved.request_tools, resolved.folded_system_messages), (all, 5));
}

fn assistant_content() -> Vec<AssistantContentBlock> {
	from_json(json!([
		{ "type": "thinking", "thinking": "reasoning" },
		{ "type": "text", "text": "first" },
		{ "type": "toolCall", "id": "1", "name": "read", "arguments": {} },
		{ "type": "text", "text": "second" }
	]))
}

// TS: text.test.ts
#[test]
fn content_text_extracts_assistant_text_blocks() {
	assert_eq!(blocks_text(&assistant_content(), "\n"), "first\nsecond");
}

// TS: text.test.ts
#[test]
fn content_text_supports_custom_separators() {
	assert_eq!(blocks_text(&assistant_content(), ""), "firstsecond");
}

// TS: text.test.ts
#[test]
fn content_text_passes_string_content_through() {
	assert_eq!(
		pi_ai::text::message_content_text(&pi_types::MessageContent::Text("hello".into())),
		"hello"
	);
}

// TS: text.test.ts
#[test]
fn content_text_extracts_text_from_tool_result_content() {
	let content: Vec<ContentBlock> = from_json(json!([
		{ "type": "text", "text": "first" }, { "type": "image", "data": "...", "mimeType": "image/png" }, { "type": "text", "text": "second" }
	]));
	assert_eq!(blocks_text(&content, ""), "firstsecond");
}
