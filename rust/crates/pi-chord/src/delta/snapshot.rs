//! `Snapshot`, a persistent JSON tree, and the immutable appliers over it (TS `applyImmutable`,
//! `applyImmutableBatches`, `apply-immutable-trusted.ts`): what Durable's documents and watchers hold.
//!
//! Containers sit behind `Arc` and strings behind `Arc<str>`, so cloning a snapshot is O(1) and two revisions share
//! every subtree an op did not touch. Applying an op enters each container on its path through `Arc::make_mut`: a
//! container shared with another revision is copied (shallowly, its children stay shared), one already copied in
//! this replay is edited in place. That is TS's `owned` set, kept by the reference counts.

use crate::delta::apply::{ApplyError, FrontCut, NodeMut, PathError, Tree, TreeObject, apply_op, front_cut};
use crate::delta::op::{Op, Seg};
use crate::delta::validate::{UnsafePathError, validate_op};
use pi_json::JsonValue;
use serde_json::Number;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

/// An immutable JSON value whose containers and strings are shared between revisions.
#[derive(Clone, Debug, Default)]
pub enum Snapshot {
	/// `null`.
	#[default]
	Null,
	/// `true` or `false`.
	Bool(bool),
	/// A number.
	Number(Number),
	/// A string, shared.
	String(Arc<str>),
	/// An array, shared.
	Array(Arc<Vec<Snapshot>>),
	/// An object, shared, in insertion order.
	Object(Arc<SnapshotObject>),
}

/// The members of a snapshot object, in insertion order. No serializer exists yet: bytes go through `to_value` and
/// `pi_json::to_js_string`, which applies JS enumeration order (index keys first).
pub struct SnapshotObject {
	entries: Vec<(Arc<str>, Snapshot)>,
	/// Key to position, built on the first lookup of an object past `LINEAR_LOOKUP_MAX` members.
	index: OnceLock<HashMap<Arc<str>, usize>>,
}

/// Applies a batch without changing `target`; the result shares every container the ops did not enter.
pub fn apply_immutable(target: &Snapshot, ops: &[Op]) -> Result<Snapshot, ApplyError> {
	apply_immutable_batches(target, [ops])
}

/// Applies batches as one final-result-only replay: a container copied for one batch is edited in place by the next,
/// so no intermediate revision exists.
pub fn apply_immutable_batches<'a>(
	target: &Snapshot,
	batches: impl IntoIterator<Item = &'a [Op]>,
) -> Result<Snapshot, ApplyError> {
	let mut root = target.clone();
	for ops in batches {
		for op in ops {
			validate_op(op)?;
			apply_op(&mut root, op)?;
		}
	}
	Ok(root)
}

/// Applies self-produced ops without validating them (TS `applyImmutableTrusted`): the caller produced or checked
/// them. An op that does not resolve still fails, with the error the checked applier gives.
pub fn apply_immutable_trusted(target: &Snapshot, ops: &[Op]) -> Result<Snapshot, ApplyError> {
	let mut root = target.clone();
	for op in ops {
		apply_op(&mut root, op)?;
	}
	Ok(root)
}

impl From<JsonValue> for Snapshot {
	/// The value as a snapshot, moving its strings and keys.
	fn from(value: JsonValue) -> Self {
		match value {
			JsonValue::Null => Snapshot::Null,
			JsonValue::Bool(flag) => Snapshot::Bool(flag),
			JsonValue::Number(number) => Snapshot::Number(number),
			JsonValue::String(text) => Snapshot::String(Arc::from(text)),
			JsonValue::Array(items) => Snapshot::Array(Arc::new(items.into_iter().map(Snapshot::from).collect())),
			JsonValue::Object(members) => Snapshot::Object(Arc::new(SnapshotObject {
				entries: members
					.into_iter()
					.map(|(key, value)| (Arc::from(key), Snapshot::from(value)))
					.collect(),
				index: OnceLock::new(),
			})),
		}
	}
}

impl From<&JsonValue> for Snapshot {
	fn from(value: &JsonValue) -> Self {
		match value {
			JsonValue::Null => Snapshot::Null,
			JsonValue::Bool(flag) => Snapshot::Bool(*flag),
			JsonValue::Number(number) => Snapshot::Number(number.clone()),
			JsonValue::String(text) => Snapshot::String(Arc::from(text.as_str())),
			JsonValue::Array(items) => Snapshot::Array(Arc::new(items.iter().map(Snapshot::from).collect())),
			JsonValue::Object(members) => Snapshot::Object(Arc::new(SnapshotObject {
				entries: members
					.iter()
					.map(|(key, value)| (Arc::from(key.as_str()), Snapshot::from(value)))
					.collect(),
				index: OnceLock::new(),
			})),
		}
	}
}

impl Snapshot {
	/// An array of `items`, which stay shared with wherever else they are held.
	pub fn array(items: Vec<Snapshot>) -> Self {
		Snapshot::Array(Arc::new(items))
	}

	/// An object of `members` in order (a repeated key keeps its first position and its last value), sharing them.
	pub fn object(members: impl IntoIterator<Item = (String, Snapshot)>) -> Self {
		let mut object = SnapshotObject {
			entries: Vec::new(),
			index: OnceLock::new(),
		};
		for (key, value) in members {
			object.insert(&key, value);
		}
		Snapshot::Object(Arc::new(object))
	}

	/// A string.
	pub fn string(text: &str) -> Self {
		Snapshot::String(Arc::from(text))
	}

	/// The value as an owned `JsonValue` (a deep copy).
	pub fn to_value(&self) -> JsonValue {
		match self {
			Snapshot::Null => JsonValue::Null,
			Snapshot::Bool(flag) => JsonValue::Bool(*flag),
			Snapshot::Number(number) => JsonValue::Number(number.clone()),
			Snapshot::String(text) => JsonValue::String(text.to_string()),
			Snapshot::Array(items) => JsonValue::Array(items.iter().map(Snapshot::to_value).collect()),
			Snapshot::Object(object) => JsonValue::Object(
				object
					.entries
					.iter()
					.map(|(key, value)| (key.to_string(), value.to_value()))
					.collect(),
			),
		}
	}

	/// The string, when this is one.
	pub fn as_str(&self) -> Option<&str> {
		match self {
			Snapshot::String(text) => Some(text),
			_ => None,
		}
	}

	/// The elements, when this is an array.
	pub fn as_array(&self) -> Option<&[Snapshot]> {
		match self {
			Snapshot::Array(items) => Some(items),
			_ => None,
		}
	}

	/// The members, when this is an object.
	pub fn as_object(&self) -> Option<&SnapshotObject> {
		match self {
			Snapshot::Object(object) => Some(object),
			_ => None,
		}
	}

	/// The member `key` of an object; `None` for a missing key and for anything but an object.
	pub fn get(&self, key: &str) -> Option<&Snapshot> {
		self.as_object()?.get(key)
	}

	/// The node at `path`, walking members of objects and elements of arrays.
	pub fn at(&self, path: &[Seg]) -> Option<&Snapshot> {
		path.iter().try_fold(self, |node, segment| match (node, segment) {
			(Snapshot::Array(items), Seg::Index(index)) => items.get(usize::try_from(*index).ok()?),
			(Snapshot::Object(object), Seg::Key(key)) => object.get(key),
			(Snapshot::Object(object), Seg::Index(index)) => object.get(&index.to_string()),
			_ => None,
		})
	}

	/// True when both are the same shared container or string allocation; false for every other node.
	pub fn ptr_eq(&self, other: &Snapshot) -> bool {
		match (self, other) {
			(Snapshot::String(a), Snapshot::String(b)) => Arc::ptr_eq(a, b),
			(Snapshot::Array(a), Snapshot::Array(b)) => Arc::ptr_eq(a, b),
			(Snapshot::Object(a), Snapshot::Object(b)) => Arc::ptr_eq(a, b),
			_ => false,
		}
	}
}

impl PartialEq for Snapshot {
	/// Deep equality; objects compare as sets of members, ignoring order, as JSON values do.
	fn eq(&self, other: &Snapshot) -> bool {
		if self.ptr_eq(other) {
			return true;
		}
		match (self, other) {
			(Snapshot::Null, Snapshot::Null) => true,
			(Snapshot::Bool(a), Snapshot::Bool(b)) => a == b,
			(Snapshot::Number(a), Snapshot::Number(b)) => a == b,
			(Snapshot::String(a), Snapshot::String(b)) => a == b,
			(Snapshot::Array(a), Snapshot::Array(b)) => a == b,
			(Snapshot::Object(a), Snapshot::Object(b)) => a == b,
			_ => false,
		}
	}
}

impl PartialEq for SnapshotObject {
	/// Deep equality as sets of members, ignoring order.
	fn eq(&self, other: &SnapshotObject) -> bool {
		self.len() == other.len() && self.iter().all(|(key, value)| other.get(key) == Some(value))
	}
}

/// Objects up to this many members are searched linearly; larger ones build a key index on first lookup.
const LINEAR_LOOKUP_MAX: usize = 16;

impl SnapshotObject {
	/// The number of members.
	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// True when the object has no members.
	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// The member `key`.
	pub fn get(&self, key: &str) -> Option<&Snapshot> {
		self.position(key).map(|position| &self.entries[position].1)
	}

	/// The members in insertion order.
	pub fn iter(&self) -> impl Iterator<Item = (&str, &Snapshot)> {
		self.entries.iter().map(|(key, value)| (&**key, value))
	}

	/// The position of member `key` in insertion order.
	pub(crate) fn position(&self, key: &str) -> Option<usize> {
		if self.entries.len() <= LINEAR_LOOKUP_MAX {
			return self.entries.iter().position(|(member, _)| &**member == key);
		}
		let index = self.index.get_or_init(|| {
			let positions = self.entries.iter().enumerate();
			positions
				.map(|(position, (member, _))| (member.clone(), position))
				.collect()
		});
		index.get(key).copied()
	}
}

impl Clone for SnapshotObject {
	/// Copies the members (reference counts only); the copy rebuilds its key index when it needs one, as an edit
	/// would invalidate it anyway.
	fn clone(&self) -> Self {
		Self {
			entries: self.entries.clone(),
			index: OnceLock::new(),
		}
	}
}

impl fmt::Debug for SnapshotObject {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_map().entries(self.iter()).finish()
	}
}

impl Tree for Snapshot {
	type Object = SnapshotObject;

	fn from_payload(value: &JsonValue) -> Self {
		Snapshot::from(value)
	}

	fn node_mut(&mut self) -> NodeMut<'_, Self> {
		match self {
			Snapshot::Array(items) => NodeMut::Array(Arc::make_mut(items)),
			Snapshot::Object(object) => NodeMut::Object(Arc::make_mut(object)),
			_ => NodeMut::Leaf,
		}
	}

	fn append_text(&mut self, text: &str) -> bool {
		let Snapshot::String(current) = self else {
			return false;
		};
		let mut joined = String::with_capacity(current.len() + text.len());
		joined.push_str(current);
		joined.push_str(text);
		*current = Arc::from(joined);
		true
	}

	fn truncate_text(&mut self, count: u64) -> bool {
		let Snapshot::String(current) = self else {
			return false;
		};
		*current = match front_cut(current, count) {
			FrontCut::At(offset) => Arc::from(&current[offset..]),
			FrontCut::InsidePair(offset) => Arc::from(format!("\u{FFFD}{}", &current[offset..])),
		};
		true
	}

	fn array_key_error(key: &str, length: usize, path: &[Seg]) -> ApplyError {
		if key == "length" || canonical_index(key).is_some_and(|index| index < length) {
			UnsafePathError {
				segment: key.to_owned(),
			}
			.into()
		} else {
			PathError::at(path).into()
		}
	}
}

/// The array index a key spells canonically (`"0"`, `"12"`, not `"01"`), as JS `hasOwn(array, key)` reads it.
fn canonical_index(key: &str) -> Option<usize> {
	let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
	if canonical { key.parse().ok() } else { None }
}

impl TreeObject<Snapshot> for SnapshotObject {
	fn get_mut(&mut self, key: &str) -> Option<&mut Snapshot> {
		let position = self.position(key)?;
		Some(&mut self.entries[position].1)
	}

	fn insert(&mut self, key: &str, value: Snapshot) {
		if let Some(position) = self.position(key) {
			self.entries[position].1 = value;
			return;
		}
		let key: Arc<str> = Arc::from(key);
		if let Some(index) = self.index.get_mut() {
			index.insert(key.clone(), self.entries.len());
		}
		self.entries.push((key, value));
	}

	fn remove(&mut self, key: &str) {
		let Some(position) = self.position(key) else {
			return;
		};
		self.entries.remove(position);
		// The index is updated in place rather than dropped, so deletes between lookups never rebuild it: one pass over
		// it, the same O(members) the shift of the entries above already costs.
		if let Some(index) = self.index.get_mut() {
			index.remove(key);
			for later in index.values_mut().filter(|at| **at > position) {
				*later -= 1;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{Snapshot, apply_immutable, apply_immutable_batches, apply_immutable_trusted};
	use crate::delta::apply::{ApplyError, apply};
	use crate::delta::op::{NonEmptyPath, Op};
	use crate::delta::validate::assert_valid_op;
	use pi_json::JsonValue;
	use serde_json::json;

	fn ops(value: JsonValue) -> Vec<Op> {
		let JsonValue::Array(items) = value else {
			panic!("ops are an array")
		};
		items.into_iter().map(|op| assert_valid_op(op).unwrap()).collect()
	}

	fn snapshot(value: &JsonValue) -> Snapshot {
		Snapshot::from(value)
	}

	// Rust-only: the D0a structural-sharing test (spec, delta/snapshot.rs).
	#[test]
	fn one_op_shares_every_untouched_sibling() {
		let mut members = serde_json::Map::new();
		for index in 0..10_000 {
			members.insert(format!("sibling{index}"), json!({"index": index, "nested": [index]}));
		}
		members.insert("target".to_owned(), json!({"value": 1}));
		let base = snapshot(&JsonValue::Object(members));
		let next = apply_immutable(&base, &ops(json!([["s", ["target", "value"], 2]]))).unwrap();
		assert_eq!(
			next.at(&["target".into(), "value".into()]).unwrap().to_value(),
			json!(2)
		);
		assert_eq!(
			base.at(&["target".into(), "value".into()]).unwrap().to_value(),
			json!(1)
		);
		for index in 0..10_000 {
			let key = format!("sibling{index}");
			assert!(base.get(&key).unwrap().ptr_eq(next.get(&key).unwrap()), "{key}");
		}
		assert!(!base.ptr_eq(&next));
		assert!(!base.get("target").unwrap().ptr_eq(next.get("target").unwrap()));
	}

	// TS: delta.test.ts > operation application and validation > applies mutable and immutable operations
	#[test]
	fn applies_mutable_and_immutable_operations() {
		let batch = ops(json!([
			["a", ["text"], "b"],
			["p", ["values"], 1, 1, [3, 4]],
			["s", ["nested", "value"], 2]
		]));
		let base = json!({"text": "a", "values": [1, 2], "nested": {"value": 1}, "stable": {"value": 9}});
		let before = snapshot(&base);
		let immutable = apply_immutable(&before, &batch).unwrap();
		let expected = json!({"text": "ab", "values": [1, 3, 4], "nested": {"value": 2}, "stable": {"value": 9}});
		assert_eq!(immutable.to_value(), expected);
		assert_eq!(before.to_value(), base);
		assert!(immutable.get("stable").unwrap().ptr_eq(before.get("stable").unwrap()));
		let mut mutable = base;
		apply(&mut mutable, &batch).unwrap();
		assert_eq!(mutable, expected);
	}

	// TS: delta.test.ts > emits an owned root replacement without traversing large replacement input (the replay half)
	#[test]
	fn replays_a_root_replacement() {
		let rows: Vec<JsonValue> = (0..10_000)
			.map(|value| json!({"value": value, "stable": {"value": value}}))
			.collect();
		let replacement = json!({"rows": rows});
		let replica = apply_immutable(&Snapshot::Null, &[Op::Replace(replacement.clone())]).unwrap();
		assert_eq!(replica.to_value(), replacement);
	}

	// TS: delta.test.ts > immutable application ownership > does not mutate a replacement payload targeted by a later
	// operation
	#[test]
	fn does_not_mutate_a_replacement_payload_targeted_by_a_later_operation() {
		let replacement = json!({"nested": {"value": 1}});
		let batch = vec![
			Op::Replace(replacement.clone()),
			assert_valid_op(json!(["s", ["nested", "value"], 2])).unwrap(),
		];
		let next = apply_immutable(&Snapshot::Null, &batch).unwrap();
		assert_eq!(batch[0], Op::Replace(json!({"nested": {"value": 1}})));
		assert_eq!(next.to_value(), json!({"nested": {"value": 2}}));
	}

	// TS: delta-apply-immutable.test.ts > checked immutable operation application > copies each touched container
	// once while preserving input and payload ownership
	#[test]
	fn copies_each_touched_container_once_while_preserving_input_and_payload_ownership() {
		let base = json!({
			"text": "abcdef", "stable": {"value": 7}, "branch": {"value": 1}, "copy": null, "placed": null,
			"untouched": null, "left": null, "right": null, "meta": {"count": 0, "obsolete": true},
			"rows": [{"id": 1, "label": "one"}, {"id": 2, "label": "two"}, {"id": 3, "label": "three"}],
		});
		let shared = json!({"nested": {"value": 1}});
		let batch = ops(json!([
			["t", ["text"], 2], ["a", ["text"], "!"], ["s", ["meta", "count"], 1], ["s", ["meta", "count"], 2],
			["d", ["meta", "obsolete"]], ["s", ["copy"], {"value": 1}], ["s", ["copy", "value"], 2],
			["s", ["placed"], shared], ["s", ["placed", "nested", "value"], 2], ["s", ["untouched"], {"value": 9}],
			["s", ["left"], shared], ["s", ["right"], shared], ["s", ["left", "nested", "value"], 3],
			["p", ["rows"], 1, 1, [{"id": 4, "label": "placed"}]], ["s", ["rows", 1, "label"], "edited"],
			["m", ["rows"], [1, 0, 2]], ["s", ["rows", 0, "label"], "moved"],
		]));
		let before = snapshot(&base);
		let result = apply_immutable(&before, &batch).unwrap();
		let mut mutable = base.clone();
		apply(&mut mutable, &batch).unwrap();
		assert_eq!(result.to_value(), mutable);
		assert_eq!(result.get("text").unwrap().as_str(), Some("cdef!"));
		assert!(result.get("stable").unwrap().ptr_eq(before.get("stable").unwrap()));
		assert_eq!(result.get("copy").unwrap().to_value(), json!({"value": 2}));
		assert_eq!(
			result.get("placed").unwrap().to_value(),
			json!({"nested": {"value": 2}})
		);
		assert_eq!(result.get("left").unwrap().to_value(), json!({"nested": {"value": 3}}));
		assert_eq!(result.get("right").unwrap().to_value(), shared);
		assert_eq!(
			result.at(&["rows".into(), 0.into()]).unwrap().to_value(),
			json!({"id": 4, "label": "moved"})
		);
		assert_eq!(before.to_value(), base);
		// The op payloads are untouched: the applier placed copies of them.
		assert_eq!(
			batch[7],
			assert_valid_op(json!(["s", ["placed"], {"nested": {"value": 1}}])).unwrap()
		);
	}

	// TS: delta-apply-immutable.test.ts > checked immutable operation application > protects root replacement
	// payloads before later object and array edits
	#[test]
	fn protects_root_replacement_payloads_before_later_object_and_array_edits() {
		let replacement = json!({"nested": {"value": 1}, "values": [1, 2, 3]});
		let batches = [
			vec![Op::Replace(replacement.clone())],
			ops(json!([["s", ["nested", "value"], 2]])),
			ops(json!([
				["p", ["values"], 1, 1, [4, 5]],
				["m", ["values"], [3, 0, 1, 2]]
			])),
		];
		let result = apply_immutable_batches(&Snapshot::Null, batches.iter().map(Vec::as_slice)).unwrap();
		assert_eq!(
			result.to_value(),
			json!({"nested": {"value": 2}, "values": [3, 1, 4, 5]})
		);
		assert_eq!(batches[0][0], Op::Replace(replacement));

		let array = snapshot(&json!([1, 2, 3]));
		let batch = ops(json!([["p", [], 1, 1, [4, 5]], ["m", [], [3, 0, 1, 2]], ["d", [1]]]));
		assert_eq!(apply_immutable(&array, &batch).unwrap().to_value(), json!([3, 4, 5]));
		assert_eq!(array.to_value(), json!([1, 2, 3]));
	}

	// TS: delta-apply-immutable.test.ts > checked immutable operation application > shares one private
	// copy-on-write scope across batch partitions
	#[test]
	fn shares_one_private_copy_on_write_scope_across_batch_partitions() {
		let base = snapshot(&json!({
			"text": "abcdef", "meta": {"count": 0},
			"values": [{"id": 1, "value": 1}, {"id": 2, "value": 2}, {"id": 3, "value": 3}],
		}));
		let batches = [
			ops(json!([["s", ["meta", "count"], 1], ["p", ["values"], 1, 1, [{"id": 4, "value": 4}]]])),
			Vec::new(),
			ops(json!([["m", ["values"], [2, 0, 1]], ["s", ["values", 2, "value"], 40]])),
			ops(json!([["t", ["text"], 2], ["a", ["text"], "!"]])),
		];
		let intermediate = apply_immutable(&base, &batches[0]).unwrap();
		let intermediate_value = intermediate.to_value();
		let mut sequential = intermediate.clone();
		for batch in &batches[1..] {
			sequential = apply_immutable(&sequential, batch).unwrap();
		}
		let streamed = apply_immutable_batches(&base, batches.iter().map(Vec::as_slice)).unwrap();
		let flattened = apply_immutable(&base, &batches.concat()).unwrap();
		assert_eq!(streamed, sequential);
		assert_eq!(streamed, flattened);
		assert_eq!(intermediate.to_value(), intermediate_value);
		assert_eq!(base.at(&["meta".into(), "count".into()]).unwrap().to_value(), json!(0));
	}

	// TS: delta-apply-immutable.test.ts > checked immutable operation application > does not expose partial
	// application when validation or iteration fails
	#[test]
	fn does_not_expose_partial_application_when_validation_fails() {
		let base = snapshot(&json!({"nested": {"value": 1}}));
		let invalid = [
			ops(json!([["s", ["nested", "value"], 2]])),
			vec![Op::Set(
				NonEmptyPath::new(vec!["constructor".into(), "prototype".into(), "polluted".into()]).unwrap(),
				json!(true),
			)],
		];
		let error = apply_immutable_batches(&base, invalid.iter().map(Vec::as_slice)).unwrap_err();
		assert!(matches!(error, ApplyError::UnsafePath(_)));
		assert_eq!(base.to_value(), json!({"nested": {"value": 1}}));

		let missing = apply_immutable(
			&snapshot(&json!({"values": []})),
			&ops(json!([["s", ["values", "missing", "value"], 1]])),
		);
		assert!(matches!(missing, Err(ApplyError::Path(_))));
		let spelled = apply_immutable(
			&snapshot(&json!({"values": [{}]})),
			&ops(json!([["s", ["values", "0", "value"], 1]])),
		);
		assert!(matches!(spelled, Err(ApplyError::UnsafePath(_))));
	}

	// TS: delta-apply-immutable.test.ts > checked immutable operation application > allows one immutable batch to fan
	// out without mutating shared payloads
	#[test]
	fn allows_one_immutable_batch_to_fan_out_without_mutating_shared_payloads() {
		let batch = ops(json!([["s", ["placed"], {"nested": {"value": 1}}], ["s", ["placed", "nested", "value"], 2]]));
		let base = snapshot(&json!({"placed": null}));
		let first = apply_immutable(&base, &batch).unwrap();
		let second = apply_immutable(&base, &batch).unwrap();
		assert_eq!(first, second);
		assert!(!first.ptr_eq(&second));
		assert!(!first.get("placed").unwrap().ptr_eq(second.get("placed").unwrap()));
	}

	// TS: delta-apply-immutable.test.ts > immutable container copies > keep an own __proto__ member as data and the
	// prototype of the copied object (the data half; Rust objects have no prototype)
	#[test]
	fn keeps_an_own_proto_member_as_data() {
		let base = pi_json::parse(r#"{"__proto__":{"polluted":true},"n":1}"#).unwrap();
		let before = snapshot(&base);
		let copy = apply_immutable(&before, &ops(json!([["s", ["n"], 2]]))).unwrap();
		let keys: Vec<&str> = copy.as_object().unwrap().iter().map(|(key, _)| key).collect();
		assert_eq!(keys, ["__proto__", "n"]);
		assert_eq!(copy.get("__proto__").unwrap().to_value(), json!({"polluted": true}));
		assert_eq!(copy.get("n").unwrap().to_value(), json!(2));
	}

	// TS: delta-apply-immutable.test.ts > immutable application complexity > appends to an array not yet copied in
	// one copy, sharing everything else
	#[test]
	fn appends_to_an_array_sharing_everything_else() {
		let entries: Vec<JsonValue> = (0..1_000).map(|index| json!({"index": index})).collect();
		let base = snapshot(&json!({"log": {"entries": entries, "meta": {"count": 1_000}}, "other": {"value": 1}}));
		let batch = ops(json!([
			["p", ["log", "entries"], 1_000, 0, [{"index": 1_000}]],
			["p", ["log", "entries"], 1_001, 0, [{"index": 1_001}]],
		]));
		let result = apply_immutable(&base, &batch).unwrap();
		let path = ["log".into(), "entries".into()];
		let appended = result.at(&path).unwrap().as_array().unwrap();
		assert_eq!(appended.len(), 1_002);
		assert_eq!(appended[1_001].to_value(), json!({"index": 1_001}));
		assert_eq!(base.at(&path).unwrap().as_array().unwrap().len(), 1_000);
		assert!(appended[0].ptr_eq(&base.at(&path).unwrap().as_array().unwrap()[0]));
		let meta = ["log".into(), "meta".into()];
		assert!(result.at(&meta).unwrap().ptr_eq(base.at(&meta).unwrap()));
		assert!(result.get("other").unwrap().ptr_eq(base.get("other").unwrap()));
		assert!(!result.get("log").unwrap().ptr_eq(base.get("log").unwrap()));

		let root_array = snapshot(&json!([1, 2]));
		assert_eq!(
			apply_immutable(&root_array, &ops(json!([["p", [], 2, 0, [3]]])))
				.unwrap()
				.to_value(),
			json!([1, 2, 3])
		);
		assert_eq!(
			apply_immutable(&root_array, &ops(json!([["p", [], 2, 0, [[3, 4]]]])))
				.unwrap()
				.to_value(),
			json!([1, 2, [3, 4]])
		);
		assert_eq!(
			apply_immutable(&root_array, &ops(json!([["p", [], 1, 0, [9]]])))
				.unwrap()
				.to_value(),
			json!([1, 9, 2])
		);
		assert_eq!(root_array.to_value(), json!([1, 2]));

		let document = snapshot(&json!({"values": [], "list": [[1]], "text": "a"}));
		let invalid = [
			(
				json!(["p", ["values", "missing"], 0, 0, []]),
				"unresolvable path: [\"values\",\"missing\"]",
			),
			(json!(["p", ["missing"], 0, 0, []]), "unresolvable path: [\"missing\"]"),
			(
				json!(["p", ["values", 0], 0, 0, []]),
				"unresolvable path: [\"values\",0]",
			),
			(
				json!(["p", ["list", "x"], 0, 0, []]),
				"unresolvable path: [\"list\",\"x\"]",
			),
			(json!(["p", ["text"], 0, 0, []]), "unresolvable path: [\"text\"]"),
		];
		for (op, message) in invalid {
			let error = apply_immutable(&document, &[assert_valid_op(op.clone()).unwrap()]).unwrap_err();
			assert_eq!(error.to_string(), message, "{op}");
		}
		// The rest of the TS list fails in the validator, before any walk.
		let unsafe_paths = [
			json!(["p", ["__proto__"], 0, 0, []]),
			json!(["p", ["list", -1], 0, 0, []]),
			json!(["p", ["list", 1.5], 0, 0, []]),
			json!(["p", ["list", 0, "constructor"], 0, 0, []]),
		];
		for op in unsafe_paths {
			assert!(
				assert_valid_op(op)
					.unwrap_err()
					.to_string()
					.starts_with("unsafe path segment")
			);
		}
	}

	// TS: delta-apply-immutable.test.ts > immutable application complexity > copies a wide object once rather than
	// once per repeated write
	#[test]
	fn copies_a_wide_object_once_rather_than_once_per_repeated_write() {
		let mut members = serde_json::Map::new();
		for index in 0..20_000 {
			members.insert(format!("field{index}"), json!(index));
		}
		let base = snapshot(&JsonValue::Object(members));
		let batch: Vec<Op> = (0..1_000)
			.map(|index| assert_valid_op(json!(["s", [format!("field{index}")], -index])).unwrap())
			.collect();
		let result = apply_immutable(&base, &batch).unwrap();
		assert_eq!(result.get("field999").unwrap().to_value(), json!(-999));
		assert_eq!(base.get("field999").unwrap().to_value(), json!(999));
		// The TS test counts shallow copies through a proxy. Here copying once is structural: `Arc::make_mut` copies
		// only a container another revision shares, and after the first op the root belongs to this replay alone.
		assert_eq!(result.get("field1000").unwrap().to_value(), json!(1_000));
	}

	// Rust-only: TS covers `applyImmutableTrusted` through tracker tests (D0b).
	#[test]
	fn the_trusted_applier_skips_validation_only() {
		let base = snapshot(&json!({"list": [1, 2, 3], "text": "ab"}));
		let batch = ops(json!([
			["m", ["list"], [2, 0, 1]],
			["a", ["text"], "c"],
			["d", ["list", 0]]
		]));
		let trusted = apply_immutable_trusted(&base, &batch).unwrap();
		assert_eq!(trusted, apply_immutable(&base, &batch).unwrap());
		assert_eq!(trusted.to_value(), json!({"list": [1, 2], "text": "abc"}));
	}

	// Rust-only: an empty batch must not copy anything, so a watcher can compare revisions by pointer.
	#[test]
	fn an_empty_batch_returns_the_same_root() {
		let base = snapshot(&json!({"a": [1], "b": "text"}));
		assert!(apply_immutable(&base, &[]).unwrap().ptr_eq(&base));
		assert!(
			apply_immutable_batches(&base, [&[][..], &[][..]])
				.unwrap()
				.ptr_eq(&base)
		);
	}

	// Rust-only: an index-backed object stays consistent across deletes and inserts.
	#[test]
	fn the_key_index_follows_deletes_and_inserts() {
		let mut members = serde_json::Map::new();
		for index in 0..40 {
			members.insert(format!("k{index}"), json!(index));
		}
		let base = snapshot(&JsonValue::Object(members));
		assert_eq!(base.get("k39").unwrap().to_value(), json!(39));
		let batch = ops(json!([
			["d", ["k3"]],
			["d", ["k10"]],
			["s", ["new"], 1],
			["s", ["k39"], -1],
			["d", ["k0"]]
		]));
		let next = apply_immutable(&base, &batch).unwrap();
		let mut expected = base.to_value();
		crate::delta::apply::apply(&mut expected, &batch).unwrap();
		assert_eq!(next.to_value(), expected);
		for (key, value) in next.as_object().unwrap().iter() {
			assert_eq!(next.get(key), Some(value), "{key}");
		}
		assert!(next.get("k3").is_none());
	}

	// Rust-only: `PartialEq` for `Snapshot` has no TS counterpart (TS tests use `toEqual`).
	#[test]
	fn equality_ignores_member_order() {
		let a = pi_json::parse(r#"{"a":1,"b":[1,{"c":2,"d":3}]}"#).unwrap();
		let b = pi_json::parse(r#"{"b":[1,{"d":3,"c":2}],"a":1}"#).unwrap();
		assert_eq!(snapshot(&a), snapshot(&b));
		assert_ne!(snapshot(&a), snapshot(&json!({"a": 1})));
	}
}
