//! The faux provider (`providers/faux.rs`): ports `faux-provider.test.ts` through a `Models` registry (the TS test
//! uses the legacy global registry of `compat.ts`).

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

use futures_util::FutureExt;
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::models::Models;
use pi_ai::options::{SimpleStreamOptions, StreamOptions};
use pi_ai::providers::faux::{
	FauxMessageOptions, FauxModelDefinition, FauxProviderOptions, FauxProviderRegistration, FauxResponseFactory,
	FauxResponseStep, faux_assistant_message, faux_text, faux_text_message, faux_thinking, faux_tool_call,
	register_faux_provider,
};
use pi_ai::transcript::Context;
use pi_types::{
	AssistantMessage, AssistantMessageEvent, CacheRetention, KnownAssistantMessageEvent, Message, StopReason, Tool,
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn register(options: FauxProviderOptions) -> (Models, FauxProviderRegistration) {
	let models = Models::default();
	let registration = register_faux_provider(&models, &options);
	(models, registration)
}

fn hi() -> Context {
	Context {
		messages: vec![common::user("hi", 1)],
		..Context::default()
	}
}

fn event_type(event: &AssistantMessageEvent) -> &'static str {
	match event {
		AssistantMessageEvent::Known(known) => match known {
			KnownAssistantMessageEvent::Start(_) => "start",
			KnownAssistantMessageEvent::TextStart(_) => "text_start",
			KnownAssistantMessageEvent::TextDelta(_) => "text_delta",
			KnownAssistantMessageEvent::TextEnd(_) => "text_end",
			KnownAssistantMessageEvent::ThinkingStart(_) => "thinking_start",
			KnownAssistantMessageEvent::ThinkingDelta(_) => "thinking_delta",
			KnownAssistantMessageEvent::ThinkingEnd(_) => "thinking_end",
			KnownAssistantMessageEvent::ToolCallStart(_) => "toolcall_start",
			KnownAssistantMessageEvent::ToolCallDelta(_) => "toolcall_delta",
			KnownAssistantMessageEvent::ToolCallEnd(_) => "toolcall_end",
			KnownAssistantMessageEvent::Done(_) => "done",
			KnownAssistantMessageEvent::Error(_) => "error",
		},
		AssistantMessageEvent::Other(_) => "other",
	}
}

async fn collect(stream: AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
	let mut events = Vec::new();
	while let Some(event) = stream.next().await {
		events.push(event);
	}
	events
}

fn terminal_error(event: &AssistantMessageEvent) -> &pi_types::StreamError {
	match event {
		AssistantMessageEvent::Known(KnownAssistantMessageEvent::Error(error)) => error,
		_ => panic!("expected an error event"),
	}
}

fn session(id: &str, retention: CacheRetention) -> StreamOptions {
	StreamOptions {
		session_id: Some(id.into()),
		cache_retention: Some(retention),
		..StreamOptions::default()
	}
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn registers_a_custom_provider_and_estimates_usage() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![faux_text_message("hello world").into()]);
	let context = Context {
		system_prompt: Some("Be concise.".into()),
		messages: vec![common::user("hi there", 1)],
		tools: None,
	};
	let response = models
		.complete(faux.handle.model(), context, StreamOptions::default())
		.await;
	assert_eq!(
		common::to_json(&response.content),
		json!([{ "type": "text", "text": "hello world" }])
	);
	assert!(response.usage.input > 0 && response.usage.output > 0);
	assert_eq!(
		response.usage.total_tokens,
		Some(response.usage.input + response.usage.output)
	);
	assert_eq!(faux.handle.state().call_count, 1);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_helper_blocks_for_text_thinking_and_tool_calls() {
	let (models, faux) = register(FauxProviderOptions::default());
	let content = vec![
		faux_thinking("think"),
		faux_tool_call("echo", common::object(json!({ "text": "hi" })), None),
		faux_text("done"),
	];
	faux.handle.set_responses(vec![
		faux_assistant_message(
			content,
			FauxMessageOptions {
				stop_reason: Some(StopReason::ToolUse),
				..FauxMessageOptions::default()
			},
		)
		.into(),
	]);
	let response = models
		.complete(faux.handle.model(), hi(), StreamOptions::default())
		.await;
	let content = common::to_json(&response.content);
	assert_eq!(content[0], json!({ "type": "thinking", "thinking": "think" }));
	assert_eq!(
		(
			content[1]["type"].clone(),
			content[1]["name"].clone(),
			content[1]["arguments"].clone()
		),
		(json!("toolCall"), json!("echo"), json!({ "text": "hi" }))
	);
	assert!(content[1]["id"].is_string());
	assert_eq!(content[2], json!({ "type": "text", "text": "done" }));
	assert_eq!(response.stop_reason, StopReason::ToolUse);
}

fn model_echo() -> FauxResponseStep {
	let factory: FauxResponseFactory = Arc::new(|_, _, _, model| {
		let text = format!("{}:{}", model.id, model.reasoning);
		async move { Ok(faux_text_message(&text)) }.boxed()
	});
	FauxResponseStep::Factory(factory)
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_multiple_models_with_per_model_reasoning_and_model_aware_factories() {
	let definitions = vec![
		FauxModelDefinition {
			id: "faux-fast".into(),
			name: Some("Faux Fast".into()),
			reasoning: Some(false),
			..FauxModelDefinition::default()
		},
		FauxModelDefinition {
			id: "faux-thinker".into(),
			name: Some("Faux Thinker".into()),
			reasoning: Some(true),
			..FauxModelDefinition::default()
		},
	];
	let (models, faux) = register(FauxProviderOptions {
		models: definitions,
		..FauxProviderOptions::default()
	});
	faux.handle.set_responses(vec![model_echo(), model_echo()]);
	assert_eq!(
		faux.handle
			.models
			.iter()
			.map(|model| model.id.as_str())
			.collect::<Vec<_>>(),
		["faux-fast", "faux-thinker"]
	);
	assert_eq!(faux.handle.model(), &faux.handle.models[0]);
	assert_eq!(
		faux.handle.model_by_id("faux-fast").map(|model| model.reasoning),
		Some(false)
	);
	assert_eq!(
		faux.handle.model_by_id("faux-thinker").map(|model| model.reasoning),
		Some(true)
	);
	let fast = models
		.complete(
			faux.handle.model_by_id("faux-fast").unwrap(),
			hi(),
			StreamOptions::default(),
		)
		.await;
	let thinker = models
		.complete(
			faux.handle.model_by_id("faux-thinker").unwrap(),
			hi(),
			StreamOptions::default(),
		)
		.await;
	assert_eq!(
		common::to_json(&fast.content),
		json!([{ "type": "text", "text": "faux-fast:false" }])
	);
	assert_eq!(
		common::to_json(&thinker.content),
		json!([{ "type": "text", "text": "faux-thinker:true" }])
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn rewrites_api_provider_and_model_on_returned_messages() {
	let options = FauxProviderOptions {
		api: Some("faux:test".into()),
		provider: Some("faux-provider".into()),
		models: vec![FauxModelDefinition {
			id: "faux-model".into(),
			..FauxModelDefinition::default()
		}],
		..FauxProviderOptions::default()
	};
	let (models, faux) = register(options);
	faux.handle.set_responses(vec![faux_text_message("hello").into()]);
	let response = models
		.complete(faux.handle.model(), hi(), StreamOptions::default())
		.await;
	assert_eq!(
		(
			response.api.as_str(),
			response.provider.as_str(),
			response.model.as_str()
		),
		("faux:test", "faux-provider", "faux-model")
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn consumes_queued_responses_in_order_and_errors_when_exhausted() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![
		faux_text_message("first").into(),
		faux_text_message("second").into(),
	]);
	let model = faux.handle.model();
	let first = models.complete(model, hi(), StreamOptions::default()).await;
	let second = models.complete(model, hi(), StreamOptions::default()).await;
	let exhausted = models.complete(model, hi(), StreamOptions::default()).await;
	assert_eq!(
		common::to_json(&first.content),
		json!([{ "type": "text", "text": "first" }])
	);
	assert_eq!(
		common::to_json(&second.content),
		json!([{ "type": "text", "text": "second" }])
	);
	assert_eq!(
		(exhausted.stop_reason, exhausted.error_message.as_deref()),
		(StopReason::Error, Some("No more faux responses queued"))
	);
	assert_eq!(faux.handle.pending_response_count(), 0);
	assert_eq!(faux.handle.state().call_count, 3);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn can_replace_and_append_queued_responses() {
	let (models, faux) = register(FauxProviderOptions::default());
	let text = |message: AssistantMessage| common::to_json(&message.content)[0]["text"].clone();
	let model = faux.handle.model();
	faux.handle.set_responses(vec![faux_text_message("first").into()]);
	assert_eq!(
		text(models.complete(model, hi(), StreamOptions::default()).await),
		"first"
	);
	assert_eq!(faux.handle.pending_response_count(), 0);
	faux.handle.set_responses(vec![faux_text_message("second").into()]);
	assert_eq!(faux.handle.pending_response_count(), 1);
	assert_eq!(
		text(models.complete(model, hi(), StreamOptions::default()).await),
		"second"
	);
	faux.handle.append_responses(vec![
		faux_text_message("third").into(),
		faux_text_message("fourth").into(),
	]);
	assert_eq!(faux.handle.pending_response_count(), 2);
	assert_eq!(
		text(models.complete(model, hi(), StreamOptions::default()).await),
		"third"
	);
	assert_eq!(
		text(models.complete(model, hi(), StreamOptions::default()).await),
		"fourth"
	);
	assert_eq!(faux.handle.pending_response_count(), 0);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_async_response_factories() {
	let (models, faux) = register(FauxProviderOptions::default());
	let factory: FauxResponseFactory = Arc::new(|context, _, state, _| {
		let text = format!("{}:{}", context.messages().len(), state.call_count);
		async move {
			tokio::task::yield_now().await;
			Ok(faux_text_message(&text))
		}
		.boxed()
	});
	faux.handle.set_responses(vec![FauxResponseStep::Factory(factory)]);
	let response = models
		.complete(faux.handle.model(), hi(), StreamOptions::default())
		.await;
	assert_eq!(
		common::to_json(&response.content),
		json!([{ "type": "text", "text": "1:1" }])
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn emits_an_error_when_a_response_factory_throws() {
	let (models, faux) = register(FauxProviderOptions::default());
	let factory: FauxResponseFactory = Arc::new(|_, _, _, _| async { Err("boom".to_owned()) }.boxed());
	faux.handle.set_responses(vec![FauxResponseStep::Factory(factory)]);
	let events = collect(models.stream(faux.handle.model(), hi(), StreamOptions::default())).await;
	assert_eq!(events.len(), 1);
	let error = terminal_error(&events[0]);
	assert_eq!(
		(error.error.stop_reason, error.error.error_message.as_deref()),
		(StopReason::Error, Some("boom"))
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn rejects_a_queued_response_without_a_terminal_stop_reason() {
	let (models, faux) = register(FauxProviderOptions::default());
	let pending = faux_assistant_message(
		vec![faux_text("partial")],
		FauxMessageOptions {
			stop_reason: Some(StopReason::Pending),
			..FauxMessageOptions::default()
		},
	);
	faux.handle.set_responses(vec![pending.into()]);
	let events = collect(models.stream(faux.handle.model(), hi(), StreamOptions::default())).await;
	assert!(!events.iter().any(|event| event_type(event) == "done"));
	let error = terminal_error(events.last().unwrap());
	assert_eq!(
		(error.error.stop_reason, error.error.error_message.as_deref()),
		(StopReason::Error, Some("Faux response ended without a stop reason"))
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn estimates_prompt_and_output_tokens_from_serialized_context() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![faux_text_message("done").into()]);
	let tool: Tool = common::from_json(
		json!({ "name": "echo", "description": "Echo back text", "parameters": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } }),
	);
	let messages: Vec<Message> = vec![
		common::from_json(
			json!({ "role": "user", "content": [{ "type": "text", "text": "hello" }, { "type": "image", "mimeType": "image/png", "data": "abcd" }], "timestamp": 1 }),
		),
		Message::Assistant(faux_text_message("prior")),
		common::from_json(
			json!({ "role": "toolResult", "toolCallId": "tool-1", "toolName": "echo", "content": [{ "type": "text", "text": "tool out" }], "isError": false, "timestamp": 2 }),
		),
	];
	let context = Context {
		system_prompt: Some("sys".into()),
		messages,
		tools: Some(vec![tool.clone()]),
	};
	let response = models
		.complete(faux.handle.model(), context, StreamOptions::default())
		.await;
	// The tools live in the leading system message (`tool+:` lines) since normalization.
	let prompt = [
		format!("system:sys\ntool+:{}", pi_json::to_js_string(&common::to_json(&tool))),
		"user:hello\n[image:image/png:4]".to_owned(),
		"assistant:prior".to_owned(),
		"toolResult:echo\ntool out".to_owned(),
	]
	.join("\n\n");
	let expected_prompt = i64::try_from(prompt.encode_utf16().count().div_ceil(4)).unwrap();
	assert_eq!((response.usage.input, response.usage.output), (expected_prompt, 1));
	assert_eq!((response.usage.cache_read, response.usage.cache_write), (0, 0));
	assert_eq!(response.usage.total_tokens, Some(expected_prompt + 1));
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn does_not_share_cache_across_sessions_or_requests_without_session_id() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![
		faux_text_message("first").into(),
		faux_text_message("second").into(),
		faux_text_message("third").into(),
	]);
	let model = faux.handle.model();
	let mut context = Context {
		messages: vec![common::user("hello", 1)],
		..Context::default()
	};
	let first = models
		.complete(model, context.clone(), session("session-1", CacheRetention::Short))
		.await;
	assert!(first.usage.cache_write > 0);
	context.messages.push(Message::Assistant(first));
	context.messages.push(common::user("follow up", 2));
	let second = models
		.complete(model, context.clone(), session("session-2", CacheRetention::Short))
		.await;
	assert_eq!(second.usage.cache_read, 0);
	assert!(second.usage.cache_write > 0);
	let third = models.complete(model, context, StreamOptions::default()).await;
	assert_eq!((third.usage.cache_read, third.usage.cache_write), (0, 0));
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn simulates_prompt_caching_per_session_id() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![
		faux_text_message("first").into(),
		faux_text_message("second").into(),
	]);
	let model = faux.handle.model();
	let mut context = Context {
		system_prompt: Some("Be concise.".into()),
		messages: vec![common::user("hello", 1)],
		tools: None,
	};
	let first = models
		.complete(model, context.clone(), session("session-1", CacheRetention::Short))
		.await;
	assert_eq!(first.usage.cache_read, 0);
	assert!(first.usage.cache_write > 0);
	context.messages.push(Message::Assistant(first));
	context.messages.push(common::user("follow up", 2));
	let second = models
		.complete(model, context, session("session-1", CacheRetention::Short))
		.await;
	assert!(second.usage.cache_read > 0);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn counts_cached_characters_up_to_the_first_difference_in_the_joined_prompt() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![
		faux_text_message("a").into(),
		faux_text_message("b").into(),
		faux_text_message("c").into(),
	]);
	let model = faux.handle.model();
	let ask = |texts: &[&str]| Context {
		messages: texts.iter().map(|text| common::user(text, 1)).collect(),
		..Context::default()
	};
	let usage = |message: AssistantMessage| (message.usage.input, message.usage.cache_read, message.usage.cache_write);
	// Prompt texts: "user:hello world" (16 characters), then 16 + 2 + "user:next" (9) = 27.
	assert_eq!(
		usage(
			models
				.complete(
					model,
					ask(&["hello world"]),
					session("session-1", CacheRetention::Short)
				)
				.await
		),
		(0, 0, 4)
	);
	assert_eq!(
		usage(
			models
				.complete(
					model,
					ask(&["hello world", "next"]),
					session("session-1", CacheRetention::Short)
				)
				.await
		),
		(0, 4, 3)
	);
	// The first message now differs after "user:hello w" (12 characters): "user:hello wide" + 2 + 9 = 26.
	assert_eq!(
		usage(
			models
				.complete(
					model,
					ask(&["hello wide", "next"]),
					session("session-1", CacheRetention::Short)
				)
				.await
		),
		(0, 3, 4)
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn does_not_simulate_caching_when_cache_retention_is_none() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![
		faux_text_message("first").into(),
		faux_text_message("second").into(),
	]);
	let model = faux.handle.model();
	let mut context = Context {
		messages: vec![common::user("hello", 1)],
		..Context::default()
	};
	models
		.complete(model, context.clone(), session("session-1", CacheRetention::None))
		.await;
	context.messages.push(Message::Assistant(faux_text_message("first")));
	context.messages.push(common::user("follow up", 2));
	let second = models
		.complete(model, context, session("session-1", CacheRetention::None))
		.await;
	assert_eq!((second.usage.cache_read, second.usage.cache_write), (0, 0));
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn streams_thinking_text_and_partial_tool_call_deltas() {
	let (models, faux) = register(FauxProviderOptions::default());
	let content = vec![
		faux_thinking("thinking text"),
		faux_text("answer text"),
		faux_tool_call(
			"echo",
			common::object(json!({ "text": "hi", "count": 12 })),
			Some("tool-1"),
		),
	];
	faux.handle.set_responses(vec![
		faux_assistant_message(
			content,
			FauxMessageOptions {
				stop_reason: Some(StopReason::ToolUse),
				..FauxMessageOptions::default()
			},
		)
		.into(),
	]);
	let events = collect(models.stream(faux.handle.model(), hi(), StreamOptions::default())).await;
	let types: Vec<&str> = events.iter().map(event_type).collect();
	for expected in [
		"thinking_start",
		"thinking_delta",
		"text_start",
		"text_delta",
		"toolcall_start",
		"toolcall_delta",
		"toolcall_end",
	] {
		assert!(types.contains(&expected), "{expected}");
	}
	let deltas: Vec<String> = events
		.iter()
		.filter_map(|event| match event {
			AssistantMessageEvent::Known(KnownAssistantMessageEvent::ToolCallDelta(delta)) => Some(delta.delta.clone()),
			_ => None,
		})
		.collect();
	assert!(deltas.len() > 1);
	assert_eq!(
		pi_json::parse(&deltas.concat()).unwrap(),
		json!({ "text": "hi", "count": 12 })
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn streams_an_exact_event_order_for_fixed_size_chunks() {
	let (models, faux) = register(FauxProviderOptions {
		token_size_min: Some(1),
		token_size_max: Some(1),
		..FauxProviderOptions::default()
	});
	let content = vec![
		faux_thinking("go"),
		faux_text("ok"),
		faux_tool_call("echo", common::object(json!({})), Some("tool-1")),
	];
	faux.handle.set_responses(vec![
		faux_assistant_message(
			content,
			FauxMessageOptions {
				stop_reason: Some(StopReason::ToolUse),
				..FauxMessageOptions::default()
			},
		)
		.into(),
	]);
	let events = collect(models.stream(faux.handle.model(), hi(), StreamOptions::default())).await;
	let AssistantMessageEvent::Known(KnownAssistantMessageEvent::Start(start)) = &events[0] else {
		panic!("start")
	};
	assert_eq!(start.partial.stop_reason, StopReason::Pending);
	assert_eq!(
		events.iter().map(event_type).collect::<Vec<_>>(),
		[
			"start",
			"thinking_start",
			"thinking_delta",
			"thinking_end",
			"text_start",
			"text_delta",
			"text_end",
			"toolcall_start",
			"toolcall_delta",
			"toolcall_end",
			"done"
		]
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn streams_multiple_tool_calls_in_one_message() {
	let (models, faux) = register(FauxProviderOptions::default());
	let content = vec![
		faux_tool_call("echo", common::object(json!({ "text": "one" })), Some("tool-1")),
		faux_tool_call("echo", common::object(json!({ "text": "two" })), Some("tool-2")),
	];
	faux.handle.set_responses(vec![
		faux_assistant_message(
			content,
			FauxMessageOptions {
				stop_reason: Some(StopReason::ToolUse),
				..FauxMessageOptions::default()
			},
		)
		.into(),
	]);
	let events = collect(models.stream(faux.handle.model(), hi(), StreamOptions::default())).await;
	let types: Vec<&str> = events.iter().map(event_type).collect();
	assert_eq!(types.iter().filter(|kind| **kind == "toolcall_start").count(), 2);
	assert_eq!(types.iter().filter(|kind| **kind == "toolcall_end").count(), 2);
}

async fn explicit_terminal(stop_reason: StopReason, error: &str) -> Vec<AssistantMessageEvent> {
	let (models, faux) = register(FauxProviderOptions {
		token_size_min: Some(2),
		token_size_max: Some(2),
		..FauxProviderOptions::default()
	});
	let message = faux_assistant_message(
		vec![faux_text("partial")],
		FauxMessageOptions {
			stop_reason: Some(stop_reason),
			error_message: Some(error.into()),
			..FauxMessageOptions::default()
		},
	);
	faux.handle.set_responses(vec![message.into()]);
	collect(models.stream(faux.handle.model(), hi(), StreamOptions::default())).await
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn streams_an_explicit_assistant_error_message_as_a_terminal_error() {
	let events = explicit_terminal(StopReason::Error, "upstream failed").await;
	assert_eq!(
		events.iter().map(event_type).collect::<Vec<_>>(),
		["start", "text_start", "text_delta", "text_end", "error"]
	);
	let error = terminal_error(events.last().unwrap());
	assert_eq!(
		(
			error.reason,
			error.error.stop_reason,
			error.error.error_message.as_deref()
		),
		(pi_types::ErrorReason::Error, StopReason::Error, Some("upstream failed"))
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn streams_an_explicit_assistant_aborted_message_as_a_terminal_error() {
	let events = explicit_terminal(StopReason::Aborted, "Request was aborted").await;
	assert_eq!(
		events.iter().map(event_type).collect::<Vec<_>>(),
		["start", "text_start", "text_delta", "text_end", "error"]
	);
	let error = terminal_error(events.last().unwrap());
	assert_eq!(
		(
			error.reason,
			error.error.stop_reason,
			error.error.error_message.as_deref()
		),
		(
			pi_types::ErrorReason::Aborted,
			StopReason::Aborted,
			Some("Request was aborted")
		)
	);
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_aborting_before_the_first_chunk() {
	let (models, faux) = register(FauxProviderOptions {
		tokens_per_second: Some(50.0),
		token_size_min: Some(3),
		token_size_max: Some(3),
		..FauxProviderOptions::default()
	});
	faux.handle
		.set_responses(vec![faux_text_message("abcdefghijklmnopqrstuvwxyz").into()]);
	let cancel = CancellationToken::new();
	cancel.cancel();
	let mut options = StreamOptions::default();
	options.request.cancel = Some(cancel);
	let events = collect(models.stream(faux.handle.model(), hi(), options)).await;
	assert_eq!(events.len(), 1);
	let error = terminal_error(&events[0]);
	assert_eq!(
		(error.reason, error.error.stop_reason),
		(pi_types::ErrorReason::Aborted, StopReason::Aborted)
	);
}

/// Streams `message` paced and aborts at the first delta of `kind`; the event types seen.
async fn abort_at_first(kind: &str, message: AssistantMessage) -> (Vec<&'static str>, usize) {
	let (models, faux) = register(FauxProviderOptions {
		tokens_per_second: Some(100.0),
		token_size_min: Some(3),
		token_size_max: Some(3),
		..FauxProviderOptions::default()
	});
	faux.handle.set_responses(vec![message.into()]);
	let cancel = CancellationToken::new();
	let mut options = SimpleStreamOptions::default();
	options.stream.request.cancel = Some(cancel.clone());
	let stream = models.stream_simple(faux.handle.model(), hi(), options);
	let (mut types, mut deltas) = (Vec::new(), 0);
	while let Some(event) = stream.next().await {
		let event_kind = event_type(&event);
		types.push(event_kind);
		if event_kind == kind {
			deltas += 1;
			cancel.cancel();
		}
	}
	(types, deltas)
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_aborting_mid_text_stream_when_paced() {
	let (types, deltas) = abort_at_first("text_delta", faux_text_message("abcdefghijklmnopqrstuvwxyz")).await;
	assert_eq!(deltas, 1);
	assert!(types.contains(&"text_start") && types.contains(&"error") && !types.contains(&"text_end"));
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_aborting_mid_thinking_stream_when_paced() {
	let message = faux_assistant_message(
		vec![faux_thinking("abcdefghijklmnopqrstuvwxyz")],
		FauxMessageOptions::default(),
	);
	let (types, deltas) = abort_at_first("thinking_delta", message).await;
	assert_eq!(deltas, 1);
	assert!(types.contains(&"thinking_start") && types.contains(&"error") && !types.contains(&"thinking_end"));
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn supports_aborting_mid_toolcall_stream_when_paced() {
	let call = faux_tool_call(
		"echo",
		common::object(json!({ "text": "abcdefghijklmnopqrstuvwxyz", "count": 123_456_789 })),
		Some("tool-1"),
	);
	let message = faux_assistant_message(
		vec![call],
		FauxMessageOptions {
			stop_reason: Some(StopReason::ToolUse),
			..FauxMessageOptions::default()
		},
	);
	let (types, deltas) = abort_at_first("toolcall_delta", message).await;
	assert_eq!(deltas, 1);
	assert!(types.contains(&"toolcall_start") && types.contains(&"error") && !types.contains(&"toolcall_end"));
}

// TS: faux-provider.test.ts
#[tokio::test]
async fn unregisters_the_provider() {
	let (models, faux) = register(FauxProviderOptions::default());
	faux.handle.set_responses(vec![faux_text_message("hello").into()]);
	let model = faux.handle.model().clone();
	faux.unregister();
	let response = models.complete(&model, hi(), StreamOptions::default()).await;
	assert_eq!(
		(response.stop_reason, response.error_message.as_deref()),
		(StopReason::Error, Some("Unknown provider: faux"))
	);
}
