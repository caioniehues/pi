//! The in-memory adapter as a store (`memory.rs`, `store.rs`); ports `packages/telemetry/test/store.test.ts` and
//! the in-memory run of `store-conformance.ts`. Cases that mock `Date.now` assert against the recorded times.

use pi_telemetry::{
	InMemoryTelemetryContext, InstrumentOptions, MetricQuery, SpanAttributes, SpanLink, SpanOptions, SpanStatus,
	StoredLink, TelemetryContext, TelemetryMetrics, TelemetryPropagation, TelemetryStore, TelemetryStoreError,
	TraceQuery, telemetry_now,
};

/// Lets the clock move past the microsecond, so spans started after it start strictly later (TS mocks the clock).
fn tick() {
	std::thread::sleep(std::time::Duration::from_millis(2));
}

fn names(store: &InMemoryTelemetryContext, query: &TraceQuery) -> Vec<String> {
	let page = store.find_traces(query).unwrap_or_else(|error| panic!("{error}"));
	page.traces
		.into_iter()
		.map(|trace| trace.spans[0].name.clone())
		.collect()
}

#[test]
fn returns_only_settled_traces_complete_span_snapshots_and_detached_results() {
	let telemetry = InMemoryTelemetryContext::new();
	let root = telemetry
		.start_span(SpanOptions::new("root").with_attributes(SpanAttributes::new().with("tags", vec!["original"])));
	root.add_event("event", SpanAttributes::new().with("tags", vec!["original"]));
	assert_eq!(
		telemetry.find_traces(&TraceQuery::default()).unwrap().traces,
		Vec::new()
	);
	assert_eq!(telemetry.get_trace("missing").unwrap(), None);
	root.start_span(SpanOptions::new("child")).end();
	root.end();
	let mut trace = telemetry.find_traces(&TraceQuery::default()).unwrap().traces.remove(0);
	let root = trace.spans.iter().find(|span| span.name == "root").unwrap().clone();
	let child = trace.spans.iter().find(|span| span.name == "child").unwrap();
	let (scope, counter) = root.id.split_once(':').unwrap();
	assert_eq!(scope.len(), 36);
	assert!(counter.parse::<u64>().unwrap() >= 1);
	assert_eq!(root.trace_id, root.id);
	assert_eq!(root.parent_id, None);
	assert_eq!(child.parent_id.as_ref(), Some(&root.id));
	assert_eq!(child.trace_id, root.trace_id);
	assert!(root.ended_ms >= root.started_ms);
	assert!(root.events[0].time_ms >= 0.0);
	trace.spans[0].attributes.set("tags", vec!["changed"]);
	let fresh = telemetry.get_trace(&trace.trace_id).unwrap().unwrap();
	let fresh_root = fresh.spans.iter().find(|span| span.id == root.id).unwrap();
	assert_eq!(
		fresh_root.attributes,
		SpanAttributes::new().with("tags", vec!["original"])
	);
}

#[test]
fn filters_by_first_span_name_and_start_any_error_and_all_attributes_on_one_span() {
	let telemetry = InMemoryTelemetryContext::new();
	let old = telemetry.start_span(SpanOptions::new("old"));
	old.start_span(SpanOptions::new("recent-child").with_attributes(SpanAttributes::new().with("left", 1)))
		.end();
	old.start_span(SpanOptions::new("split").with_attributes(SpanAttributes::new().with("right", 2)))
		.end();
	old.end();
	tick();
	let wanted = telemetry.start_span(SpanOptions::new("wanted"));
	let failure_attributes = SpanAttributes::new()
		.with("left", 1)
		.with("right", 2)
		.with("valid", true)
		.with("odd.\"key", "value");
	let failure = wanted.start_span(SpanOptions::new("failure").with_attributes(failure_attributes));
	failure.set_status(SpanStatus::Error(None));
	failure.end();
	wanted.end();
	tick();
	telemetry
		.start_span(SpanOptions::new("new").with_attributes(SpanAttributes::new().with("valid", 1)))
		.end();
	let starts: Vec<f64> = telemetry
		.find_traces(&TraceQuery::default())
		.unwrap()
		.traces
		.iter()
		.map(|trace| trace.spans[0].started_ms)
		.collect();
	let (new_start, wanted_start, old_start) = (starts[0], starts[1], starts[2]);
	let attributes = |pairs: SpanAttributes| Some(pairs);
	let query = TraceQuery {
		name: Some("wanted".into()),
		attributes: attributes(SpanAttributes::new().with("left", 1).with("right", 2)),
		errors: true,
		start_min: Some(wanted_start),
		start_max: Some(wanted_start),
		..TraceQuery::default()
	};
	assert_eq!(names(&telemetry, &query), ["wanted"]);
	let by_name = |name: &str| TraceQuery {
		name: Some(name.into()),
		..TraceQuery::default()
	};
	assert_eq!(names(&telemetry, &by_name("failure")), Vec::<String>::new());
	let by_attributes = |wanted: SpanAttributes| TraceQuery {
		attributes: Some(wanted),
		..TraceQuery::default()
	};
	assert_eq!(
		names(
			&telemetry,
			&by_attributes(SpanAttributes::new().with("left", 1).with("right", 2))
		),
		["wanted"]
	);
	assert_eq!(
		names(
			&telemetry,
			&by_attributes(SpanAttributes::new().with("valid", true).with("odd.\"key", "value"))
		),
		["wanted"]
	);
	assert_eq!(
		names(&telemetry, &by_attributes(SpanAttributes::new().with("valid", 1))),
		["new"]
	);
	let errors = TraceQuery {
		errors: true,
		..TraceQuery::default()
	};
	assert_eq!(names(&telemetry, &errors), ["wanted"]);
	let from = TraceQuery {
		start_min: Some(wanted_start),
		..TraceQuery::default()
	};
	assert_eq!(names(&telemetry, &from), ["new", "wanted"]);
	let until = TraceQuery {
		start_max: Some(old_start),
		..TraceQuery::default()
	};
	assert_eq!(names(&telemetry, &until), ["old"]);
	assert!(new_start > wanted_start && wanted_start > old_start);
}

// TS: store-conformance.ts "pages complete traces without duplicates when trace starts tie", on the real clock; the
// tie itself is tested with fixed clocks in `src/memory.rs`.
#[test]
fn pages_complete_traces_without_duplicates() {
	let telemetry = InMemoryTelemetryContext::new();
	for index in 0..25 {
		let root =
			telemetry.start_span(SpanOptions::new("root").with_attributes(SpanAttributes::new().with("index", index)));
		root.start_span(SpanOptions::new("child")).end();
		root.end();
	}
	let first = telemetry.find_traces(&TraceQuery::default()).unwrap();
	assert_eq!(first.traces.len(), 20);
	let rest = telemetry
		.find_traces(&TraceQuery {
			cursor: first.cursor.clone(),
			..TraceQuery::default()
		})
		.unwrap();
	assert_eq!(rest.traces.len(), 5);
	assert_eq!(rest.cursor, None);
	let all = telemetry
		.find_traces(&TraceQuery {
			limit: Some(100),
			..TraceQuery::default()
		})
		.unwrap()
		.traces;
	assert_eq!([first.traces, rest.traces].concat(), all);
	assert!(all.iter().all(|trace| trace.spans.len() == 2));
	let (mut ids, mut cursor) = (Vec::new(), None);
	loop {
		let page = telemetry
			.find_traces(&TraceQuery {
				name: Some("root".into()),
				limit: Some(3),
				cursor,
				..TraceQuery::default()
			})
			.unwrap();
		ids.extend(page.traces.into_iter().map(|trace| trace.trace_id));
		cursor = page.cursor;
		if cursor.is_none() {
			break;
		}
	}
	assert_eq!(ids, all.iter().map(|trace| trace.trace_id.clone()).collect::<Vec<_>>());
	let invalid = TraceQuery {
		cursor: Some("not a cursor".into()),
		..TraceQuery::default()
	};
	assert!(matches!(
		telemetry.find_traces(&invalid),
		Err(TelemetryStoreError::InvalidCursor(_))
	));
}

#[test]
fn inherits_remote_trace_ids_and_preserves_remote_parents_and_cross_trace_links() {
	let (sender, receiver) = (InMemoryTelemetryContext::new(), InMemoryTelemetryContext::new());
	let span = sender.start_span(SpanOptions::new("sender"));
	let carrier = sender.inject(span.as_ref()).unwrap();
	span.end();
	let sent = sender.find_traces(&TraceQuery::default()).unwrap().traces.remove(0);
	let remote = receiver.extract(Some(&carrier));
	remote.start_span(SpanOptions::new("receiver")).end();
	let link = SpanLink::new(remote.as_ref()).with_attributes(SpanAttributes::new().with("reason", "background"));
	receiver.start_span(SpanOptions::new("linked").with_link(link)).end();
	let continued = receiver.get_trace(&sent.trace_id).unwrap().unwrap();
	assert_eq!(continued.spans.len(), 1);
	assert_eq!(continued.spans[0].trace_id, sent.trace_id);
	assert_eq!(continued.spans[0].parent_id.as_ref(), Some(&sent.spans[0].id));
	assert_eq!(continued.spans[0].name, "receiver");
	assert_ne!(continued.spans[0].id, sent.spans[0].id);
	let by_name = |name: &str| TraceQuery {
		name: Some(name.into()),
		..TraceQuery::default()
	};
	let linked = receiver.find_traces(&by_name("linked")).unwrap().traces.remove(0);
	assert_ne!(linked.trace_id, sent.trace_id);
	assert_eq!(
		linked.spans[0].links,
		[StoredLink {
			trace_id: sent.trace_id.clone(),
			span_id: sent.spans[0].id.clone(),
			attributes: Some(SpanAttributes::new().with("reason", "background")),
		}]
	);
	// TS also checks non-string carrier values; a `TelemetryCarrier` holds only strings.
	for key in carrier.keys() {
		let mut incomplete = carrier.clone();
		incomplete.remove(key);
		receiver
			.extract(Some(&incomplete))
			.start_span(SpanOptions::new("fallback"))
			.end();
	}
	let fallbacks = receiver.find_traces(&by_name("fallback")).unwrap().traces;
	assert_eq!(fallbacks.len(), carrier.len());
	assert!(
		fallbacks
			.iter()
			.all(|trace| trace.spans[0].parent_id.is_none() && trace.trace_id == trace.spans[0].id)
	);
}

#[test]
fn accepts_external_string_identities_without_imposing_an_id_format() {
	let telemetry = InMemoryTelemetryContext::new();
	let span = telemetry.start_span(SpanOptions::new("source"));
	let carrier = telemetry.inject(span.as_ref()).unwrap();
	span.end();
	let external = carrier
		.keys()
		.map(|key| (key.clone(), format!("external-{key}")))
		.collect();
	let parent = telemetry.extract(Some(&external));
	assert_eq!(telemetry.inject(parent.as_ref()), Some(external));
	parent.start_span(SpanOptions::new("continued")).end();
	let query = TraceQuery {
		name: Some("continued".into()),
		..TraceQuery::default()
	};
	let trace = telemetry.find_traces(&query).unwrap().traces.remove(0);
	assert!(trace.trace_id.starts_with("external-"));
	assert!(trace.spans[0].parent_id.as_ref().unwrap().starts_with("external-"));
}

#[test]
fn queries_metric_names_interval_bounds_typed_attribute_equality_and_detached_values() {
	let telemetry = InMemoryTelemetryContext::new();
	let metrics = telemetry.metrics().unwrap();
	let counter = metrics.counter(
		"hits",
		InstrumentOptions {
			unit: Some("By"),
			..InstrumentOptions::default()
		},
	);
	let read = SpanAttributes::new().with("method", "read").with("valid", true);
	counter.add(2.0, &read);
	counter.add(3.0, &SpanAttributes::new().with("valid", true).with("method", "read"));
	counter.add(11.0, &SpanAttributes::new().with("method", "write").with("valid", 1));
	metrics.counter("other", InstrumentOptions::default()).add(100.0, &read);
	let query = MetricQuery {
		name: "hits".into(),
		attributes: Some(read.clone()),
		..MetricQuery::default()
	};
	let mut rows = telemetry.find_metrics(&query).unwrap();
	assert_eq!(rows.iter().map(|row| row.count).sum::<u64>(), 2);
	assert_eq!(rows.iter().map(|row| row.sum).sum::<f64>(), 5.0);
	assert!(
		rows.iter()
			.all(|row| row.unit.as_deref() == Some("By") && row.interval_ms % 10_000 == 0)
	);
	let interval = rows[0].interval_ms;
	let bounded = MetricQuery {
		start_min: Some(interval),
		start_max: Some(interval),
		..query.clone()
	};
	// Bounds are inclusive interval starts; the two measurements may straddle an interval boundary.
	let in_interval = rows.iter().filter(|row| row.interval_ms == interval).count();
	assert_eq!(telemetry.find_metrics(&bounded).unwrap().len(), in_interval);
	let last = rows.iter().map(|row| row.interval_ms).max().unwrap();
	let later = MetricQuery {
		start_min: Some(last + 10_000),
		..query.clone()
	};
	assert_eq!(telemetry.find_metrics(&later).unwrap(), Vec::new());
	rows[0].attributes.set("method", "changed");
	assert_eq!(telemetry.find_metrics(&query).unwrap()[0].attributes, read);
	let missing = MetricQuery {
		name: "missing".into(),
		..MetricQuery::default()
	};
	assert_eq!(telemetry.find_metrics(&missing).unwrap(), Vec::new());
	let numeric = MetricQuery {
		name: "hits".into(),
		attributes: Some(SpanAttributes::new().with("valid", 1)),
		..MetricQuery::default()
	};
	assert_eq!(
		telemetry
			.find_metrics(&numeric)
			.unwrap()
			.iter()
			.map(|row| row.sum)
			.sum::<f64>(),
		11.0
	);
}

#[test]
fn maps_raw_metric_measurements_to_count_one_rows() {
	let telemetry = InMemoryTelemetryContext::new();
	telemetry
		.counter("calls", InstrumentOptions::default())
		.add(2.0, &SpanAttributes::new());
	telemetry
		.counter("calls", InstrumentOptions::default())
		.add(3.0, &SpanAttributes::new());
	let query = MetricQuery {
		name: "calls".into(),
		..MetricQuery::default()
	};
	let rows: Vec<(u64, f64)> = telemetry
		.find_metrics(&query)
		.unwrap()
		.iter()
		.map(|row| (row.count, row.sum))
		.collect();
	assert_eq!(rows, [(1, 2.0), (1, 3.0)]);
	assert_eq!(telemetry.get_metrics().len(), 2);
}

#[test]
fn stamps_starts_with_telemetry_now_in_their_order() {
	let telemetry = InMemoryTelemetryContext::new();
	let before = telemetry_now();
	telemetry.start_span(SpanOptions::new("first")).end();
	tick();
	telemetry.start_span(SpanOptions::new("second")).end();
	let spans: Vec<_> = telemetry
		.find_traces(&TraceQuery::default())
		.unwrap()
		.traces
		.into_iter()
		.flat_map(|trace| trace.spans)
		.collect();
	assert_eq!(spans[0].name, "second");
	assert!(spans[1].started_ms >= before && spans[0].started_ms > spans[1].started_ms);
	for span in &spans {
		assert_eq!((span.started_ms * 1000.0).fract(), 0.0);
		assert!(span.ended_ms >= span.started_ms);
	}
}

#[test]
fn detaches_link_attributes_and_error_details() {
	let telemetry = InMemoryTelemetryContext::new();
	let target = telemetry.start_span(SpanOptions::new("target"));
	let handle = target.handle();
	target.end();
	let link = SpanLink::new(handle.as_ref()).with_attributes(SpanAttributes::new().with("tags", vec!["original"]));
	let source = telemetry.start_span(SpanOptions::new("source").with_link(link));
	source.set_status(SpanStatus::error("Expected", "original"));
	source.end();
	let query = TraceQuery {
		name: Some("source".into()),
		..TraceQuery::default()
	};
	let mut trace = telemetry.find_traces(&query).unwrap().traces.remove(0);
	trace.spans[0].status = SpanStatus::error("Expected", "changed");
	let fresh = telemetry.get_trace(&trace.trace_id).unwrap().unwrap();
	assert_eq!(
		fresh.spans[0].links[0].attributes,
		Some(SpanAttributes::new().with("tags", vec!["original"]))
	);
	assert_eq!(fresh.spans[0].status, SpanStatus::error("Expected", "original"));
}
