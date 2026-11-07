//! The wire codec (TS `encoder`, `decoder`): path interning and short forms between the op vocabulary and a
//! boundary, byte-compatible with TS (same ids, same short forms, same order).
//!
//! One encoder/decoder pair per independent state stream: every decoder must see exactly the batches its encoder
//! produced, from that state's base on. Path ids are the only state that spans batches; a short form never does, so a
//! reader that skips or reorders a batch cannot decode it into the wrong path. A base batch (`r`) is a recovery point:
//! it resets both dictionaries, so everything after it is self-contained. After a decode error, discard the decoder and
//! the replica and recover from a later `r`.

use crate::delta::apply::{PathError, UnresolvedPath};
use crate::delta::op::{NonEmptyPath, Op, Path, PathRef, Seg, WireOp};
use crate::delta::validate::{OpError, assert_valid_wire_op};
use pi_json::JsonValue;
use std::collections::{HashMap, HashSet};

/// Interns repeated paths of one stream into `WireOp` tuples.
#[derive(Debug, Default)]
pub struct Encoder {
	seen: HashSet<Path>,
	ids: HashMap<Path, u64>,
	next_id: u64,
}

/// Restores ops from the `WireOp` tuples of one stream.
#[derive(Debug, Default)]
pub struct Decoder {
	paths: HashMap<u64, Path>,
}

/// Why a wire batch did not decode.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
	/// A tuple that is not a wire op.
	#[error(transparent)]
	Op(#[from] OpError),
	/// A path id without a definition, a short form without a previous path, or an empty path where one is needed.
	#[error(transparent)]
	Path(#[from] PathError),
}

impl Encoder {
	/// An encoder with empty dictionaries.
	pub fn new() -> Self {
		Self::default()
	}

	/// Encodes one batch. A path is inline on first use, defined (`["#", id, path]`) and referenced on its second, a
	/// reference from then on, and omitted when it equals the previous op's path in this batch. Payloads are copied
	/// once, into the wire ops; `WireOp::into_json` moves them on into the frame.
	pub fn encode(&mut self, ops: &[Op]) -> Vec<WireOp> {
		let mut out = Vec::with_capacity(ops.len());
		let mut previous: Option<&[Seg]> = None;
		for op in ops {
			let Some(path) = op.path() else {
				out.push(wire_op(op, None));
				self.seen.clear();
				self.ids.clear();
				self.next_id = 0;
				previous = None;
				continue;
			};
			if previous == Some(path) {
				out.push(wire_op(op, None));
				continue;
			}
			let reference = self.reference(path, &mut out);
			out.push(wire_op(op, Some(reference)));
			previous = Some(path);
		}
		out
	}

	/// The reference for `path`, defining an id first on its second use: a definition costs more than the path it
	/// replaces, so interning on first use loses on the many paths written once.
	fn reference(&mut self, path: &[Seg], out: &mut Vec<WireOp>) -> PathRef {
		if let Some(&id) = self.ids.get(path) {
			return PathRef::Id(id);
		}
		if !self.seen.contains(path) {
			self.seen.insert(path.to_vec());
			return PathRef::Path(path.to_vec());
		}
		let id = self.next_id;
		self.next_id += 1;
		self.ids.insert(path.to_vec(), id);
		out.push(WireOp::Define(id, path.to_vec()));
		PathRef::Id(id)
	}
}

/// The wire form of `op` with `reference`, or its short form for `None`.
fn wire_op(op: &Op, reference: Option<PathRef>) -> WireOp {
	match op {
		Op::Replace(value) => WireOp::Replace(value.clone()),
		Op::Set(_, value) => WireOp::Set(reference, value.clone()),
		Op::Delete(_) => WireOp::Delete(reference),
		Op::Append(_, text) => WireOp::Append(reference, text.clone()),
		Op::Truncate(_, count) => WireOp::Truncate(reference, *count),
		Op::Splice(_, index, remove, items) => WireOp::Splice(reference, *index, *remove, items.clone()),
		Op::Move(_, permutation) => WireOp::Move(reference, permutation.clone()),
	}
}

impl Decoder {
	/// A decoder with an empty dictionary.
	pub fn new() -> Self {
		Self::default()
	}

	/// Decodes one batch of typed wire ops (validated when they were deserialized); copies their payloads.
	pub fn decode(&mut self, wire: &[WireOp]) -> Result<Vec<Op>, DecodeError> {
		let mut batch = Batch::default();
		for op in wire {
			self.decode_op(op.clone(), &mut batch)?;
		}
		Ok(batch.out)
	}

	/// Decodes one batch as received (TS `decode`): each tuple is validated just before it is used, so the first
	/// failing tuple, by validation or by resolution, gives the error TS throws. Payloads move into the ops.
	pub fn decode_values(&mut self, wire: Vec<JsonValue>) -> Result<Vec<Op>, DecodeError> {
		let mut batch = Batch::default();
		for op in wire {
			self.decode_op(assert_valid_wire_op(op)?, &mut batch)?;
		}
		Ok(batch.out)
	}

	fn decode_op(&mut self, op: WireOp, batch: &mut Batch) -> Result<(), DecodeError> {
		let decoded = match op {
			WireOp::Define(id, path) => {
				self.paths.insert(id, path);
				return Ok(());
			}
			WireOp::Replace(value) => {
				self.paths.clear();
				batch.previous = None;
				Op::Replace(value)
			}
			WireOp::Set(reference, value) => Op::Set(leaf(self.path(reference, batch)?)?, value),
			WireOp::Delete(reference) => Op::Delete(leaf(self.path(reference, batch)?)?),
			WireOp::Append(reference, text) => Op::Append(leaf(self.path(reference, batch)?)?, text),
			WireOp::Truncate(reference, count) => Op::Truncate(leaf(self.path(reference, batch)?)?, count),
			WireOp::Splice(reference, index, remove, items) => {
				Op::Splice(self.path(reference, batch)?, index, remove, items)
			}
			WireOp::Move(reference, permutation) => Op::Move(self.path(reference, batch)?, permutation),
		};
		batch.out.push(decoded);
		Ok(())
	}

	/// The path an op names: the previous op's for a short form, else the referenced one, which becomes the previous.
	fn path(&self, reference: Option<PathRef>, batch: &mut Batch) -> Result<Path, PathError> {
		let Some(reference) = reference else {
			return batch.previous.clone().ok_or_else(|| PathError::at(&[]));
		};
		let path = match reference {
			PathRef::Path(path) => path,
			PathRef::Id(id) => self.paths.get(&id).cloned().ok_or(PathError {
				path: UnresolvedPath::Id(id),
			})?,
		};
		batch.previous = Some(path.clone());
		Ok(path)
	}
}

/// The ops decoded so far in one batch and the path a short form reuses; both are scoped to the batch.
#[derive(Default)]
struct Batch {
	out: Vec<Op>,
	previous: Option<Path>,
}

/// The path of an `s`, `d`, `a` or `t`, which cannot target the root.
fn leaf(path: Path) -> Result<NonEmptyPath, PathError> {
	NonEmptyPath::new(path).ok_or_else(|| PathError::at(&[]))
}

#[cfg(test)]
mod tests {
	use super::{DecodeError, Decoder, Encoder};
	use crate::delta::op::{Op, WireOp};
	use crate::delta::validate::{assert_valid_op, assert_valid_wire_op};
	use pi_json::{JsonValue, to_js_string};
	use serde_json::json;

	fn ops(value: JsonValue) -> Vec<Op> {
		let JsonValue::Array(items) = value else {
			panic!("ops are an array")
		};
		items.into_iter().map(|op| assert_valid_op(op).unwrap()).collect()
	}

	fn wire_json(wire: &[WireOp]) -> String {
		to_js_string(&JsonValue::Array(wire.iter().cloned().map(WireOp::into_json).collect()))
	}

	fn values(value: JsonValue) -> Vec<JsonValue> {
		let JsonValue::Array(items) = value else {
			panic!("wire is an array")
		};
		items
	}

	// TS: delta.test.ts > codec > interns paths, omits adjacent paths, and round-trips
	#[test]
	fn interns_paths_omits_adjacent_paths_and_round_trips() {
		let mut encoder = Encoder::new();
		let mut decoder = Decoder::new();
		let first = ops(json!([["t", ["nested", "text"], 1], ["a", ["nested", "text"], "x"]]));
		assert_eq!(decoder.decode(&encoder.encode(&first)).unwrap(), first);
		let second = ops(json!([["a", ["nested", "text"], "y"]]));
		let wire = encoder.encode(&second);
		assert_eq!(wire_json(&wire), r##"[["#",0,["nested","text"]],["a",0,"y"]]"##);
		assert_eq!(decoder.decode(&wire).unwrap(), second);
	}

	// TS: delta.test.ts > codec > resets path dictionaries on a base
	#[test]
	fn resets_path_dictionaries_on_a_base() {
		let mut encoder = Encoder::new();
		encoder.encode(&ops(json!([["s", ["value"], 1]])));
		encoder.encode(&ops(json!([["s", ["value"], 2]])));
		assert_eq!(
			wire_json(&encoder.encode(&ops(json!([["r", {"value": 3}]])))),
			r#"[["r",{"value":3}]]"#
		);
		assert_eq!(
			wire_json(&encoder.encode(&ops(json!([["s", ["value"], 4]])))),
			r#"[["s",["value"],4]]"#
		);
	}

	// TS: delta.test.ts > codec > rejects unresolved short forms and unsafe interned paths
	#[test]
	fn rejects_unresolved_short_forms_and_unsafe_interned_paths() {
		assert!(Decoder::new().decode_values(values(json!([["a", "x"]]))).is_err());
		let error = Decoder::new().decode_values(values(json!([["#", 0, ["__proto__"]], ["s", 0, true]])));
		assert_eq!(error.unwrap_err().to_string(), "unsafe path segment: __proto__");
	}

	// TS: delta.test.ts > codec > omits an adjacent repeated path
	#[test]
	fn omits_an_adjacent_repeated_path() {
		let wire = Encoder::new().encode(&ops(json!([["s", ["value"], 1], ["s", ["value"], 2]])));
		assert_eq!(wire_json(&wire), r#"[["s",["value"],1],["s",2]]"#);
	}

	// TS: delta.test.ts > codec > interns on second use rather than first
	#[test]
	fn interns_on_second_use_rather_than_first() {
		let mut encoder = Encoder::new();
		assert_eq!(
			wire_json(&encoder.encode(&ops(json!([["a", ["a", "deep"], "1"]])))),
			r#"[["a",["a","deep"],"1"]]"#
		);
		assert_eq!(
			wire_json(&encoder.encode(&ops(json!([["a", ["a", "deep"], "2"]])))),
			r##"[["#",0,["a","deep"]],["a",0,"2"]]"##
		);
	}

	// TS: delta.test.ts > codec > does not collide paths containing null characters
	#[test]
	fn does_not_collide_paths_containing_null_characters() {
		let batch = ops(json!([["s", ["a\u{0}b"], 1], ["s", ["a", "b"], 2]]));
		assert_eq!(Decoder::new().decode(&Encoder::new().encode(&batch)).unwrap(), batch);
	}

	// TS: delta.test.ts > codec > clears decoder ids on a base batch
	#[test]
	fn clears_decoder_ids_on_a_base_batch() {
		let mut decoder = Decoder::new();
		decoder
			.decode_values(values(json!([["#", 0, ["a"]], ["a", 0, "1"]])))
			.unwrap();
		decoder.decode_values(values(json!([["r", {"a": ""}]]))).unwrap();
		let error = decoder.decode_values(values(json!([["a", 0, "2"]]))).unwrap_err();
		assert_eq!(error.to_string(), "unresolvable path: 0");
	}

	// TS: delta.test.ts > codec > makes batches after a base self-contained
	#[test]
	fn makes_batches_after_a_base_self_contained() {
		let mut encoder = Encoder::new();
		encoder.encode(&ops(json!([["a", ["a", "deep"], "1"]])));
		encoder.encode(&ops(json!([["a", ["a", "deep"], "2"]])));
		let base = encoder.encode(&ops(json!([["r", {"a": {"deep": "x"}}]])));
		let after = encoder.encode(&ops(json!([["a", ["a", "deep"], "3"]])));
		assert_eq!(wire_json(&after), r#"[["a",["a","deep"],"3"]]"#);
		let mut decoder = Decoder::new();
		assert_eq!(
			decoder.decode(&base).unwrap(),
			ops(json!([["r", {"a": {"deep": "x"}}]]))
		);
		assert_eq!(decoder.decode(&after).unwrap(), ops(json!([["a", ["a", "deep"], "3"]])));
	}

	// TS: delta.test.ts > codec > round-trips deterministic mixed operation streams
	#[test]
	fn round_trips_deterministic_mixed_operation_streams() {
		let batches: Vec<Vec<Op>> = (0..100)
			.map(|index| {
				ops(json!([
					["s", ["rows", index, "value"], index],
					["a", ["output"], index.to_string()],
					["p", ["tail"], index, 0, [index]],
				]))
			})
			.collect();
		let mut encoder = Encoder::new();
		let mut decoder = Decoder::new();
		for batch in &batches {
			assert_eq!(&decoder.decode(&encoder.encode(batch)).unwrap(), batch);
		}
	}

	// TS: delta.test.ts > path and prototype safety > rejects constructor walks and forbidden interned paths (the
	// decode half)
	#[test]
	fn rejects_forbidden_interned_paths() {
		let wire = values(json!([["#", 0, ["__proto__", "w"]], ["s", 0, true]]));
		assert!(Decoder::new().decode_values(wire).is_err());
	}

	// TS: delta.test.ts > operation structure safety > rejects invalid append and truncation operations (the decode
	// half)
	#[test]
	fn rejects_a_negative_truncation_count() {
		let error = Decoder::new()
			.decode_values(values(json!([["t", ["value"], -1]])))
			.unwrap_err();
		assert!(matches!(error, DecodeError::Op(_)));
		assert_eq!(error.to_string(), "t count");
	}

	// Rust-only: pins each short form and arity look-alike the corpus covers in bulk.
	#[test]
	fn short_forms_and_arity_lookalikes_round_trip() {
		let batch = ops(json!([
			["s", ["k"], [1]],
			["s", ["k"], 2],
			["p", [], 0, 0, [0]],
			["p", [], 1, 0, []],
			["m", [], [0]],
			["m", ["k"], [0]],
			["t", ["k"], 3],
			["t", ["k"], 1],
			["d", ["k"]],
			["d", ["k"]],
		]));
		let wire = Encoder::new().encode(&batch);
		let expected = concat!(
			r#"[["s",["k"],[1]],["s",2],["p",[],0,0,[0]],["p",1,0,[]],["m",[0]],"#,
			r##"["#",0,["k"]],["m",0,[0]],["t",3],["t",1],["d"],["d"]]"##,
		);
		assert_eq!(wire_json(&wire), expected);
		let parsed: Vec<WireOp> = wire
			.iter()
			.map(|op| assert_valid_wire_op(op.to_json()).unwrap())
			.collect();
		assert_eq!(parsed, wire);
		assert_eq!(Decoder::new().decode(&parsed).unwrap(), batch);
	}
}
