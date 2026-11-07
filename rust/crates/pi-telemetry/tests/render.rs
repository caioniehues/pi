//! Trace rendering (`render.rs`); ports the goldens of `packages/telemetry/test/render.test.ts`.

use pi_telemetry::{SpanAttributes, SpanStatus, StoredEvent, StoredLink, StoredSpan, render_trace};

fn span(id: &str) -> StoredSpan {
	StoredSpan {
		id: id.to_owned(),
		trace_id: "trace".to_owned(),
		parent_id: None,
		name: id.to_owned(),
		started_ms: 0.0,
		ended_ms: 10.0,
		attributes: SpanAttributes::new(),
		events: Vec::new(),
		links: Vec::new(),
		status: SpanStatus::Ok,
	}
}

fn child(id: &str, parent: &str) -> StoredSpan {
	StoredSpan {
		parent_id: Some(parent.to_owned()),
		..span(id)
	}
}

fn timed(mut span: StoredSpan, started_ms: f64, ended_ms: f64) -> StoredSpan {
	span.started_ms = started_ms;
	span.ended_ms = ended_ms;
	span
}

fn event(name: &str, time_ms: f64) -> StoredEvent {
	StoredEvent {
		name: name.to_owned(),
		time_ms,
		attributes: SpanAttributes::new(),
	}
}

fn link(trace_id: &str, span_id: &str) -> StoredLink {
	StoredLink {
		trace_id: trace_id.to_owned(),
		span_id: span_id.to_owned(),
		attributes: None,
	}
}

#[test]
fn renders_an_unordered_trace_with_attributes_durations_errors_events_and_links() {
	let mut root = timed(span("root"), 1000.0, 1020.0);
	root.attributes = SpanAttributes::new()
		.with("request", "a b")
		.with("attempt", 2)
		.with("cached", false)
		.with("tags", vec!["a", "b"])
		.with("sizes", vec![1.0, 2.0])
		.with("flags", vec![true, false]);
	root.events = vec![
		StoredEvent {
			attributes: SpanAttributes::new().with("phase", "tools"),
			..event("checkpoint", 2.0)
		},
		event("tick", 3.5),
	];
	root.links = vec![
		StoredLink {
			attributes: Some(SpanAttributes::new().with("reason", "steer")),
			..link("remote-trace", "remote")
		},
		link("other-trace", "other"),
	];
	let mut first = timed(child("first", "root"), 1005.0, 1009.5);
	first.status = SpanStatus::error("Error", "failed call");
	let nested = timed(child("nested", "first"), 1006.0, 1007.0);
	let mut second = timed(child("second", "root"), 1010.0, 1012.0);
	second.status = SpanStatus::Error(None);
	let spans = [second, nested, first, root];
	let before = spans.clone();
	assert_eq!(
		render_trace(&spans),
		[
			r#"root request="a b" attempt=2 cached=false tags=["a","b"] sizes=[1,2] flags=[true,false] (20ms)"#,
			r#"  · checkpoint phase="tools" (+2ms)"#,
			"  · tick (+3.5ms)",
			r#"  → link trace remote-trace span remote reason="steer""#,
			"  → link trace other-trace span other",
			"  first (4.5ms) error Error: failed call",
			"    nested (1ms)",
			"  second (2ms) error",
		]
		.join("\n")
	);
	assert_eq!(spans, before);
}

#[test]
fn treats_missing_parents_as_roots_and_orders_roots_and_children_by_start_time() {
	let mut earlier = timed(span("earlier"), 10.0, 15.0);
	earlier.links = vec![link("trace", "later")];
	let spans = [
		timed(child("later", "external"), 20.0, 30.0),
		timed(child("child", "later"), 25.0, 30.0),
		earlier,
	];
	assert_eq!(
		render_trace(&spans),
		[
			"earlier (5ms)",
			"  → link trace trace span later",
			"later (10ms)",
			"  child (5ms)"
		]
		.join("\n")
	);
}

#[test]
fn prints_cross_trace_links_without_following_them_including_cycles() {
	let mut server = span("server:1");
	server.name = "server".into();
	server.trace_id = "server-trace".into();
	server.links = vec![link("worker-trace", "worker:1")];
	let mut worker = span("worker:1");
	worker.name = "worker".into();
	worker.trace_id = "worker-trace".into();
	worker.links = vec![link("server-trace", "server:1")];
	assert_eq!(
		render_trace(&[server, worker]),
		[
			"server (10ms)",
			"  → link trace worker-trace span worker:1",
			"worker (10ms)",
			"  → link trace server-trace span server:1",
		]
		.join("\n")
	);
}

#[test]
fn deduplicates_spans_merged_from_stores_by_global_id() {
	let mut root = span("server:1");
	root.name = "root".into();
	let mut child_span = child("worker:1", "server:1");
	child_span.name = "child".into();
	child_span.events = vec![event("done", 3.0)];
	let mut grandchild = child("worker:2", "worker:1");
	grandchild.name = "grandchild".into();
	assert_eq!(
		render_trace(&[child_span.clone(), root.clone(), grandchild, root, child_span]),
		"root (10ms)\n  child (10ms)\n    · done (+3ms)\n    grandchild (10ms)"
	);
}

#[test]
fn preserves_input_order_for_equal_start_times_without_mutating_the_input() {
	let spans = [span("root"), child("z", "root"), child("a", "root"), span("other")];
	assert_eq!(
		render_trace(&spans),
		"root (10ms)\n  z (10ms)\n  a (10ms)\nother (10ms)"
	);
}

#[test]
fn escapes_string_attribute_values_without_hiding_their_contents() {
	let mut root = span("root");
	root.attributes = SpanAttributes::new().with("text", "line one\n\"line two\"");
	assert_eq!(render_trace(&[root]), r#"root text="line one\n\"line two\"" (10ms)"#);
}

#[test]
fn renders_deep_trees_without_recursive_calls() {
	let spans: Vec<StoredSpan> = (0..2000)
		.map(|index| {
			if index == 0 {
				span("0")
			} else {
				child(&index.to_string(), &(index - 1).to_string())
			}
		})
		.collect();
	let rendered = render_trace(&spans);
	assert_eq!(rendered.split('\n').count(), spans.len());
	assert!(rendered.ends_with(&format!("{}1999 (10ms)", "  ".repeat(1999))));
}

#[test]
fn renders_lengths_of_time_to_the_microsecond_as_the_adapters_record_them() {
	let mut root = timed(span("root"), 1_793_789_452_816.123, 1_793_789_452_816.431);
	root.events = vec![event("half", 0.154_000_000_02)];
	assert_eq!(render_trace(&[root]), "root (0.308ms)\n  · half (+0.154ms)");
}

#[test]
fn renders_an_empty_input_as_an_empty_string() {
	assert_eq!(render_trace(&[]), "");
}

#[test]
fn escapes_control_characters_in_ids_names_attribute_keys_and_error_messages() {
	let hostile = "\u{1b}]0;PWNED\u{7}\u{1b}[2J";
	let mut root = span("root");
	root.name = format!("name{hostile}");
	root.attributes = SpanAttributes::new().with(format!("key{hostile}"), hostile);
	root.events = vec![event(&format!("event{hostile}"), 1.0)];
	root.links = vec![link(hostile, &format!("span\u{9b}{hostile}"))];
	root.status = SpanStatus::error(format!("error{hostile}"), format!("message\u{7f}{hostile}"));
	let rendered = render_trace(&[root]);
	for line in rendered.split('\n') {
		assert!(
			!line
				.chars()
				.any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')),
			"{line:?}"
		);
	}
	assert!(rendered.contains(r"name\u001b]0;PWNED\u0007\u001b[2J"));
	assert!(rendered.contains(r"→ link trace \u001b]0;PWNED"));
	assert!(rendered.contains(r"span\u009b"));
	assert!(rendered.contains(r"message\u007f"));
	assert!(rendered.contains(r#"key\u001b]0;PWNED\u0007\u001b[2J="\u001b]0;PWNED\u0007\u001b[2J""#));
}
