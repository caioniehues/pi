//! The `tracing` bridge (`tracing_bridge.rs`): spans, attributes, events, links and status reach a subscriber.

use pi_telemetry::{SpanAttributes, SpanLink, SpanOptions, SpanStatus, TelemetryContext, TracingTelemetryContext};
use std::fmt::Debug;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// What the subscriber saw, one line per call.
#[derive(Default)]
struct Log {
	lines: Mutex<Vec<String>>,
	next_id: AtomicU64,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Log>);

struct Fields(Vec<String>);

impl Visit for Fields {
	fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
		self.0.push(format!("{}={value:?}", field.name()));
	}

	fn record_str(&mut self, field: &Field, value: &str) {
		self.0.push(format!("{}={value}", field.name()));
	}
}

impl Recorder {
	fn push(&self, line: String) {
		self.0.lines.lock().unwrap_or_else(PoisonError::into_inner).push(line);
	}

	fn lines(&self) -> Vec<String> {
		self.0.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
	}
}

impl Subscriber for Recorder {
	fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
		true
	}

	fn new_span(&self, span: &Attributes<'_>) -> Id {
		let id = self.0.next_id.fetch_add(1, Ordering::Relaxed) + 1;
		let mut fields = Fields(Vec::new());
		span.record(&mut fields);
		let parent = span
			.parent()
			.map_or("root".to_owned(), |parent| parent.into_u64().to_string());
		self.push(format!(
			"new {id} parent={parent} {} {}",
			span.metadata().name(),
			fields.0.join(" ")
		));
		Id::from_u64(id)
	}

	fn record(&self, span: &Id, values: &Record<'_>) {
		let mut fields = Fields(Vec::new());
		values.record(&mut fields);
		self.push(format!("record {} {}", span.into_u64(), fields.0.join(" ")));
	}

	fn record_follows_from(&self, span: &Id, follows: &Id) {
		self.push(format!("follows {} {}", span.into_u64(), follows.into_u64()));
	}

	fn event(&self, event: &Event<'_>) {
		let mut fields = Fields(Vec::new());
		event.record(&mut fields);
		let parent = event.parent().map_or(0, Id::into_u64);
		self.push(format!("event parent={parent} {}", fields.0.join(" ")));
	}

	fn enter(&self, _span: &Id) {}

	fn exit(&self, _span: &Id) {}

	fn try_close(&self, id: Id) -> bool {
		self.push(format!("close {}", id.into_u64()));
		true
	}
}

#[test]
fn spans_events_links_and_status_reach_the_subscriber() {
	let recorder = Recorder::default();
	tracing::subscriber::with_default(recorder.clone(), || {
		let context = TracingTelemetryContext;
		let target = context.start_span(SpanOptions::new("target"));
		let handle = target.handle();
		target.end();
		let attributes = SpanAttributes::new().with("provider", "anthropic");
		let parent = context.start_span(
			SpanOptions::new("request")
				.with_attributes(attributes)
				.with_link(SpanLink::new(handle.as_ref())),
		);
		parent.set_attributes(SpanAttributes::new().with("tokens", 12));
		parent.add_event("retry", SpanAttributes::new().with("attempt", 2));
		let child = parent.start_span(SpanOptions::new("child"));
		child.set_status(SpanStatus::error("Error", "failed"));
		child.end();
		parent.end();
		handle.start_span(SpanOptions::new("inert")).end();
	});
	assert_eq!(
		recorder.lines(),
		[
			"new 1 parent=root pi.telemetry.span otel.name=target attributes={}",
			"close 1",
			r#"new 2 parent=root pi.telemetry.span otel.name=request attributes={"provider":"anthropic"}"#,
			"follows 2 1",
			r#"record 2 attributes={"provider":"anthropic","tokens":12}"#,
			r#"event parent=2 message=pi.telemetry.event event=retry attributes={"attempt":2}"#,
			"new 3 parent=2 pi.telemetry.span otel.name=child attributes={}",
			"record 3 otel.status_code=error",
			"record 3 otel.status_message=Error: failed",
			"close 3",
			"close 2",
		]
	);
}

#[test]
fn a_span_dropped_while_panicking_records_an_error() {
	let recorder = Recorder::default();
	tracing::subscriber::with_default(recorder.clone(), || {
		let unwound = std::panic::catch_unwind(|| {
			let _span = TracingTelemetryContext.start_span(SpanOptions::new("panicked"));
			std::panic::resume_unwind(Box::new("expected"));
		});
		assert!(unwound.is_err());
	});
	assert_eq!(
		recorder.lines()[1..],
		["record 1 otel.status_code=error".to_owned(), "close 1".to_owned()]
	);
}
