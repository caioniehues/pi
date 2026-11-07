//! The delta op vocabulary (TS `delta/index.ts`: `Seg`, `Path`, `Op`, `WireOp`, `isReplace`, `isBase`, `overlap`)
//! and the exact tuple JSON both vocabularies have in memory, on the wire and on disk.

use crate::delta::validate::{assert_valid_op, assert_valid_wire_op};
use pi_json::JsonValue;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;

/// One path segment: an object key, or an array index (a JSON integer on the wire).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Seg {
	/// An object key.
	Key(String),
	/// An array index; on an object it names the key of its decimal spelling, as in JS.
	Index(u64),
}

/// A path from the root; empty for the root itself.
pub type Path = Vec<Seg>;

/// A path with at least one segment: what `s`, `d`, `a` and `t` take, as they cannot target the root.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NonEmptyPath(Path);

/// A decoded op (TS `Op`). `Replace` is the only op that replaces a whole value.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
	/// `["r", value]`: replace the complete value.
	Replace(JsonValue),
	/// `["s", path, value]`: set an object member or an array element (one past the end appends).
	Set(NonEmptyPath, JsonValue),
	/// `["d", path]`: delete an object member or remove an array element.
	Delete(NonEmptyPath),
	/// `["a", path, text]`: append to a string.
	Append(NonEmptyPath, String),
	/// `["t", path, count]`: remove `count` UTF-16 code units from the front of a string.
	Truncate(NonEmptyPath, u64),
	/// `["p", path, index, remove, items]`: splice an array.
	Splice(Path, u64, u64, Vec<JsonValue>),
	/// `["m", path, permutation]`: reorder an array, `new[i] = old[permutation[i]]`.
	Move(Path, Vec<u64>),
}

/// A path inline, or the id the encoder assigned to it on its second use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathRef {
	/// The path itself.
	Path(Path),
	/// An id defined by an earlier `["#", id, path]`.
	Id(u64),
}

/// What crosses a boundary (TS `WireOp`): an op whose path is inline, an id, or omitted (`None`, the short form
/// that reuses the previous op's path in the batch; arity tells them apart), or the definition of an id.
#[derive(Clone, Debug, PartialEq)]
pub enum WireOp {
	/// `["r", value]`.
	Replace(JsonValue),
	/// `["s", ref, value]` or `["s", value]`.
	Set(Option<PathRef>, JsonValue),
	/// `["d", ref]` or `["d"]`.
	Delete(Option<PathRef>),
	/// `["a", ref, text]` or `["a", text]`.
	Append(Option<PathRef>, String),
	/// `["t", ref, count]` or `["t", count]`.
	Truncate(Option<PathRef>, u64),
	/// `["p", ref, index, remove, items]` or `["p", index, remove, items]`.
	Splice(Option<PathRef>, u64, u64, Vec<JsonValue>),
	/// `["m", ref, permutation]` or `["m", permutation]`.
	Move(Option<PathRef>, Vec<u64>),
	/// `["#", id, path]`: defines `id`, emitted on a path's second use.
	Define(u64, Path),
}

impl From<&str> for Seg {
	fn from(key: &str) -> Self {
		Seg::Key(key.to_owned())
	}
}

impl From<u64> for Seg {
	fn from(index: u64) -> Self {
		Seg::Index(index)
	}
}

impl fmt::Display for Seg {
	/// The segment as JS `String(segment)` prints it.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Seg::Key(key) => f.write_str(key),
			Seg::Index(index) => write!(f, "{index}"),
		}
	}
}

impl NonEmptyPath {
	/// The path, or `None` when it is empty.
	pub fn new(path: Path) -> Option<Self> {
		if path.is_empty() { None } else { Some(Self(path)) }
	}

	/// Every segment but the last: the path of the container the op writes into.
	pub fn parent(&self) -> &[Seg] {
		// invariant: `new` refuses an empty path, and nothing else builds one.
		&self.0[..self.0.len() - 1]
	}

	/// The last segment: the key or index the op writes.
	pub fn leaf(&self) -> &Seg {
		// invariant: `new` refuses an empty path, and nothing else builds one.
		&self.0[self.0.len() - 1]
	}

	/// The segments as a plain path.
	pub fn into_path(self) -> Path {
		self.0
	}
}

impl Deref for NonEmptyPath {
	type Target = [Seg];

	fn deref(&self) -> &[Seg] {
		&self.0
	}
}

impl Op {
	/// The op's path; `None` for `Replace`, which has none.
	pub fn path(&self) -> Option<&[Seg]> {
		match self {
			Op::Replace(_) => None,
			Op::Set(path, _) | Op::Delete(path) | Op::Append(path, _) | Op::Truncate(path, _) => Some(path),
			Op::Splice(path, ..) | Op::Move(path, _) => Some(path),
		}
	}

	/// The op as its tuple JSON.
	pub fn to_json(&self) -> JsonValue {
		// invariant: an `Op` serializes to a JSON array of strings, integers and JSON values; that cannot fail.
		serde_json::to_value(self).unwrap_or(JsonValue::Null)
	}
}

impl WireOp {
	/// The wire op as its tuple JSON, copying its payload.
	pub fn to_json(&self) -> JsonValue {
		// invariant: as for `Op::to_json`.
		serde_json::to_value(self).unwrap_or(JsonValue::Null)
	}

	/// The wire op as its tuple JSON, moving its payload: the frame an encoded batch becomes without a second copy.
	pub fn into_json(self) -> JsonValue {
		let (verb, reference, rest): (&str, Option<PathRef>, Vec<JsonValue>) = match self {
			WireOp::Replace(value) => ("r", None, vec![value]),
			WireOp::Set(reference, value) => ("s", reference, vec![value]),
			WireOp::Delete(reference) => ("d", reference, Vec::new()),
			WireOp::Append(reference, text) => ("a", reference, vec![JsonValue::String(text)]),
			WireOp::Truncate(reference, count) => ("t", reference, vec![count.into()]),
			WireOp::Splice(reference, index, remove, items) => (
				"p",
				reference,
				vec![index.into(), remove.into(), JsonValue::Array(items)],
			),
			WireOp::Move(reference, permutation) => ("m", reference, vec![permutation.into()]),
			WireOp::Define(id, path) => ("#", None, vec![id.into(), path_json(path)]),
		};
		let reference = reference.map(|reference| match reference {
			PathRef::Path(path) => path_json(path),
			PathRef::Id(id) => id.into(),
		});
		let tuple = std::iter::once(verb.into()).chain(reference).chain(rest);
		JsonValue::Array(tuple.collect())
	}
}

/// A path as its JSON array.
fn path_json(path: Path) -> JsonValue {
	let segments = path.into_iter().map(|segment| match segment {
		Seg::Key(key) => JsonValue::String(key),
		Seg::Index(index) => index.into(),
	});
	JsonValue::Array(segments.collect())
}

/// Ops of either vocabulary, for `is_base`.
pub trait Replaces {
	/// True for `["r", value]` (TS `isReplace`).
	fn is_replace(&self) -> bool;
}

impl Replaces for Op {
	fn is_replace(&self) -> bool {
		matches!(self, Op::Replace(_))
	}
}

impl Replaces for WireOp {
	fn is_replace(&self) -> bool {
		matches!(self, WireOp::Replace(_))
	}
}

/// True when a batch begins with a replacement (TS `isBase`); `r` is at index 0 or absent, so this is exact.
pub fn is_base<T: Replaces>(ops: &[T]) -> bool {
	ops.first().is_some_and(Replaces::is_replace)
}

/// The longest suffix of `a`, within its last `scan` units, that is a prefix of `b` (TS `overlap`; TS defaults:
/// `probe` 64, `max_candidates` 8). Counted in UTF-16 code units, as in TS: the result `n` satisfies
/// `a.slice(a.length - n) === b.slice(0, n)` in JS.
pub fn overlap(a: &str, b: &str, scan: usize, probe: usize, max_candidates: usize) -> usize {
	if a.is_empty() || b.is_empty() || scan == 0 {
		return 0;
	}
	let tail = utf16_tail(a, scan);
	// Only the first `scan` units of `b` can take part in an overlap, and only the first `probe` form a head.
	let b: Vec<u16> = b.encode_utf16().take(scan.max(probe)).collect();
	// A head of length h can only find overlaps of at least h, so a long head first (few candidates, and it catches the
	// large overlaps a rolling window produces), then one unit, which finds any overlap at the cost of more candidates.
	// Candidates are bounded because repetitive output makes a head match at thousands of positions; giving up returns
	// 0, which makes the caller emit a set: larger, never wrong.
	for head_len in [probe.min(b.len()), 1] {
		let head = &b[..head_len];
		let mut tried = 0;
		let mut from = 0;
		// `from` may reach `tail.len()`: an empty head (a probe of 0) matches there too, as JS `indexOf("")` does.
		while let Some(found) = tail.get(from..).and_then(|rest| find(rest, head)) {
			let k = from + found;
			tried += 1;
			if tried > max_candidates {
				break;
			}
			let n = tail.len() - k;
			if n <= b.len() && tail[k..] == b[..n] {
				return n;
			}
			from = k + 1;
		}
		if head_len == 1 {
			break;
		}
	}
	0
}

/// The last `scan` UTF-16 code units of `text` (all of it when shorter), which may begin with half a pair as in JS.
fn utf16_tail(text: &str, scan: usize) -> Vec<u16> {
	let mut units = 0;
	let mut start = text.len();
	for (offset, ch) in text.char_indices().rev() {
		if units >= scan {
			break;
		}
		units += ch.len_utf16();
		start = offset;
	}
	let mut tail: Vec<u16> = text[start..].encode_utf16().collect();
	if units > scan {
		// The cut fell inside a surrogate pair: JS keeps its low half.
		tail.remove(0);
	}
	tail
}

/// The first position of `needle` in `haystack` (JS `indexOf` over code units).
fn find(haystack: &[u16], needle: &[u16]) -> Option<usize> {
	if needle.is_empty() {
		return Some(0);
	}
	haystack.windows(needle.len()).position(|window| window == needle)
}

impl Serialize for Seg {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			Seg::Key(key) => serializer.serialize_str(key),
			Seg::Index(index) => serializer.serialize_u64(*index),
		}
	}
}

impl Serialize for NonEmptyPath {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		self.0.serialize(serializer)
	}
}

impl Serialize for PathRef {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			PathRef::Path(path) => path.serialize(serializer),
			PathRef::Id(id) => serializer.serialize_u64(*id),
		}
	}
}

impl Serialize for Op {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			Op::Replace(value) => ("r", value).serialize(serializer),
			Op::Set(path, value) => ("s", path, value).serialize(serializer),
			Op::Delete(path) => ("d", path).serialize(serializer),
			Op::Append(path, text) => ("a", path, text).serialize(serializer),
			Op::Truncate(path, count) => ("t", path, count).serialize(serializer),
			Op::Splice(path, index, remove, items) => ("p", path, index, remove, items).serialize(serializer),
			Op::Move(path, permutation) => ("m", path, permutation).serialize(serializer),
		}
	}
}

impl<'de> Deserialize<'de> for Op {
	/// Accepts exactly the tuples `assert_valid_op` accepts, with its messages.
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		assert_valid_op(JsonValue::deserialize(deserializer)?).map_err(D::Error::custom)
	}
}

impl Serialize for WireOp {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self {
			WireOp::Replace(value) => ("r", value).serialize(serializer),
			WireOp::Set(None, value) => ("s", value).serialize(serializer),
			WireOp::Set(Some(path), value) => ("s", path, value).serialize(serializer),
			WireOp::Delete(None) => ("d",).serialize(serializer),
			WireOp::Delete(Some(path)) => ("d", path).serialize(serializer),
			WireOp::Append(None, text) => ("a", text).serialize(serializer),
			WireOp::Append(Some(path), text) => ("a", path, text).serialize(serializer),
			WireOp::Truncate(None, count) => ("t", count).serialize(serializer),
			WireOp::Truncate(Some(path), count) => ("t", path, count).serialize(serializer),
			WireOp::Splice(None, index, remove, items) => ("p", index, remove, items).serialize(serializer),
			WireOp::Splice(Some(path), index, remove, items) => ("p", path, index, remove, items).serialize(serializer),
			WireOp::Move(None, permutation) => ("m", permutation).serialize(serializer),
			WireOp::Move(Some(path), permutation) => ("m", path, permutation).serialize(serializer),
			WireOp::Define(id, path) => ("#", id, path).serialize(serializer),
		}
	}
}

impl<'de> Deserialize<'de> for WireOp {
	/// Accepts exactly the tuples `assert_valid_wire_op` accepts, with its messages.
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		assert_valid_wire_op(JsonValue::deserialize(deserializer)?).map_err(D::Error::custom)
	}
}

#[cfg(test)]
mod tests {
	use super::{Op, WireOp, is_base, overlap};
	use pi_json::to_js_string;
	use serde_json::json;

	// TS: delta.test.ts > canonical strings > finds bounded overlaps
	#[test]
	fn finds_bounded_overlaps() {
		assert_eq!(overlap("abcdefgh", "defghxyz", 65_536, 64, 8), 5);
		assert_eq!(overlap("abcdef", "defghi", 0, 64, 8), 0);
	}

	// Rust-only: a Rust `str` is UTF-8; this pins the UTF-16 counting TS does natively.
	#[test]
	fn overlap_counts_utf16_units_and_may_split_a_pair_like_js() {
		// JS: overlap("x😀", "😀y", 2) is 2 (the whole pair); with scan 1 the tail is half a pair and cannot match.
		assert_eq!(overlap("x😀", "😀y", 2, 64, 8), 2);
		assert_eq!(overlap("x😀", "😀y", 1, 64, 8), 0);
		assert_eq!(overlap("aaaa", "aab", 10, 64, 0), 0);
	}

	// TS: delta.test.ts > operation application and validation > supports root replacement, root splice, and
	// permutations (the isBase half)
	#[test]
	fn is_base_on_both_vocabularies() {
		let replace: Op = serde_json::from_value(json!(["r", [1, 2, 3]])).unwrap();
		let set: Op = serde_json::from_value(json!(["s", ["a"], 1])).unwrap();
		assert!(is_base(&[replace.clone(), set.clone()]));
		assert!(!is_base(&[set, replace]));
		assert!(!is_base::<Op>(&[]));
		assert!(is_base(&[WireOp::Replace(json!(null))]));
	}

	// Rust-only: TS ops are their tuples; here serde must write the same tuples.
	#[test]
	fn tuples_serialize_exactly_as_ts_writes_them() {
		let cases = [
			json!(["r", {"a": [1]}]),
			json!(["s", ["a", 0], 1]),
			json!(["d", ["a"]]),
			json!(["a", ["a"], "x"]),
			json!(["t", ["a"], 2]),
			json!(["p", [], 0, 1, [null]]),
			json!(["m", ["l"], [1, 0]]),
		];
		for case in cases {
			let op: Op = serde_json::from_value(case.clone()).unwrap();
			assert_eq!(to_js_string(&op.to_json()), to_js_string(&case));
		}
		let wire = [
			json!(["s", 1]),
			json!(["d"]),
			json!(["p", 0, 0, []]),
			json!(["#", 3, ["x", 1]]),
			json!(["m", 2, [0]]),
		];
		for case in wire {
			let op: WireOp = serde_json::from_value(case.clone()).unwrap();
			assert_eq!(to_js_string(&op.to_json()), to_js_string(&case));
			assert_eq!(op.into_json(), case);
		}
	}

	// Rust-only: deserialization is the Rust trust boundary and must keep the TS message.
	#[test]
	fn deserializing_reports_the_validator_message() {
		let error = serde_json::from_value::<Op>(json!(["m", [], [0, 0]])).unwrap_err();
		assert_eq!(error.to_string(), "m permutation is not a bijection");
	}
}
