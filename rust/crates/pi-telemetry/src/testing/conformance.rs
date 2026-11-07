//! The adapter conformance suite (ports `packages/telemetry/src/testing/conformance.ts` and `types.ts`): grouped,
//! runner-independent cases over a fixture factory, checking the observable span, link, propagation and metric
//! semantics on an adapter's normalized snapshots.
//!
//! The TS cases about callbacks map onto the Rust lifecycle: "admits once synchronously and preserves the result"
//! becomes "opens on start and settles ok on end"; "preserves synchronous and asynchronous rejection values" becomes
//! "a span dropped while panicking records an error" (a panic is the Rust throw); "ignores links added after source
//! settlement" has no case, because `end` consumes the span and the compiler rejects later calls, and "makes calls
//! after settlement inert" keeps its one expressible part, a child started from a settled span's handle.

use crate::context::{AttributeValue, SpanAttributes, SpanLink, SpanOptions, SpanStatus, TelemetryContext};
use crate::memory::{RecordedTelemetryLink, RecordedTelemetryMeasurement, RecordedTelemetrySpan};
use crate::metrics::{HistogramOptions, InstrumentKind, InstrumentOptions, TelemetryMetrics};
use crate::noop::NOOP_TELEMETRY_CONTEXT;
use crate::propagation::{TelemetryCarrier, TelemetryPropagation};
use std::fmt::Debug;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

/// A fresh adapter and the normalized readers of what it recorded, owned by one case; dropping it disposes of it.
pub trait TelemetryAdapterFixture {
	/// The adapter's root context.
	fn context(&self) -> &dyn TelemetryContext;

	/// The adapter's propagation, to run the propagation cases.
	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		None
	}

	/// The adapter's metrics, to run the metric cases.
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		None
	}

	/// The recorded measurements in order; required with `metrics`.
	fn get_metrics(&self) -> Option<Vec<RecordedTelemetryMeasurement>> {
		None
	}

	/// The recorded spans in start order (an exporter may flush first).
	fn get_spans(&self) -> Vec<RecordedTelemetrySpan>;
}

/// Creates an isolated fixture for one case.
pub type TelemetryAdapterFixtureFactory = Arc<dyn Fn() -> Box<dyn TelemetryAdapterFixture> + Send + Sync>;

/// Why a case failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConformanceFailure(pub String);

/// A grouped case that any test runner can execute.
pub struct TelemetryAdapterConformanceCase {
	/// The group, such as `links`.
	pub group: &'static str,
	/// The case name.
	pub name: &'static str,
	factory: TelemetryAdapterFixtureFactory,
	test: CaseTest,
}

type CaseTest = fn(&TelemetryAdapterFixtureFactory, &dyn TelemetryAdapterFixture) -> CaseResult;
type CaseResult = Result<(), ConformanceFailure>;

impl TelemetryAdapterConformanceCase {
	/// Runs the case on a fresh fixture.
	pub fn run(&self) -> CaseResult {
		let fixture = (self.factory)();
		(self.test)(&self.factory, fixture.as_ref())
	}
}

/// The cases of the callback adapter contract, as the Rust span lifecycle expresses it.
pub fn telemetry_adapter_conformance(factory: &TelemetryAdapterFixtureFactory) -> Vec<TelemetryAdapterConformanceCase> {
	let cases: [(&'static str, &'static str, CaseTest); 13] = [
		(
			"capabilities",
			"carries adapter capabilities through contexts and spans",
			capabilities,
		),
		(
			"metrics",
			"records ordered measurements and optional histogram exemplars",
			ordered_measurements,
		),
		(
			"metrics",
			"copies instrument metadata and measurement attributes",
			copied_metadata,
		),
		(
			"links",
			"records initial and late links without changing parents",
			initial_and_late_links,
		),
		(
			"propagation",
			"roundtrips parent identity through JSON without retaining the carrier",
			json_round_trip,
		),
		(
			"propagation",
			"absent and unknown carriers produce recorded roots",
			fallback_roots,
		),
		(
			"propagation",
			"propagates across fresh adapters and multiple hops",
			multiple_hops,
		),
		(
			"span lifecycle",
			"opens on start and settles ok on end",
			opens_and_settles,
		),
		(
			"span lifecycle",
			"a span dropped while panicking records an error",
			panic_records_error,
		),
		(
			"status",
			"uses last explicit status without automatic overwrite",
			explicit_status,
		),
		(
			"recording",
			"merges attributes and records ordered events",
			merged_attributes,
		),
		(
			"recording",
			"makes calls after settlement inert",
			inert_after_settlement,
		),
		(
			"parentage",
			"records nested and concurrent child relationships",
			parentage,
		),
	];
	cases
		.into_iter()
		.map(|(group, name, test)| TelemetryAdapterConformanceCase {
			group,
			name,
			factory: factory.clone(),
			test,
		})
		.collect()
}

fn capabilities(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let metrics = fixture.metrics().or_else(|| fixture.context().metrics());
	let propagation = fixture.propagation().or_else(|| fixture.context().propagation());
	let check = |context: &dyn TelemetryContext, what: &str| {
		ensure(
			same(context.metrics(), metrics) && same(context.propagation(), propagation),
			|| format!("{what} does not carry the adapter's capabilities"),
		)
	};
	check(fixture.context(), "the root")?;
	let span = fixture.context().start_span(SpanOptions::new("capabilities"));
	check(span.as_ref(), "a span")?;
	let child = span.start_span(SpanOptions::new("capabilities.child"));
	check(child.as_ref(), "a child span")?;
	child.end();
	if let Some(propagation) = propagation {
		let parent = propagation.extract(propagation.inject(span.as_ref()).as_ref());
		check(parent.as_ref(), "an extracted parent")?;
		let extracted = parent.start_span(SpanOptions::new("capabilities.extracted"));
		check(extracted.as_ref(), "a span of an extracted parent")?;
		extracted.end();
	}
	let handle = span.handle();
	span.end();
	let inert = handle.start_span(SpanOptions::new("capabilities.inert"));
	check(inert.as_ref(), "a span of a settled span")
}

/// Whether two optional capabilities are the same object (or both absent).
fn same<T: ?Sized>(left: Option<&T>, right: Option<&T>) -> bool {
	match (left, right) {
		(Some(left), Some(right)) => std::ptr::addr_eq(left, right),
		(None, None) => true,
		_ => false,
	}
}

fn ordered_measurements(
	_factory: &TelemetryAdapterFixtureFactory,
	fixture: &dyn TelemetryAdapterFixture,
) -> CaseResult {
	let Some(metrics) = fixture.metrics() else {
		return Ok(());
	};
	let bytes_options = InstrumentOptions {
		description: Some("Encoded bytes"),
		unit: Some("By"),
	};
	metrics
		.counter("bytes", bytes_options)
		.add(12.0, &SpanAttributes::new().with("direction", "out"));
	let active = metrics.up_down_counter("active", InstrumentOptions::default());
	active.add(1.0, &SpanAttributes::new());
	active.add(-1.0, &SpanAttributes::new());
	let histogram_options = HistogramOptions {
		unit: Some("ms"),
		boundaries: Some(&[1.0, 10.0]),
		..HistogramOptions::default()
	};
	let histogram = metrics.histogram("duration", histogram_options);
	let span = fixture.context().start_span(SpanOptions::new("measured"));
	histogram.record(5.0, &SpanAttributes::new().with("method", "read"), Some(span.as_ref()));
	span.end();
	let measurements = measurements(fixture)?;
	let summary: Vec<(InstrumentKind, &str, f64)> = measurements
		.iter()
		.map(|measurement| (measurement.kind, measurement.name.as_str(), measurement.value))
		.collect();
	equal(
		&summary,
		&vec![
			(InstrumentKind::Counter, "bytes", 12.0),
			(InstrumentKind::UpDownCounter, "active", 1.0),
			(InstrumentKind::UpDownCounter, "active", -1.0),
			(InstrumentKind::Histogram, "duration", 5.0),
		],
	)?;
	equal(
		&measurements[0].attributes,
		&SpanAttributes::new().with("direction", "out"),
	)?;
	equal(&measurements[0].description.as_deref(), &Some("Encoded bytes"))?;
	equal(&measurements[0].unit.as_deref(), &Some("By"))?;
	equal(&measurements[3].boundaries, &Some(vec![1.0, 10.0]))?;
	let measured = find_span(&fixture.get_spans(), "measured")?.context;
	equal(&measurements[3].context, &Some(measured))
}

fn measurements(
	fixture: &dyn TelemetryAdapterFixture,
) -> Result<Vec<RecordedTelemetryMeasurement>, ConformanceFailure> {
	fixture
		.get_metrics()
		.ok_or_else(|| ConformanceFailure("metrics fixtures require a normalized measurement reader".into()))
}

fn copied_metadata(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let Some(metrics) = fixture.metrics() else {
		return Ok(());
	};
	let mut boundaries = vec![1.0, 10.0];
	let mut description = String::from("Original");
	let options = HistogramOptions {
		description: Some(&description),
		boundaries: Some(&boundaries),
		..HistogramOptions::default()
	};
	let histogram = metrics.histogram("snapshot", options);
	boundaries.push(100.0);
	description.replace_range(.., "Changed");
	let mut attributes = SpanAttributes::new().with("tags", vec!["original"]);
	histogram.record(-1.0, &attributes, None);
	attributes.set("tags", vec!["original", "changed"]);
	let measurements = measurements(fixture)?;
	equal(&measurements.len(), &1)?;
	equal(
		&measurements[0].attributes,
		&SpanAttributes::new().with("tags", vec!["original"]),
	)?;
	equal(&measurements[0].boundaries, &Some(vec![1.0, 10.0]))?;
	equal(&measurements[0].description.as_deref(), &Some("Original"))?;
	equal(&measurements[0].context, &None)
}

fn initial_and_late_links(
	_factory: &TelemetryAdapterFixtureFactory,
	fixture: &dyn TelemetryAdapterFixture,
) -> CaseResult {
	let target_span = fixture.context().start_span(SpanOptions::new("target"));
	let target = target_span.handle();
	target_span.end();
	let initial = SpanLink::new(target.as_ref()).with_attributes(SpanAttributes::new().with("reason", vec!["initial"]));
	let linked = fixture
		.context()
		.start_span(SpanOptions::new("linked").with_link(initial));
	linked.add_link(SpanLink::new(target.as_ref()).with_attributes(SpanAttributes::new().with("reason", "late")));
	linked.add_link(SpanLink::new(&NOOP_TELEMETRY_CONTEXT));
	linked.end();
	let spans = fixture.get_spans();
	let linked = find_span(&spans, "linked")?;
	let target = find_span(&spans, "target")?.context;
	equal(&linked.parent_id, &None)?;
	equal(&linked.parent_context, &None)?;
	equal(
		&linked.links,
		&vec![
			RecordedTelemetryLink {
				context: target.clone(),
				attributes: SpanAttributes::new().with("reason", vec!["initial"]),
			},
			RecordedTelemetryLink {
				context: target,
				attributes: SpanAttributes::new().with("reason", "late"),
			},
		],
	)
}

fn json_round_trip(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let Some(propagation) = fixture.propagation() else {
		return Ok(());
	};
	let sender = fixture.context().start_span(SpanOptions::new("sender"));
	let carrier = propagation
		.inject(sender.as_ref())
		.ok_or_else(|| ConformanceFailure("a span injects a carrier".into()))?;
	let mut wire = through_json(&carrier)?;
	let parent = propagation.extract(Some(&wire));
	for value in wire.values_mut() {
		*value = "mutated".into();
	}
	parent.start_span(SpanOptions::new("receiver")).end();
	equal(&propagation.inject(sender.as_ref()), &Some(carrier))?;
	sender.end();
	let spans = fixture.get_spans();
	equal(
		&find_span(&spans, "receiver")?.parent_context,
		&Some(find_span(&spans, "sender")?.context),
	)
}

/// The carrier after a trip through JSON text, as a transport sends it.
fn through_json(carrier: &TelemetryCarrier) -> Result<TelemetryCarrier, ConformanceFailure> {
	let failure = |error: serde_json::Error| ConformanceFailure(format!("carrier is not JSON-safe: {error}"));
	let text = pi_json::to_js_string(&serde_json::to_value(carrier).map_err(failure)?);
	serde_json::from_value(pi_json::parse(&text).map_err(failure)?).map_err(failure)
}

fn fallback_roots(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let Some(propagation) = fixture.propagation() else {
		return Ok(());
	};
	equal(&propagation.inject(&NOOP_TELEMETRY_CONTEXT), &None)?;
	let unknown = TelemetryCarrier::from([("unknown".to_owned(), "format".to_owned())]);
	let carriers = [None, Some(TelemetryCarrier::new()), Some(unknown)];
	for (index, carrier) in carriers.iter().enumerate() {
		let context = propagation.extract(carrier.as_ref());
		context.start_span(SpanOptions::new(format!("fallback-{index}"))).end();
	}
	for span in fixture.get_spans() {
		equal(&span.parent_context, &None)?;
	}
	Ok(())
}

fn multiple_hops(factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let Some(propagation) = fixture.propagation() else {
		return Ok(());
	};
	let (receiver, worker) = (factory(), factory());
	let (Some(receiver_propagation), Some(worker_propagation)) = (receiver.propagation(), worker.propagation()) else {
		return Err(ConformanceFailure("expected propagation on each fixture".into()));
	};
	let sender = fixture.context().start_span(SpanOptions::new("sender"));
	let sender_carrier = propagation.inject(sender.as_ref());
	sender.end();
	let sender_carrier = sender_carrier.ok_or_else(|| ConformanceFailure("the sender injects a carrier".into()))?;
	let receiver_span = receiver_propagation
		.extract(Some(&through_json(&sender_carrier)?))
		.start_span(SpanOptions::new("receiver"));
	let receiver_carrier = receiver_propagation.inject(receiver_span.as_ref());
	receiver_span.end();
	let receiver_carrier =
		receiver_carrier.ok_or_else(|| ConformanceFailure("the receiver injects a carrier".into()))?;
	worker_propagation
		.extract(Some(&through_json(&receiver_carrier)?))
		.start_span(SpanOptions::new("worker"))
		.end();
	equal(
		&find_span(&receiver.get_spans(), "receiver")?.parent_context,
		&Some(sender_carrier),
	)?;
	equal(
		&find_span(&worker.get_spans(), "worker")?.parent_context,
		&Some(receiver_carrier),
	)
}

fn opens_and_settles(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let span = fixture.context().start_span(SpanOptions::new("success"));
	equal(&find_span(&fixture.get_spans(), "success")?.settled, &false)?;
	span.end();
	let recorded = find_span(&fixture.get_spans(), "success")?;
	equal(&recorded.status, &SpanStatus::Ok)?;
	equal(&recorded.settled, &true)
}

fn panic_records_error(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	panic_in_span(fixture, "panicked", |_| {});
	let recorded = find_span(&fixture.get_spans(), "panicked")?;
	equal(&recorded.status, &SpanStatus::Error(None))?;
	equal(&recorded.settled, &true)
}

/// Starts a span called `name`, runs `before` on it, then unwinds with the span in scope, as a panic would.
fn panic_in_span(
	fixture: &dyn TelemetryAdapterFixture,
	name: &str,
	before: impl FnOnce(&dyn crate::context::TelemetrySpan),
) {
	let unwound = catch_unwind(AssertUnwindSafe(|| {
		let span = fixture.context().start_span(SpanOptions::new(name));
		before(span.as_ref());
		// `resume_unwind` unwinds like a panic without running the panic hook, so the case prints nothing.
		resume_unwind(Box::new("expected"));
	}));
	debug_assert!(unwound.is_err());
}

fn explicit_status(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let span = fixture.context().start_span(SpanOptions::new("last-status"));
	span.set_status(SpanStatus::error("Expected", "first"));
	span.set_status(SpanStatus::Ok);
	span.end();
	panic_in_span(fixture, "explicit-before-panic", |span| span.set_status(SpanStatus::Ok));
	panic_in_span(fixture, "explicit-error-before-panic", |span| {
		span.set_status(SpanStatus::error("Expected", "async failure"));
	});
	let span = fixture.context().start_span(SpanOptions::new("expected-failure"));
	span.set_status(SpanStatus::error("Expected", "returned failure"));
	span.end();
	let spans = fixture.get_spans();
	equal(&find_span(&spans, "last-status")?.status, &SpanStatus::Ok)?;
	equal(&find_span(&spans, "explicit-before-panic")?.status, &SpanStatus::Ok)?;
	equal(
		&find_span(&spans, "explicit-error-before-panic")?.status,
		&SpanStatus::error("Expected", "async failure"),
	)?;
	equal(
		&find_span(&spans, "expected-failure")?.status,
		&SpanStatus::error("Expected", "returned failure"),
	)
}

fn merged_attributes(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let start = SpanAttributes::new().with("start", "value").with("overwrite", "start");
	let span = fixture
		.context()
		.start_span(SpanOptions::new("recording").with_attributes(start));
	span.set_attributes(SpanAttributes::new().with("count", 1).with("overwrite", "middle"));
	// TS's `count: undefined` is ignored; in Rust an absent attribute is simply not set.
	span.set_attributes(SpanAttributes::new().with("overwrite", "end"));
	span.add_event("first", SpanAttributes::new().with("index", 1));
	span.add_event("second", SpanAttributes::new().with("index", 2));
	span.end();
	let recorded = find_span(&fixture.get_spans(), "recording")?;
	let expected = SpanAttributes::new()
		.with("start", "value")
		.with("overwrite", "end")
		.with("count", 1);
	equal(&recorded.attributes, &expected)?;
	let events: Vec<(&str, Option<&AttributeValue>)> = recorded
		.events
		.iter()
		.map(|event| (event.name.as_str(), event.attributes.get("index")))
		.collect();
	equal(
		&events,
		&vec![
			("first", Some(&AttributeValue::Number(1.0))),
			("second", Some(&AttributeValue::Number(2.0))),
		],
	)
}

fn inert_after_settlement(
	_factory: &TelemetryAdapterFixtureFactory,
	fixture: &dyn TelemetryAdapterFixture,
) -> CaseResult {
	let attributes = SpanAttributes::new().with("value", "initial");
	let span = fixture
		.context()
		.start_span(SpanOptions::new("settled").with_attributes(attributes.clone()));
	let handle = span.handle();
	span.end();
	let late = handle.start_span(SpanOptions::new("late-child"));
	late.set_attributes(SpanAttributes::new().with("value", "late"));
	late.end();
	let spans = fixture.get_spans();
	equal(&spans.len(), &1)?;
	equal(&spans[0].attributes, &attributes)?;
	equal(&spans[0].events.len(), &0)?;
	equal(&spans[0].status, &SpanStatus::Ok)
}

fn parentage(_factory: &TelemetryAdapterFixtureFactory, fixture: &dyn TelemetryAdapterFixture) -> CaseResult {
	let parent = fixture.context().start_span(SpanOptions::new("parent"));
	let first = parent.start_span(SpanOptions::new("first-child"));
	let second = parent.start_span(SpanOptions::new("second-child"));
	second.end();
	first.end();
	parent.end();
	let spans = fixture.get_spans();
	let (parent, first, second) = (
		find_span(&spans, "parent")?,
		find_span(&spans, "first-child")?,
		find_span(&spans, "second-child")?,
	);
	equal(&parent.parent_id, &None)?;
	equal(&first.parent_id, &Some(parent.id))?;
	equal(&second.parent_id, &Some(parent.id))?;
	let (Some(parent_end), Some(first_end), Some(second_end)) =
		(parent.end_sequence, first.end_sequence, second.end_sequence)
	else {
		return Err(ConformanceFailure("settled spans have end sequences".into()));
	};
	ensure(second_end < first_end && first_end < parent_end, || {
		format!("end order second {second_end} < first {first_end} < parent {parent_end}")
	})
}

fn find_span(spans: &[RecordedTelemetrySpan], name: &str) -> Result<RecordedTelemetrySpan, ConformanceFailure> {
	spans
		.iter()
		.find(|span| span.name == name)
		.cloned()
		.ok_or_else(|| ConformanceFailure(format!("expected recorded span {name}")))
}

fn equal<T: PartialEq + Debug>(actual: &T, expected: &T) -> CaseResult {
	ensure(actual == expected, || {
		format!("expected {expected:?}, found {actual:?}")
	})
}

fn ensure(condition: bool, message: impl FnOnce() -> String) -> CaseResult {
	if condition {
		Ok(())
	} else {
		Err(ConformanceFailure(message()))
	}
}
