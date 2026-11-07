//! An adapter that forwards spans and events to the `tracing` crate (feature `tracing`), so a host that already
//! runs a `tracing` subscriber sees pi's telemetry there.
//!
//! `tracing` span names are static metadata, so every span is a `pi.telemetry.span` with the pi name in the field
//! `otel.name` (the OpenTelemetry bridge's convention for a dynamic name), its merged attributes as JSON in
//! `attributes`, and its outcome in `otel.status_code` and `otel.status_message`. Events are `pi.telemetry.event`
//! events under their span; links become `follows_from`. The adapter has no metrics or propagation.

use crate::context::{SpanAttributes, SpanLink, SpanOptions, SpanStatus, TelemetryContext, TelemetrySpan};
use crate::metrics::TelemetryMetrics;
use crate::noop::NoopTelemetry;
use crate::propagation::TelemetryPropagation;
use pi_json::{JsonObject, JsonValue, to_js_string};
use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tracing::field::Empty;

/// The root context of the bridge: its spans are `tracing` root spans, whatever span is current.
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingTelemetryContext;

impl TelemetryContext for TracingTelemetryContext {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		None
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		None
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		TracingSpan::start(None, options)
	}
}

/// An open `tracing` span with the state the trait's merge and status rules need.
struct TracingSpan {
	span: tracing::Span,
	attributes: Mutex<SpanAttributes>,
	explicit_status: AtomicBool,
	/// Shared with handles: children of a handle are inert once the span settled.
	settled: Arc<AtomicBool>,
}

impl TracingSpan {
	fn start(parent: Option<tracing::Id>, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		let span = tracing::info_span!(
			parent: parent,
			"pi.telemetry.span",
			otel.name = %options.name,
			attributes = %attributes_json(&options.attributes),
			otel.status_code = Empty,
			otel.status_message = Empty,
		);
		for link in &options.links {
			follow(&span, link);
		}
		Box::new(Self {
			span,
			attributes: Mutex::new(options.attributes),
			explicit_status: AtomicBool::new(false),
			settled: Arc::new(AtomicBool::new(false)),
		})
	}

	fn record_status(&self, status: &SpanStatus) {
		match status {
			SpanStatus::Ok => {
				self.span.record("otel.status_code", "ok");
			}
			SpanStatus::Error(error) => {
				self.span.record("otel.status_code", "error");
				if let Some(error) = error {
					self.span
						.record("otel.status_message", format!("{}: {}", error.name, error.message));
				}
			}
		}
	}

	fn settle(&self, panicking: bool) {
		if self.settled.swap(true, Ordering::AcqRel) {
			return;
		}
		if panicking && !self.explicit_status.load(Ordering::Acquire) {
			self.record_status(&SpanStatus::Error(None));
		}
	}
}

impl TelemetryContext for TracingSpan {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		None
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		None
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		TracingSpan::start(self.span.id(), options)
	}
}

impl TelemetrySpan for TracingSpan {
	fn add_link(&self, link: SpanLink<'_>) {
		follow(&self.span, &link);
	}

	fn add_event(&self, name: &str, attributes: SpanAttributes) {
		tracing::info!(
			parent: &self.span,
			event = %name,
			attributes = %attributes_json(&attributes),
			"pi.telemetry.event"
		);
	}

	fn set_attributes(&self, attributes: SpanAttributes) {
		let mut merged = self.attributes.lock().unwrap_or_else(PoisonError::into_inner);
		merged.merge(attributes);
		self.span.record("attributes", attributes_json(&merged).as_str());
	}

	fn set_status(&self, status: SpanStatus) {
		self.explicit_status.store(true, Ordering::Release);
		self.record_status(&status);
	}

	fn handle(&self) -> Arc<dyn TelemetryContext> {
		Arc::new(TracingSpanHandle {
			id: self.span.id(),
			settled: self.settled.clone(),
		})
	}

	fn end(self: Box<Self>) {
		self.settle(false);
	}
}

impl Drop for TracingSpan {
	fn drop(&mut self) {
		// Dropping `span` afterwards closes the `tracing` span.
		self.settle(std::thread::panicking());
	}
}

/// A span's id without its `tracing` handle, so holding it does not keep the span open.
struct TracingSpanHandle {
	id: Option<tracing::Id>,
	settled: Arc<AtomicBool>,
}

impl TelemetryContext for TracingSpanHandle {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		None
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		None
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		if self.settled.load(Ordering::Acquire) {
			return Box::new(NoopTelemetry);
		}
		TracingSpan::start(self.id.clone(), options)
	}
}

/// Adds `follows_from` for a link to one of the bridge's spans or handles; other contexts are ignored.
fn follow(span: &tracing::Span, link: &SpanLink<'_>) {
	let context: &dyn Any = link.context;
	let target = if let Some(target) = context.downcast_ref::<TracingSpan>() {
		target.span.id()
	} else {
		context
			.downcast_ref::<TracingSpanHandle>()
			.and_then(|handle| handle.id.clone())
	};
	if let Some(target) = target {
		span.follows_from(target);
	}
}

/// Attributes as a JSON object, the form `tracing` subscribers can parse back.
fn attributes_json(attributes: &SpanAttributes) -> String {
	let object: JsonObject = attributes
		.iter()
		.map(|(name, value)| (name.to_owned(), value.to_json()))
		.collect();
	to_js_string(&JsonValue::Object(object))
}
