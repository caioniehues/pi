//! Op validation (TS `assertValidOp`, `assertValidWireOp`, `assertSafePath`, `assertPermutation`,
//! `assertIndexInRange`): the trust boundary between JSON tuples and typed ops, with the TS `TypeError` messages.
//!
//! Ops arrive from facets, plugins and tools whose details may echo model output, so none of it is trusted input.
//! Checks run in the TS order, so the first failing check of a tuple gives the message TS would throw.

use crate::delta::op::{NonEmptyPath, Op, Path, PathRef, Seg, WireOp};
use pi_json::{JsonValue, js_number_to_string};

/// Segments that reach the JS prototype chain; no applier or decoder accepts them in a path (TS parity: paths are
/// shared with the TS implementation, where `parent[key] = value` with them pollutes `Object.prototype`).
pub const RESERVED_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// A path segment that is reserved, not a non-negative integer, or not a key of the container it addresses.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unsafe path segment: {segment}")]
pub struct UnsafePathError {
	/// The segment as JS `String(segment)` prints it.
	pub segment: String,
}

/// A tuple that is not an op of the vocabulary it was checked against; messages equal the TS `TypeError` messages.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpError {
	/// Not a non-empty JSON array.
	#[error("op is not a tuple")]
	NotATuple,
	/// A verb the vocabulary does not have; silently skipping it is how a newer producer's op would vanish.
	#[error("unknown op verb: {0}")]
	UnknownVerb(String),
	/// The tuple has the wrong length for its verb.
	#[error("{0} arity")]
	Arity(&'static str),
	/// A path that is not an array (a string would otherwise resolve to the root).
	#[error("path is not an array")]
	PathNotArray,
	/// An empty path for a verb that cannot target the root.
	#[error("path is empty")]
	PathEmpty,
	/// `a` without three elements or with a text that is not a string.
	#[error("a shape")]
	AppendShape,
	/// `t` without three elements or with a count that is not a non-negative integer.
	#[error("t shape")]
	TruncateShape,
	/// A wire `a` whose text is not a string.
	#[error("a value")]
	AppendValue,
	/// A wire `t` whose count is not a non-negative integer.
	#[error("t count")]
	TruncateCount,
	/// A `p` index that is not a non-negative integer.
	#[error("p index")]
	SpliceIndex,
	/// A `p` removal count that is not a non-negative integer.
	#[error("p remove")]
	SpliceRemove,
	/// `p` items that are not an array.
	#[error("p items")]
	SpliceItems,
	/// An `m` permutation that is not an array.
	#[error("m permutation is not an array")]
	PermutationNotArray,
	/// An `m` permutation that does not map `0..n` onto itself.
	#[error("m permutation is not a bijection")]
	PermutationNotBijection,
	/// A path id that is not a non-negative integer.
	#[error("bad path id")]
	BadPathId,
	/// A `#` definition that is not `["#", id, path]`.
	#[error("# shape")]
	DefineShape,
	/// A path segment that is reserved or not a non-negative integer.
	#[error(transparent)]
	UnsafePath(#[from] UnsafePathError),
}

/// The verbs of both vocabularies; `#` is a wire verb only.
#[derive(Clone, Copy)]
enum Verb {
	Replace,
	Set,
	Delete,
	Append,
	Truncate,
	Splice,
	Move,
	Define,
}

/// Checks a JSON tuple against the decoded-op grammar (paths inline, no `#`, no short forms) and returns the op.
pub fn assert_valid_op(op: JsonValue) -> Result<Op, OpError> {
	let (verb, items) = tuple(op)?;
	match verb {
		Verb::Replace => {
			let [_, value] = fixed(items).ok_or(OpError::Arity("r"))?;
			Ok(Op::Replace(value))
		}
		Verb::Set => {
			let [_, path, value] = fixed(items).ok_or(OpError::Arity("s"))?;
			Ok(Op::Set(non_empty_path(&path)?, value))
		}
		Verb::Delete => {
			let [_, path] = fixed(items).ok_or(OpError::Arity("d"))?;
			Ok(Op::Delete(non_empty_path(&path)?))
		}
		Verb::Append => {
			let Some([_, path, JsonValue::String(text)]) = fixed(items) else {
				return Err(OpError::AppendShape);
			};
			Ok(Op::Append(non_empty_path(&path)?, text))
		}
		Verb::Truncate => {
			let [_, path, count] = fixed(items).ok_or(OpError::TruncateShape)?;
			let count = js_index(&count).ok_or(OpError::TruncateShape)?;
			Ok(Op::Truncate(non_empty_path(&path)?, count))
		}
		Verb::Splice => {
			let [_, path, index, remove, items] = fixed(items).ok_or(OpError::Arity("p"))?;
			let path = path_arg(&path)?;
			let (index, remove, items) = splice_args(&index, &remove, items)?;
			Ok(Op::Splice(path, index, remove, items))
		}
		Verb::Move => {
			let [_, path, permutation] = fixed(items).ok_or(OpError::Arity("m"))?;
			let path = path_arg(&path)?;
			Ok(Op::Move(path, permutation_arg(&permutation)?))
		}
		Verb::Define => Err(OpError::UnknownVerb("#".to_owned())),
	}
}

/// The verb and elements of a tuple; an empty or non-array value is not a tuple.
fn tuple(op: JsonValue) -> Result<(Verb, Vec<JsonValue>), OpError> {
	let JsonValue::Array(items) = op else {
		return Err(OpError::NotATuple);
	};
	let Some(first) = items.first() else {
		return Err(OpError::NotATuple);
	};
	let verb = match first.as_str() {
		Some("r") => Verb::Replace,
		Some("s") => Verb::Set,
		Some("d") => Verb::Delete,
		Some("a") => Verb::Append,
		Some("t") => Verb::Truncate,
		Some("p") => Verb::Splice,
		Some("m") => Verb::Move,
		Some("#") => Verb::Define,
		_ => return Err(OpError::UnknownVerb(js_string(first))),
	};
	Ok((verb, items))
}

/// The elements of a tuple of exactly `N` elements.
fn fixed<const N: usize>(items: Vec<JsonValue>) -> Option<[JsonValue; N]> {
	items.try_into().ok()
}

fn non_empty_path(value: &JsonValue) -> Result<NonEmptyPath, OpError> {
	let JsonValue::Array(segments) = value else {
		return Err(OpError::PathNotArray);
	};
	// An empty path has no segment to reject, so checking emptiness last keeps the TS order.
	NonEmptyPath::new(safe_segments(segments)?).ok_or(OpError::PathEmpty)
}

fn path_arg(value: &JsonValue) -> Result<Path, OpError> {
	let JsonValue::Array(segments) = value else {
		return Err(OpError::PathNotArray);
	};
	Ok(safe_segments(segments)?)
}

/// The segments of a JSON path: strings that are not reserved, and non-negative integers.
fn safe_segments(segments: &[JsonValue]) -> Result<Path, UnsafePathError> {
	segments.iter().map(safe_segment).collect()
}

fn safe_segment(segment: &JsonValue) -> Result<Seg, UnsafePathError> {
	match segment {
		JsonValue::String(key) if !RESERVED_SEGMENTS.contains(&key.as_str()) => Ok(Seg::Key(key.clone())),
		// Integers of 2^64 and more fail here; see `js_index`.
		other => js_index(other).map(Seg::Index).ok_or_else(|| UnsafePathError {
			segment: js_string(other),
		}),
	}
}

/// 2^64, the first double a `u64` cannot hold.
const U64_LIMIT: f64 = 18_446_744_073_709_551_616.0;

/// The value as a non-negative integer (TS `Number.isInteger(value) && value >= 0`), when it fits a `u64`.
///
/// Decided divergence (rust/docs/slices.md, "Decided divergences"): TS also accepts integral numbers of 2^64 and more,
/// which no producer writes. Here they are rejected everywhere this reads an integer: path segments (unsafe path
/// segment), `t` counts (`t shape`, `t count`), `p` index and remove count (`p index`, `p remove`), `#` ids
/// (`# shape`) and path ids (`bad path id`), and permutation entries (not a bijection). Indices above 2^53 are kept
/// exactly, where a JS number rounds them, so an index segment on an object names the key of its exact digits.
fn js_index(value: &JsonValue) -> Option<u64> {
	let JsonValue::Number(number) = value else {
		return None;
	};
	if let Some(index) = number.as_u64() {
		return Some(index);
	}
	// `1.0` and `-0` are integers in JS; JSON text written by other producers may spell indices that way.
	let float = number.as_f64()?;
	if float.fract() != 0.0 || float < 0.0 || float >= U64_LIMIT {
		return None;
	}
	#[expect(
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "integral, non-negative and below 2^64, checked above"
	)]
	Some(float as u64)
}

/// JS `String(value)` of a JSON value, for messages: arrays join their elements with `,` (null as empty).
fn js_string(value: &JsonValue) -> String {
	match value {
		JsonValue::Null => "null".to_owned(),
		JsonValue::Bool(flag) => flag.to_string(),
		// invariant: `as_f64` fails only under serde_json's `arbitrary_precision`, which the workspace never enables.
		JsonValue::Number(number) => js_number_to_string(number.as_f64().unwrap_or(0.0)),
		JsonValue::String(text) => text.clone(),
		JsonValue::Array(items) => items
			.iter()
			.map(|item| if item.is_null() { String::new() } else { js_string(item) })
			.collect::<Vec<_>>()
			.join(","),
		JsonValue::Object(_) => "[object Object]".to_owned(),
	}
}

fn splice_args(index: &JsonValue, remove: &JsonValue, items: JsonValue) -> Result<(u64, u64, Vec<JsonValue>), OpError> {
	let index = js_index(index).ok_or(OpError::SpliceIndex)?;
	let remove = js_index(remove).ok_or(OpError::SpliceRemove)?;
	let JsonValue::Array(items) = items else {
		return Err(OpError::SpliceItems);
	};
	Ok((index, remove, items))
}

fn permutation_arg(value: &JsonValue) -> Result<Vec<u64>, OpError> {
	let JsonValue::Array(indices) = value else {
		return Err(OpError::PermutationNotArray);
	};
	let permutation = indices
		.iter()
		.map(js_index)
		.collect::<Option<Vec<u64>>>()
		.ok_or(OpError::PermutationNotBijection)?;
	assert_permutation(&permutation)?;
	Ok(permutation)
}

/// Checks that `permutation` maps `0..n` onto itself, each index once.
pub fn assert_permutation(permutation: &[u64]) -> Result<(), OpError> {
	let mut seen = vec![false; permutation.len()];
	for &index in permutation {
		let slot = usize::try_from(index).ok().and_then(|index| seen.get_mut(index));
		match slot {
			Some(seen) if !*seen => *seen = true,
			_ => return Err(OpError::PermutationNotBijection),
		}
	}
	Ok(())
}

/// Checks a JSON tuple against the wire grammar (path ids, short forms and `#` definitions) and returns the op.
pub fn assert_valid_wire_op(op: JsonValue) -> Result<WireOp, OpError> {
	let (verb, items) = tuple(op)?;
	let length = items.len();
	match verb {
		Verb::Replace => {
			let [_, value] = fixed(items).ok_or(OpError::Arity("r"))?;
			Ok(WireOp::Replace(value))
		}
		Verb::Set if length == 3 => {
			let [_, reference, value] = fixed(items).ok_or(OpError::Arity("s"))?;
			Ok(WireOp::Set(Some(path_ref(&reference)?), value))
		}
		Verb::Set => {
			let [_, value] = fixed(items).ok_or(OpError::Arity("s"))?;
			Ok(WireOp::Set(None, value))
		}
		Verb::Delete if length == 2 => {
			let [_, reference] = fixed(items).ok_or(OpError::Arity("d"))?;
			Ok(WireOp::Delete(Some(path_ref(&reference)?)))
		}
		Verb::Delete => fixed::<1>(items)
			.map(|_| WireOp::Delete(None))
			.ok_or(OpError::Arity("d")),
		Verb::Append => wire_append(items),
		Verb::Truncate => wire_truncate(items),
		Verb::Splice => wire_splice(items),
		Verb::Move => wire_move(items),
		Verb::Define => {
			let Some([_, id, JsonValue::Array(path)]) = fixed(items) else {
				return Err(OpError::DefineShape);
			};
			let id = js_index(&id).ok_or(OpError::DefineShape)?;
			Ok(WireOp::Define(id, safe_segments(&path)?))
		}
	}
}

/// A path id (a non-negative integer) or an inline path.
fn path_ref(value: &JsonValue) -> Result<PathRef, OpError> {
	if value.is_number() {
		return js_index(value).map(PathRef::Id).ok_or(OpError::BadPathId);
	}
	Ok(PathRef::Path(path_arg(value)?))
}

fn wire_append(items: Vec<JsonValue>) -> Result<WireOp, OpError> {
	let (reference, text) = match items.len() {
		3 => {
			let [_, reference, text] = fixed(items).ok_or(OpError::Arity("a"))?;
			(Some(path_ref(&reference)?), text)
		}
		2 => {
			let [_, text] = fixed(items).ok_or(OpError::Arity("a"))?;
			(None, text)
		}
		_ => return Err(OpError::Arity("a")),
	};
	let JsonValue::String(text) = text else {
		return Err(OpError::AppendValue);
	};
	Ok(WireOp::Append(reference, text))
}

fn wire_truncate(items: Vec<JsonValue>) -> Result<WireOp, OpError> {
	let (reference, count) = match items.len() {
		3 => {
			let [_, reference, count] = fixed(items).ok_or(OpError::Arity("t"))?;
			(Some(path_ref(&reference)?), count)
		}
		2 => {
			let [_, count] = fixed(items).ok_or(OpError::Arity("t"))?;
			(None, count)
		}
		_ => return Err(OpError::Arity("t")),
	};
	Ok(WireOp::Truncate(
		reference,
		js_index(&count).ok_or(OpError::TruncateCount)?,
	))
}

fn wire_splice(items: Vec<JsonValue>) -> Result<WireOp, OpError> {
	let (reference, index, remove, items) = match items.len() {
		5 => {
			let [_, reference, index, remove, items] = fixed(items).ok_or(OpError::Arity("p"))?;
			(Some(path_ref(&reference)?), index, remove, items)
		}
		4 => {
			let [_, index, remove, items] = fixed(items).ok_or(OpError::Arity("p"))?;
			(None, index, remove, items)
		}
		_ => return Err(OpError::Arity("p")),
	};
	let (index, remove, items) = splice_args(&index, &remove, items)?;
	Ok(WireOp::Splice(reference, index, remove, items))
}

fn wire_move(items: Vec<JsonValue>) -> Result<WireOp, OpError> {
	let (reference, permutation) = match items.len() {
		3 => {
			let [_, reference, permutation] = fixed(items).ok_or(OpError::Arity("m"))?;
			(Some(path_ref(&reference)?), permutation)
		}
		2 => {
			let [_, permutation] = fixed(items).ok_or(OpError::Arity("m"))?;
			(None, permutation)
		}
		_ => return Err(OpError::Arity("m")),
	};
	Ok(WireOp::Move(reference, permutation_arg(&permutation)?))
}

/// Checks a typed path for reserved segments (indices are non-negative integers by type).
pub fn assert_safe_path(path: &[Seg]) -> Result<(), UnsafePathError> {
	match path
		.iter()
		.find(|segment| matches!(segment, Seg::Key(key) if RESERVED_SEGMENTS.contains(&key.as_str())))
	{
		Some(segment) => Err(UnsafePathError {
			segment: segment.to_string(),
		}),
		None => Ok(()),
	}
}

/// Checks what the `Op` type cannot express: safe path segments and, for `Move`, a bijective permutation.
pub fn validate_op(op: &Op) -> Result<(), OpError> {
	if let Some(path) = op.path() {
		assert_safe_path(path)?;
	}
	if let Op::Move(_, permutation) = op {
		assert_permutation(permutation)?;
	}
	Ok(())
}

/// An index may address an existing element or append exactly one past the end (TS `assertIndexInRange`).
///
/// This is what keeps the value JSON: a sparse array does not survive a JSON round trip, and
/// `["s", ["xs", 4294967290], 1]` would otherwise allocate billions of entries from one op. Returns the index.
pub fn assert_index_in_range(length: usize, index: u64) -> Result<usize, UnsafePathError> {
	usize::try_from(index)
		.ok()
		.filter(|&index| index <= length)
		.ok_or_else(|| UnsafePathError {
			segment: index.to_string(),
		})
}

#[cfg(test)]
mod tests {
	use super::{assert_valid_op, assert_valid_wire_op};
	use serde_json::json;

	// TS: delta.test.ts > operation application and validation > validates decoded and wire vocabularies separately
	#[test]
	fn validates_decoded_and_wire_vocabularies_separately() {
		assert!(assert_valid_op(json!(["s", ["value"], 1])).is_ok());
		assert!(assert_valid_op(json!(["s", 1])).is_err());
		assert!(assert_valid_wire_op(json!(["s", 1])).is_ok());
		assert!(assert_valid_wire_op(json!(["#", 0, ["value"]])).is_ok());
	}

	// TS: delta.test.ts > operation assertions > accepts decoded operations and rejects wire-only forms
	#[test]
	fn accepts_decoded_operations_and_rejects_wire_only_forms() {
		let decoded = [
			json!(["r", {"value": 1}]),
			json!(["s", ["value"], 1]),
			json!(["d", ["value"]]),
			json!(["a", ["value"], "x"]),
			json!(["t", ["value"], 2]),
			json!(["p", ["value"], 0, 0, []]),
			json!(["m", ["value"], [0]]),
		];
		for op in decoded {
			assert!(assert_valid_op(op.clone()).is_ok(), "{op}");
		}
		let wire_only = [
			json!(["s", 1]),
			json!(["d"]),
			json!(["a", "x"]),
			json!(["t", 2]),
			json!(["p", 0, 0, []]),
			json!(["#", 0, ["value"]]),
			json!(["s", 0, 1]),
		];
		for op in wire_only {
			assert!(assert_valid_op(op.clone()).is_err(), "{op}");
			assert!(assert_valid_wire_op(op.clone()).is_ok(), "{op}");
		}
	}

	// TS: delta.test.ts > operation application and validation > rejects unsafe and malformed paths (the validator
	// half)
	#[test]
	fn rejects_unsafe_and_malformed_paths() {
		assert!(assert_valid_op(json!(["s", "value", 1])).is_err());
		let error = assert_valid_op(json!(["m", [], [0, 0]])).unwrap_err();
		assert!(error.to_string().contains("bijection"));
	}

	// Rust-only: pins the TS check order and `String()` rendering that the corpus covers in bulk.
	#[test]
	fn messages_follow_the_ts_check_order() {
		let cases = [
			(json!(null), "op is not a tuple"),
			(json!([]), "op is not a tuple"),
			(json!([[1, [null, 2]], ["a"], 1]), "unknown op verb: 1,,2"),
			(json!([{}, ["a"]]), "unknown op verb: [object Object]"),
			(json!(["#", 0, ["a"]]), "unknown op verb: #"),
			(json!(["t", ["__proto__"], -1]), "t shape"),
			(json!(["p", ["__proto__"], -1, 0, []]), "unsafe path segment: __proto__"),
			(json!(["s", [1.5], 1]), "unsafe path segment: 1.5"),
			(json!(["s", [-0.0, null], 1]), "unsafe path segment: null"),
			(json!(["d", []]), "path is empty"),
			(json!(["m", "a", "x"]), "path is not an array"),
		];
		for (op, message) in cases {
			assert_eq!(assert_valid_op(op.clone()).unwrap_err().to_string(), message, "{op}");
		}
		// Integral floats are integers in JS.
		assert!(assert_valid_op(json!(["m", ["a"], [1.0, 0]])).is_ok());
	}

	// TS: delta.test.ts > operation assertions > does not recursively inspect operation payloads
	#[test]
	fn does_not_recursively_inspect_operation_payloads() {
		let deep = json!({"__proto__": {"constructor": [{"prototype": 1}]}});
		assert!(assert_valid_op(json!(["s", ["value"], deep.clone()])).is_ok());
		assert!(assert_valid_wire_op(json!(["r", deep])).is_ok());
	}
}
