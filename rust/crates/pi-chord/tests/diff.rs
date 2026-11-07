//! `diff_revisions` (src/delta/diff.rs), ported from `packages/chord/test/delta-diff.test.ts` case by case: the ops
//! TS gives where the TS test pins them, and every result replays to `after`, key order included.

use pi_chord::delta::{
	Decoder, Encoder, Op, Snapshot, WireOp, apply_immutable, assert_valid_op, assert_valid_wire_op, diff_revisions,
};
use pi_json::{JsonValue, to_js_string};
use serde_json::json;

fn snap(value: &JsonValue) -> Snapshot {
	Snapshot::from(value)
}

fn ops(value: JsonValue) -> Vec<Op> {
	ok(serde_json::from_value(value))
}

/// The value of a result a test helper expects to succeed.
fn ok<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
	result.unwrap_or_else(|error| panic!("{error:?}"))
}

fn object(members: Vec<(&str, Snapshot)>) -> Snapshot {
	Snapshot::object(members.into_iter().map(|(key, value)| (key.to_owned(), value)))
}

fn values(items: Vec<Snapshot>) -> Snapshot {
	object(vec![("values", Snapshot::array(items))])
}

/// The diff replays to a value equal to `after` and every op is valid.
fn check_replay(before: &Snapshot, after: &Snapshot, diff: &[Op]) {
	for op in diff {
		ok(assert_valid_op(op.to_json()));
	}
	let replayed = ok(apply_immutable(before, diff));
	// TS `toEqual`: key order is not compared; `diff_revisions` ignores it, as TS does.
	assert_eq!(replayed, *after);
}

fn expect_diff(before: &Snapshot, after: &Snapshot, expected: JsonValue) {
	let diff = diff_revisions(before, after);
	assert_eq!(diff, ops(expected));
	check_replay(before, after, &diff);
}

fn ids(names: &[&str]) -> Vec<Snapshot> {
	names.iter().map(|id| snap(&json!({"id": id}))).collect()
}

// TS: delta-diff.test.ts > immutable revision diff > emits sets and deletes
#[test]
fn emits_sets_and_deletes() {
	expect_diff(
		&snap(&json!({"keep": 1, "change": 1, "remove": true})),
		&snap(&json!({"keep": 1, "change": 2, "add": 3})),
		json!([["s", ["change"], 2], ["s", ["add"], 3], ["d", ["remove"]]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > emits string append and front truncation
#[test]
fn emits_string_append_and_front_truncation() {
	expect_diff(
		&snap(&json!({"text": "hello"})),
		&snap(&json!({"text": "hello world"})),
		json!([["a", ["text"], " world"]]),
	);
	expect_diff(
		&snap(&json!({"text": "hello world"})),
		&snap(&json!({"text": "world"})),
		json!([["t", ["text"], 6]]),
	);
	expect_diff(
		&snap(&json!({"text": "abcdefgh"})),
		&snap(&json!({"text": "defghxyz"})),
		json!([["t", ["text"], 3], ["a", ["text"], "xyz"]]),
	);
}

// Rust-only: `t` counts UTF-16 units and the appended rest starts at the right byte (D0a carry-over).
#[test]
fn rolling_windows_count_utf16_units() {
	expect_diff(
		&snap(&json!({"text": "😀é日x"})),
		&snap(&json!({"text": "é日xyz😀"})),
		json!([["t", ["text"], 2], ["a", ["text"], "yz😀"]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > represents array insertion, removal, and shift with splices
#[test]
fn represents_array_insertion_removal_and_shift_with_splices() {
	let [a, b, c] = <[Snapshot; 3]>::try_from(ids(&["a", "b", "c"])).unwrap();
	expect_diff(
		&values(vec![a.clone(), b.clone()]),
		&values(vec![a.clone(), c.clone(), b.clone()]),
		json!([["p", ["values"], 1, 0, [{"id": "c"}]]]),
	);
	expect_diff(
		&values(vec![a, b.clone(), c.clone()]),
		&values(vec![b, c]),
		json!([["p", ["values"], 0, 1, []]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > collapses a same-length queue update to two splices
#[test]
fn collapses_a_same_length_queue_update_to_two_splices() {
	let [a, b, c, d] = <[Snapshot; 4]>::try_from(ids(&["a", "b", "c", "d"])).unwrap();
	expect_diff(
		&values(vec![a, b.clone(), c.clone()]),
		&values(vec![b, c, d]),
		json!([["p", ["values"], 0, 1, []], ["p", ["values"], 2, 0, [{"id": "d"}]]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > emits a permutation for a pure reorder
#[test]
fn emits_a_permutation_for_a_pure_reorder() {
	let [a, b, c] = <[Snapshot; 3]>::try_from(ids(&["a", "b", "c"])).unwrap();
	expect_diff(
		&values(vec![a.clone(), b.clone(), c.clone()]),
		&values(vec![c, a, b]),
		json!([["m", ["values"], [2, 0, 1]]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > normalizes reordered distinct deeply-equal objects to a no-op
#[test]
fn normalizes_reordered_distinct_deeply_equal_objects_to_a_no_op() {
	let first = snap(&json!({"nested": {"value": 1}}));
	let second = snap(&json!({"nested": {"value": 1}}));
	assert!(!first.ptr_eq(&second));
	let diff = diff_revisions(
		&values(vec![first.clone(), second.clone()]),
		&values(vec![second, first]),
	);
	assert_eq!(diff.len(), 0);
}

// TS: delta-diff.test.ts > immutable revision diff > validates and encodes permutations
#[test]
fn validates_and_encodes_permutations() {
	let batch = ops(json!([["m", ["values"], [2, 0, 1]], ["m", ["values"], [1, 2, 0]]]));
	let wire = Encoder::new().encode(&batch);
	let frame = JsonValue::Array(wire.iter().map(WireOp::to_json).collect());
	assert_eq!(frame, json!([["m", ["values"], [2, 0, 1]], ["m", [1, 2, 0]]]));
	for op in &wire {
		assert_valid_wire_op(op.to_json()).unwrap();
	}
	assert_eq!(Decoder::new().decode(&wire).unwrap(), batch);
	let error = assert_valid_op(json!(["m", ["values"], [0, 0]])).unwrap_err();
	assert!(error.to_string().contains("bijection"));
}

// TS: delta-diff.test.ts > immutable revision diff > emits nothing for deeply equal reconstructed values
#[test]
fn emits_nothing_for_deeply_equal_reconstructed_values() {
	let pairs = [
		json!({"value": {"nested": [1, 2]}}),
		json!({"values": [{"id": 1}, {"id": 2}]}),
		json!({"values": [true, true, true]}),
	];
	for value in pairs {
		assert_eq!(diff_revisions(&snap(&value), &snap(&value)).len(), 0);
	}
}

// TS: delta-diff.test.ts > immutable revision diff > keeps a leaf edit inside a reconstructed array narrow
#[test]
fn keeps_a_leaf_edit_inside_a_reconstructed_array_narrow() {
	expect_diff(
		&snap(&json!({"values": [{"id": 1, "label": "one"}, {"id": 2, "label": "two"}]})),
		&snap(&json!({"values": [{"id": 1, "label": "one"}, {"id": 2, "label": "changed"}]})),
		json!([["s", ["values", 1, "label"], "changed"]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > emits payload-free splices for scattered removals
#[test]
#[expect(clippy::many_single_char_names, reason = "the rows of the TS test, a to e")]
fn emits_payload_free_splices_for_scattered_removals() {
	let [a, b, c, d, e] = <[Snapshot; 5]>::try_from(ids(&["a", "b", "c", "d", "e"])).unwrap();
	expect_diff(
		&values(vec![a.clone(), b, c.clone(), d, e.clone()]),
		&values(vec![a, c, e]),
		json!([["p", ["values"], 1, 1, []], ["p", ["values"], 2, 1, []]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > encodes %s removal canonically (front, tail, middle, all, none)
#[test]
fn encodes_removals_canonically() {
	let cases = [
		(json!([2, 3, 4]), json!([["p", ["values"], 0, 1, []]])),
		(json!([1, 2, 3]), json!([["p", ["values"], 3, 1, []]])),
		(json!([1, 3, 4]), json!([["p", ["values"], 1, 1, []]])),
		(json!([]), json!([["p", ["values"], 0, 4, []]])),
		(json!([1, 2, 3, 4]), json!([])),
	];
	for (after, expected) in cases {
		expect_diff(
			&snap(&json!({"values": [1, 2, 3, 4]})),
			&snap(&json!({"values": after})),
			expected,
		);
	}
}

// TS: delta-diff.test.ts > immutable revision diff > does not field-diff unrelated shifted objects with common fields
#[test]
fn does_not_field_diff_unrelated_shifted_objects_with_common_fields() {
	expect_diff(
		&snap(
			&json!({"values": [{"type": "row", "value": 1}, {"type": "row", "value": 2}, {"type": "row", "value": 3}]}),
		),
		&snap(
			&json!({"values": [{"type": "row", "value": 2}, {"type": "row", "value": 3}, {"type": "row", "value": 4}]}),
		),
		json!([["p", ["values"], 0, 1, []], ["p", ["values"], 2, 0, [{"type": "row", "value": 4}]]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > does not treat coincidental id or key fields as structural
// identity
#[test]
fn does_not_treat_coincidental_id_or_key_fields_as_structural_identity() {
	let left = snap(&json!({"value": "left"}));
	let right = snap(&json!({"value": "right"}));
	let first = snap(&json!({"id": 1, "key": "a", "value": "first"}));
	let second = snap(&json!({"id": 2, "key": "b", "value": "second"}));
	let replacements =
		json!([{"id": 2, "key": "b", "value": "edited-second"}, {"id": 1, "key": "a", "value": "edited-first"}]);
	let edited: Vec<Snapshot> = replacements.as_array().unwrap().iter().map(snap).collect();
	expect_diff(
		&values(vec![left.clone(), first, second, right.clone()]),
		&values(vec![left, edited[0].clone(), edited[1].clone(), right]),
		json!([["p", ["values"], 1, 2, replacements]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > combines removals, append, and a shared-subtree survivor edit
#[test]
fn combines_removals_append_and_a_shared_subtree_survivor_edit() {
	let row = |id: &str| {
		let stable = snap(&json!({}));
		let row = object(vec![
			("id", Snapshot::string(id)),
			("stable", stable.clone()),
			("detail", snap(&json!({"text": id}))),
		]);
		(row, stable)
	};
	let ((a, _), (b, _), (c, c_stable), (d, _)) = (row("a"), row("b"), row("c"), row("d"));
	let changed_c = object(vec![
		("id", Snapshot::string("c")),
		("stable", c_stable),
		("detail", snap(&json!({"text": "changed"}))),
	]);
	let appended = snap(&json!({"id": "e", "stable": {}, "detail": {"text": "e"}}));
	expect_diff(
		&values(vec![a.clone(), b, c, d.clone()]),
		&values(vec![a, changed_c, d, appended]),
		json!([
			["p", ["values"], 1, 1, []],
			["a", ["values", 1, "detail", "text"], "hanged"],
			["p", ["values"], 3, 0, [{"id": "e", "stable": {}, "detail": {"text": "e"}}]],
		]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > does not retain a %i-byte removed-neighbor payload
#[test]
fn does_not_retain_a_removed_neighbor_payload() {
	for size in [256 * 1024, 1024 * 1024] {
		let payload = Snapshot::string(&"x".repeat(size));
		let retained: Vec<Snapshot> = ["a", "b", "c", "d", "e"]
			.iter()
			.map(|id| object(vec![("id", Snapshot::string(id)), ("payload", payload.clone())]))
			.collect();
		let before = values(retained.clone());
		let after = values(vec![retained[0].clone(), retained[2].clone(), retained[4].clone()]);
		let diff = diff_revisions(&before, &after);
		assert_eq!(
			diff,
			ops(json!([["p", ["values"], 1, 1, []], ["p", ["values"], 2, 1, []]]))
		);
		let bytes = to_js_string(&JsonValue::Array(diff.iter().map(Op::to_json).collect()));
		assert!(bytes.len() < 100);
		check_replay(&before, &after, &diff);
	}
}

// TS: delta-diff.test.ts > immutable revision diff > keeps push, pop, and middle removal narrow at %,i items
#[test]
fn keeps_push_pop_and_middle_removal_narrow() {
	for size in [1_001_usize, 10_000] {
		let rows: Vec<Snapshot> = (0..size).map(|value| snap(&json!({"value": value}))).collect();
		let appended = snap(&json!({"value": size}));
		let mut pushed = rows.clone();
		pushed.push(appended);
		expect_diff(
			&values(rows.clone()),
			&values(pushed),
			json!([["p", ["values"], size, 0, [{"value": size}]]]),
		);
		expect_diff(
			&values(rows.clone()),
			&values(rows[..size - 1].to_vec()),
			json!([["p", ["values"], size - 1, 1, []]]),
		);
		let middle = size / 2;
		let mut removed = rows.clone();
		removed.remove(middle);
		expect_diff(
			&values(rows),
			&values(removed),
			json!([["p", ["values"], middle, 1, []]]),
		);
	}
}

// TS: delta-diff.test.ts > immutable revision diff > keeps forty-thousand-row sparse edits narrow
#[test]
fn keeps_forty_thousand_row_sparse_edits_narrow() {
	let stables: Vec<Snapshot> = (0..40_000).map(|value| snap(&json!({"value": value}))).collect();
	let rows: Vec<Snapshot> = stables
		.iter()
		.enumerate()
		.map(|(value, stable)| object(vec![("value", snap(&json!(value))), ("stable", stable.clone())]))
		.collect();
	let mut after = rows.clone();
	let mut expected = Vec::new();
	for index in (100..after.len()).step_by(400) {
		let value = -i64::try_from(index).unwrap();
		after[index] = object(vec![("value", snap(&json!(value))), ("stable", stables[index].clone())]);
		expected.push(json!(["s", ["values", index, "value"], value]));
	}
	let (before, after) = (values(rows), values(after));
	let diff = diff_revisions(&before, &after);
	assert_eq!(diff, ops(JsonValue::Array(expected)));
	assert!(to_js_string(&JsonValue::Array(diff.iter().map(Op::to_json).collect())).len() < 7_500);
	check_replay(&before, &after, &diff);
}

// TS: delta-diff.test.ts > immutable revision diff > keeps a reconstructed large-array leaf edit narrow
#[test]
fn keeps_a_reconstructed_large_array_leaf_edit_narrow() {
	let rows: Vec<JsonValue> = (0..1_000)
		.map(|value| json!({"value": value, "label": format!("row-{value}")}))
		.collect();
	let mut edited = rows.clone();
	edited[700]["label"] = json!("changed");
	let (before, after) = (snap(&json!({"values": rows})), snap(&json!({"values": edited})));
	let diff = diff_revisions(&before, &after);
	assert_eq!(diff, ops(json!([["s", ["values", 700, "label"], "changed"]])));
	check_replay(&before, &after, &diff);
}

// TS: delta-diff.test.ts > immutable revision diff > splices an ambiguous equal-count moved-and-edited gap
#[test]
fn splices_an_ambiguous_equal_count_moved_and_edited_gap() {
	let left = snap(&json!({"value": "left"}));
	let right = snap(&json!({"value": "right"}));
	let replacements = json!([{"id": 2, "value": "edited"}, {"id": 1, "value": "also-edited"}]);
	let edited: Vec<Snapshot> = replacements.as_array().unwrap().iter().map(snap).collect();
	expect_diff(
		&values(vec![
			left.clone(),
			snap(&json!({"id": 1, "value": "a"})),
			snap(&json!({"id": 2, "value": "b"})),
			right.clone(),
		]),
		&values(vec![left, edited[0].clone(), edited[1].clone(), right]),
		json!([["p", ["values"], 1, 2, replacements]]),
	);
}

// TS: delta-diff.test.ts > immutable revision diff > keeps a large rotation payload-free
#[test]
fn keeps_a_large_rotation_payload_free() {
	let rows: Vec<Snapshot> = (0..10_000).map(|value| snap(&json!({"value": value}))).collect();
	let rotated = [&rows[1_000..], &rows[..1_000]].concat();
	let (before, after) = (values(rows), values(rotated));
	let diff = diff_revisions(&before, &after);
	assert_eq!(diff.len(), 1);
	assert!(matches!(diff[0], Op::Move(..)));
	assert!(to_js_string(&diff[0].to_json()).len() < 60_000);
	check_replay(&before, &after, &diff);
}

// TS: delta-diff.test.ts > immutable revision diff > encodes five hundred unshifts without snapshotting retained rows
#[test]
fn encodes_five_hundred_unshifts_without_snapshotting_retained_rows() {
	let payload = "x".repeat(100);
	let retained: Vec<Snapshot> = (0..10_000)
		.map(|value| snap(&json!({"value": value, "payload": payload})))
		.collect();
	let inserted: Vec<JsonValue> = (0..500).map(|value| json!({"value": -value - 1})).collect();
	let after: Vec<Snapshot> = inserted.iter().map(snap).chain(retained.iter().cloned()).collect();
	let (before, after) = (values(retained), values(after));
	let diff = diff_revisions(&before, &after);
	assert_eq!(diff, ops(json!([["p", ["values"], 0, 0, inserted]])));
	assert!(to_js_string(&diff[0].to_json()).len() < 20_000);
	check_replay(&before, &after, &diff);
}

// TS: delta-diff.test.ts > immutable revision diff > bounds wide-object operation emission with a root replacement
#[test]
fn bounds_wide_object_operation_emission_with_a_root_replacement() {
	let before: serde_json::Map<String, JsonValue> =
		(0..20_000).map(|index| (format!("field{index}"), json!(0))).collect();
	let after: serde_json::Map<String, JsonValue> =
		(0..20_000).map(|index| (format!("field{index}"), json!(1))).collect();
	let after = JsonValue::Object(after);
	let diff = diff_revisions(&snap(&JsonValue::Object(before)), &snap(&after));
	assert_eq!(diff, vec![Op::Replace(after)]);
}

// TS: delta-diff.test.ts > immutable revision diff > bounds a wide normalized array fallback with a root replacement
#[test]
fn bounds_a_wide_normalized_array_fallback_with_a_root_replacement() {
	let before = snap(&json!({"values": vec![0; 40_000]}));
	let after = json!({"values": vec![1; 40_000]});
	let diff = diff_revisions(&before, &snap(&after));
	assert_eq!(diff, vec![Op::Replace(after.clone())]);
	check_replay(&before, &snap(&after), &diff);
}

// Rust-only (D0b review): a key-order change costs what it costs in TS, which ignores order: one `s` here, none for
// a reorder alone.
#[test]
fn ignores_key_order_as_ts_does() {
	let deep: Vec<u64> = (0..50).collect();
	let before = snap(&json!({"a": 1, "b": {"deep": deep}, "c": 3}));
	let after = pi_json::parse(&format!(r#"{{"c":4,"a":1,"b":{{"deep":{deep:?}}}}}"#)).unwrap();
	let diff = diff_revisions(&before, &snap(&after));
	assert_eq!(diff, ops(json!([["s", ["c"], 4]])));
	check_replay(&before, &snap(&after), &diff);
	let reordered = pi_json::parse(&format!(r#"{{"c":3,"b":{{"deep":{deep:?}}},"a":1}}"#)).unwrap();
	assert_eq!(diff_revisions(&before, &snap(&reordered)).len(), 0);
}
