//! The op applier (TS `apply`, `PathError`): ops applied in place to a `JsonValue`, and the one applier both trees
//! share, in place here and copy-on-write for `Snapshot` (`delta/snapshot.rs`).

use crate::delta::op::{NonEmptyPath, Op, Seg};
use crate::delta::validate::{OpError, UnsafePathError, assert_index_in_range, validate_op};
use pi_json::{JsonValue, to_js_string};
use std::borrow::Cow;
use std::fmt;

/// What an unresolvable-path error names: a path, or a wire path id nothing defined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnresolvedPath {
	/// A path that does not lead to a container of the kind the op needs.
	Path(Vec<Seg>),
	/// A path id without a definition.
	Id(u64),
}

/// A path that does not resolve: a missing member or element, or a value of the wrong kind on the way.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unresolvable path: {path}")]
pub struct PathError {
	/// The path (or id) as TS reports it: the prefix that failed to resolve, or the op's full path.
	pub path: UnresolvedPath,
}

/// Why a batch did not apply.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
	/// An op the `Op` type allows but the vocabulary does not (a non-bijective permutation).
	#[error(transparent)]
	Op(OpError),
	/// A path that does not resolve in the value.
	#[error(transparent)]
	Path(#[from] PathError),
	/// A reserved segment, a key on an array, or an index past the end.
	#[error(transparent)]
	UnsafePath(#[from] UnsafePathError),
}

impl PathError {
	/// The error for `path`.
	pub fn at(path: &[Seg]) -> Self {
		Self {
			path: UnresolvedPath::Path(path.to_vec()),
		}
	}
}

impl fmt::Display for UnresolvedPath {
	/// The path or id as TS `JSON.stringify` prints it.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			UnresolvedPath::Path(path) => {
				// invariant: a path serializes to a JSON array of strings and integers.
				let json = serde_json::to_value(path).unwrap_or(JsonValue::Null);
				f.write_str(&to_js_string(&json))
			}
			UnresolvedPath::Id(id) => write!(f, "{id}"),
		}
	}
}

impl From<OpError> for ApplyError {
	fn from(error: OpError) -> Self {
		match error {
			OpError::UnsafePath(error) => ApplyError::UnsafePath(error),
			other => ApplyError::Op(other),
		}
	}
}

/// Applies a batch in place. On an error the ops before the failing one stay applied; discard the value then.
///
/// `Replace` clones its payload into the value: the batch is borrowed, so the TS rule that `apply` adopts payload
/// containers (and so must own the batch) does not exist here.
pub fn apply(target: &mut JsonValue, ops: &[Op]) -> Result<(), ApplyError> {
	for op in ops {
		validate_op(op)?;
		apply_op(target, op)?;
	}
	Ok(())
}

/// Applies batches in order, in place, as one `apply` of their concatenation.
pub fn apply_batches<'a>(
	target: &mut JsonValue,
	batches: impl IntoIterator<Item = &'a [Op]>,
) -> Result<(), ApplyError> {
	batches.into_iter().try_for_each(|ops| apply(target, ops))
}

/// A JSON tree the applier edits: `JsonValue` in place, `Snapshot` by copying each shared container it enters.
pub(crate) trait Tree: Sized {
	/// The tree's object container.
	type Object: TreeObject<Self>;

	/// A node holding a copy of an op payload.
	fn from_payload(value: &JsonValue) -> Self;

	/// The node's container, made exclusively owned first for a persistent tree.
	fn node_mut(&mut self) -> NodeMut<'_, Self>;

	/// Appends to a string node; false when the node is not a string.
	fn append_text(&mut self, text: &str) -> bool;

	/// Drops `count` UTF-16 code units from the front of a string node; false when the node is not a string.
	fn truncate_text(&mut self, count: u64) -> bool;

	/// The error for key `key` met on an array of `length` elements while walking `path`.
	///
	/// TS parity: the in-place applier (TS `resolveValue`) reports any key on an array as unsafe, the copying applier
	/// (TS `copyContainers`) first asks whether the array has such an own property (`length`, an index in range) and
	/// reports an unresolvable path when not.
	fn array_key_error(key: &str, length: usize, path: &[Seg]) -> ApplyError;
}

/// The members of an object container, in insertion order.
pub(crate) trait TreeObject<N> {
	/// The member `key`.
	fn get_mut(&mut self, key: &str) -> Option<&mut N>;

	/// Sets `key`, keeping its position when present (JS `defineProperty`).
	fn insert(&mut self, key: &str, value: N);

	/// Removes `key`, keeping the order of the others (JS `delete`).
	fn remove(&mut self, key: &str);
}

/// A node seen as a container.
pub(crate) enum NodeMut<'a, N: Tree> {
	Array(&'a mut Vec<N>),
	Object(&'a mut N::Object),
	Leaf,
}

/// Applies one validated op to `root`.
pub(crate) fn apply_op<N: Tree>(root: &mut N, op: &Op) -> Result<(), ApplyError> {
	match op {
		Op::Replace(value) => *root = N::from_payload(value),
		Op::Splice(path, index, remove, items) => {
			let NodeMut::Array(target) = walk(root, path)?.node_mut() else {
				return Err(PathError::at(path).into());
			};
			splice(target, *index, *remove, items);
		}
		Op::Move(path, permutation) => {
			let NodeMut::Array(target) = walk(root, path)?.node_mut() else {
				return Err(PathError::at(path).into());
			};
			if target.len() != permutation.len() {
				return Err(PathError::at(path).into());
			}
			permute(target, permutation);
		}
		Op::Set(path, value) => match slot(root, path)? {
			Slot::Element(items, index) if index == items.len() => items.push(N::from_payload(value)),
			// The guard above leaves an index below the length.
			Slot::Element(items, index) => items[index] = N::from_payload(value),
			Slot::Member(object, key) => object.insert(&key, N::from_payload(value)),
		},
		Op::Delete(path) => match slot(root, path)? {
			Slot::Element(items, index) if index == items.len() => return Err(PathError::at(path).into()),
			Slot::Element(items, index) => {
				items.remove(index);
			}
			Slot::Member(object, key) => object.remove(&key),
		},
		Op::Append(path, text) => edit_text(root, path, |node| node.append_text(text))?,
		Op::Truncate(path, count) => edit_text(root, path, |node| node.truncate_text(*count))?,
	}
	Ok(())
}

/// The node at `path`; every node on the way must be a container with that member or element.
fn walk<'a, N: Tree>(root: &'a mut N, path: &[Seg]) -> Result<&'a mut N, ApplyError> {
	let mut node = root;
	for segment in path {
		let child = match (node.node_mut(), segment) {
			(NodeMut::Array(items), Seg::Index(index)) => usize::try_from(*index).ok().and_then(|i| items.get_mut(i)),
			(NodeMut::Array(items), Seg::Key(key)) => return Err(N::array_key_error(key, items.len(), path)),
			(NodeMut::Object(object), segment) => object.get_mut(&object_key(segment)),
			(NodeMut::Leaf, _) => None,
		};
		node = child.ok_or_else(|| PathError::at(path))?;
	}
	Ok(node)
}

/// The object key a segment names: a key as is, an index by its decimal spelling (JS property keys are strings).
fn object_key(segment: &Seg) -> Cow<'_, str> {
	match segment {
		Seg::Key(key) => Cow::Borrowed(key),
		Seg::Index(index) => Cow::Owned(index.to_string()),
	}
}

/// Where an `s`, `d`, `a` or `t` writes: an array element (the index may be one past the end) or an object member.
enum Slot<'a, N: Tree> {
	Element(&'a mut Vec<N>, usize),
	Member(&'a mut N::Object, Cow<'a, str>),
}

fn slot<'a, N: Tree>(root: &'a mut N, path: &'a NonEmptyPath) -> Result<Slot<'a, N>, ApplyError> {
	let parent = path.parent();
	match walk(root, parent)?.node_mut() {
		NodeMut::Array(items) => match path.leaf() {
			Seg::Index(index) => {
				let index = assert_index_in_range(items.len(), *index)?;
				Ok(Slot::Element(items, index))
			}
			Seg::Key(key) => Err(UnsafePathError { segment: key.clone() }.into()),
		},
		NodeMut::Object(object) => Ok(Slot::Member(object, object_key(path.leaf()))),
		NodeMut::Leaf => Err(PathError::at(parent).into()),
	}
}

/// Runs a string edit on the node at `path`; a missing node or one that is not a string does not resolve.
fn edit_text<N: Tree>(root: &mut N, path: &NonEmptyPath, edit: impl FnOnce(&mut N) -> bool) -> Result<(), ApplyError> {
	let node = match slot(root, path)? {
		Slot::Element(items, index) => items.get_mut(index),
		Slot::Member(object, key) => object.get_mut(&key),
	};
	if node.is_some_and(edit) {
		Ok(())
	} else {
		Err(PathError::at(path).into())
	}
}

/// JS `splice(index, remove, ...items)`: the start and the removal clamp to the array.
fn splice<N: Tree>(target: &mut Vec<N>, index: u64, remove: u64, items: &[JsonValue]) {
	let start = usize::try_from(index).unwrap_or(usize::MAX).min(target.len());
	let end = usize::try_from(remove)
		.unwrap_or(usize::MAX)
		.saturating_add(start)
		.min(target.len());
	target.splice(start..end, items.iter().map(N::from_payload));
}

/// `new[i] = old[permutation[i]]` in place, by following each cycle once; a non-bijection (only from an unchecked
/// trusted op) stops early instead of looping.
fn permute<N>(items: &mut [N], permutation: &[u64]) {
	let mut placed = vec![false; items.len()];
	for start in 0..items.len() {
		if placed[start] {
			continue;
		}
		placed[start] = true;
		let mut at = start;
		loop {
			let from = usize::try_from(permutation[at]).unwrap_or(usize::MAX);
			match placed.get_mut(from) {
				Some(done) if !*done => *done = true,
				_ => break,
			}
			items.swap(at, from);
			at = from;
		}
	}
}

/// Where dropping UTF-16 code units from the front of a string cuts it.
pub(crate) enum FrontCut {
	/// At a character boundary: keep `text[offset..]`.
	At(usize),
	/// Inside a surrogate pair: keep `text[offset..]` after the pair, preceded by U+FFFD for the low half JS keeps
	/// (a Rust string cannot hold half a pair).
	InsidePair(usize),
}

/// Where JS `text.slice(count)` cuts `text`.
pub(crate) fn front_cut(text: &str, count: u64) -> FrontCut {
	let mut units: u64 = 0;
	for (offset, ch) in text.char_indices() {
		if units >= count {
			return FrontCut::At(offset);
		}
		units += ch.len_utf16() as u64;
		if units > count {
			return FrontCut::InsidePair(offset + ch.len_utf8());
		}
	}
	FrontCut::At(text.len())
}

impl Tree for JsonValue {
	type Object = serde_json::Map<String, JsonValue>;

	fn from_payload(value: &JsonValue) -> Self {
		value.clone()
	}

	fn node_mut(&mut self) -> NodeMut<'_, Self> {
		match self {
			JsonValue::Array(items) => NodeMut::Array(items),
			JsonValue::Object(object) => NodeMut::Object(object),
			_ => NodeMut::Leaf,
		}
	}

	fn append_text(&mut self, text: &str) -> bool {
		let JsonValue::String(current) = self else {
			return false;
		};
		current.push_str(text);
		true
	}

	fn truncate_text(&mut self, count: u64) -> bool {
		let JsonValue::String(current) = self else {
			return false;
		};
		match front_cut(current, count) {
			FrontCut::At(offset) => {
				current.drain(..offset);
			}
			FrontCut::InsidePair(offset) => {
				current.replace_range(..offset, "\u{FFFD}");
			}
		}
		true
	}

	fn array_key_error(key: &str, _length: usize, _path: &[Seg]) -> ApplyError {
		UnsafePathError {
			segment: key.to_owned(),
		}
		.into()
	}
}

impl TreeObject<JsonValue> for serde_json::Map<String, JsonValue> {
	fn get_mut(&mut self, key: &str) -> Option<&mut JsonValue> {
		serde_json::Map::get_mut(self, key)
	}

	fn insert(&mut self, key: &str, value: JsonValue) {
		match serde_json::Map::get_mut(self, key) {
			Some(slot) => *slot = value,
			None => {
				serde_json::Map::insert(self, key.to_owned(), value);
			}
		}
	}

	fn remove(&mut self, key: &str) {
		// `Map::remove` is a swap remove under `preserve_order`; JS `delete` keeps the other keys in order.
		self.shift_remove(key);
	}
}

#[cfg(test)]
mod tests {
	use super::{ApplyError, apply};
	use crate::delta::op::Op;
	use crate::delta::validate::assert_valid_op;
	use pi_json::JsonValue;
	use serde_json::json;

	// TS: not applicable: delta.test.ts:218 "validates immutable operations before traversing the target" (Rust
	// values have no getters to trap a read), delta.test.ts:354 "adopts a mutable replacement payload rather than
	// copying it" (`apply` borrows the batch and clones payloads, so replicas cannot alias), delta.test.ts:396
	// "imports own properties without invoking inherited setters" (no prototypes; `track` is D0b).

	fn ops(value: JsonValue) -> Vec<Op> {
		let JsonValue::Array(items) = value else {
			panic!("ops are an array")
		};
		items.into_iter().map(|op| assert_valid_op(op).unwrap()).collect()
	}

	fn applied(mut base: JsonValue, batch: JsonValue) -> Result<JsonValue, ApplyError> {
		apply(&mut base, &ops(batch))?;
		Ok(base)
	}

	/// TS `apply` of JSON ops that may not validate: the first failing op's message, validation included.
	fn applied_json(mut base: JsonValue, batch: JsonValue) -> Result<JsonValue, String> {
		let JsonValue::Array(items) = batch else {
			panic!("ops are an array")
		};
		for op in items {
			let op = assert_valid_op(op).map_err(|error| error.to_string())?;
			apply(&mut base, &[op]).map_err(|error| error.to_string())?;
		}
		Ok(base)
	}

	// TS: delta.test.ts > operation application and validation > supports root replacement, root splice, and
	// permutations
	#[test]
	fn supports_root_replacement_root_splice_and_permutations() {
		let mut value = JsonValue::Null;
		apply(&mut value, &ops(json!([["r", [1, 2, 3]]]))).unwrap();
		apply(&mut value, &ops(json!([["p", [], 1, 1, [4]]]))).unwrap();
		apply(&mut value, &ops(json!([["m", [], [2, 0, 1]]]))).unwrap();
		assert_eq!(value, json!([3, 1, 4]));
	}

	// TS: delta.test.ts > operation application and validation > rejects unsafe and malformed paths
	#[test]
	fn rejects_unsafe_and_malformed_paths() {
		let unsafe_walk = applied_json(json!({}), json!([["s", ["constructor", "prototype", "x"], true]]));
		assert_eq!(unsafe_walk.unwrap_err(), "unsafe path segment: constructor");
		let past_end = applied(json!({"values": [1]}), json!([["s", ["values", 3], 2]]));
		assert!(matches!(past_end, Err(ApplyError::UnsafePath(_))));
		assert!(applied(json!({"value": 1}), json!([["a", ["value"], "x"]])).is_err());
	}

	// TS: delta.test.ts > path and prototype safety > rejects constructor walks and forbidden interned paths (the
	// apply half)
	#[test]
	fn rejects_constructor_walks() {
		let error = applied_json(json!({}), json!([["s", ["constructor", "prototype", "gadget"], true]]));
		assert_eq!(error.unwrap_err(), "unsafe path segment: constructor");
	}

	// TS: delta.test.ts > path and prototype safety > does not run inherited setters
	#[test]
	fn does_not_run_inherited_setters() {
		// Rust objects have no prototype; the TS test's point, a plain own member, is what `s` writes.
		assert_eq!(
			applied(json!({}), json!([["s", ["trap"], 1]])).unwrap(),
			json!({"trap": 1})
		);
	}

	// TS: delta.test.ts > path and prototype safety > allows reserved names inside values without prototype
	// pollution
	#[test]
	fn allows_reserved_names_inside_values() {
		let value = pi_json::parse(r#"{"__proto__":{"z":1}}"#).unwrap();
		let out = applied(json!({}), json!([["s", ["value"], value]])).unwrap();
		assert_eq!(out["value"]["__proto__"], json!({"z": 1}));
	}

	// TS: delta.test.ts > array index safety > writes an existing index and appends exactly one past the end
	#[test]
	fn writes_an_existing_index_and_appends_exactly_one_past_the_end() {
		let base = json!({"values": [1, 2, 3]});
		assert_eq!(
			applied(base.clone(), json!([["s", ["values", 1], 9]])).unwrap(),
			json!({"values": [1, 9, 3]})
		);
		assert_eq!(
			applied(base, json!([["s", ["values", 3], 9]])).unwrap(),
			json!({"values": [1, 2, 3, 9]})
		);
	}

	// TS: delta.test.ts > array index safety > rejects gaps, huge indices, and string-spelled indices
	#[test]
	fn rejects_gaps_huge_indices_and_string_spelled_indices() {
		assert!(applied(json!({"values": [1, 2, 3]}), json!([["s", ["values", 5], 9]])).is_err());
		assert!(applied(json!({"values": []}), json!([["s", ["values", 4_294_967_290_u64], 1]])).is_err());
		assert!(applied(json!({"values": [1]}), json!([["s", ["values", "0"], 9]])).is_err());
		assert!(applied(json!({"values": ["a"]}), json!([["a", ["values", "0"], "b"]])).is_err());
	}

	// TS: delta.test.ts > array index safety > allows explicit growth values and rejects deletion past the end
	#[test]
	fn allows_explicit_growth_values_and_rejects_deletion_past_the_end() {
		assert_eq!(
			applied(
				json!({"values": [1]}),
				json!([["p", ["values"], 1, 0, [null, null, 9]]])
			)
			.unwrap(),
			json!({"values": [1, null, null, 9]})
		);
		assert!(applied(json!({"values": [1]}), json!([["d", ["values", 1]]])).is_err());
	}

	// TS: delta.test.ts > array index safety > applies large splice payloads without spreading them at once
	#[test]
	fn applies_large_splice_payloads() {
		let items = vec![JsonValue::Null; 300_000];
		let mut value = json!({"values": []});
		apply(&mut value, &[Op::Splice(vec!["values".into()], 0, 0, items)]).unwrap();
		assert_eq!(value["values"].as_array().unwrap().len(), 300_000);
	}

	// TS: delta.test.ts > operation structure safety > rejects unknown verbs, malformed tuples, paths, and splice
	// payloads
	#[test]
	fn rejects_unknown_verbs_malformed_tuples_paths_and_splice_payloads() {
		assert!(applied_json(json!({"value": 1}), json!([["ZZZ", ["value"], 9]])).is_err());
		assert!(applied_json(json!({"values": [1]}), json!([["p", ["values"], 0, 0, "not-an-array"]])).is_err());
		assert!(applied_json(json!({"value": 1}), json!([["s", "value", 9]])).is_err());
		assert!(applied_json(json!({"value": 1}), json!([{"op": "s"}])).is_err());
		assert!(applied_json(json!({"value": 1}), json!([null])).is_err());
	}

	// TS: delta.test.ts > operation structure safety > rejects invalid append and truncation operations (the apply
	// half; the decode half is in wire.rs)
	#[test]
	fn rejects_invalid_append_and_truncation_operations() {
		assert!(applied_json(json!({"value": 1}), json!([["a", ["missing"], "x"]])).is_err());
		assert!(applied_json(json!({"value": 1}), json!([["a", ["value"], "x"]])).is_err());
		assert!(applied_json(json!({"value": "abc"}), json!([["t", ["value"], -1]])).is_err());
	}

	// TS: delta.test.ts > operation structure safety > clamps splice removal past the end
	#[test]
	fn clamps_splice_removal_past_the_end() {
		assert_eq!(
			applied(json!({"values": [1, 2]}), json!([["p", ["values"], 0, 1e9, []]])).unwrap(),
			json!({"values": []})
		);
	}

	// Rust-only: the U+FFFD rule for a split pair (decided divergence, rust/docs/slices.md).
	#[test]
	fn truncate_counts_utf16_units_and_replaces_a_split_pair() {
		let base = json!({"s": "😀é"});
		assert_eq!(
			applied(base.clone(), json!([["t", ["s"], 2]])).unwrap(),
			json!({"s": "é"})
		);
		// JS keeps the low half of the pair; a Rust string holds U+FFFD in its place.
		assert_eq!(
			applied(base, json!([["t", ["s"], 1]])).unwrap(),
			json!({"s": "\u{FFFD}é"})
		);
	}

	// Rust-only: `serde_json::Map::remove` is a swap remove; JS `delete` keeps key order.
	#[test]
	fn deletes_keep_the_order_of_the_other_keys() {
		let out = applied(json!({"a": 1, "b": 2, "c": 3}), json!([["d", ["a"]], ["s", ["a"], 4]])).unwrap();
		assert_eq!(pi_json::to_js_string(&out), r#"{"b":2,"c":3,"a":4}"#);
	}
}
