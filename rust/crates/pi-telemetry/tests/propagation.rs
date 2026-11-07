//! Propagation and links (`propagation.rs`, with the in-memory adapter); ports
//! `packages/telemetry/test/propagation.test.ts`, including the second process.

use pi_telemetry::{
	InMemoryTelemetryContext, NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_PROPAGATION, SpanAttributes, SpanLink,
	SpanOptions, TelemetryCarrier, TelemetryContext, TelemetryPropagation, TelemetrySpan,
};
use std::process::Command;

/// A carrier as a transport writes it.
fn carrier_text(carrier: &TelemetryCarrier) -> String {
	pi_json::to_js_string(&serde_json::to_value(carrier).unwrap_or_else(|error| panic!("{error}")))
}

/// A carrier as a transport reads it.
fn read_carrier(text: &str) -> TelemetryCarrier {
	let value = pi_json::parse(text).unwrap_or_else(|error| panic!("{error}: {text}"));
	serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}: {text}"))
}

/// The worker process of the first case: when this variable holds a carrier, `propagation_worker` continues it in
/// a fresh recorder and prints the parent fields of what it recorded.
const WORKER_CARRIER: &str = "PI_TELEMETRY_WORKER_CARRIER";

#[test]
fn propagation_worker() {
	let Ok(carrier) = std::env::var(WORKER_CARRIER) else {
		return;
	};
	let carrier = read_carrier(&carrier);
	let telemetry = InMemoryTelemetryContext::new();
	telemetry
		.extract(Some(&carrier))
		.start_span(SpanOptions::new("remote"))
		.end();
	let spans = telemetry.get_spans();
	let recorded = serde_json::json!({
		"count": spans.len(),
		"parentId": spans[0].parent_id,
		"parentContext": spans[0].parent_context,
		"context": spans[0].context,
	});
	println!("WORKER {}", pi_json::to_js_string(&recorded));
}

#[test]
fn continues_a_parent_in_another_process_without_shared_recorder_state() {
	let telemetry = InMemoryTelemetryContext::new();
	let span = telemetry.start_span(SpanOptions::new("caller"));
	let carrier = telemetry.inject(span.as_ref()).unwrap();
	span.end();
	let output = Command::new(std::env::current_exe().unwrap())
		.args(["--exact", "propagation_worker", "--nocapture", "--test-threads=1"])
		.env(WORKER_CARRIER, carrier_text(&carrier))
		.output()
		.unwrap();
	let stdout = String::from_utf8(output.stdout).unwrap();
	let line = stdout
		.lines()
		// The test runner may print the test name on the same line.
		.find_map(|line| line.split_once("WORKER ").map(|(_, recorded)| recorded))
		.unwrap_or_else(|| panic!("no worker output: {stdout} {}", String::from_utf8_lossy(&output.stderr)));
	let recorded = pi_json::parse(line).unwrap();
	assert_eq!(recorded["count"], 1);
	assert_eq!(recorded["parentContext"], serde_json::to_value(&carrier).unwrap());
	assert!(recorded["parentId"].is_null());
	assert_ne!(recorded["context"], serde_json::to_value(&carrier).unwrap());
}

#[test]
fn isolates_crossed_concurrent_remote_parents_and_links_shared_roots() {
	let (caller, provider) = (InMemoryTelemetryContext::new(), InMemoryTelemetryContext::new());
	let first_trigger = caller.start_span(SpanOptions::new("first"));
	let later_trigger = caller.start_span(SpanOptions::new("later"));
	let first_parent = provider.extract(caller.inject(first_trigger.as_ref()).as_ref());
	let later_parent = provider.extract(caller.inject(later_trigger.as_ref()).as_ref());
	let first_getter = first_parent.start_span(SpanOptions::new("first.method"));
	let activation =
		provider.start_span(SpanOptions::new("activation").with_link(SpanLink::new(first_getter.as_ref())));
	first_getter.add_link(SpanLink::new(activation.as_ref()));
	let later_getter = later_parent.start_span(SpanOptions::new("later.method"));
	later_getter.add_link(SpanLink::new(activation.as_ref()));
	for span in [activation, later_getter, first_getter, later_trigger, first_trigger] {
		span.end();
	}
	let spans = provider.get_spans();
	let find = |name: &str| spans.iter().find(|span| span.name == name).unwrap();
	let callers = caller.get_spans();
	assert_eq!(find("activation").parent_context, None);
	assert_eq!(find("first.method").parent_context.as_ref(), Some(&callers[0].context));
	assert_eq!(find("later.method").parent_context.as_ref(), Some(&callers[1].context));
	assert_eq!(find("activation").links[0].context, find("first.method").context);
	assert_eq!(find("later.method").links[0].context, find("activation").context);
}

#[test]
fn detaches_carrier_and_link_snapshots_and_retains_settled_target_identities() {
	let telemetry = InMemoryTelemetryContext::new();
	let span = telemetry.start_span(SpanOptions::new("target"));
	let target = span.handle();
	span.end();
	let original = telemetry.inject(target.as_ref());
	let mut injected = telemetry.inject(target.as_ref()).unwrap();
	injected.insert("pi-memory-span".into(), "999".into());
	assert_eq!(telemetry.inject(target.as_ref()), original);
	let link = SpanLink::new(target.as_ref()).with_attributes(SpanAttributes::new().with("tags", vec!["original"]));
	telemetry.start_span(SpanOptions::new("linked").with_link(link)).end();
	let mut spans = telemetry.get_spans();
	spans[0].context.insert("pi-memory-span".into(), "123".into());
	spans[1].links[0].attributes.set("tags", vec!["mutated"]);
	let fresh = &telemetry.get_spans()[1].links[0];
	assert_eq!(Some(&fresh.context), original.as_ref());
	assert_eq!(fresh.attributes, SpanAttributes::new().with("tags", vec!["original"]));
}

#[test]
fn links_a_new_root_to_an_extracted_remote_target() {
	let (sender, receiver) = (InMemoryTelemetryContext::new(), InMemoryTelemetryContext::new());
	let trigger = sender.start_span(SpanOptions::new("trigger"));
	let carrier = sender.inject(trigger.as_ref()).unwrap();
	trigger.end();
	let wire = read_carrier(&carrier_text(&carrier));
	let target = receiver.extract(Some(&wire));
	let link = SpanLink::new(target.as_ref()).with_attributes(SpanAttributes::new().with("reason", "restart"));
	let background = receiver.start_span(SpanOptions::new("background").with_link(link));
	background.add_link(SpanLink::new(target.as_ref()));
	background.end();
	let recorded = &receiver.get_spans()[0];
	assert_eq!(recorded.parent_context, None);
	assert_eq!(recorded.links.len(), 2);
	assert_eq!(recorded.links[0].context, carrier);
	assert_eq!(
		recorded.links[0].attributes,
		SpanAttributes::new().with("reason", "restart")
	);
	assert_eq!(recorded.links[1].attributes, SpanAttributes::new());
}

#[test]
fn uses_the_root_for_absent_and_unknown_carriers() {
	let telemetry = InMemoryTelemetryContext::new();
	let unknown = TelemetryCarrier::from([("unknown".to_owned(), "format".to_owned())]);
	for carrier in [None, Some(TelemetryCarrier::new()), Some(unknown.clone())] {
		let root = telemetry.extract(carrier.as_ref());
		assert_eq!(telemetry.inject(root.as_ref()), None);
		root.start_span(SpanOptions::new("root")).end();
	}
	assert!(telemetry.get_spans().iter().all(|span| span.parent_context.is_none()));
	assert_eq!(NOOP_TELEMETRY_PROPAGATION.inject(&NOOP_TELEMETRY_CONTEXT), None);
	let noop = NOOP_TELEMETRY_PROPAGATION.extract(Some(&unknown));
	assert_eq!(NOOP_TELEMETRY_PROPAGATION.inject(noop.as_ref()), None);
	let span = NOOP_TELEMETRY_CONTEXT.start_span(SpanOptions::new("noop"));
	span.add_link(SpanLink::new(span.as_ref() as &dyn TelemetrySpan));
	span.end();
}

#[test]
fn treats_recognized_carrier_identifiers_as_opaque_strings() {
	let telemetry = InMemoryTelemetryContext::new();
	let carrier = TelemetryCarrier::from([
		("pi-memory-scope".to_owned(), "external-recorder".to_owned()),
		("pi-memory-span".to_owned(), "external-span".to_owned()),
		("pi-memory-trace".to_owned(), "external-trace".to_owned()),
	]);
	telemetry
		.extract(Some(&carrier))
		.start_span(SpanOptions::new("receiver"))
		.end();
	assert_eq!(telemetry.get_spans()[0].parent_context, Some(carrier));
}

#[test]
fn a_span_handle_starts_children_of_the_open_span() {
	let telemetry = InMemoryTelemetryContext::new();
	let parent = telemetry.start_span(SpanOptions::new("parent"));
	let handle = parent.handle();
	handle.start_span(SpanOptions::new("child")).end();
	parent.end();
	let spans = telemetry.get_spans();
	assert_eq!(spans[1].parent_id, Some(spans[0].id));
	let _: &dyn TelemetryContext = handle.as_ref();
}
