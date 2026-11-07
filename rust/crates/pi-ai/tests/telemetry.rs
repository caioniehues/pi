//! AI telemetry (`telemetry.rs`, `lazy_stream.rs`): ports `telemetry.test.ts` and `telemetry-options.test.ts`.
//! Not ported: "records low-cardinality metrics with monotonic durations" mocks `performance.now`, which has no
//! injection point here (durations are checked to be recorded), and "creates instruments once per metrics object"
//! checks TS's `WeakMap` instrument cache, which the port does not keep (see `telemetry.rs`).

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

use async_trait::async_trait;
use pi_ai::auth::types::ProviderAuth;
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::lazy_stream::{LazyStreamObserver, lazy_stream};
use pi_ai::model_operations::AnyModel;
use pi_ai::models::Models;
use pi_ai::options::{DeferredFetchOptions, SimpleStreamOptions, StreamOptions};
use pi_ai::provider::{CreateProviderOptions, DeferredSupport, ProviderApi, ProviderStreams, create_provider};
use pi_ai::telemetry::{AI_TELEMETRY_SCHEMA, RequestKind, observe_model_request};
use pi_ai::transcript::{Context, TranscriptContext};
use pi_telemetry::{
	AttributeValue, InMemoryTelemetryContext, InstrumentKind, RecordedTelemetrySpan, SpanOptions, SpanStatus,
	TelemetryContext, TelemetryMetrics, TelemetryPropagation, TelemetrySpan, validate_against_schema,
};
use pi_types::{AssistantMessage, AssistantMessageEvent, DeferredHandle, Model, StopReason, ThinkingLevel};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn model() -> Model {
	common::model(
		"telemetry-test",
		"test",
		"model",
		json!({ "reasoning": true, "cost": { "input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4 }, "contextWindow": 1000, "maxTokens": 100 }),
	)
}

fn handle() -> DeferredHandle {
	common::from_json(
		json!({ "provider": "test", "modelId": "model", "api": "telemetry-test", "id": "deferred-secret" }),
	)
}

fn message(stop: &str) -> AssistantMessage {
	common::assistant(json!({
		"api": "telemetry-test", "provider": "test", "model": "model", "stopReason": stop, "responseId": "response", "timestamp": pi_ai::diagnostics::now_ms(),
		"content": [
			{ "type": "text", "text": "text-secret" },
			{ "type": "thinking", "thinking": "thinking-secret" },
			{ "type": "toolCall", "id": "call-secret", "name": "lookup", "arguments": { "value": "argument-secret" } }
		],
		"usage": { "input": 11, "output": 22, "cacheRead": 33, "cacheWrite": 44, "totalTokens": 110, "cost": { "input": 0.1, "output": 0.2, "cacheRead": 0.3, "cacheWrite": 0.4, "total": 1 } }
	}))
}

fn events(result: &AssistantMessage) -> Vec<AssistantMessageEvent> {
	let partial = common::to_json(result);
	let tool_call = partial["content"][2].clone();
	let terminal = match result.stop_reason {
		StopReason::Error | StopReason::Aborted => {
			json!({ "type": "error", "reason": partial["stopReason"], "error": partial })
		}
		StopReason::Pending => json!({ "type": "done", "reason": "deferred", "message": partial }),
		_ => json!({ "type": "done", "reason": partial["stopReason"], "message": partial }),
	};
	common::from_json(json!([
		{ "type": "start", "partial": partial },
		{ "type": "text_delta", "contentIndex": 0, "delta": "text-secret", "partial": partial },
		{ "type": "text_end", "contentIndex": 0, "content": "text-secret", "partial": partial },
		{ "type": "thinking_end", "contentIndex": 1, "content": "thinking-secret", "partial": partial },
		{ "type": "toolcall_delta", "contentIndex": 2, "delta": "{\"value\":", "partial": partial },
		{ "type": "toolcall_delta", "contentIndex": 2, "delta": "\"argument-secret\"}", "partial": partial },
		{ "type": "toolcall_end", "contentIndex": 2, "toolCall": tool_call, "partial": partial },
		terminal
	]))
}

/// Replays fixed events for every operation.
struct Replay {
	result: AssistantMessage,
}

impl Replay {
	fn respond(&self) -> AssistantMessageEventStream {
		let stream = AssistantMessageEventStream::new();
		for event in events(&self.result) {
			stream.push(event);
		}
		stream.end(Some(self.result.clone()));
		stream
	}
}

#[async_trait]
impl ProviderStreams for Replay {
	fn deferred(&self) -> DeferredSupport {
		DeferredSupport {
			fetch: true,
			cancel: false,
		}
	}
	fn stream(
		&self,
		_model: &Model,
		_context: TranscriptContext,
		_options: StreamOptions,
	) -> AssistantMessageEventStream {
		self.respond()
	}
	fn stream_simple(
		&self,
		_model: &Model,
		_context: TranscriptContext,
		_options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		self.respond()
	}
	fn fetch_deferred(
		&self,
		_model: &Model,
		_handle: &DeferredHandle,
		_options: DeferredFetchOptions,
	) -> AssistantMessageEventStream {
		self.respond()
	}
}

fn setup(result: AssistantMessage) -> Models {
	let models = Models::default();
	models.set_provider(
		create_provider(CreateProviderOptions {
			id: "test".into(),
			auth: ProviderAuth {
				api_key: Some(pi_ai::auth::helpers::env_api_key_auth("Test", &[])),
				oauth: None,
			},
			models: vec![AnyModel::Chat(model())],
			api: Some(ProviderApi::Single(Arc::new(Replay { result }))),
			..CreateProviderOptions::default()
		})
		.unwrap(),
	);
	models
}

/// Runs `request` under a `caller` span and waits until every span settled; the request span.
async fn traced<F, Fut, T>(
	telemetry: &InMemoryTelemetryContext,
	request: F,
) -> (T, RecordedTelemetrySpan, RecordedTelemetrySpan)
where
	F: FnOnce(Arc<dyn TelemetryContext>) -> Fut,
	Fut: std::future::Future<Output = T>,
{
	let parent = telemetry.start_span(SpanOptions::new("caller"));
	let value = request(parent.handle()).await;
	parent.end();
	for _ in 0..1000 {
		if telemetry.get_spans().iter().all(|span| span.settled) {
			break;
		}
		tokio::task::yield_now().await;
	}
	let spans = telemetry.get_spans();
	(value, spans[0].clone(), spans[1].clone())
}

fn with_key(telemetry: Arc<dyn TelemetryContext>) -> SimpleStreamOptions {
	let mut options = SimpleStreamOptions::default();
	options.stream.request.api_key = Some("key".into());
	options.stream.request.telemetry = Some(telemetry);
	options
}

fn attribute(span: &RecordedTelemetrySpan, name: &str) -> Option<AttributeValue> {
	span.attributes.get(name).cloned()
}

fn text(value: &str) -> AttributeValue {
	AttributeValue::String(value.into())
}

fn events_of(span: &RecordedTelemetrySpan) -> Vec<(String, Vec<(String, AttributeValue)>)> {
	span.events
		.iter()
		.map(|event| {
			(
				event.name.clone(),
				event
					.attributes
					.iter()
					.map(|(name, value)| (name.to_owned(), value.clone()))
					.collect(),
			)
		})
		.collect()
}

fn block(kind: &str, index: f64, chars: f64, tool: Option<&str>) -> (String, Vec<(String, AttributeValue)>) {
	let mut attributes = vec![
		("type".to_owned(), text(kind)),
		("index".to_owned(), AttributeValue::Number(index)),
		("chars".to_owned(), AttributeValue::Number(chars)),
	];
	if let Some(tool) = tool {
		attributes.push(("tool".to_owned(), text(tool)));
	}
	("pi.ai.block".into(), attributes)
}

async fn drain(stream: &AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
	let mut received = Vec::new();
	while let Some(event) = stream.next().await {
		received.push(event);
	}
	received
}

// TS: telemetry.test.ts
#[tokio::test]
async fn observes_stream_stream_simple_and_stream_deferred_without_changing_delivered_events_or_results() {
	for method in ["stream", "streamSimple", "streamDeferred"] {
		let telemetry = InMemoryTelemetryContext::new();
		let result = message("stop");
		let models = setup(result.clone());
		let (received, parent, span) = traced(&telemetry, |parent| async {
			let mut options = with_key(parent);
			options.reasoning = Some(ThinkingLevel::High);
			let stream = match method {
				"stream" => models.stream(&model(), Context::default(), options.stream),
				"streamSimple" => models.stream_simple(&model(), Context::default(), options),
				_ => models.stream_deferred(
					&model(),
					&handle(),
					DeferredFetchOptions {
						request: options.stream.request,
						wait: None,
					},
				),
			};
			let received = drain(&stream).await;
			let mut final_message = stream.result().await;
			final_message.duration_ms = None;
			assert_eq!(final_message, result);
			received
		})
		.await;
		let mut expected = events(&result);
		// The final message of a response the stream saw start gets its duration; nothing else changes.
		for (index, event) in received.iter().enumerate() {
			let mut event = common::to_json(event);
			if let Some(message) = event.get_mut("message") {
				message.as_object_mut().unwrap().remove("durationMs");
			}
			assert_eq!(event, common::to_json(&expected.remove(0)), "{method} event {index}");
		}
		assert_eq!(
			span.name,
			if method == "streamDeferred" {
				"pi.ai.poll"
			} else {
				"pi.ai.request"
			}
		);
		assert_eq!(span.parent_id, Some(parent.id));
		let mut names: Vec<&str> = span.attributes.iter().map(|(name, _)| name).collect();
		names.sort_unstable();
		let mut expected_names = vec![
			"provider",
			"api",
			"model",
			"stop",
			"usage.input",
			"usage.output",
			"usage.cache_read",
			"usage.cache_write",
			"cost.total",
			"response.id",
		];
		if method == "streamSimple" {
			expected_names.push("reasoning");
		}
		expected_names.sort_unstable();
		assert_eq!(names, expected_names, "{method}");
		assert_eq!(
			attribute(&span, "usage.cache_write"),
			Some(AttributeValue::Number(44.0))
		);
		assert_eq!(attribute(&span, "response.id"), Some(text("response")));
		assert_eq!(
			events_of(&span),
			[
				("pi.ai.first_chunk".into(), vec![]),
				block("text", 0.0, 11.0, None),
				block("thinking", 1.0, 15.0, None),
				block("tool_call", 2.0, 27.0, Some("lookup"))
			]
		);
		let recorded = format!("{span:?}");
		for secret in [
			"text-secret",
			"thinking-secret",
			"argument-secret",
			"call-secret",
			"deferred-secret",
		] {
			assert!(!recorded.contains(secret), "{secret}");
		}
		let measurements = telemetry.get_metrics();
		assert_eq!(measurements.len(), 7);
		let counters: Vec<f64> = measurements
			.iter()
			.filter(|measurement| measurement.kind == InstrumentKind::Counter)
			.map(|measurement| measurement.value)
			.collect();
		assert_eq!(counters, [11.0, 22.0, 33.0, 44.0, 1.0]);
		for measurement in &measurements {
			let mut keys: Vec<&str> = measurement.attributes.iter().map(|(name, _)| name).collect();
			keys.sort_unstable();
			let expected: &[&str] = match measurement.name.as_str() {
				"pi.ai.tokens" => &["kind", "model", "provider"],
				"pi.ai.request.duration" => &["model", "provider", "stop"],
				_ => &["model", "provider"],
			};
			assert_eq!(keys, expected);
			if measurement.kind == InstrumentKind::Histogram {
				assert_eq!(measurement.context.as_ref(), Some(&span.context));
				assert!(measurement.value >= 0.0);
			}
		}
		assert_eq!(
			validate_against_schema(&telemetry.get_spans()[1..], &measurements, &[&AI_TELEMETRY_SCHEMA]),
			[]
		);
	}
}

// TS: telemetry.test.ts
#[tokio::test]
async fn reports_error_and_aborted_status_without_error_attributes_or_events() {
	for reason in ["error", "aborted"] {
		let telemetry = InMemoryTelemetryContext::new();
		let mut result = message(reason);
		result.error_message = Some("error-secret".into());
		let models = setup(result);
		let (_, _, span) = traced(&telemetry, |parent| async move {
			models
				.complete_simple(&model(), Context::default(), with_key(parent))
				.await
		})
		.await;
		let expected = if reason == "error" {
			SpanStatus::error("provider_error", "error-secret")
		} else {
			SpanStatus::Ok
		};
		assert_eq!(span.status, expected);
		assert_eq!(attribute(&span, "stop"), Some(text(reason)));
		assert!(!format!("{:?}{:?}", span.attributes, span.events).contains("error-secret"));
	}
}

// TS: telemetry.test.ts
#[tokio::test]
async fn bounds_poll_error_messages_including_missing_messages() {
	for missing in [false, true] {
		let telemetry = InMemoryTelemetryContext::new();
		let mut result = message("error");
		if !missing {
			result.error_message = Some(format!("{}discarded-tail", "x".repeat(500)));
		}
		let models = setup(result);
		let (_, _, span) = traced(&telemetry, |parent| async move {
			let request = with_key(parent).stream.request;
			models
				.fetch_deferred(&model(), &handle(), DeferredFetchOptions { request, wait: None })
				.await
		})
		.await;
		let message = if missing { String::new() } else { "x".repeat(500) };
		assert_eq!(span.status, SpanStatus::error("provider_error", message));
		assert!(!format!("{span:?}").contains("discarded-tail"));
	}
}

// TS: telemetry.test.ts
#[tokio::test]
async fn includes_setup_failures_and_omits_non_scalar_native_reasoning() {
	let telemetry = InMemoryTelemetryContext::new();
	let models = Models::default();
	let (result, _, span) = traced(&telemetry, |parent| async move {
		let mut options = StreamOptions::default();
		options.request.telemetry = Some(parent);
		models.complete(&model(), Context::default(), options).await
	})
	.await;
	assert_eq!(result.stop_reason, StopReason::Error);
	assert_eq!(attribute(&span, "reasoning"), None);
	assert_eq!(events_of(&span), [("pi.ai.first_chunk".into(), vec![])]);
	assert_eq!(
		span.status,
		SpanStatus::error("provider_error", "Unknown provider: test")
	);
	assert!(!format!("{:?}", span.attributes).contains("Unknown provider"));
}

fn delta(index: u64, text: &str, partial: &AssistantMessage) -> AssistantMessageEvent {
	common::from_json(
		json!({ "type": "toolcall_delta", "contentIndex": index, "delta": text, "partial": common::to_json(partial) }),
	)
}

fn end(index: u64, partial: &AssistantMessage) -> AssistantMessageEvent {
	let partial = common::to_json(partial);
	common::from_json(
		json!({ "type": "toolcall_end", "contentIndex": index, "toolCall": partial["content"][2], "partial": partial }),
	)
}

// TS: telemetry.test.ts
#[tokio::test]
async fn counts_interleaved_tool_deltas_independently_without_reading_arguments() {
	let telemetry = InMemoryTelemetryContext::new();
	let result = message("stop");
	let (_, _, span) = traced(&telemetry, |parent| async move {
		let mut observer = observe_model_request(&model(), Some(&parent), None, RequestKind::Request);
		for (index, text) in [(2, "ab"), (3, "123"), (2, "c")] {
			observer.on_event(&delta(index, text, &result));
		}
		for index in [3, 2, 4] {
			observer.on_event(&end(index, &result));
		}
		observer.on_result(Some(&result));
	})
	.await;
	assert_eq!(
		events_of(&span)[1..],
		[
			block("tool_call", 3.0, 3.0, Some("lookup")),
			block("tool_call", 2.0, 3.0, Some("lookup")),
			block("tool_call", 4.0, 0.0, Some("lookup"))
		]
	);
}

/// A span context without metric capabilities: the TS `{ ...span, metrics: undefined }`.
struct TracingOnly(Arc<dyn TelemetryContext>);

impl TelemetryContext for TracingOnly {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		None
	}
	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		self.0.propagation()
	}
	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		self.0.start_span(options)
	}
}

// TS: telemetry.test.ts
#[tokio::test]
async fn supports_a_tracing_only_span_without_metric_capabilities() {
	let adapter = InMemoryTelemetryContext::new();
	let models = setup(message("stop"));
	traced(&adapter, |parent| async move {
		models
			.complete_simple(&model(), Context::default(), with_key(Arc::new(TracingOnly(parent))))
			.await
	})
	.await;
	assert_eq!(adapter.get_spans().len(), 2);
	assert!(adapter.get_metrics().is_empty());
}

// TS: telemetry.test.ts
#[tokio::test]
async fn defaults_to_no_op_telemetry() {
	let models = setup(message("stop"));
	let mut options = SimpleStreamOptions::default();
	options.stream.request.api_key = Some("key".into());
	assert_eq!(
		models
			.complete_simple(&model(), Context::default(), options)
			.await
			.stop_reason,
		StopReason::Stop
	);
	let request = AI_TELEMETRY_SCHEMA
		.spans
		.iter()
		.find(|span| span.name == "pi.ai.request")
		.unwrap();
	assert!(
		request
			.start_attributes
			.iter()
			.find(|attribute| attribute.name == "provider")
			.unwrap()
			.required
	);
}

// TS: telemetry.test.ts
#[tokio::test]
async fn records_usage_for_a_poll_only_when_final() {
	for reason in ["pending", "deferred", "stop", "error"] {
		let telemetry = InMemoryTelemetryContext::new();
		let models = setup(message(reason));
		traced(&telemetry, |parent| async move {
			let request = with_key(parent).stream.request;
			models
				.fetch_deferred(&model(), &handle(), DeferredFetchOptions { request, wait: None })
				.await
		})
		.await;
		let counters = telemetry
			.get_metrics()
			.into_iter()
			.filter(|metric| metric.kind == InstrumentKind::Counter)
			.count();
		assert_eq!(
			counters,
			if matches!(reason, "pending" | "deferred") { 0 } else { 5 },
			"{reason}"
		);
	}
}

/// Records what the lazy stream observer saw, in order, with whether the stream had resolved by then.
#[derive(Clone, Default)]
struct Order(Arc<Mutex<Vec<String>>>);

impl LazyStreamObserver for Order {
	fn on_event(&mut self, event: &AssistantMessageEvent) {
		self.0
			.lock()
			.unwrap()
			.push(format!("event:{}", common::to_json(event)["type"].as_str().unwrap()));
	}
	fn on_result(&mut self, result: Option<&AssistantMessage>) {
		self.0.lock().unwrap().push(format!(
			"result:{}",
			result.map_or("missing".into(), |message| common::to_json(&message.stop_reason)
				.as_str()
				.unwrap()
				.to_owned())
		));
	}
}

// TS: telemetry.test.ts
#[tokio::test]
async fn lazy_stream_uses_one_source_and_observes_after_push_once_per_terminal_result() {
	let result = message("stop");
	let order = Order::default();
	let source = AssistantMessageEventStream::new();
	source.push(common::from_json(
		json!({ "type": "done", "reason": "stop", "message": common::to_json(&result) }),
	));
	source.end(Some(result.clone()));
	let stream = lazy_stream(&model(), async move { Ok(source) }, Box::new(order.clone()), None);
	assert_eq!(stream.result().await.response_id, result.response_id);
	for _ in 0..100 {
		if order.0.lock().unwrap().len() == 2 {
			break;
		}
		tokio::task::yield_now().await;
	}
	assert_eq!(*order.0.lock().unwrap(), ["event:done", "result:stop"]);
}

// TS: telemetry.test.ts
#[tokio::test]
async fn lazy_stream_observes_a_result_only_source_once() {
	let result = message("stop");
	let order = Order::default();
	let source = AssistantMessageEventStream::new();
	source.end(Some(result));
	let stream = lazy_stream(&model(), async move { Ok(source) }, Box::new(order.clone()), None);
	stream.result().await;
	for _ in 0..100 {
		if !order.0.lock().unwrap().is_empty() {
			break;
		}
		tokio::task::yield_now().await;
	}
	assert_eq!(*order.0.lock().unwrap(), ["result:stop"]);
}

// TS: telemetry.test.ts
#[tokio::test]
async fn lazy_stream_reports_setup_failure_events_and_the_result_once() {
	let order = Order::default();
	let setup = async {
		Err(pi_ai::error::ModelsError::new(
			pi_ai::error::ModelsErrorCode::Stream,
			"setup-secret",
		))
	};
	let stream = lazy_stream(&model(), setup, Box::new(order.clone()), None);
	assert_eq!(stream.result().await.error_message.as_deref(), Some("setup-secret"));
	for _ in 0..100 {
		if order.0.lock().unwrap().len() == 2 {
			break;
		}
		tokio::task::yield_now().await;
	}
	assert_eq!(*order.0.lock().unwrap(), ["event:error", "result:error"]);
}

// TS: telemetry-options.test.ts
#[tokio::test]
async fn telemetry_options_survive_provider_and_models_stream_and_deferred_dispatch() {
	// `telemetry-options.test.ts`: the parent span reaches the provider through every request option surface.
	let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
	struct Probe(Arc<Mutex<Vec<bool>>>);
	#[async_trait]
	impl ProviderStreams for Probe {
		fn deferred(&self) -> DeferredSupport {
			DeferredSupport {
				fetch: true,
				cancel: false,
			}
		}
		fn stream(
			&self,
			_model: &Model,
			_context: TranscriptContext,
			options: StreamOptions,
		) -> AssistantMessageEventStream {
			self.0.lock().unwrap().push(options.request.telemetry.is_some());
			let stream = AssistantMessageEventStream::new();
			stream.end(Some(common::assistant(json!({}))));
			stream
		}
		fn stream_simple(
			&self,
			model: &Model,
			context: TranscriptContext,
			options: SimpleStreamOptions,
		) -> AssistantMessageEventStream {
			let base = pi_ai::simple_options::build_base_options(model, &context, &options, None);
			self.stream(model, context, base)
		}
		fn fetch_deferred(
			&self,
			model: &Model,
			_handle: &DeferredHandle,
			options: DeferredFetchOptions,
		) -> AssistantMessageEventStream {
			self.stream(
				model,
				TranscriptContext::default(),
				StreamOptions {
					request: options.request,
					..StreamOptions::default()
				},
			)
		}
	}
	let models = Models::default();
	models.set_provider(
		create_provider(CreateProviderOptions {
			id: "test".into(),
			auth: ProviderAuth {
				api_key: Some(pi_ai::auth::helpers::env_api_key_auth("Test", &[])),
				oauth: None,
			},
			api: Some(ProviderApi::Single(Arc::new(Probe(Arc::clone(&seen))))),
			..CreateProviderOptions::default()
		})
		.unwrap(),
	);
	let telemetry: Arc<dyn TelemetryContext> = Arc::new(InMemoryTelemetryContext::new());
	let options = with_key(Arc::clone(&telemetry));
	models
		.complete(&model(), Context::default(), options.stream.clone())
		.await;
	models
		.complete_simple(&model(), Context::default(), options.clone())
		.await;
	models
		.fetch_deferred(
			&model(),
			&handle(),
			DeferredFetchOptions {
				request: options.stream.request.clone(),
				wait: None,
			},
		)
		.await;
	assert_eq!(*seen.lock().unwrap(), [true, true, true]);
}
