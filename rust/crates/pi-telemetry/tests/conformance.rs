//! The adapter conformance suite (`testing/conformance.rs`) against the in-memory adapter (ports
//! `packages/telemetry/test/conformance.test.ts`) and, where the suite allows, the no-op context.

use pi_telemetry::testing::conformance::{
	TelemetryAdapterFixture, TelemetryAdapterFixtureFactory, telemetry_adapter_conformance,
};
use pi_telemetry::{
	InMemoryTelemetryContext, NOOP_TELEMETRY_CONTEXT, RecordedTelemetryMeasurement, RecordedTelemetrySpan,
	SpanAttributes, SpanOptions, TelemetryContext, TelemetryMetrics, TelemetryPropagation,
};
use std::sync::Arc;

struct InMemoryFixture(InMemoryTelemetryContext);

impl TelemetryAdapterFixture for InMemoryFixture {
	fn context(&self) -> &dyn TelemetryContext {
		&self.0
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		self.0.propagation()
	}

	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		self.0.metrics()
	}

	fn get_metrics(&self) -> Option<Vec<RecordedTelemetryMeasurement>> {
		Some(self.0.get_metrics())
	}

	fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
		self.0.get_spans()
	}
}

/// The no-op context records nothing, so only the cases that read no spans run against it; the metric and
/// propagation cases return early without the optional capabilities, as in TS.
struct NoopFixture;

impl TelemetryAdapterFixture for NoopFixture {
	fn context(&self) -> &dyn TelemetryContext {
		&NOOP_TELEMETRY_CONTEXT
	}

	fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
		Vec::new()
	}
}

const GROUPS_WITHOUT_RECORDING: [&str; 3] = ["capabilities", "metrics", "propagation"];

#[test]
fn in_memory_telemetry_context_conformance() {
	let factory: TelemetryAdapterFixtureFactory =
		Arc::new(|| Box::new(InMemoryFixture(InMemoryTelemetryContext::new())));
	let cases = telemetry_adapter_conformance(&factory);
	assert_eq!(cases.len(), 13);
	for case in cases {
		if let Err(failure) = case.run() {
			panic!("{} / {}: {failure}", case.group, case.name);
		}
	}
}

#[test]
fn noop_telemetry_context_conformance() {
	let factory: TelemetryAdapterFixtureFactory = Arc::new(|| Box::new(NoopFixture));
	let cases = telemetry_adapter_conformance(&factory);
	for case in cases
		.iter()
		.filter(|case| GROUPS_WITHOUT_RECORDING.contains(&case.group))
	{
		if let Err(failure) = case.run() {
			panic!("{} / {}: {failure}", case.group, case.name);
		}
	}
}

#[test]
fn returns_detached_snapshots_without_exposing_mutable_recording_state() {
	let context = InMemoryTelemetryContext::new();
	let span = context
		.start_span(SpanOptions::new("snapshot").with_attributes(SpanAttributes::new().with("tags", vec!["initial"])));
	span.add_event("event", SpanAttributes::new().with("value", 1));
	let open = &context.get_spans()[0];
	assert!(!open.settled);
	assert_eq!(open.end_sequence, None);
	span.end();
	let mut first = context.get_spans().remove(0);
	assert!(first.settled);
	assert_eq!(first.end_sequence, Some(1));
	first.attributes.set("tags", vec!["mutated"]);
	let second = &context.get_spans()[0];
	assert_eq!(second.attributes, SpanAttributes::new().with("tags", vec!["initial"]));
	assert_eq!(second.events[0].attributes, SpanAttributes::new().with("value", 1));
}
