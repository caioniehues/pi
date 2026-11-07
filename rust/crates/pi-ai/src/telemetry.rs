//! Telemetry of model requests (ports `packages/ai/src/telemetry.ts`): `AI_TELEMETRY_SCHEMA` and the observer that
//! records a `pi.ai.request` (or `pi.ai.poll`) span with block events and low-cardinality metrics. Content is never
//! recorded, only its size.
//!
//! TS caches the four instruments per metrics object in a `WeakMap`; a foreign trait object has no weak identity to
//! key on, so instruments are looked up per request (one map lookup each in the adapters).

use crate::lazy_stream::LazyStreamObserver;
use crate::utf16::{utf16_len, utf16_prefix};
use pi_telemetry::{
	AttributeDefinition, AttributeKind, Cardinality, EventDefinition, InstrumentKind, MetricDefinition,
	NOOP_TELEMETRY_METRICS, ParentRule, SchemaMetrics, SpanAttributes, SpanDefinition, SpanOptions, SpanStatus,
	TelemetryContext, TelemetryMetrics, TelemetrySchema, TelemetrySpan,
};
use pi_types::{AssistantMessage, AssistantMessageEvent, KnownAssistantMessageEvent, Model, StopReason, ThinkingLevel};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

const STOP_VALUES: &[&str] = &["pending", "stop", "length", "toolUse", "error", "aborted", "deferred"];
const PROVIDER: AttributeDefinition = AttributeDefinition::new(
	"provider",
	"Provider identifier.",
	AttributeKind::String { values: None },
)
.required();
const MODEL: AttributeDefinition = AttributeDefinition::new(
	"model",
	"Requested model identifier.",
	AttributeKind::String { values: None },
)
.required();
const STOP: AttributeDefinition = AttributeDefinition::new(
	"stop",
	"Normalized stop reason.",
	AttributeKind::String {
		values: Some(STOP_VALUES),
	},
);

const START_ATTRIBUTES: &[AttributeDefinition] = &[
	PROVIDER,
	AttributeDefinition::new("api", "API identifier.", AttributeKind::String { values: None }).required(),
	MODEL,
	AttributeDefinition::new(
		"reasoning",
		"Scalar requested reasoning level.",
		AttributeKind::String { values: None },
	),
];

const NUMBER: AttributeKind = AttributeKind::Number { values: None };

const END_ATTRIBUTES: &[AttributeDefinition] = &[
	STOP,
	AttributeDefinition::new("usage.input", "Uncached input tokens.", NUMBER),
	AttributeDefinition::new("usage.output", "Output tokens.", NUMBER),
	AttributeDefinition::new("usage.cache_read", "Cache-read tokens.", NUMBER),
	AttributeDefinition::new("usage.cache_write", "Cache-write tokens.", NUMBER),
	AttributeDefinition::new("cost.total", "Total request cost in USD.", NUMBER),
	AttributeDefinition::new(
		"response.id",
		"Provider response identifier.",
		AttributeKind::String { values: None },
	)
	.cardinality(Cardinality::High),
];

const EVENTS: &[EventDefinition] = &[
	EventDefinition {
		name: "pi.ai.first_chunk",
		description: "First normalized stream event delivered.",
		attributes: &[],
	},
	EventDefinition {
		name: "pi.ai.block",
		description: "A completed content block; content is never recorded.",
		attributes: &[
			AttributeDefinition::new(
				"type",
				"Block kind.",
				AttributeKind::String {
					values: Some(&["text", "thinking", "tool_call"]),
				},
			)
			.required(),
			AttributeDefinition::new("index", "Content block index.", NUMBER).required(),
			AttributeDefinition::new("chars", "UTF-16 character count.", NUMBER).required(),
			AttributeDefinition::new(
				"tool",
				"Tool name, for tool-call blocks only.",
				AttributeKind::String { values: None },
			),
		],
	},
];

const ERROR_WHEN: &str =
	"Stop reason is error: provider_error with at most 500 error-message characters. Aborted stays ok.";

/// Pi-owned request metadata, bounded error status, events and low-cardinality metrics (`AI_TELEMETRY_SCHEMA`).
pub static AI_TELEMETRY_SCHEMA: TelemetrySchema = TelemetrySchema {
	version: 1,
	spans: &[
		SpanDefinition {
			name: "pi.ai.request",
			description: "One logical model request, including authentication and provider setup.",
			parents: ParentRule::Any,
			start_attributes: START_ATTRIBUTES,
			end_attributes: END_ATTRIBUTES,
			events: EVENTS,
			error_when: ERROR_WHEN,
		},
		SpanDefinition {
			name: "pi.ai.poll",
			description: "One deferred-response poll, not the deferred request's lifetime.",
			parents: ParentRule::Any,
			start_attributes: START_ATTRIBUTES,
			end_attributes: END_ATTRIBUTES,
			events: EVENTS,
			error_when: ERROR_WHEN,
		},
	],
	metrics: &[
		MetricDefinition {
			name: "pi.ai.request.duration",
			description: "Logical request or poll duration.",
			unit: Some("ms"),
			kind: InstrumentKind::Histogram,
			boundaries: None,
			attributes: &[PROVIDER, MODEL, STOP.required()],
		},
		MetricDefinition {
			name: "pi.ai.first_chunk.duration",
			description: "Time to the first normalized stream event.",
			unit: Some("ms"),
			kind: InstrumentKind::Histogram,
			boundaries: None,
			attributes: &[PROVIDER, MODEL],
		},
		MetricDefinition {
			name: "pi.ai.tokens",
			description: "Reported model token usage; pending and deferred polls are excluded.",
			unit: Some("token"),
			kind: InstrumentKind::Counter,
			boundaries: None,
			attributes: &[
				PROVIDER,
				MODEL,
				AttributeDefinition::new(
					"kind",
					"Token usage category.",
					AttributeKind::String {
						values: Some(&["input", "output", "cache_read", "cache_write"]),
					},
				)
				.required(),
			],
		},
		MetricDefinition {
			name: "pi.ai.cost",
			description: "Reported model cost; pending and deferred polls are excluded.",
			unit: Some("USD"),
			kind: InstrumentKind::Counter,
			boundaries: None,
			attributes: &[PROVIDER, MODEL],
		},
	],
};

/// Which span a request records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
	/// `pi.ai.request`: a stream request.
	Request,
	/// `pi.ai.poll`: one deferred-response poll.
	Poll,
}

/// Starts the request span under `telemetry` (no-op when absent) and returns the observer that records the rest
/// (`observeModelRequest`).
pub fn observe_model_request(
	model: &Model,
	telemetry: Option<&Arc<dyn TelemetryContext>>,
	reasoning: Option<ThinkingLevel>,
	kind: RequestKind,
) -> Box<dyn LazyStreamObserver> {
	// No parent: the no-op adapter, as TS falls back to `NOOP_TELEMETRY_CONTEXT`.
	let context: Arc<dyn TelemetryContext> = telemetry.map_or_else(
		|| Arc::new(pi_telemetry::NoopTelemetry) as Arc<dyn TelemetryContext>,
		Arc::clone,
	);
	let attributes = SpanAttributes::new()
		.with("provider", model.provider.as_str())
		.with("model", model.id.as_str());
	let mut start = attributes.clone().with("api", model.api.as_str());
	if let Some(level) = reasoning {
		start.set("reasoning", thinking_level_name(level));
	}
	let name = match kind {
		RequestKind::Request => "pi.ai.request",
		RequestKind::Poll => "pi.ai.poll",
	};
	let span = context.start_span(SpanOptions::new(name).with_attributes(start));
	Box::new(RequestObserver {
		context,
		span: Some(span),
		attributes,
		started: Instant::now(),
		first: true,
		tool_chars: HashMap::new(),
		kind,
	})
}

fn thinking_level_name(level: ThinkingLevel) -> &'static str {
	match level {
		ThinkingLevel::Minimal => "minimal",
		ThinkingLevel::Low => "low",
		ThinkingLevel::Medium => "medium",
		ThinkingLevel::High => "high",
		ThinkingLevel::Xhigh => "xhigh",
		ThinkingLevel::Max => "max",
	}
}

struct RequestObserver {
	context: Arc<dyn TelemetryContext>,
	span: Option<Box<dyn TelemetrySpan>>,
	attributes: SpanAttributes,
	started: Instant,
	first: bool,
	tool_chars: HashMap<u64, usize>,
	kind: RequestKind,
}

impl RequestObserver {
	fn metrics(&self) -> &dyn TelemetryMetrics {
		self.context.metrics().unwrap_or(&NOOP_TELEMETRY_METRICS)
	}

	fn elapsed_ms(&self) -> f64 {
		self.started.elapsed().as_secs_f64() * 1000.0
	}

	fn block_event(&self, block: &str, index: u64, chars: usize, tool: Option<&str>) {
		let Some(span) = &self.span else { return };
		#[allow(
			clippy::cast_precision_loss,
			reason = "indices and character counts are far below 2^52"
		)]
		let mut attributes = SpanAttributes::new()
			.with("type", block)
			.with("index", index as f64)
			.with("chars", chars as f64);
		if let Some(tool) = tool {
			attributes.set("tool", tool);
		}
		span.add_event("pi.ai.block", attributes);
	}
}

#[allow(
	clippy::cast_precision_loss,
	reason = "token counts are far below 2^52, as JS numbers"
)]
fn tokens(count: i64) -> f64 {
	count as f64
}

fn stop_name(stop: StopReason) -> &'static str {
	match stop {
		StopReason::Pending => "pending",
		StopReason::Stop => "stop",
		StopReason::Length => "length",
		StopReason::ToolUse => "toolUse",
		StopReason::Error => "error",
		StopReason::Aborted => "aborted",
		StopReason::Deferred => "deferred",
	}
}

/// The most error-message characters a span status keeps.
const MAX_STATUS_MESSAGE_CHARS: usize = 500;

impl LazyStreamObserver for RequestObserver {
	fn on_event(&mut self, event: &AssistantMessageEvent) {
		if self.first {
			self.first = false;
			if let Some(span) = &self.span {
				span.add_event("pi.ai.first_chunk", SpanAttributes::new());
				let schemas = [&AI_TELEMETRY_SCHEMA];
				let histogram = SchemaMetrics::new(self.metrics(), &schemas).histogram("pi.ai.first_chunk.duration");
				histogram.record(
					self.elapsed_ms(),
					&self.attributes,
					Some(span.as_ref() as &dyn TelemetryContext),
				);
			}
		}
		let AssistantMessageEvent::Known(event) = event else {
			return;
		};
		match event {
			KnownAssistantMessageEvent::ToolCallDelta(delta) => {
				*self.tool_chars.entry(delta.content_index).or_default() += utf16_len(&delta.delta);
			}
			KnownAssistantMessageEvent::TextEnd(end) => {
				self.block_event("text", end.content_index, utf16_len(&end.content), None);
			}
			KnownAssistantMessageEvent::ThinkingEnd(end) => {
				self.block_event("thinking", end.content_index, utf16_len(&end.content), None);
			}
			KnownAssistantMessageEvent::ToolCallEnd(end) => {
				let chars = self.tool_chars.remove(&end.content_index).unwrap_or(0);
				self.block_event("tool_call", end.content_index, chars, Some(&end.tool_call.name));
			}
			_ => {}
		}
	}

	fn on_result(&mut self, result: Option<&AssistantMessage>) {
		let Some(span) = self.span.take() else { return };
		if let Some(message) = result {
			let usage = &message.usage;
			let mut attributes = SpanAttributes::new()
				.with("stop", stop_name(message.stop_reason))
				.with("usage.input", tokens(usage.input))
				.with("usage.output", tokens(usage.output))
				.with("usage.cache_read", tokens(usage.cache_read))
				.with("usage.cache_write", tokens(usage.cache_write))
				.with("cost.total", usage.cost.total);
			if let Some(id) = &message.response_id {
				attributes.set("response.id", id.as_str());
			}
			span.set_attributes(attributes);
			if message.stop_reason == StopReason::Error {
				let error = utf16_prefix(
					message.error_message.as_deref().unwrap_or_default(),
					MAX_STATUS_MESSAGE_CHARS,
				);
				span.set_status(SpanStatus::error("provider_error", error));
			}
			let schemas = [&AI_TELEMETRY_SCHEMA];
			let metrics = SchemaMetrics::new(self.metrics(), &schemas);
			let stop_attributes = self.attributes.clone().with("stop", stop_name(message.stop_reason));
			metrics.histogram("pi.ai.request.duration").record(
				self.elapsed_ms(),
				&stop_attributes,
				Some(span.as_ref() as &dyn TelemetryContext),
			);
			let pending = matches!(message.stop_reason, StopReason::Pending | StopReason::Deferred);
			if self.kind == RequestKind::Request || !pending {
				let counter = metrics.counter("pi.ai.tokens");
				for (kind, count) in [
					("input", usage.input),
					("output", usage.output),
					("cache_read", usage.cache_read),
					("cache_write", usage.cache_write),
				] {
					counter.add(tokens(count), &self.attributes.clone().with("kind", kind));
				}
				metrics.counter("pi.ai.cost").add(usage.cost.total, &self.attributes);
			}
		}
		span.end();
	}
}
