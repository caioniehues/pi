//! The no-op context (`noop.rs`); ports the `NOOP_TELEMETRY_CONTEXT` cases of
//! `packages/telemetry/test/telemetry.test.ts`.

use pi_telemetry::{
	NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_METRICS, NOOP_TELEMETRY_PROPAGATION, SpanAttributes, SpanLink, SpanOptions,
	SpanStatus, TelemetryContext, TelemetryMetrics, TelemetryPropagation, TelemetrySpan,
};
use std::sync::Arc;

fn same<T: ?Sized>(left: Option<&T>, right: &T) -> bool {
	left.is_some_and(|left| std::ptr::addr_eq(left, right))
}

// TS: telemetry.test.ts "admits callbacks synchronously and reuses one inert span".
#[test]
fn starts_inert_spans_that_carry_the_noop_capabilities() {
	let span = NOOP_TELEMETRY_CONTEXT.start_span(SpanOptions::new("first"));
	let child = span.start_span(SpanOptions::new("child"));
	for context in [
		&NOOP_TELEMETRY_CONTEXT as &dyn TelemetryContext,
		span.as_ref(),
		child.as_ref(),
	] {
		assert!(same(
			context.metrics(),
			&NOOP_TELEMETRY_METRICS as &dyn TelemetryMetrics
		));
		assert!(same(
			context.propagation(),
			&NOOP_TELEMETRY_PROPAGATION as &dyn TelemetryPropagation
		));
	}
	// The no-op span is zero-sized: starting one allocates nothing, and every handle is one shared value.
	assert_eq!(size_of_val(span.as_ref()), 0);
	assert!(Arc::ptr_eq(&span.handle(), &child.handle()));
	child.end();
	span.end();
}

// TS: telemetry.test.ts "ignores typed recording payloads".
#[test]
fn ignores_recording_payloads() {
	let span = NOOP_TELEMETRY_CONTEXT
		.start_span(SpanOptions::new("operation").with_attributes(SpanAttributes::new().with("kind", "test")));
	span.add_event("event", SpanAttributes::new().with("count", 1));
	span.set_attributes(SpanAttributes::new().with("complete", true));
	span.set_status(SpanStatus::error("Expected", "test"));
	span.add_link(SpanLink::new(span.as_ref() as &dyn TelemetrySpan));
	span.end();
}
