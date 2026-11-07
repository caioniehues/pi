//! Tool-call argument streaming end to end (`streaming_json.rs` through a provider): ports the faux path of
//! `streaming-tool-arguments.test.ts` (#9265). The anthropic, openai-completions, mistral, Responses and pi-messages
//! paths come with their wire APIs (C3b, C13a, C14).

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
#![cfg(feature = "faux")]

mod common;

use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::options::SimpleStreamOptions;
use pi_ai::providers::faux::{
	FauxMessageOptions, FauxProviderOptions, faux_assistant_message, faux_provider, faux_tool_call,
};
use pi_ai::transcript::{Context, normalize_context};
use pi_json::{JsonObject, JsonValue};
use pi_types::{
	AssistantContentBlock, AssistantMessage, AssistantMessageEvent, KnownAssistantMessageEvent, StopReason,
};
use serde_json::json;

fn write_call(size: usize) -> JsonObject {
	let content: String = (0..20_000)
		.map(|line| format!("const line{line} = \"value {line}\"; // \\ done\n"))
		.collect();
	common::object(json!({ "path": "/src/file.ts", "content": &content[..size] }))
}

fn faux_stream(arguments: JsonObject) -> AssistantMessageEventStream {
	let faux = faux_provider(&FauxProviderOptions {
		token_size_min: Some(8),
		token_size_max: Some(8),
		..FauxProviderOptions::default()
	});
	let message = faux_assistant_message(
		vec![faux_tool_call("write", arguments, Some("call_1"))],
		FauxMessageOptions {
			stop_reason: Some(StopReason::ToolUse),
			..FauxMessageOptions::default()
		},
	);
	faux.set_responses(vec![message.into()]);
	let context = normalize_context(Context {
		messages: vec![common::user("write", 0)],
		..Context::default()
	});
	faux.provider
		.stream_simple(faux.model(), context, SimpleStreamOptions::default())
}

/// Own keys a block may carry; anything else is stream state leaking into partial messages.
fn expect_no_stream_state(message: &AssistantMessage) {
	for block in common::to_json(&message.content).as_array().unwrap() {
		let allowed: &[&str] = match block["type"].as_str().unwrap() {
			"text" => &["type", "text", "textSignature"],
			"thinking" => &["type", "thinking", "thinkingSignature", "redacted"],
			_ => &[
				"type",
				"id",
				"name",
				"arguments",
				"argumentsError",
				"thoughtSignature",
				"namespace",
			],
		};
		for key in block.as_object().unwrap().keys() {
			assert!(allowed.contains(&key.as_str()), "{key}");
		}
	}
}

// TS: streaming-tool-arguments.test.ts
#[tokio::test]
async fn shows_growing_partial_arguments_keeps_stream_state_off_every_partial_and_finishes_with_the_exact_arguments() {
	let arguments = write_call(5_000);
	let expected = arguments["content"].as_str().unwrap().to_owned();
	let stream = faux_stream(arguments.clone());
	let (mut shown, mut partials) = (0, 0);
	while let Some(event) = stream.next().await {
		let AssistantMessageEvent::Known(event) = event else {
			continue;
		};
		match &event {
			KnownAssistantMessageEvent::Done(done) => expect_no_stream_state(&done.message),
			KnownAssistantMessageEvent::ToolCallDelta(delta) => {
				expect_no_stream_state(&delta.partial);
				let Some(AssistantContentBlock::ToolCall(call)) =
					delta.partial.content.get(usize::try_from(delta.content_index).unwrap())
				else {
					continue;
				};
				let Some(content) = call.arguments.get("content").and_then(JsonValue::as_str) else {
					continue;
				};
				partials += 1;
				assert!(expected.starts_with(content));
				assert!(content.len() >= shown);
				shown = content.len();
			}
			_ => {}
		}
	}
	let message = stream.result().await;
	assert_eq!(message.stop_reason, StopReason::ToolUse);
	assert!(partials > 100, "{partials} partials");
	let AssistantContentBlock::ToolCall(call) = &message.content[0] else {
		panic!("tool call")
	};
	assert_eq!((call.name.as_str(), &call.arguments), ("write", &arguments));
}

async fn consume(arguments: JsonObject) -> AssistantMessage {
	let stream = faux_stream(arguments);
	while stream.next().await.is_some() {}
	stream.result().await
}

// TS: streaming-tool-arguments.test.ts
#[test]
fn allocates_linearly_in_the_argument_text() {
	let run = |size: usize| {
		let arguments = write_call(size);
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.unwrap();
		allocation_counter::measure(|| {
			runtime.block_on(consume(arguments));
		})
		.bytes_total
	};
	let (small, large) = (run(25_000), run(250_000));
	// Ten times the text: linear growth is about 10x, re-parsing the accumulated text about 100x.
	assert!(large / small < 25, "{large} / {small}");
}

// TS: streaming-tool-arguments.test.ts
#[test]
fn releases_the_stream_its_partial_messages_and_argument_snapshots_once_the_caller_drops_them() {
	let arguments = write_call(2_000);
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap();
	let info = allocation_counter::measure(|| {
		runtime.block_on(async {
			consume(arguments).await;
			// Let the spawned producer task finish and drop its clones.
			tokio::task::yield_now().await;
		});
	});
	drop(runtime);
	assert!(
		info.bytes_current < 64 * 1024,
		"{} bytes still held",
		info.bytes_current
	);
}
