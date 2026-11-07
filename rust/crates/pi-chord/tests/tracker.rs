//! The tracker (src/delta/tracker.rs), ported from `packages/chord/test/delta-tracker/tracker.test.ts`,
//! `delta.test.ts` (tracker lifecycle, canonical strings), `delta-append-text.test.ts`, `delta-clone.test.ts`,
//! `delta-op-text-retention.test.ts`, `delta-tracker/retention.test.ts` and the tracker cases of
//! `delta-apply-immutable.test.ts`, under the decided rule (rust/docs/slices.md, D0b): values equal the TS draft's,
//! ops replay to them and are valid, and op shapes are asserted only where the TS test's intent is a minimality bound.
//!
//! Not ported, with the reason (each is a JS-object or proxy behaviour Rust values do not have): native reads,
//! descriptors, `then`, `isProxy` and proxy identity per container; argument coercion (`valueOf`) and comparators that
//! write the draft or restructure it during `sort` (a Rust comparator borrows the elements; the draft cannot be
//! reached from it); borrowed mutators on other receivers; prototypes, null-prototype objects, inherited setters and
//! accessors; non-strict JSON placements other than `undefined` (a `Snapshot` is strict JSON by type); held handles
//! that follow an element through reindexing (Rust drafts address by path); mutable permutation metadata (ops are
//! immutable); payload sharing between ops and the candidate (D0a carry-over, D2b); `WeakRef` counts and V8 heap
//! measurements of retained strings (owned Rust strings never retain a larger one).

use anyhow::{Context, Result};
use pi_chord::delta::{
	Change, Decoder, DraftError, Encoder, Op, Prepared, Seg, Snapshot, Tracker, TrackerError, apply, apply_immutable,
	apply_immutable_batches, assert_valid_op, track,
};
use pi_json::{JsonValue, to_js_string};
use serde_json::json;
use std::cmp::Ordering;
use std::sync::Arc;

fn snap(value: JsonValue) -> Snapshot {
	Snapshot::from(value)
}

/// A path segment from a test literal.
trait IntoSeg {
	fn seg(self) -> Seg;
}

impl IntoSeg for &str {
	fn seg(self) -> Seg {
		Seg::Key(self.to_owned())
	}
}

impl IntoSeg for usize {
	fn seg(self) -> Seg {
		Seg::Index(self as u64)
	}
}

impl IntoSeg for i32 {
	fn seg(self) -> Seg {
		Seg::Index(ok(u64::try_from(self)))
	}
}

macro_rules! p {
	($($segment:expr),* $(,)?) => { [$(IntoSeg::seg($segment)),*] };
}

/// The value of a result a test helper expects to succeed.
fn ok<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
	result.unwrap_or_else(|error| panic!("{error:?}"))
}

fn ops(value: JsonValue) -> Vec<Op> {
	ok(serde_json::from_value(value))
}

fn ops_json(prepared: &Prepared) -> JsonValue {
	JsonValue::Array(prepared.ops().iter().map(Op::to_json).collect())
}

/// The ops are valid and replay to the candidate in place and persistently, bytes (key order) included.
fn check_replay(prepared: &Prepared) {
	for op in prepared.ops().iter() {
		ok(assert_valid_op(op.to_json()));
	}
	let expected = to_js_string(&prepared.value().to_value());
	let mut in_place = prepared.base().to_value();
	ok(apply(&mut in_place, prepared.ops()));
	assert_eq!(to_js_string(&in_place), expected);
	let persistent = ok(apply_immutable(prepared.base(), prepared.ops()));
	assert_eq!(to_js_string(&persistent.to_value()), expected);
}

/// TS `settle`: one change, prepared, checked, adopted; returns the preparation.
fn settle(tracker: &mut Tracker, mutate: impl FnOnce(&mut Change)) -> Prepared {
	let base = tracker.value().clone();
	let base_json = base.to_value();
	let mut change = tracker.begin_change();
	mutate(&mut change);
	let prepared = ok(change.prepare());
	check_replay(&prepared);
	assert_eq!(base.to_value(), base_json);
	ok(tracker.adopt(&prepared));
	assert_eq!(base.to_value(), base_json);
	assert_eq!(tracker.value(), prepared.value());
	prepared
}

fn value_at(tracker: &Tracker, path: &[Seg]) -> JsonValue {
	tracker.value().at(path).map_or(JsonValue::Null, Snapshot::to_value)
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > materializes an immutable next revision and
// adopts it by pointer swap (and delta.test.ts > keeps a draft alive across await and adopts only a prepared change)
#[test]
fn materializes_an_immutable_next_revision_and_adopts_it_by_pointer_swap() {
	let initial = snap(json!({"count": 1, "nested": {"text": "a"}, "values": [1]}));
	let mut tracker = track(initial.clone());
	assert!(tracker.value().ptr_eq(&initial));
	let mut change = tracker.begin_change();
	let draft = change.state();
	draft.set(&p!["count"], Some(snap(json!(2)))).unwrap();
	let text = format!(
		"{}b",
		draft.get(&p!["nested", "text"]).unwrap().unwrap().as_str().unwrap()
	);
	draft.set(&p!["nested", "text"], Some(Snapshot::string(&text))).unwrap();
	draft.push(&p!["values"], vec![snap(json!(2))]).unwrap();
	assert_eq!(
		initial.to_value(),
		json!({"count": 1, "nested": {"text": "a"}, "values": [1]})
	);
	let prepared = change.prepare().unwrap();
	assert_eq!(prepared.base_revision(), 0);
	assert!(prepared.base().ptr_eq(&initial));
	assert_eq!(
		prepared.value().to_value(),
		json!({"count": 2, "nested": {"text": "ab"}, "values": [1, 2]})
	);
	assert_eq!(
		ops_json(&prepared),
		json!([
			["s", ["count"], 2],
			["a", ["nested", "text"], "b"],
			["p", ["values"], 1, 0, [2]]
		])
	);
	check_replay(&prepared);
	assert!(tracker.value().ptr_eq(&initial));
	tracker.adopt(&prepared).unwrap();
	assert!(tracker.value().ptr_eq(prepared.value()));
	assert_eq!(
		initial.to_value(),
		json!({"count": 1, "nested": {"text": "a"}, "values": [1]})
	);
	assert_eq!(change.state().get(&p!["count"]), Err(DraftError::Settled));
	assert_eq!(
		change.state().set(&p!["count"], Some(snap(json!(3)))),
		Err(DraftError::Settled)
	);
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > aborts idempotently and revokes held
// descendants (and delta.test.ts > aborts and revokes without changing the committed value)
#[test]
fn aborts_idempotently_and_revokes_the_draft() {
	let mut tracker = track(snap(json!({"child": {"value": 1}})));
	let mut change = tracker.begin_change();
	change.state().set(&p!["child", "value"], Some(snap(json!(2)))).unwrap();
	change.abort();
	change.abort();
	assert_eq!(value_at(&tracker, &p!["child", "value"]), json!(1));
	assert_eq!(change.state().get(&p!["child"]), Err(DraftError::Settled));
	assert_eq!(change.prepare().unwrap_err(), TrackerError::AlreadySettled);
	tracker.begin_change().abort();
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > allows competing contexts and invalidates
// loser views (and delta.test.ts > rejects foreign, stale, and repeated preparations while allowing competing changes)
#[test]
fn allows_competing_contexts_and_invalidates_loser_views() {
	let mut tracker = track(snap(json!({"value": 0})));
	let mut first = tracker.begin_change();
	let mut second = tracker.begin_change();
	first.state().set(&p!["value"], Some(snap(json!(1)))).unwrap();
	second.state().set(&p!["value"], Some(snap(json!(2)))).unwrap();
	let first_prepared = first.prepare().unwrap();
	let second_prepared = second.prepare().unwrap();
	let held = second_prepared.value().clone();
	let other = track(snap(json!({"value": 0})));
	let mut other = other;
	assert_eq!(other.adopt(&first_prepared), Err(TrackerError::ForeignTracker));
	tracker.adopt(&first_prepared).unwrap();
	assert_eq!(value_at(&tracker, &p!["value"]), json!(1));
	assert_eq!(held.to_value(), json!({"value": 2}));
	assert_eq!(tracker.adopt(&second_prepared), Err(TrackerError::Stale));
	assert_eq!(tracker.adopt(&first_prepared), Err(TrackerError::Consumed));

	let stale = tracker.prepare_replace(snap(json!({"value": 2})));
	let winner = tracker.prepare_replace(snap(json!({"value": 3})));
	tracker.adopt(&winner).unwrap();
	assert_eq!(tracker.adopt(&stale), Err(TrackerError::Stale));
	assert_eq!(tracker.adopt(&stale), Err(TrackerError::Stale));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > invalidates competing open overlays without
// changing their base revision
#[test]
fn invalidates_competing_open_overlays_without_changing_their_base_revision() {
	let initial = snap(json!({"rows": [{"value": 0}, {"value": 1}]}));
	let mut tracker = track(initial.clone());
	let mut stale = tracker.begin_change();
	stale
		.state()
		.set(&p!["rows", 1, "value"], Some(snap(json!(2))))
		.unwrap();
	let mut winner = tracker.begin_change();
	winner
		.state()
		.set(&p!["rows", 0, "value"], Some(snap(json!(3))))
		.unwrap();
	tracker.adopt(&winner.prepare().unwrap()).unwrap();
	assert_eq!(initial.to_value(), json!({"rows": [{"value": 0}, {"value": 1}]}));
	assert_eq!(
		tracker.value().to_value(),
		json!({"rows": [{"value": 3}, {"value": 1}]})
	);
	assert_eq!(stale.state().get(&p!["rows", 1]), Err(DraftError::Settled));
	assert_eq!(stale.prepare().unwrap_err(), TrackerError::Settled);
	stale.abort();
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > adopts object edits as ordinary fast-layout
// immutable revisions
#[test]
fn adopts_object_edits() {
	let initial = snap(json!({"first": 1, "second": 2}));
	let mut tracker = track(initial.clone());
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["first"], Some(snap(json!(3)))).unwrap();
		draft.delete(&p!["second"]).unwrap();
		draft.set(&p!["third"], Some(snap(json!(4)))).unwrap();
	});
	assert!(!tracker.value().ptr_eq(&initial));
	assert_eq!(initial.to_value(), json!({"first": 1, "second": 2}));
	assert_eq!(tracker.value().to_value(), json!({"first": 3, "third": 4}));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > supports adopt-then-publish ordering and
// retained publication reads
#[test]
fn supports_adopt_then_publish_ordering_and_retained_publication_reads() {
	let mut tracker = track(snap(json!({"value": 0, "nested": {"count": 0}})));
	let mut change = tracker.begin_change();
	change.state().set(&p!["value"], Some(snap(json!(1)))).unwrap();
	change
		.state()
		.set(&p!["nested", "count"], Some(snap(json!(1))))
		.unwrap();
	let prepared = change.prepare().unwrap();
	let operations = prepared.ops().clone();
	tracker.adopt(&prepared).unwrap();
	assert!(Arc::ptr_eq(prepared.ops(), &operations));
	assert_eq!(operations.len(), 2);
	let published = prepared.value().clone();
	assert!(published.ptr_eq(tracker.value()));
	settle(&mut tracker, |change| {
		change.state().set(&p!["value"], Some(snap(json!(2)))).unwrap();
	});
	assert!(!published.ptr_eq(tracker.value()));
	assert_eq!(published.to_value(), json!({"value": 1, "nested": {"count": 1}}));
	assert_eq!(tracker.value().to_value(), json!({"value": 2, "nested": {"count": 1}}));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > keeps replacement base/value structurally
// compatible and readable after adoption (and takes O(1) immutable ownership for replacement and its operation payload)
#[test]
fn keeps_a_replacement_readable_and_owned() {
	let original = snap(json!({"value": 0, "rows": []}));
	let replacement = snap(json!({"value": 1, "rows": [{"value": 2}]}));
	let mut tracker = track(original.clone());
	let prepared = tracker.prepare_replace(replacement.clone());
	assert!(prepared.base().ptr_eq(&original));
	assert!(prepared.value().ptr_eq(&replacement));
	assert_eq!(
		ops_json(&prepared),
		json!([["r", {"value": 1, "rows": [{"value": 2}]}]])
	);
	tracker.adopt(&prepared).unwrap();
	assert!(prepared.base().ptr_eq(&original));
	assert!(tracker.value().ptr_eq(&replacement));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > keeps aborted and stale materialized
// candidates readable
#[test]
fn keeps_aborted_and_stale_materialized_candidates_readable() {
	let mut tracker = track(snap(json!({"value": 0})));
	let mut aborted_change = tracker.begin_change();
	aborted_change.state().set(&p!["value"], Some(snap(json!(1)))).unwrap();
	let aborted = aborted_change.prepare().unwrap();
	let aborted_view = aborted.value().clone();
	aborted.abort();
	assert_eq!(aborted_view.to_value(), json!({"value": 1}));
	assert_eq!(aborted.value().to_value(), json!({"value": 1}));

	let mut loser_change = tracker.begin_change();
	loser_change.state().set(&p!["value"], Some(snap(json!(2)))).unwrap();
	let loser = loser_change.prepare().unwrap();
	tracker
		.adopt(&tracker.prepare_replace(snap(json!({"value": 3}))))
		.unwrap();
	assert_eq!(loser.value().to_value(), json!({"value": 2}));
	assert_eq!(tracker.adopt(&loser), Err(TrackerError::Stale));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > lets a settled Change abort its prepared
// result without retaining its context (and delta.test.ts > invalidates a prepared result when its change is aborted)
#[test]
fn lets_a_settled_change_abort_its_prepared_result() {
	let mut tracker = track(snap(json!({"value": 0, "nested": {"value": 1}})));
	let mut change = tracker.begin_change();
	change.state().set(&p!["value"], Some(snap(json!(1)))).unwrap();
	let prepared = change.prepare().unwrap();
	let operations = prepared.ops().clone();
	change.abort();
	change.abort();
	assert!(Arc::ptr_eq(prepared.ops(), &operations));
	assert_eq!(prepared.value().to_value(), json!({"value": 1, "nested": {"value": 1}}));
	assert_eq!(tracker.adopt(&prepared), Err(TrackerError::Aborted));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > rejects foreign and aborted prepared values
#[test]
fn rejects_foreign_and_aborted_prepared_values() {
	let mut first = track(snap(json!({"value": 0})));
	let mut second = track(snap(json!({"value": 0})));
	let prepared = first.prepare_replace(snap(json!({"value": 1})));
	assert_eq!(second.adopt(&prepared), Err(TrackerError::ForeignTracker));
	prepared.abort();
	prepared.abort();
	assert_eq!(first.adopt(&prepared), Err(TrackerError::Aborted));
	assert_eq!(TrackerError::Aborted.to_string(), "Prepared change has been aborted");
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > reads deleted own properties as absent
#[test]
fn reads_deleted_members_as_absent() {
	let mut tracker = track(snap(json!({"value": 1})));
	let prepared = settle(&mut tracker, |change| {
		let draft = change.state();
		draft.delete(&p!["value"]).unwrap();
		draft.set(&p!["other"], Some(snap(json!(1)))).unwrap();
		draft.set(&p!["other"], None).unwrap();
		assert_eq!(draft.get(&p!["value"]), Ok(None));
		assert_eq!(draft.get(&p!["other"]), Ok(None));
	});
	assert_eq!(prepared.value().to_value(), json!({}));
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > detaches old handles for deeply equal object
// and array assignments before normalizing (and delta.test.ts > normalizes a deep no-op to exact previous identity)
#[test]
fn normalizes_deeply_equal_assignments_to_no_ops() {
	let initial = snap(json!({"object": {"nested": {"value": 1}}, "array": [{"value": 1}], "rows": [{"value": 1}]}));
	let mut tracker = track(initial.clone());
	let mut change = tracker.begin_change();
	let draft = change.state();
	let old_object = draft.get(&p!["object"]).unwrap().unwrap().clone();
	draft
		.set(&p!["object"], Some(snap(json!({"nested": {"value": 1}}))))
		.unwrap();
	draft.set(&p!["array"], Some(snap(json!([{"value": 1}])))).unwrap();
	draft.set(&p!["rows", 0], Some(snap(json!({"value": 1})))).unwrap();
	assert!(!draft.get(&p!["object"]).unwrap().unwrap().ptr_eq(&old_object));
	let prepared = change.prepare().unwrap();
	assert_eq!(prepared.ops().len(), 0);
	assert!(prepared.value().ptr_eq(prepared.base()));
	tracker.adopt(&prepared).unwrap();
	assert!(tracker.value().ptr_eq(&initial));
	assert!(
		tracker
			.value()
			.get("object")
			.unwrap()
			.ptr_eq(initial.get("object").unwrap())
	);
}

// TS: delta-tracker/tracker.test.ts > transactional overlay lifecycle > normalizes a deeply equal replacement without
// changing committed identity (and delta.test.ts > takes immutable ownership of replacement input and applies no-op
// normalization, makes competing same-base preparations stale after adopting a no-op)
#[test]
fn normalizes_a_deeply_equal_replacement_without_changing_committed_identity() {
	let initial = snap(json!({"nested": {"value": 1}, "rows": [1, 2, 3]}));
	let mut tracker = track(initial.clone());
	let first = tracker.prepare_replace(snap(json!({"nested": {"value": 1}, "rows": [1, 2, 3]})));
	let competing = tracker.prepare_replace(snap(json!({"rows": [1, 2, 3], "nested": {"value": 1}})));
	assert_eq!(first.ops().len(), 0);
	assert!(first.value().ptr_eq(first.base()));
	assert!(competing.value().ptr_eq(competing.base()));
	tracker.adopt(&first).unwrap();
	assert!(tracker.value().ptr_eq(&initial));
	assert_eq!(tracker.revision(), 1);
	assert_eq!(tracker.adopt(&first), Err(TrackerError::Consumed));
	assert_eq!(tracker.adopt(&competing), Err(TrackerError::Stale));
}

// TS: delta.test.ts > immutable tracker lifecycle > emits an owned root replacement without traversing large
// replacement input
#[test]
fn emits_a_root_replacement_sharing_the_rows() {
	let rows: Vec<JsonValue> = (0..10_000)
		.map(|value| json!({"value": value, "stable": {"value": value}}))
		.collect();
	let tracker = track(snap(json!({"rows": rows})));
	let mut replaced = tracker.value().get("rows").unwrap().as_array().unwrap().to_vec();
	replaced[5_000] = Snapshot::object([
		("value".to_owned(), snap(json!(-1))),
		("stable".to_owned(), replaced[5_000].get("stable").unwrap().clone()),
	]);
	let replacement = Snapshot::object([("rows".to_owned(), Snapshot::array(replaced))]);
	let prepared = tracker.prepare_replace(replacement.clone());
	assert!(prepared.value().ptr_eq(&replacement));
	assert_eq!(prepared.ops().len(), 1);
	check_replay(&prepared);
}

// TS: delta-tracker/tracker.test.ts > competing changes > tracks one change at a time without a WeakRef
#[test]
fn tracks_one_change_at_a_time() {
	let mut tracker = track(snap(json!({"value": 0, "child": {"rows": [0]}})));
	for index in 1..=100_u64 {
		settle(&mut tracker, |change| {
			let draft = change.state();
			draft.set(&p!["value"], Some(snap(json!(index)))).unwrap();
			draft.push(&p!["child", "rows"], vec![snap(json!(index))]).unwrap();
			draft.set(&p!["child"], Some(snap(json!({"rows": [index]})))).unwrap();
		});
		tracker.begin_change().abort();
		let mut aborted = tracker.begin_change();
		aborted.state().set(&p!["value"], Some(snap(json!(-1)))).unwrap();
		aborted.prepare().unwrap().abort();
		let replacement = tracker.prepare_replace(snap(json!({"value": index, "child": {"rows": [index]}})));
		tracker.adopt(&replacement).unwrap();
	}
	assert_eq!(
		tracker.value().to_value(),
		json!({"value": 100, "child": {"rows": [100]}})
	);
	assert_eq!(tracker.revision(), 200);
}

// TS: delta-tracker/tracker.test.ts > competing changes > makes the older of two open drafts stale when the newer is
// adopted, and the newer when the older is
#[test]
fn makes_either_of_two_open_drafts_stale_when_the_other_is_adopted() {
	let mut tracker = track(snap(json!({"value": 0, "child": {"value": 0}})));
	let mut older = tracker.begin_change();
	older.state().set(&p!["child", "value"], Some(snap(json!(1)))).unwrap();
	let mut newer = tracker.begin_change();
	newer.state().set(&p!["value"], Some(snap(json!(2)))).unwrap();
	tracker.adopt(&newer.prepare().unwrap()).unwrap();
	assert_eq!(tracker.value().to_value(), json!({"value": 2, "child": {"value": 0}}));
	assert_eq!(older.state().get(&p!["child"]), Err(DraftError::Settled));
	assert_eq!(older.prepare().unwrap_err(), TrackerError::Settled);
	older.abort();

	let mut older = tracker.begin_change();
	older.state().set(&p!["value"], Some(snap(json!(1)))).unwrap();
	let mut newer = tracker.begin_change();
	newer.state().set(&p!["child", "value"], Some(snap(json!(2)))).unwrap();
	tracker.adopt(&older.prepare().unwrap()).unwrap();
	assert_eq!(newer.state().get(&p!["child"]), Err(DraftError::Settled));
	assert_eq!(newer.prepare().unwrap_err(), TrackerError::Settled);
	settle(&mut tracker, |change| {
		change.state().set(&p!["value"], Some(snap(json!(3)))).unwrap();
	});
	assert_eq!(tracker.value().to_value(), json!({"value": 3, "child": {"value": 0}}));
}

// TS: delta-tracker/tracker.test.ts > competing changes > makes every other of three drafts stale, open or prepared,
// whichever is adopted
#[test]
fn makes_every_other_of_three_drafts_stale() {
	for winner in 0..3_usize {
		let mut tracker = track(snap(json!({"value": 0, "rows": [{"value": 0}]})));
		let mut changes: Vec<Change> = (0..3).map(|_| tracker.begin_change()).collect();
		for (index, change) in changes.iter_mut().enumerate() {
			change
				.state()
				.set(&p!["rows", 0, "value"], Some(snap(json!(index + 1))))
				.unwrap();
		}
		let prepared = changes[(winner + 1) % 3].prepare().unwrap();
		tracker.adopt(&changes[winner].prepare().unwrap()).unwrap();
		assert_eq!(
			tracker.value().to_value(),
			json!({"value": 0, "rows": [{"value": winner + 1}]})
		);
		assert_eq!(tracker.adopt(&prepared), Err(TrackerError::Stale));
		let open = &mut changes[(winner + 2) % 3];
		assert_eq!(open.state().get(&p!["rows"]), Err(DraftError::Settled));
		assert_eq!(open.prepare().unwrap_err(), TrackerError::Settled);
		assert_eq!(
			prepared.value().to_value(),
			json!({"value": 0, "rows": [{"value": (winner + 1) % 3 + 1}]})
		);
	}
}

// TS: delta-tracker/tracker.test.ts > competing changes > keeps an abandoned draft a competitor of the next change
#[test]
fn keeps_an_abandoned_draft_a_competitor_of_the_next_change() {
	let mut tracker = track(snap(json!({"value": 0, "child": {"value": 0}})));
	let mut abandoned = tracker.begin_change();
	abandoned
		.state()
		.set(&p!["child", "value"], Some(snap(json!(1))))
		.unwrap();
	settle(&mut tracker, |change| {
		change.state().set(&p!["value"], Some(snap(json!(2)))).unwrap();
	});
	settle(&mut tracker, |change| {
		change.state().set(&p!["value"], Some(snap(json!(3)))).unwrap();
	});
	assert_eq!(tracker.value().to_value(), json!({"value": 3, "child": {"value": 0}}));
	assert_eq!(abandoned.state().get(&p!["child"]), Err(DraftError::Settled));
	assert_eq!(abandoned.prepare().unwrap_err(), TrackerError::Settled);
}

// TS: delta-tracker/tracker.test.ts > policy view > supports native reads, descriptors, keys, iteration, map, and
// JSON (the reads Rust has: `get` and the snapshot it returns)
#[test]
fn reads_the_draft_through_get() {
	let mut tracker = track(snap(json!({"values": [1, 2, 3], "object": {"a": 1}})));
	let mut change = tracker.begin_change();
	let draft = change.state();
	assert_eq!(
		draft
			.splice(&p!["values"], 1, Some(1), vec![snap(json!(4)), snap(json!(5))])
			.unwrap(),
		[snap(json!(2))]
	);
	draft.set(&p!["object", "b"], Some(snap(json!(2)))).unwrap();
	assert_eq!(
		draft.get(&p!["values"]).unwrap().unwrap().to_value(),
		json!([1, 4, 5, 3])
	);
	assert_eq!(draft.get(&p!["values", 2]).unwrap().unwrap().to_value(), json!(5));
	let object = draft.get(&p!["object"]).unwrap().unwrap();
	let keys: Vec<&str> = object.as_object().unwrap().iter().map(|(key, _)| key).collect();
	assert_eq!(keys, ["a", "b"]);
	assert_eq!(
		draft.get(&[]).unwrap().unwrap().to_value(),
		json!({"values": [1, 4, 5, 3], "object": {"a": 1, "b": 2}})
	);
	change.abort();
}

// TS: delta-tracker/tracker.test.ts > policy view > keeps document keys distinct from object proxy target fields
#[test]
fn keeps_document_keys_distinct_from_internal_fields() {
	let mut tracker = track(snap(
		json!({"object": {"context": 1, "base": 2, "parent": 3, "dirty": 4, "target": 5, "proxy": 6}}),
	));
	let prepared = settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["object", "context"], Some(snap(json!(7))))
			.unwrap();
		change
			.state()
			.set(&p!["object", "proxy"], Some(snap(json!(8))))
			.unwrap();
	});
	assert_eq!(
		to_js_string(&prepared.value().get("object").unwrap().to_value()),
		r#"{"context":7,"base":2,"parent":3,"dirty":4,"target":5,"proxy":8}"#
	);
}

// TS: delta-tracker/tracker.test.ts > policy view > keeps native coercion side effects and string operation forms
// (the string forms; coercion is JS-only) and delta.test.ts > canonical strings > emits append and rolling-window
// operations
#[test]
fn emits_append_and_rolling_window_operations() {
	let mut tracker = track(snap(json!({"text": "abcdefgh"})));
	let prepared = settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["text"], Some(Snapshot::string("abcdefghij")))
			.unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["a", ["text"], "ij"]]));
	let prepared = settle(&mut tracker, |change| {
		let text = change.state().get(&p!["text"]).unwrap().unwrap().as_str().unwrap()[3..].to_owned();
		change
			.state()
			.set(&p!["text"], Some(Snapshot::string(&format!("{text}xyz"))))
			.unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["t", ["text"], 3], ["a", ["text"], "xyz"]]));
	let wire = Encoder::new().encode(prepared.ops());
	let decoded = Decoder::new().decode(&wire).unwrap();
	assert_eq!(apply_immutable(prepared.base(), &decoded).unwrap(), *prepared.value());
}

// TS: delta-tracker/tracker.test.ts > policy view > emits deletion and supports root arrays
#[test]
fn emits_deletion_and_supports_root_arrays() {
	let mut objects = track(snap(json!({"keep": 1, "remove": 2})));
	settle(&mut objects, |change| change.state().delete(&p!["remove"]).unwrap());
	assert_eq!(objects.value().to_value(), json!({"keep": 1}));
	let mut arrays = track(snap(json!([1, 2, 3])));
	settle(&mut arrays, |change| {
		change.state().reverse(&[]).unwrap();
		change.state().push(&[], vec![snap(json!(4))]).unwrap();
	});
	assert_eq!(arrays.value().to_value(), json!([3, 2, 1, 4]));
}

// TS: delta-tracker/tracker.test.ts > policy view > preserves native object key order after delete and re-add
#[test]
fn preserves_object_key_order_after_delete_and_re_add() {
	let mut tracker = track(snap(json!({"first": 1, "second": 2})));
	settle(&mut tracker, |change| {
		change.state().delete(&p!["first"]).unwrap();
		change.state().set(&p!["first"], Some(snap(json!(1)))).unwrap();
	});
	assert_eq!(to_js_string(&tracker.value().to_value()), r#"{"second":2,"first":1}"#);
}

// TS: delta-tracker/tracker.test.ts > policy view > orders new integer-like object keys before strings, and keeps
// native integer and string ordering across deletion and re-addition (JS orders index keys first; a `Snapshot`
// iterates in insertion order and serializes in JS order, which the replay checks)
#[test]
fn keeps_js_key_order_across_deletion_and_re_addition() {
	let mut tracker = track(snap(json!({"object": {"label": "x"}})));
	settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["object", "2"], Some(Snapshot::string("two")))
			.unwrap();
		change
			.state()
			.set(&p!["object", "1"], Some(Snapshot::string("one")))
			.unwrap();
	});
	assert_eq!(
		to_js_string(&tracker.value().to_value()),
		r#"{"object":{"1":"one","2":"two","label":"x"}}"#
	);

	let base = pi_json::parse(r#"{"object":{"1":"one","2":"two","first":"a","second":"b"}}"#).unwrap();
	let mut tracker = track(snap(base));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.delete(&p!["object", "2"]).unwrap();
		draft.set(&p!["object", "2"], Some(Snapshot::string("two"))).unwrap();
		draft.delete(&p!["object", "first"]).unwrap();
		draft.set(&p!["object", "first"], Some(Snapshot::string("a"))).unwrap();
		draft.set(&p!["object", "3"], Some(Snapshot::string("three"))).unwrap();
		draft.set(&p!["object", "0"], Some(Snapshot::string("zero"))).unwrap();
	});
	assert_eq!(
		to_js_string(&tracker.value().to_value()),
		r#"{"object":{"0":"zero","1":"one","2":"two","3":"three","second":"b","first":"a"}}"#
	);
}

// TS: delta-tracker/tracker.test.ts > policy view > normalizes deeply equal container assignments before direct
// candidate materialization
#[test]
fn normalizes_deeply_equal_container_assignments_keeping_the_base_order() {
	let mut tracker = track(snap(json!({"child": {"a": 1, "b": 2}, "count": 0})));
	let prepared = settle(&mut tracker, |change| {
		let reordered = pi_json::parse(r#"{"b":2,"a":1}"#).unwrap();
		change.state().set(&p!["child"], Some(snap(reordered))).unwrap();
		change.state().set(&p!["count"], Some(snap(json!(1)))).unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["s", ["count"], 1]]));
	assert_eq!(
		to_js_string(&prepared.value().get("child").unwrap().to_value()),
		r#"{"a":1,"b":2}"#
	);
}

// TS: delta-tracker/tracker.test.ts > policy view > keeps repeated identical writes instead of reverting their pending
// override
#[test]
fn keeps_repeated_identical_writes() {
	let mut tracker = track(snap(json!({"value": 0, "values": [0]})));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["value"], Some(snap(json!(1)))).unwrap();
		draft.set(&p!["value"], Some(snap(json!(1)))).unwrap();
		draft.set(&p!["values", 0], Some(Snapshot::Null)).unwrap();
		draft.set(&p!["values", 0], Some(Snapshot::Null)).unwrap();
		draft.push(&p!["values"], vec![snap(json!(2))]).unwrap();
		draft.set(&p!["values", 1], Some(Snapshot::Null)).unwrap();
		draft.set(&p!["values", 1], Some(Snapshot::Null)).unwrap();
	});
	assert_eq!(tracker.value().to_value(), json!({"value": 1, "values": [null, null]}));
}

// TS: delta-tracker/tracker.test.ts > policy view > rejects array index gaps without mutation while allowing
// replacement and append
#[test]
fn rejects_array_index_gaps_without_mutation() {
	let mut tracker = track(snap(json!({"values": [1, 2]})));
	let mut change = tracker.begin_change();
	let draft = change.state();
	assert_eq!(
		draft.set(&p!["values", 3], Some(snap(json!(4)))),
		Err(DraftError::Holes)
	);
	assert_eq!(DraftError::Holes.to_string(), "Overlay arrays cannot contain holes");
	assert_eq!(draft.get(&p!["values"]).unwrap().unwrap().to_value(), json!([1, 2]));
	draft.set(&p!["values", 1], Some(snap(json!(9)))).unwrap();
	draft.set(&p!["values", 2], Some(snap(json!(3)))).unwrap();
	let prepared = change.prepare().unwrap();
	assert_eq!(prepared.value().to_value(), json!({"values": [1, 9, 3]}));
	check_replay(&prepared);
	tracker.adopt(&prepared).unwrap();
}

// TS: delta-tracker/tracker.test.ts > policy view > supports optional deletion, explicit null length growth,
// shrinking, and default sort (and delta.test.ts > grows arrays with explicit nulls)
#[test]
fn supports_deletion_null_growth_shrinking_and_sort() {
	let mut tracker = track(snap(json!({"optional": "remove", "values": [3, 1, 2]})));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["optional"], None).unwrap();
		draft.set_len(&p!["values"], 5).unwrap();
		assert_eq!(
			draft.get(&p!["values"]).unwrap().unwrap().to_value(),
			json!([3, 1, 2, null, null])
		);
		draft.set_len(&p!["values"], 4).unwrap();
		draft.sort_by(&p!["values"], js_default_order).unwrap();
		assert_eq!(draft.delete(&p!["values", 0]), Err(DraftError::Holes));
		assert_eq!(draft.set(&p!["values", 0], None), Err(DraftError::Undefined));
		assert_eq!(
			draft.set(&p!["values", "length"], Some(snap(json!(1)))),
			Err(DraftError::ArrayKey)
		);
	});
	assert_eq!(tracker.value().to_value(), json!({"values": [1, 2, 3, null]}));
}

/// JS `sort()` without a comparator, for the primitives these tests sort: by `String(value)`.
fn js_default_order(left: &Snapshot, right: &Snapshot) -> Ordering {
	let text = |value: &Snapshot| match value.to_value() {
		JsonValue::String(text) => text,
		other => to_js_string(&other),
	};
	text(left).encode_utf16().cmp(text(right).encode_utf16())
}

// TS: delta-tracker/tracker.test.ts > policy view > enumerates wide objects without duplicate keys
#[test]
fn enumerates_wide_objects_without_duplicate_keys() {
	let mut tracker = track(snap(json!({"values": {}})));
	let mut change = tracker.begin_change();
	for index in 0..20_000 {
		change
			.state()
			.set(
				&p!["values", format!("field{index}").as_str()],
				Some(snap(json!(index))),
			)
			.unwrap();
	}
	let values = change.state().get(&p!["values"]).unwrap().unwrap();
	let keys: Vec<&str> = values.as_object().unwrap().iter().map(|(key, _)| key).collect();
	assert_eq!(keys.len(), 20_000);
	assert_eq!((keys[0], keys[19_999]), ("field0", "field19999"));
	change.abort();
}

// TS: delta-tracker/tracker.test.ts > policy view > does not dirty read-only traversals
#[test]
fn does_not_dirty_read_only_traversals() {
	let mut tracker = track(snap(json!({"nested": {"rows": [{"value": 1}]}})));
	let prepared = settle(&mut tracker, |change| {
		assert_eq!(
			change
				.state()
				.get(&p!["nested", "rows", 0, "value"])
				.unwrap()
				.unwrap()
				.to_value(),
			json!(1)
		);
		assert_eq!(change.state().get(&p!["nested", "missing", 0]), Ok(None));
	});
	assert_eq!(prepared.ops().len(), 0);
}

// TS: delta-tracker/tracker.test.ts > by-value placements > clones property, index, push, unshift, splice, fill, and
// copyWithin placements (and delta-clone.test.ts > copies assigned and inserted values immediately)
#[test]
fn places_values_independently() {
	let mut tracker = track(snap(
		json!({"property": null, "values": [{"value": 0}, {"value": 1}, {"value": 2}]}),
	));
	let external = snap(json!({"value": 5}));
	let prepared = settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["property"], Some(external.clone())).unwrap();
		draft.set(&p!["values", 0], Some(external.clone())).unwrap();
		draft.push(&p!["values"], vec![external.clone()]).unwrap();
		draft.unshift(&p!["values"], vec![external.clone()]).unwrap();
		draft.splice(&p!["values"], 2, Some(0), vec![external.clone()]).unwrap();
		let first = draft.get(&p!["values", 0]).unwrap().unwrap().clone();
		draft.fill(&p!["values"], &first, 1, Some(3)).unwrap();
		draft.copy_within(&p!["values"], 3, 0, Some(2)).unwrap();
		draft.set(&p!["values", 0, "value"], Some(snap(json!(99)))).unwrap();
	});
	let values = prepared.value().get("values").unwrap().to_value();
	assert_eq!(values[0], json!({"value": 99}));
	for index in 1..5 {
		assert_eq!(values[index], json!({"value": 5}));
	}
	assert_eq!(external.to_value(), json!({"value": 5}));
}

// TS: delta-tracker/tracker.test.ts > by-value placements > rejects non-strict JSON placements before changing the
// draft (the `undefined` cases; the others cannot be built as a `Snapshot`)
#[test]
fn rejects_undefined_placements_before_changing_the_draft() {
	let initial = json!({"optional": "remove", "payload": null, "number": 0, "values": [1, 2]});
	let mut tracker = track(snap(initial.clone()));
	let mut change = tracker.begin_change();
	let draft = change.state();
	assert_eq!(draft.set(&p!["values", 0], None), Err(DraftError::Undefined));
	assert_eq!(
		DraftError::Undefined.to_string(),
		"Value contains a non-JSON undefined; expected strict JSON"
	);
	assert_eq!(draft.get(&[]).unwrap().unwrap().to_value(), initial);
	draft.set(&p!["optional"], None).unwrap();
	draft.set(&p!["payload"], Some(snap(json!({"valid": true})))).unwrap();
	draft.push(&p!["values"], vec![snap(json!({"valid": true}))]).unwrap();
	let prepared = change.prepare().unwrap();
	assert_eq!(
		prepared.value().to_value(),
		json!({"payload": {"valid": true}, "number": 0, "values": [1, 2, {"valid": true}]})
	);
	check_replay(&prepared);
}

// TS: delta-tracker/tracker.test.ts > by-value placements > expands repeated source aliases into independent
// placements, deep-clones draft-sourced placements within and across revisions, distinguishes raw committed
// references from draft references
#[test]
fn placements_of_one_value_edit_independently() {
	let mut tracker = track(snap(json!({"left": null, "right": null, "rows": []})));
	let shared = snap(json!({"nested": {"value": 1}}));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["left"], Some(shared.clone())).unwrap();
		draft.set(&p!["right"], Some(shared.clone())).unwrap();
		draft.push(&p!["rows"], vec![shared.clone(), shared.clone()]).unwrap();
		draft.set(&p!["left", "nested", "value"], Some(snap(json!(9)))).unwrap();
		draft
			.set(&p!["rows", 0, "nested", "value"], Some(snap(json!(8))))
			.unwrap();
	});
	assert_eq!(
		tracker.value().to_value(),
		json!({"left": {"nested": {"value": 9}}, "right": {"nested": {"value": 1}},
			"rows": [{"nested": {"value": 8}}, {"nested": {"value": 1}}]})
	);

	let mut tracker = track(snap(json!({"source": {"value": 1}, "raw": null, "drafted": null})));
	let raw = tracker.value().get("source").unwrap().clone();
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["source", "value"], Some(snap(json!(2)))).unwrap();
		draft.set(&p!["raw"], Some(raw.clone())).unwrap();
		let drafted = draft.get(&p!["source"]).unwrap().unwrap().clone();
		draft.set(&p!["drafted"], Some(drafted)).unwrap();
	});
	assert_eq!(
		tracker.value().to_value(),
		json!({"source": {"value": 2}, "raw": {"value": 1}, "drafted": {"value": 2}})
	);
	settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["drafted", "value"], Some(snap(json!(4))))
			.unwrap();
	});
	assert_eq!(value_at(&tracker, &p!["source", "value"]), json!(2));
}

// TS: delta-tracker/tracker.test.ts > by-value placements > folds edits to introduced object and array subtrees into
// placement payloads
#[test]
fn folds_edits_to_introduced_subtrees_into_placement_payloads() {
	let mut tracker = track(snap(json!({"nested": null, "rows": []})));
	let prepared = settle(&mut tracker, |change| {
		let draft = change.state();
		draft
			.set(&p!["nested"], Some(snap(json!({"rows": [{"value": 1}]}))))
			.unwrap();
		draft
			.set(&p!["nested", "rows", 0, "value"], Some(snap(json!(2))))
			.unwrap();
		draft
			.push(&p!["nested", "rows"], vec![snap(json!({"value": 3}))])
			.unwrap();
		draft.push(&p!["rows"], vec![snap(json!({"values": [1]}))]).unwrap();
		draft.push(&p!["rows", 0, "values"], vec![snap(json!(2))]).unwrap();
	});
	assert_eq!(
		ops_json(&prepared),
		json!([
			["s", ["nested"], {"rows": [{"value": 2}, {"value": 3}]}],
			["p", ["rows"], 0, 0, [{"values": [1, 2]}]],
		])
	);
}

// TS: delta-tracker/tracker.test.ts > by-value placements > folds pathological operation counts without payload-cost
// estimation
#[test]
fn folds_pathological_operation_counts_into_a_root_replacement() {
	let wide: serde_json::Map<String, JsonValue> =
		(0..5_000).map(|index| (format!("field{index}"), json!(0))).collect();
	let mut tracker = track(snap(JsonValue::Object(wide)));
	let prepared = settle(&mut tracker, |change| {
		for index in 0..5_000 {
			change
				.state()
				.set(&p![format!("field{index}").as_str()], Some(snap(json!(1))))
				.unwrap();
		}
	});
	assert!(matches!(prepared.ops()[..], [Op::Replace(_)]));

	let rows: Vec<JsonValue> = (0..15_000).map(|value| json!({"value": value})).collect();
	let mut tracker = track(snap(json!({"rows": rows})));
	let prepared = settle(&mut tracker, |change| {
		for index in (0..15_000_i64).step_by(3) {
			let path = p!["rows", usize::try_from(index).unwrap(), "value"];
			change.state().set(&path, Some(snap(json!(-index - 1)))).unwrap();
		}
	});
	assert!(matches!(prepared.ops()[..], [Op::Replace(_)]));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > emits one insert for an append to a long array
#[test]
fn emits_one_insert_for_an_append_to_a_long_array() {
	let mut tracker = track(snap(json!({"values": (0..10_000).collect::<Vec<_>>()})));
	let prepared = settle(&mut tracker, |change| {
		change.state().push(&p!["values"], vec![snap(json!(-1))]).unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["p", ["values"], 10_000, 0, [-1]]]));
	assert_eq!(value_at(&tracker, &p!["values", 10_000]), json!(-1));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > emits removals from the end backwards, then inserts, when kept
// elements keep their order (the value, and at most the TS op count)
#[test]
fn emits_removals_and_inserts_when_kept_elements_keep_their_order() {
	let mut tracker = track(snap(json!({"values": [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]})));
	let prepared = settle(&mut tracker, |change| {
		let draft = change.state();
		draft.shift(&p!["values"]).unwrap();
		draft.splice(&p!["values"], 1, Some(2), vec![snap(json!(20))]).unwrap();
		draft.splice(&p!["values"], 5, Some(1), vec![]).unwrap();
		draft.pop(&p!["values"]).unwrap();
		draft.push(&p!["values"], vec![snap(json!(30))]).unwrap();
	});
	assert_eq!(value_at(&tracker, &p!["values"]), json!([1, 20, 4, 5, 6, 8, 30]));
	assert!(prepared.ops().len() <= 6);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > emits a permutation when kept elements change order, and
// combines movement and edits with permutation plus final paths (the value; Rust finds the edited element by its new
// identity, so the batch is splices, no longer than the TS one)
#[test]
fn emits_a_permutation_when_kept_elements_change_order() {
	let by_parity = |left: &Snapshot, right: &Snapshot| {
		let number = |value: &Snapshot| value.to_value().as_i64().unwrap();
		(number(left) % 2, number(left)).cmp(&(number(right) % 2, number(right)))
	};
	for sort in [false, true] {
		let mut tracker = track(snap(json!({"values": [0, 1, 2, 3, 4]})));
		let prepared = settle(&mut tracker, |change| {
			if sort {
				change.state().sort_by(&p!["values"], by_parity).unwrap();
			} else {
				change.state().reverse(&p!["values"]).unwrap();
			}
		});
		assert!(matches!(prepared.ops()[..], [Op::Move(..)]), "{:?}", prepared.ops());
		let expected = if sort {
			json!([0, 2, 4, 1, 3])
		} else {
			json!([4, 3, 2, 1, 0])
		};
		assert_eq!(value_at(&tracker, &p!["values"]), expected);
	}

	let mut tracker = track(snap(
		json!({"values": [{"rank": 3, "edited": 0}, {"rank": 1, "edited": 0}, {"rank": 2, "edited": 0}]}),
	));
	let prepared = settle(&mut tracker, |change| {
		let rank = |value: &Snapshot| value.get("rank").unwrap().to_value().as_i64().unwrap();
		change
			.state()
			.sort_by(&p!["values"], |left, right| rank(left).cmp(&rank(right)))
			.unwrap();
		change
			.state()
			.set(&p!["values", 2, "edited"], Some(snap(json!(1))))
			.unwrap();
	});
	assert_eq!(
		value_at(&tracker, &p!["values"]),
		json!([{"rank": 1, "edited": 0}, {"rank": 2, "edited": 0}, {"rank": 3, "edited": 1}])
	);
	assert!(prepared.ops().len() <= 2);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > matches native splice with zero or one argument
#[test]
fn matches_native_splice_with_zero_or_one_argument() {
	let mut tracker = track(snap(json!({"values": [1, 2, 3]})));
	let prepared = settle(&mut tracker, |change| {
		assert_eq!(
			change.state().splice(&p!["values"], 0, Some(0), vec![]).unwrap().len(),
			0
		);
	});
	assert_eq!(prepared.ops().len(), 0);
	let mut tracker = track(snap(json!({"values": [1, 2, 3, 4]})));
	settle(&mut tracker, |change| {
		let removed = change.state().splice(&p!["values"], 1, None, vec![]).unwrap();
		assert_eq!(Snapshot::array(removed).to_value(), json!([2, 3, 4]));
	});
	assert_eq!(value_at(&tracker, &p!["values"]), json!([1]));
	let mut tracker = track(snap(json!({"values": [1, 2, 3]})));
	settle(&mut tracker, |change| {
		assert_eq!(change.state().splice(&p!["values"], 3, None, vec![]).unwrap().len(), 0);
		assert_eq!(
			change.state().splice(&p!["values"], -1, Some(0), vec![]).unwrap().len(),
			0
		);
	});
	assert_eq!(value_at(&tracker, &p!["values"]), json!([1, 2, 3]));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > supports all structural mutators and native return values
#[test]
fn supports_all_structural_mutators_and_native_return_values() {
	let mut tracker = track(snap(json!({"values": [3, 1, 2]})));
	let number = |value: &Snapshot| value.to_value().as_i64().unwrap();
	settle(&mut tracker, |change| {
		let draft = change.state();
		let path = p!["values"];
		assert_eq!(draft.push(&path, vec![snap(json!(4))]).unwrap(), 4);
		assert_eq!(draft.pop(&path).unwrap().map(|value| value.to_value()), Some(json!(4)));
		assert_eq!(draft.unshift(&path, vec![snap(json!(0))]).unwrap(), 4);
		assert_eq!(
			draft.shift(&path).unwrap().map(|value| value.to_value()),
			Some(json!(0))
		);
		let removed = draft
			.splice(&path, 1, Some(1), vec![snap(json!(5)), snap(json!(4))])
			.unwrap();
		assert_eq!(Snapshot::array(removed).to_value(), json!([1]));
		draft
			.sort_by(&path, |left, right| number(left).cmp(&number(right)))
			.unwrap();
		draft.reverse(&path).unwrap();
		draft.fill(&path, &snap(json!(9)), 1, Some(3)).unwrap();
		draft.copy_within(&path, 1, 0, Some(2)).unwrap();
	});
	assert_eq!(value_at(&tracker, &p!["values"]), json!([5, 5, 9, 2]));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > tracks held handles after reindex and suppresses detached writes,
// and replays introduced, moved, then edited array descendants (by path)
#[test]
fn replays_moved_then_edited_array_descendants() {
	let mut tracker = track(snap(
		json!({"values": [{"value": "a"}, {"value": "b"}, {"value": "c"}]}),
	));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft
			.unshift(&p!["values"], vec![snap(json!({"value": "front"}))])
			.unwrap();
		draft
			.set(&p!["values", 2, "value"], Some(Snapshot::string("moved")))
			.unwrap();
		draft.splice(&p!["values"], 2, Some(1), vec![]).unwrap();
	});
	assert_eq!(
		value_at(&tracker, &p!["values"]),
		json!([{"value": "front"}, {"value": "a"}, {"value": "c"}])
	);

	let mut tracker = track(snap(json!({"values": []})));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft
			.push(&p!["values"], vec![snap(json!({"id": 1, "nested": [1]}))])
			.unwrap();
		draft
			.unshift(&p!["values"], vec![snap(json!({"id": 0, "nested": []}))])
			.unwrap();
		draft.push(&p!["values", 1, "nested"], vec![snap(json!(2))]).unwrap();
		draft.reverse(&p!["values"]).unwrap();
		draft.push(&p!["values", 0, "nested"], vec![snap(json!(3))]).unwrap();
	});
	assert_eq!(
		value_at(&tracker, &p!["values"]),
		json!([{"id": 1, "nested": [1, 2, 3]}, {"id": 0, "nested": []}])
	);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > folds dense child and direct-index edits into one full-array
// operation
#[test]
fn folds_dense_child_and_direct_index_edits_into_one_operation() {
	let rows: Vec<JsonValue> = (0..1_000).map(|value| json!({"value": value})).collect();
	let mut tracker = track(snap(json!({"rows": rows})));
	let prepared = settle(&mut tracker, |change| {
		for index in 0..1_000_i64 {
			change
				.state()
				.set(
					&p!["rows", usize::try_from(index).unwrap(), "value"],
					Some(snap(json!(index + 1))),
				)
				.unwrap();
		}
	});
	assert!(matches!(prepared.ops()[..], [Op::Splice(..)]));

	let mut tracker = track(snap(json!({"values": (0..1_000).collect::<Vec<_>>()})));
	let prepared = settle(&mut tracker, |change| {
		for index in 0..600_i64 {
			change
				.state()
				.set(
					&p!["values", usize::try_from(index).unwrap()],
					Some(snap(json!(-index - 1))),
				)
				.unwrap();
		}
	});
	assert!(matches!(prepared.ops()[..], [Op::Splice(..)]));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > folds only a deeply nested dense array region
#[test]
fn folds_only_a_deeply_nested_dense_array_region() {
	let rows: Vec<JsonValue> = (0..2_000).map(|value| json!({"nested": {"value": value}})).collect();
	let mut tracker = track(snap(json!({"rows": rows})));
	let prepared = settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["rows", 0, "nested", "value"], Some(snap(json!(-1))))
			.unwrap();
		for index in 500..1_000_i64 {
			let path = p!["rows", usize::try_from(index).unwrap(), "nested", "value"];
			change.state().set(&path, Some(snap(json!(-index)))).unwrap();
		}
	});
	assert_eq!(prepared.ops().len(), 2);
	let splice = prepared.ops().iter().find_map(|op| match op {
		Op::Splice(path, index, remove, items) => Some((path.clone(), *index, *remove, items.len())),
		_ => None,
	});
	assert_eq!(splice, Some((p!["rows"].to_vec(), 500, 500, 500)));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > does not cache emission paths for nested reserved-key folds
// covered by an outer region, and suppresses nested structural and leaf operations covered by an outer dense region
#[test]
fn an_outer_dense_region_covers_nested_edits() {
	let special: Vec<usize> = (0..400).collect();
	let mut rows: Vec<JsonValue> = (0..400).map(|_| json!({"flag": 0})).collect();
	rows[100] = pi_json::parse(&format!(
		r#"{{"flag":0,"special":{{"__proto__":{{"values":{special:?}}}}}}}"#
	))
	.unwrap();
	let mut tracker = track(snap(json!({"rows": rows})));
	let prepared = settle(&mut tracker, |change| {
		for index in 0..256_usize {
			change
				.state()
				.set(&p!["rows", index, "flag"], Some(snap(json!(1))))
				.unwrap();
		}
		for index in 0..256_usize {
			let path = p!["rows", 100, "special", "__proto__", "values", index];
			change.state().set(&path, Some(snap(json!(-1)))).unwrap();
		}
	});
	assert!(matches!(&prepared.ops()[..], [Op::Splice(path, 0, 256, _)] if *path == p!["rows"]));

	let rows: Vec<JsonValue> = (0..400)
		.map(|index| {
			let values: Vec<JsonValue> = if index == 100 {
				(0..400).map(|value| json!({"value": value})).collect()
			} else {
				vec![]
			};
			json!({"flag": 0, "values": values})
		})
		.collect();
	let mut tracker = track(snap(json!({"rows": rows})));
	let prepared = settle(&mut tracker, |change| {
		for index in 0..256_usize {
			change
				.state()
				.set(&p!["rows", index, "flag"], Some(snap(json!(1))))
				.unwrap();
			change
				.state()
				.set(&p!["rows", 100, "values", index, "value"], Some(snap(json!(-1))))
				.unwrap();
		}
		change
			.state()
			.push(&p!["rows", 100, "values"], vec![snap(json!({"value": 999}))])
			.unwrap();
	});
	assert!(matches!(&prepared.ops()[..], [Op::Splice(path, 0, 256, _)] if *path == p!["rows"]));
	assert_eq!(
		tracker
			.value()
			.at(&p!["rows", 100, "values"])
			.unwrap()
			.as_array()
			.unwrap()
			.len(),
		401
	);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > emits multiple disjoint dense regions and preserves operations
// outside their boundaries
#[test]
fn emits_multiple_disjoint_dense_regions() {
	let rows: Vec<JsonValue> = (0..1_400).map(|value| json!({"value": value})).collect();
	let mut tracker = track(snap(json!({"values": rows})));
	let edits: Vec<i64> = [97, 358, 897, 1_158]
		.into_iter()
		.chain(100..356)
		.chain(900..1_156)
		.collect();
	let prepared = settle(&mut tracker, |change| {
		for &index in &edits {
			let path = p!["values", usize::try_from(index).unwrap(), "value"];
			change.state().set(&path, Some(snap(json!(-index - 1)))).unwrap();
		}
	});
	let splices: Vec<(u64, u64)> = prepared
		.ops()
		.iter()
		.filter_map(|op| match op {
			Op::Splice(_, index, remove, _) => Some((*index, *remove)),
			_ => None,
		})
		.collect();
	assert_eq!(splices, [(100, 256), (900, 256)]);
	for index in [97, 358, 897, 1_158_i64] {
		let set = ops(json!([["s", ["values", index, "value"], -index - 1]])).remove(0);
		assert!(prepared.ops().contains(&set));
	}
	assert_eq!(prepared.ops().len(), 6);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > normalizes a 20,000-operation queue transaction to two
// operations
#[test]
fn normalizes_a_queue_transaction_to_two_operations() {
	let rows: Vec<JsonValue> = (0..20_000).map(|value| json!({"value": value})).collect();
	let mut tracker = track(snap(json!({"values": rows})));
	let prepared = settle(&mut tracker, |change| {
		for index in 0..10_000 {
			change.state().shift(&p!["values"]).unwrap();
			change
				.state()
				.push(&p!["values"], vec![snap(json!({"value": 20_000 + index}))])
				.unwrap();
		}
	});
	assert_eq!(prepared.ops().len(), 2);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > handles adversarial fragmentation and resolves held handles
// without piece scans (held handles become paths)
#[test]
fn handles_adversarial_fragmentation() {
	let size = 20_000_usize;
	let rows: Vec<JsonValue> = (0..size).map(|value| json!({"value": value})).collect();
	let mut expected = rows.clone();
	let mut tracker = track(snap(json!({"values": rows})));
	settle(&mut tracker, |change| {
		for index in (0..size).step_by(2) {
			let row = json!({"value": -i64::try_from(index).unwrap() - 1});
			change
				.state()
				.splice(
					&p!["values"],
					i64::try_from(index).unwrap(),
					Some(1),
					vec![snap(row.clone())],
				)
				.unwrap();
			expected[index] = row;
		}
		for index in [1_usize, 1_001, 5_001, 10_001, 15_001, 19_999] {
			let value = 100_000 + i64::try_from(index).unwrap();
			change
				.state()
				.set(&p!["values", index, "value"], Some(snap(json!(value))))
				.unwrap();
			expected[index] = json!({"value": value});
		}
	});
	assert_eq!(value_at(&tracker, &p!["values"]), JsonValue::Array(expected));
}

// TS: delta-tracker/tracker.test.ts > piece arrays > normalizes restored overrides and cancelled structural edits to
// no operations
#[test]
fn normalizes_restored_overrides_and_cancelled_structural_edits_to_no_operations() {
	let mut tracker = track(snap(json!({"values": [1, 2, 3]})));
	let prepared = settle(&mut tracker, |change| {
		let draft = change.state();
		draft.set(&p!["values", 1], Some(snap(json!(9)))).unwrap();
		draft.set(&p!["values", 1], Some(snap(json!(2)))).unwrap();
		draft.reverse(&p!["values"]).unwrap();
		draft.reverse(&p!["values"]).unwrap();
		draft.push(&p!["values"], vec![snap(json!(4))]).unwrap();
		assert_eq!(
			draft.pop(&p!["values"]).unwrap().map(|value| value.to_value()),
			Some(json!(4))
		);
	});
	assert_eq!(prepared.ops().len(), 0);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > handles null sparse overrides without confusing absence
#[test]
fn handles_null_sparse_overrides() {
	let mut tracker = track(snap(json!({"values": (0..10_000).collect::<Vec<_>>()})));
	settle(&mut tracker, |change| {
		change.state().set(&p!["values", 17], Some(Snapshot::Null)).unwrap();
		change.state().set(&p!["values", 9_000], Some(Snapshot::Null)).unwrap();
	});
	assert_eq!(value_at(&tracker, &p!["values", 17]), JsonValue::Null);
	assert_eq!(value_at(&tracker, &p!["values", 9_000]), JsonValue::Null);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > supports self-overlapping fill and copyWithin by value
#[test]
fn supports_self_overlapping_fill_and_copy_within_by_value() {
	let mut tracker = track(snap(json!({"values": [{"n": 0}, {"n": 1}, {"n": 2}, {"n": 3}]})));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.copy_within(&p!["values"], 1, 0, Some(3)).unwrap();
		let second = draft.get(&p!["values", 1]).unwrap().unwrap().clone();
		draft.fill(&p!["values"], &second, 0, Some(2)).unwrap();
		draft.set(&p!["values", 0, "n"], Some(snap(json!(9)))).unwrap();
	});
	assert_eq!(
		value_at(&tracker, &p!["values"]),
		json!([{"n": 9}, {"n": 0}, {"n": 1}, {"n": 2}])
	);
}

// TS: delta-tracker/tracker.test.ts > piece arrays > inserts 100,000 items with %s (unshift, splice)
#[test]
fn inserts_a_hundred_thousand_items() {
	for unshift in [true, false] {
		let mut tracker = track(snap(json!({"values": [-1]})));
		let items: Vec<Snapshot> = (0..100_000).map(|value| snap(json!(value))).collect();
		let prepared = settle(&mut tracker, |change| {
			if unshift {
				change.state().unshift(&p!["values"], items).unwrap();
			} else {
				change.state().splice(&p!["values"], 1, Some(0), items).unwrap();
			}
		});
		assert_eq!(prepared.ops().len(), 1);
		let values = tracker.value().get("values").unwrap().as_array().unwrap();
		assert_eq!(values.len(), 100_001);
		assert_eq!(values[if unshift { 99_999 } else { 100_000 }].to_value(), json!(99_999));
	}
}

/// A small seeded generator (TS `random` of the randomized-transactions test).
struct Random(u32);

impl Random {
	fn next(&mut self) -> f64 {
		self.0 = self.0.wrapping_add(0x6d2b_79f5);
		let seed = self.0;
		let mut value = (seed ^ (seed >> 15)).wrapping_mul(0x1 | seed);
		value = (value.wrapping_add((value ^ (value >> 7)).wrapping_mul(0x3d | value))) ^ value;
		f64::from(value ^ (value >> 14)) / 4_294_967_296.0
	}
}

/// One edit of the randomized test, on the draft and on a plain JSON model, as TS `mutate` does on both.
fn mutate_both(draft: &mut pi_chord::delta::Draft, model: &mut JsonValue, choice: u32, value: i64) -> Result<()> {
	let item = json!({"id": value, "score": value % 7});
	let values = model["values"].as_array_mut().context("values")?;
	let length = values.len();
	let path = p!["values"];
	match choice {
		0 => {
			let text = format!("{}-{value}", model["text"].as_str().context("text")?);
			draft.set(&p!["text"], Some(Snapshot::string(&text)))?;
			model["text"] = json!(text);
		}
		1 => {
			draft.push(&path, vec![snap(item.clone())])?;
			values.push(item);
		}
		2 => {
			draft.unshift(&path, vec![snap(item.clone())])?;
			values.insert(0, item);
		}
		3 if length > 0 => {
			draft.shift(&path)?;
			values.remove(0);
		}
		4 if length > 0 => {
			draft.pop(&path)?;
			values.pop();
		}
		5 => {
			let index = if length == 0 {
				0
			} else {
				usize::try_from(value)? % (length + 1)
			};
			let remove = if length == 0 { 0 } else { usize::try_from(value % 2)? };
			let remove = remove.min(length - index);
			draft.splice(&path, i64::try_from(index)?, Some(remove), vec![snap(item.clone())])?;
			values.splice(index..index + remove, [item]);
		}
		6 => {
			draft.reverse(&path)?;
			values.reverse();
		}
		7 => {
			let id = |value: &Snapshot| value.get("id").and_then(|id| id.to_value().as_i64());
			draft.sort_by(&path, |left, right| id(left).cmp(&id(right)))?;
			values.sort_by_key(|value| value["id"].as_i64());
		}
		8 if length > 0 => {
			let index = usize::try_from(value)? % length;
			draft.set(&p!["values", index, "score"], Some(snap(json!(value))))?;
			values[index]["score"] = json!(value);
		}
		9 => {
			let revision = model["meta"]["revision"].as_i64().context("revision")? + 1;
			draft.set(&p!["meta", "revision"], Some(snap(json!(revision))))?;
			draft.set(&p!["meta", "label"], Some(Snapshot::string(&format!("r-{value}"))))?;
			model["meta"]["revision"] = json!(revision);
			model["meta"]["label"] = json!(format!("r-{value}"));
		}
		3 | 4 | 8 => {}
		_ => {
			draft.delete(&p!["meta", "label"])?;
			model["meta"].as_object_mut().context("meta")?.shift_remove("label");
		}
	}
	Ok(())
}

// TS: delta-tracker/tracker.test.ts > randomized transactions > matches policy, detached replay, and adopted state
// after multi-operation transactions
#[test]
fn matches_the_model_and_the_replay_after_random_transactions() {
	for seed in 1..=40_u32 {
		let mut rng = Random(seed);
		let values: Vec<JsonValue> = (0..4).map(|id| json!({"id": id, "score": 0})).collect();
		let mut model = json!({"values": values, "text": "start", "meta": {"revision": 0}});
		let mut tracker = track(snap(model.clone()));
		for transaction in 0..25_i64 {
			let prepared = settle(&mut tracker, |change| {
				for operation in 0..5 {
					#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "in 0..11")]
					let choice = (rng.next() * 11.0).floor() as u32;
					let value = i64::from(seed) * 10_000 + transaction * 10 + operation;
					mutate_both(change.state(), &mut model, choice, value).unwrap();
				}
			});
			assert_eq!(
				prepared.value().to_value(),
				model,
				"seed {seed} transaction {transaction}"
			);
		}
	}
}

// TS: delta-tracker/tracker.test.ts > object emission scaling > orders many reverse-depth object edits through the
// bounded fallback
#[test]
fn orders_many_reverse_depth_object_edits() {
	let mut initial = json!({"value": 0});
	for _ in 0..512 {
		initial = json!({"value": 0, "next": initial});
	}
	let mut tracker = track(snap(initial));
	let prepared = settle(&mut tracker, |change| {
		for depth in (0..=512).rev() {
			let mut path: Vec<Seg> = std::iter::repeat_n(Seg::Key("next".to_owned()), depth).collect();
			path.push(Seg::Key("value".to_owned()));
			change.state().set(&path, Some(snap(json!(depth + 1)))).unwrap();
		}
	});
	assert_eq!(prepared.ops().len(), 513);
}

// TS: delta-tracker/tracker.test.ts > security and storage-style cloning > handles reserved own keys without
// prototype pollution (the tracker never emits a reserved segment: the object holding one is set whole)
#[test]
fn handles_reserved_own_keys() {
	let initial = pi_json::parse(r#"{"safe":{"__proto__":{"value":1}}}"#).unwrap();
	let mut tracker = track(snap(initial));
	let prepared = settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["safe", "__proto__", "value"], Some(snap(json!(2))))
			.unwrap();
	});
	assert!(matches!(&prepared.ops()[..], [Op::Set(path, _)] if path.to_vec() == p!["safe"]));
	assert_eq!(value_at(&tracker, &p!["safe", "__proto__", "value"]), json!(2));
}

// TS: delta-apply-immutable.test.ts > immutable container copies > keep an own __proto__ member as data (the tracker
// half)
#[test]
fn keeps_an_own_proto_member_as_data_through_the_tracker() {
	let base = pi_json::parse(r#"{"__proto__":{"polluted":true},"n":1}"#).unwrap();
	let mut tracker = track(snap(base));
	settle(&mut tracker, |change| {
		change.state().set(&p!["n"], Some(snap(json!(2)))).unwrap();
	});
	assert_eq!(
		to_js_string(&tracker.value().to_value()),
		r#"{"__proto__":{"polluted":true},"n":2}"#
	);
}

// TS: delta-apply-immutable.test.ts > checked immutable operation application > handles tracker-produced batches
// across arbitrary revision boundaries
#[test]
fn handles_tracker_produced_batches_across_arbitrary_revision_boundaries() {
	let values: Vec<JsonValue> = (0..8).map(|id| json!({"id": id, "score": 0})).collect();
	let initial = snap(json!({"text": "start", "values": values, "revision": 0}));
	let mut tracker = track(initial.clone());
	let mut batches: Vec<Arc<[Op]>> = Vec::new();
	for revision in 1..=40_usize {
		let prepared = settle(&mut tracker, |change| {
			let draft = change.state();
			draft.set(&p!["revision"], Some(snap(json!(revision)))).unwrap();
			let text = draft.get(&p!["text"]).unwrap().unwrap().as_str().unwrap()[1..].to_owned();
			draft
				.set(&p!["text"], Some(Snapshot::string(&format!("{text}{revision}"))))
				.unwrap();
			let length = draft.get(&p!["values"]).unwrap().unwrap().as_array().unwrap().len();
			match revision % 5 {
				0 => draft.reverse(&p!["values"]).unwrap(),
				1 => drop(
					draft
						.push(
							&p!["values"],
							vec![snap(json!({"id": 100 + revision, "score": revision}))],
						)
						.unwrap(),
				),
				2 => drop(draft.shift(&p!["values"]).unwrap()),
				3 => draft
					.set(&p!["values", revision % length, "score"], Some(snap(json!(revision))))
					.unwrap(),
				_ => drop(
					draft
						.splice(
							&p!["values"],
							1,
							Some(1),
							vec![snap(json!({"id": 200 + revision, "score": revision}))],
						)
						.unwrap(),
				),
			}
		});
		batches.push(prepared.ops().clone());
	}
	let replayed = apply_immutable_batches(&initial, batches.iter().map(|batch| &batch[..])).unwrap();
	assert_eq!(replayed, *tracker.value());
}

// TS: delta-append-text.test.ts > appendText > records an append op and the appended value
#[test]
fn append_text_records_an_append_op_and_the_appended_value() {
	let doc =
		json!({"title": "t", "content": [{"type": "text", "text": "hello", "tags": ["a"]}], "meta": {"note": "n"}});
	let mut tracker = track(snap(doc));
	let before = tracker.value().clone();
	let prepared = settle(&mut tracker, |change| {
		change.state().append_text(&p!["content", 0, "text"], " world").unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["a", ["content", 0, "text"], " world"]]));
	assert_eq!(before.at(&p!["content", 0, "text"]).unwrap().as_str(), Some("hello"));
	let prepared = settle(&mut tracker, |change| {
		change.state().append_text(&p!["title"], "!").unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["a", ["title"], "!"]]));
	let prepared = settle(&mut tracker, |change| {
		change.state().append_text(&p!["meta", "note"], "ote").unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["a", ["meta", "note"], "ote"]]));
	assert_eq!(
		tracker.value().to_value(),
		json!({"title": "t!", "content": [{"type": "text", "text": "hello world", "tags": ["a"]}], "meta": {"note": "note"}})
	);
}

// TS: delta-append-text.test.ts > appendText > joins several appends to one key in one change into one op, next to
// other changes
#[test]
fn append_text_joins_appends_to_one_key_into_one_op() {
	let mut tracker = track(snap(json!({"title": "t", "content": [{"text": "hello"}]})));
	let prepared = settle(&mut tracker, |change| {
		let draft = change.state();
		draft.append_text(&p!["content", 0, "text"], " a").unwrap();
		draft.set(&p!["title"], Some(Snapshot::string("new"))).unwrap();
		draft.append_text(&p!["content", 0, "text"], " b").unwrap();
		draft.append_text(&p!["content", 0, "text"], "").unwrap();
		draft.append_text(&p!["content", 0, "text"], " c").unwrap();
	});
	assert_eq!(prepared.ops().len(), 2);
	assert!(
		prepared
			.ops()
			.contains(&ops(json!([["a", ["content", 0, "text"], " a b c"]])).remove(0))
	);
	assert!(
		prepared
			.ops()
			.contains(&ops(json!([["s", ["title"], "new"]])).remove(0))
	);
}

// TS: delta-append-text.test.ts > appendText > falls back to the string comparison when the same key is also assigned
// or deleted
#[test]
fn append_text_falls_back_to_the_comparison_after_a_plain_write() {
	let mut tracker = track(snap(
		json!({"title": "t", "content": [{"text": "hello"}], "meta": {"note": "n"}}),
	));
	let prepared = settle(&mut tracker, |change| {
		change.state().append_text(&p!["content", 0, "text"], " x").unwrap();
		let text = format!(
			"{} y",
			change
				.state()
				.get(&p!["content", 0, "text"])
				.unwrap()
				.unwrap()
				.as_str()
				.unwrap()
		);
		change
			.state()
			.set(&p!["content", 0, "text"], Some(Snapshot::string(&text)))
			.unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["a", ["content", 0, "text"], " x y"]]));
	let prepared = settle(&mut tracker, |change| {
		change.state().append_text(&p!["content", 0, "text"], " z").unwrap();
		change
			.state()
			.set(&p!["content", 0, "text"], Some(Snapshot::string("replaced")))
			.unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["s", ["content", 0, "text"], "replaced"]]));
	let prepared = settle(&mut tracker, |change| {
		change
			.state()
			.set(&p!["title"], Some(Snapshot::string("assigned")))
			.unwrap();
		change.state().append_text(&p!["title"], " then appended").unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["s", ["title"], "assigned then appended"]]));
	let prepared = settle(&mut tracker, |change| {
		change.state().append_text(&p!["meta", "note"], "x").unwrap();
		change.state().delete(&p!["meta", "note"]).unwrap();
	});
	assert_eq!(ops_json(&prepared), json!([["d", ["meta", "note"]]]));
}

// TS: delta-append-text.test.ts > appendText > appends inside a block the same change inserted or replaced
#[test]
fn append_text_inside_a_block_the_change_inserted_or_replaced() {
	let mut tracker = track(snap(json!({"content": [{"text": "hello"}]})));
	settle(&mut tracker, |change| {
		let draft = change.state();
		draft.push(&p!["content"], vec![snap(json!({"text": "new"}))]).unwrap();
		draft.append_text(&p!["content", 1, "text"], " block").unwrap();
		draft
			.set(&p!["content", 0], Some(snap(json!({"text": "other"}))))
			.unwrap();
		draft.append_text(&p!["content", 0, "text"], " one").unwrap();
	});
	assert_eq!(
		value_at(&tracker, &p!["content"]),
		json!([{"text": "other one"}, {"text": "new block"}])
	);
}

// TS: delta-append-text.test.ts > appendText > rejects what a JavaScript caller can get wrong, and accepts only keys
// whose type is exactly string (the runtime half: a missing key, a non-string, an array element)
#[test]
fn append_text_rejects_what_is_not_a_string_member() {
	let mut tracker = track(snap(json!({"title": "t", "content": [{"text": "hello", "tags": []}]})));
	let mut change = tracker.begin_change();
	let draft = change.state();
	let mut message = |path: &[Seg]| draft.append_text(path, "x").err().map(|error| error.to_string());
	assert_eq!(
		message(&p!["content"]).as_deref(),
		Some(r#"appendText() needs a string at "content""#)
	);
	assert_eq!(
		message(&p!["missing"]).as_deref(),
		Some(r#"appendText() needs a string at "missing""#)
	);
	assert_eq!(
		message(&p!["content", 0]).as_deref(),
		Some(r#"appendText() needs a string at "0""#)
	);
	assert_eq!(
		message(&p!["content", 0, "tags"]).as_deref(),
		Some(r#"appendText() needs a string at "tags""#)
	);
	assert_eq!(
		message(&p!["nowhere", "text"]).as_deref(),
		Some(r#"unresolvable path: ["nowhere","text"]"#)
	);
	change.prepare().unwrap();
	assert_eq!(change.state().append_text(&p!["title"], "x"), Err(DraftError::Settled));
	let mut aborted = tracker.begin_change();
	aborted.abort();
	assert_eq!(aborted.state().append_text(&p!["title"], "x"), Err(DraftError::Settled));
}

// TS: delta-append-text.test.ts > appendText cost > costs O(chunk) per change. Measured, not asserted against TS: a
// `Snapshot` string is one `Arc<str>`, so each append copies the string (D0a carry-over, rust/docs/slices.md); this
// bounds the copying at a 500 KB stream of 40-byte chunks.
#[test]
fn append_text_streams_half_a_megabyte_in_bounded_time() {
	let mut tracker = track(snap(json!({"content": [{"text": ""}]})));
	let chunk = "0123456789abcdefghij0123456789abcdefghij";
	let started = std::time::Instant::now();
	for _ in 0..500_000 / chunk.len() {
		let mut change = tracker.begin_change();
		change.state().append_text(&p!["content", 0, "text"], chunk).unwrap();
		let prepared = change.prepare().unwrap();
		assert_eq!(prepared.ops().len(), 1);
		tracker.adopt(&prepared).unwrap();
	}
	assert_eq!(
		tracker
			.value()
			.at(&p!["content", 0, "text"])
			.unwrap()
			.as_str()
			.unwrap()
			.len(),
		500_000
	);
	// One copy of the text per change (about 3 GB moved in all): seconds in a debug build, well under one in release.
	assert!(
		started.elapsed() < std::time::Duration::from_secs(30),
		"{:?}",
		started.elapsed()
	);
}

// TS: delta-op-text-retention.test.ts > tracker append ops do not retain the string they were cut from, and a replica
// that applies a long stream of appends retains the text once (the op and replica halves; retention is V8-only)
#[test]
fn a_replica_of_a_stream_of_appends_matches_the_tracker() {
	let mut tracker = track(snap(json!({"text": "x".repeat(100_000)})));
	let mut replica = tracker.value().clone();
	for index in 0..300 {
		let prepared = settle(&mut tracker, |change| {
			let text = format!(
				"{}line {index} of a long streamed text\n",
				change.state().get(&p!["text"]).unwrap().unwrap().as_str().unwrap()
			);
			change.state().set(&p!["text"], Some(Snapshot::string(&text))).unwrap();
		});
		assert!(matches!(prepared.ops()[..], [Op::Append(..)]));
		replica = apply_immutable(&replica, prepared.ops()).unwrap();
	}
	assert_eq!(replica, *tracker.value());
}

// TS: delta-clone.test.ts > tracker ownership > takes immutable ownership of the imported revision in O(1)
#[test]
fn takes_ownership_of_the_imported_revision_in_o1() {
	let input = snap(json!({"point": {"x": 3, "y": 7}, "rows": [{"values": [0, false, null, "text", {"n": 1}]}]}));
	let tracker = track(input.clone());
	assert!(tracker.value().ptr_eq(&input));
	assert!(
		tracker
			.value()
			.get("point")
			.unwrap()
			.ptr_eq(input.get("point").unwrap())
	);
}

// TS: delta-tracker/retention.test.ts > validates %s retention semantics (abandoned-draft-released,
// tracker-with-open-draft, aborted-payload, obsolete-revisions): the tracker holds no draft, an aborted change drops
// its tree, and an adopted revision does not keep older ones alive.
#[test]
fn retains_no_draft_and_no_obsolete_revision() {
	let mut tracker = track(snap(json!({"rows": [{"value": 0}]})));
	let mut change = tracker.begin_change();
	change
		.state()
		.push(&p!["rows"], vec![snap(json!({"big": "x".repeat(1000)}))])
		.unwrap();
	let Some(Snapshot::Array(rows)) = change.state().get(&p!["rows"]).unwrap() else {
		panic!("rows")
	};
	let weak = Arc::downgrade(&rows);
	drop(rows);
	change.abort();
	assert!(weak.upgrade().is_none(), "an aborted change keeps its tree");

	let mut abandoned = tracker.begin_change();
	abandoned.state().push(&p!["rows"], vec![snap(json!(1))]).unwrap();
	let Some(Snapshot::Array(rows)) = abandoned.state().get(&p!["rows"]).unwrap() else {
		panic!("rows")
	};
	let weak = Arc::downgrade(&rows);
	drop((rows, abandoned));
	assert!(weak.upgrade().is_none(), "the tracker keeps a dropped draft");

	let Snapshot::Object(root) = tracker.value().clone() else {
		panic!("object")
	};
	let first = Arc::downgrade(&root);
	drop(root);
	for index in 0..3 {
		settle(&mut tracker, |change| {
			change
				.state()
				.set(&p!["rows", 0, "value"], Some(snap(json!(index))))
				.unwrap();
		});
	}
	assert!(first.upgrade().is_none(), "an obsolete revision stays alive");
}

// Rust-only: the D0b structural-sharing test across adopts (pointer test as in D0a).
#[test]
fn revisions_share_every_untouched_subtree_across_adopts() {
	let members: serde_json::Map<String, JsonValue> = (0..10_000)
		.map(|index| (format!("sibling{index}"), json!({"index": index, "nested": [index]})))
		.collect();
	let mut tracker = track(snap(JsonValue::Object(members)));
	let revisions: Vec<Snapshot> = (0..5)
		.map(|round| {
			settle(&mut tracker, |change| {
				change
					.state()
					.set(&p![format!("sibling{round}").as_str(), "index"], Some(snap(json!(-1))))
					.unwrap();
				change.state().append_text(&p!["log"], "x").unwrap_or_else(|_| {
					change.state().set(&p!["log"], Some(Snapshot::string("x"))).unwrap();
				});
			})
			.value()
			.clone()
		})
		.collect();
	for pair in revisions.windows(2) {
		for index in 5..10_000 {
			let key = format!("sibling{index}");
			assert!(pair[0].get(&key).unwrap().ptr_eq(pair[1].get(&key).unwrap()), "{key}");
		}
	}
	// A sibling edited in round 1 is shared by every later revision.
	assert!(
		revisions[1]
			.get("sibling1")
			.unwrap()
			.ptr_eq(revisions[4].get("sibling1").unwrap())
	);
	assert!(
		!revisions[0]
			.get("sibling1")
			.unwrap()
			.ptr_eq(revisions[1].get("sibling1").unwrap())
	);
}

// Rust-only (D0b review): a streamed append inside a block plus a structural edit of the block list (Durable's
// `assignJson` at every block boundary) keeps the append an `a` and splices only the new block, as TS pieces do.
#[test]
fn an_append_inside_an_element_survives_a_structural_edit_of_its_array() {
	let long = "x".repeat(10_000);
	// (edit, index of the new block, index of the appended block afterwards)
	for (name, index, at) in [("push", 2, 0), ("unshift", 0, 1), ("splice", 1, 0)] {
		let blocks = json!({"content": [{"type": "text", "text": long}, {"type": "text", "text": "tail"}]});
		let mut tracker = track(snap(blocks));
		let prepared = settle(&mut tracker, |change| {
			let draft = change.state();
			draft.append_text(&p!["content", 0, "text"], "!").unwrap();
			let block = vec![snap(json!({"type": "tool", "id": 1}))];
			match name {
				"push" => drop(draft.push(&p!["content"], block).unwrap()),
				"unshift" => drop(draft.unshift(&p!["content"], block).unwrap()),
				_ => drop(draft.splice(&p!["content"], 1, Some(0), block).unwrap()),
			}
		});
		let mut ops_seen = ops_json(&prepared).as_array().unwrap().clone();
		ops_seen.sort_by_key(|op| op[0].as_str().unwrap().to_owned());
		assert_eq!(
			JsonValue::Array(ops_seen),
			json!([["a", ["content", at, "text"], "!"], ["p", ["content"], index, 0, [{"type": "tool", "id": 1}]]]),
			"{name}"
		);
	}
}

// Rust-only (D0b review): a reorder with an element edited after it is one permutation plus the edit, as TS emits.
#[test]
fn a_reorder_then_an_edit_is_a_permutation_plus_the_edit() {
	let mut tracker = track(snap(
		json!({"values": [{"rank": 3, "edited": 0}, {"rank": 1, "edited": 0}, {"rank": 2, "edited": 0}]}),
	));
	let prepared = settle(&mut tracker, |change| {
		let rank = |value: &Snapshot| value.get("rank").unwrap().to_value().as_i64().unwrap();
		change
			.state()
			.sort_by(&p!["values"], |left, right| rank(left).cmp(&rank(right)))
			.unwrap();
		change
			.state()
			.set(&p!["values", 2, "edited"], Some(snap(json!(1))))
			.unwrap();
	});
	assert_eq!(
		ops_json(&prepared),
		json!([["m", ["values"], [1, 2, 0]], ["s", ["values", 2, "edited"], 1]])
	);
}

// TS: delta-tracker/retention.test.ts > validates stale-unprepared-change retention semantics: adopting a competitor
// releases a stale change's trees at once, before the change is dropped.
#[test]
fn a_stale_change_releases_its_trees_on_adoption() {
	let mut tracker = track(snap(json!({"rows": []})));
	let mut stale = tracker.begin_change();
	stale
		.state()
		.push(&p!["rows"], vec![snap(json!({"big": "x".repeat(1000)}))])
		.unwrap();
	let Some(Snapshot::Array(rows)) = stale.state().get(&p!["rows"]).unwrap() else {
		panic!("rows")
	};
	let weak = Arc::downgrade(&rows);
	drop(rows);
	settle(&mut tracker, |change| {
		change.state().set(&p!["n"], Some(snap(json!(1)))).unwrap();
	});
	assert!(weak.upgrade().is_none());
	assert_eq!(stale.state().get(&[]), Err(DraftError::Settled));
}
