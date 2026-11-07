//! Metrics (`metrics.rs`); ports `packages/telemetry/test/metrics.test.ts`. The TS compile-time failures (wrong
//! kinds, unknown names, missing or unknown attributes) are runtime findings of `validate_against_schema` here.

use pi_telemetry::{
	AttributeDefinition, AttributeKind, Cardinality, HistogramOptions, InMemoryTelemetryContext, InstrumentKind,
	InstrumentOptions, MetricDefinition, NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_METRICS, SchemaMetrics, SchemaProblem,
	SpanAttributes, SpanOptions, TelemetryContext, TelemetryMetrics, TelemetrySchema, validate_against_schema,
};
use std::sync::Arc;

static TRAFFIC: TelemetrySchema = TelemetrySchema {
	version: 1,
	spans: &[],
	metrics: &[
		MetricDefinition {
			name: "network.bytes",
			description: "Encoded bytes",
			unit: Some("By"),
			kind: InstrumentKind::Counter,
			boundaries: None,
			attributes: &[AttributeDefinition::new(
				"direction",
				"Direction",
				AttributeKind::String {
					values: Some(&["in", "out"]),
				},
			)
			.required()
			.cardinality(Cardinality::Low)],
		},
		MetricDefinition {
			name: "network.active",
			description: "Open links",
			unit: None,
			kind: InstrumentKind::UpDownCounter,
			boundaries: None,
			attributes: &[],
		},
	],
};

static LATENCY: TelemetrySchema = TelemetrySchema {
	version: 1,
	spans: &[],
	metrics: &[MetricDefinition {
		name: "call.duration",
		description: "Call duration",
		unit: Some("ms"),
		kind: InstrumentKind::Histogram,
		boundaries: Some(&[1.0, 10.0, 100.0]),
		attributes: &[AttributeDefinition::new(
			"method",
			"Method",
			AttributeKind::String { values: None },
		)],
	}],
};

#[test]
fn binds_schema_names_kinds_metadata_and_exact_measurement_attributes() {
	let adapter = InMemoryTelemetryContext::new();
	let schemas = [&TRAFFIC, &LATENCY];
	let metrics = SchemaMetrics::new(&adapter, &schemas);
	metrics
		.counter("network.bytes")
		.add(128.0, &SpanAttributes::new().with("direction", "out"));
	metrics
		.up_down_counter("network.active")
		.add(1.0, &SpanAttributes::new());
	metrics
		.up_down_counter("network.active")
		.add(-1.0, &SpanAttributes::new());
	let span = adapter.start_span(SpanOptions::new("request"));
	let duration = metrics.histogram("call.duration");
	duration.record(12.0, &SpanAttributes::new().with("method", "read"), Some(span.as_ref()));
	duration.record(13.0, &SpanAttributes::new(), None);
	duration.record(14.0, &SpanAttributes::new(), Some(span.as_ref()));
	span.end();
	let measurements = adapter.get_metrics();
	let span_context = adapter.get_spans()[0].context.clone();
	assert_eq!(measurements[0].kind, InstrumentKind::Counter);
	assert_eq!(measurements[0].name, "network.bytes");
	assert_eq!(measurements[0].description.as_deref(), Some("Encoded bytes"));
	assert_eq!(measurements[0].unit.as_deref(), Some("By"));
	assert_eq!(measurements[0].value, 128.0);
	assert_eq!(
		measurements[0].attributes,
		SpanAttributes::new().with("direction", "out")
	);
	assert_eq!(measurements[3].kind, InstrumentKind::Histogram);
	assert_eq!(measurements[3].boundaries, Some(vec![1.0, 10.0, 100.0]));
	assert_eq!(measurements[3].context.as_ref(), Some(&span_context));
	assert_eq!(measurements[4].context, None);
	assert_eq!(measurements[5].context.as_ref(), Some(&span_context));
	assert_eq!(validate_against_schema(&[], &measurements, &schemas), []);
}

// TS: metrics.test.ts "binds schema names, kinds, metadata, and exact measurement attributes" (its
// `@ts-expect-error` lines).
#[test]
fn schema_violations_replace_the_ts_compile_time_failures() {
	let adapter = InMemoryTelemetryContext::new();
	let schemas = [&TRAFFIC, &LATENCY];
	let metrics = SchemaMetrics::new(&adapter, &schemas);
	// A histogram's name used for a counter, an unknown name, a missing required attribute, a value outside the
	// closed set, an unknown attribute, and an attribute on an empty attribute schema.
	metrics.counter("call.duration").add(1.0, &SpanAttributes::new());
	metrics.up_down_counter("unknown").add(1.0, &SpanAttributes::new());
	metrics.counter("network.bytes").add(1.0, &SpanAttributes::new());
	metrics
		.counter("network.bytes")
		.add(1.0, &SpanAttributes::new().with("direction", "sideways"));
	metrics.counter("network.bytes").add(
		1.0,
		&SpanAttributes::new().with("direction", "in").with("unknown", true),
	);
	metrics
		.up_down_counter("network.active")
		.add(1.0, &SpanAttributes::new().with("unknown", true));
	let problems: Vec<(String, SchemaProblem)> = validate_against_schema(&[], &adapter.get_metrics(), &schemas)
		.into_iter()
		.map(|violation| (violation.subject, violation.problem))
		.collect();
	assert_eq!(
		problems,
		[
			("metric call.duration".to_owned(), SchemaProblem::WrongInstrumentKind),
			("metric unknown".to_owned(), SchemaProblem::Undeclared),
			(
				"metric network.bytes".to_owned(),
				SchemaProblem::MissingAttribute("direction".into())
			),
			(
				"metric network.bytes".to_owned(),
				SchemaProblem::InvalidValue("direction".into())
			),
			(
				"metric network.bytes".to_owned(),
				SchemaProblem::UndeclaredAttribute("unknown".into())
			),
			(
				"metric network.active".to_owned(),
				SchemaProblem::UndeclaredAttribute("unknown".into())
			),
		]
	);
}

static REQUIRED: TelemetrySchema = TelemetrySchema {
	version: 1,
	spans: &[],
	metrics: &[MetricDefinition {
		name: "required",
		description: "Required tag",
		unit: None,
		kind: InstrumentKind::Histogram,
		boundaries: None,
		attributes: &[AttributeDefinition::new("cache", "Cache hit", AttributeKind::Bool { values: None }).required()],
	}],
};

static HIGH_CARDINALITY: TelemetrySchema = TelemetrySchema {
	version: 1,
	spans: &[],
	metrics: &[MetricDefinition {
		name: "invalid",
		description: "Invalid",
		unit: None,
		kind: InstrumentKind::Counter,
		boundaries: None,
		attributes: &[
			AttributeDefinition::new("request", "Request", AttributeKind::String { values: None })
				.cardinality(Cardinality::High),
		],
	}],
};

#[test]
fn requires_histogram_attributes_when_the_schema_declares_them() {
	let adapter = InMemoryTelemetryContext::new();
	let schemas = [&REQUIRED, &HIGH_CARDINALITY];
	let metrics = SchemaMetrics::new(&adapter, &schemas);
	metrics
		.histogram("required")
		.record(1.0, &SpanAttributes::new().with("cache", true), None);
	metrics.histogram("required").record(1.0, &SpanAttributes::new(), None);
	metrics
		.histogram("required")
		.record(1.0, &SpanAttributes::new().with("cache", "true"), None);
	metrics.counter("invalid").add(1.0, &SpanAttributes::new());
	let problems: Vec<SchemaProblem> = validate_against_schema(&[], &adapter.get_metrics(), &schemas)
		.into_iter()
		.map(|violation| violation.problem)
		.collect();
	assert_eq!(
		problems,
		[
			SchemaProblem::MissingAttribute("cache".into()),
			SchemaProblem::InvalidValue("cache".into()),
			SchemaProblem::HighCardinalityMetricAttribute("request".into()),
		]
	);
}

#[test]
fn returns_detached_measurement_snapshots_including_exemplars_and_boundaries() {
	let telemetry = InMemoryTelemetryContext::new();
	let options = HistogramOptions {
		boundaries: Some(&[1.0, 2.0]),
		..HistogramOptions::default()
	};
	let histogram = telemetry.histogram("duration", options);
	let span = telemetry.start_span(SpanOptions::new("request"));
	histogram.record(
		1.0,
		&SpanAttributes::new().with("tags", vec!["original"]),
		Some(span.as_ref()),
	);
	span.end();
	let mut measurement = telemetry.get_metrics().remove(0);
	if let Some(boundaries) = measurement.boundaries.as_mut() {
		boundaries.push(3.0);
	}
	measurement.attributes.set("tags", vec!["mutated"]);
	let fresh = &telemetry.get_metrics()[0];
	assert_eq!(fresh.boundaries, Some(vec![1.0, 2.0]));
	assert_eq!(fresh.attributes, SpanAttributes::new().with("tags", vec!["original"]));
	assert_eq!(fresh.context.as_ref(), Some(&telemetry.get_spans()[0].context));
}

#[test]
fn shares_frozen_no_op_instruments() {
	let options = InstrumentOptions {
		unit: Some("By"),
		..InstrumentOptions::default()
	};
	let attributes = SpanAttributes::new().with("direction", "out");
	let counter = NOOP_TELEMETRY_METRICS.counter("bytes", options);
	let other = NOOP_TELEMETRY_METRICS.counter("other", InstrumentOptions::default());
	assert!(Arc::ptr_eq(&counter, &other));
	counter.add(1.0, &attributes);
	NOOP_TELEMETRY_METRICS
		.up_down_counter("active", options)
		.add(-1.0, &attributes);
	NOOP_TELEMETRY_METRICS
		.histogram("duration", HistogramOptions::default())
		.record(1.0, &attributes, Some(&NOOP_TELEMETRY_CONTEXT));
}
