//! The pending queue (TS `delta/pending.ts`): the ops a slow consumer of one state has not taken yet, folded while
//! they wait, so what it hands over replays to the same value with fewer ops.

use crate::delta::op::{NonEmptyPath, Op};
use pi_json::JsonValue;
use std::collections::HashMap;
use std::collections::hash_map::{Entry, RandomState};
use std::hash::BuildHasher;
use std::sync::Arc;

/// What `PendingOps::take` hands over.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingTake {
	/// The pending ops, folded; a single received batch is handed over as received (the same `Arc`).
	Ops(Arc<[Op]>),
	/// The queue overflowed: the owner sends the current value as `[["r", value]]`.
	Replace,
}

/// Pending ops past which only "replace with the current value" stays pending.
pub const MAX_PENDING_OPS: usize = 10_000;

/// Pending ops of one state for one consumer, in order and raw (never encoded while they wait).
///
/// `r` drops everything before it. A leaf op (`a`, or `s` of a primitive) merges into the last pending leaf op on the
/// same path (`a` + `a` joined, `s` replaces `s` or `a`, `a` after a string `s` appends into it); any other op clears
/// the leaf index, as it may change what a path names. Leaf ops on different paths commute, except after an ancestor
/// primitive `s`, after which the descendant cannot be written until a structural op clears the index. Past
/// `MAX_PENDING_OPS` only "replace" stays pending.
///
/// Ops are only those of Delta's tracker, `diff_revisions()` and `append_text()`; hand-written ops are neither
/// validated nor normalized. Received batches are never changed (they feed other consumers too); folding copies the
/// ops it keeps, payloads included, and then appends into its own copies. `S` hashes the paths of the leaf index.
#[derive(Debug)]
pub struct PendingOps<S = RandomState> {
	state: State<S>,
	hasher: S,
}

#[derive(Debug)]
enum State<S> {
	Empty,
	/// One batch, kept by reference: a consumer that keeps up never pays for folding.
	Batch(Arc<[Op]>),
	Folding(Folding<S>),
	/// Overflowed: nothing more accumulates until `take()` or a batch that starts with `r`.
	Replace,
}

#[derive(Debug)]
struct Folding<S> {
	/// Owned by the queue until `take()` hands it over; never one of the received batches.
	ops: Vec<Op>,
	/// Path to the position of the last pending leaf op on that path.
	leaves: HashMap<NonEmptyPath, usize, S>,
}

impl PendingOps {
	/// An empty queue.
	pub fn new() -> Self {
		Self::with_hasher(RandomState::new())
	}
}

impl Default for PendingOps {
	fn default() -> Self {
		Self::new()
	}
}

impl<S: BuildHasher + Clone> PendingOps<S> {
	/// An empty queue whose leaf index hashes paths with `hasher`.
	pub fn with_hasher(hasher: S) -> Self {
		Self {
			state: State::Empty,
			hasher,
		}
	}

	/// Queues the ops of one commit.
	pub fn push(&mut self, ops: &Arc<[Op]>) {
		let Some(first) = ops.first() else {
			return;
		};
		// A batch that starts with `r` (the tracker's and `diff_revisions()`'s only place for it) drops everything
		// pending, an overflow too, and is kept as received.
		if matches!(first, Op::Replace(_)) {
			self.state = State::Batch(ops.clone());
			return;
		}
		self.state = match std::mem::replace(&mut self.state, State::Empty) {
			State::Empty => State::Batch(ops.clone()),
			State::Batch(pending) => {
				let mut folding = Folding {
					ops: Vec::new(),
					leaves: HashMap::with_hasher(self.hasher.clone()),
				};
				if folding.fold(&pending) && folding.fold(ops) {
					State::Folding(folding)
				} else {
					State::Replace
				}
			}
			State::Folding(mut folding) => {
				if folding.fold(ops) {
					State::Folding(folding)
				} else {
					State::Replace
				}
			}
			State::Replace => State::Replace,
		};
	}

	/// Pending ops after folding; 1 once overflowed (the replacement the owner sends). One batch counts as received,
	/// so a single commit larger than `MAX_PENDING_OPS` counts in full until a second one arrives and overflows.
	pub fn size(&self) -> usize {
		match &self.state {
			State::Empty => 0,
			State::Batch(ops) => ops.len(),
			State::Folding(folding) => folding.ops.len(),
			State::Replace => 1,
		}
	}

	/// Hands over everything pending and starts empty.
	pub fn take(&mut self) -> PendingTake {
		match std::mem::replace(&mut self.state, State::Empty) {
			State::Empty => PendingTake::Ops(Arc::from([])),
			State::Batch(ops) => PendingTake::Ops(ops),
			State::Folding(folding) => PendingTake::Ops(Arc::from(folding.ops)),
			State::Replace => PendingTake::Replace,
		}
	}
}

impl<S: BuildHasher> Folding<S> {
	/// Folds `ops` in; false once the queue overflowed into "replace".
	fn fold(&mut self, ops: &[Op]) -> bool {
		for op in ops {
			match op {
				Op::Replace(_) => {
					self.ops.clear();
					self.leaves.clear();
				}
				Op::Append(path, _) => {
					if self.merge_leaf(path, op) {
						continue;
					}
				}
				Op::Set(path, value) if is_primitive(value) => {
					if self.merge_leaf(path, op) {
						continue;
					}
				}
				_ => self.leaves.clear(),
			}
			self.ops.push(op.clone());
			if self.ops.len() > MAX_PENDING_OPS {
				return false;
			}
		}
		true
	}

	/// Merges the leaf op `op` into the last pending leaf op on its path, or indexes it at the position it is about to
	/// be pushed to. True when merged. Hashes the path once, as TS computes one path key per leaf op.
	fn merge_leaf(&mut self, path: &NonEmptyPath, op: &Op) -> bool {
		match self.leaves.entry(path.clone()) {
			Entry::Occupied(mut entry) => {
				// The index points at leaf ops only.
				if merge_into(&mut self.ops[*entry.get()], op) {
					return true;
				}
				entry.insert(self.ops.len());
			}
			Entry::Vacant(entry) => {
				entry.insert(self.ops.len());
			}
		}
		false
	}
}

fn is_primitive(value: &JsonValue) -> bool {
	!matches!(value, JsonValue::Array(_) | JsonValue::Object(_))
}

/// Folds `next` into `previous` (both leaf ops on one path, `previous` the queue's own copy); false when they do not
/// fold. Appends extend the held string in place, so N appends to one path cost O(total length), not O(N x length).
fn merge_into(previous: &mut Op, next: &Op) -> bool {
	match (previous, next) {
		(previous, Op::Set(..)) => *previous = next.clone(),
		// Only a string set takes an append; the tracker never appends to another primitive.
		(Op::Append(_, text) | Op::Set(_, JsonValue::String(text)), Op::Append(_, more)) => text.push_str(more),
		_ => return false,
	}
	true
}

#[cfg(test)]
mod tests {
	use super::{MAX_PENDING_OPS, PendingOps, PendingTake};
	use crate::delta::op::{NonEmptyPath, Op};
	use crate::delta::snapshot::{Snapshot, apply_immutable};
	use crate::delta::validate::assert_valid_op;
	use crate::delta::wire::{Decoder, Encoder};
	use pi_json::JsonValue;
	use serde_json::json;
	use std::collections::hash_map::DefaultHasher;
	use std::hash::BuildHasher;
	use std::sync::Arc;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::time::{Duration, Instant};

	fn ops(value: JsonValue) -> Vec<Op> {
		let JsonValue::Array(items) = value else {
			panic!("ops are an array")
		};
		items.into_iter().map(|op| assert_valid_op(op).unwrap()).collect()
	}

	fn batch(value: JsonValue) -> Arc<[Op]> {
		Arc::from(ops(value))
	}

	fn taken_ops(take: PendingTake) -> Vec<Op> {
		match take {
			PendingTake::Ops(ops) => ops.to_vec(),
			PendingTake::Replace => panic!("unexpected overflow"),
		}
	}

	/// Pushes each op as its own batch, takes, checks the replay directly and through the codec; returns the folded ops.
	fn fold_one_by_one(initial: JsonValue, value: JsonValue) -> Vec<Op> {
		let ops = ops(value);
		let mut queue = PendingOps::new();
		for op in &ops {
			queue.push(&Arc::from([op.clone()]));
		}
		let folded = taken_ops(queue.take());
		let base = Snapshot::from(&initial);
		let expected = apply_immutable(&base, &ops).unwrap();
		assert_eq!(apply_immutable(&base, &folded).unwrap(), expected);
		let mut sender = Encoder::new();
		let mut receiver = Decoder::new();
		let base_ops = receiver.decode(&sender.encode(&[Op::Replace(initial)])).unwrap();
		let replica = apply_immutable(&Snapshot::Null, &base_ops).unwrap();
		let replayed = receiver.decode(&sender.encode(&folded)).unwrap();
		assert_eq!(apply_immutable(&replica, &replayed).unwrap(), expected);
		folded
	}

	/// `count` set ops on distinct paths.
	fn distinct_sets(count: usize, from: usize) -> Arc<[Op]> {
		(0..count)
			.map(|index| {
				let path = NonEmptyPath::new(vec!["k".into(), u64::try_from(from + index).unwrap().into()]).unwrap();
				Op::Set(path, json!(index))
			})
			.collect()
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > a leaf set in an array element, a splice before
	// it, the same path set again
	#[test]
	fn a_leaf_set_in_an_array_element_a_splice_before_it_the_same_path_set_again() {
		let sets = json!([["s", ["arr", 2, "x"], 1], ["p", ["arr"], 0, 0, [{"x": 9}]], ["s", ["arr", 2, "x"], 2]]);
		assert_eq!(
			fold_one_by_one(json!({"arr": [{"x": 0}, {"x": 0}, {"x": 0}]}), sets.clone()),
			ops(sets)
		);
		let appends =
			json!([["a", ["arr", 0, "x"], "1"], ["p", ["arr"], 0, 0, [{"x": "new"}]], ["a", ["arr", 0, "x"], "2"]]);
		assert_eq!(
			fold_one_by_one(json!({"arr": [{"x": "a"}, {"x": "b"}]}), appends.clone()),
			ops(appends)
		);
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > `t` then `a`, and appends around a trim
	#[test]
	fn t_then_a_and_appends_around_a_trim() {
		assert_eq!(
			fold_one_by_one(json!({"s": "hello"}), json!([["t", ["s"], 2], ["a", ["s"], "!"]])).len(),
			2
		);
		let around = json!([["a", ["s"], "AB"], ["t", ["s"], 3], ["a", ["s"], "C"]]);
		assert_eq!(fold_one_by_one(json!({"s": "hello"}), around).len(), 3);
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > an object `s` at a parent, then leaf sets below it
	#[test]
	fn an_object_s_at_a_parent_then_leaf_sets_below_it() {
		let sets = json!([["s", ["o", "a"], 2], ["s", ["o"], {"a": 5, "b": 6}], ["s", ["o", "a"], 7]]);
		assert_eq!(fold_one_by_one(json!({"o": {"a": 1}}), sets).len(), 3);
		let folded = fold_one_by_one(
			json!({"x": {"s": "a"}}),
			json!([["s", ["x"], {"s": "b"}], ["a", ["x", "s"], "c"], ["s", ["x", "s"], "d"]]),
		);
		assert_eq!(folded, ops(json!([["s", ["x"], {"s": "b"}], ["s", ["x", "s"], "d"]])));
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > an ancestor primitive `s` between leaf ops below it
	#[test]
	fn an_ancestor_primitive_s_between_leaf_ops_below_it() {
		let folded = fold_one_by_one(
			json!({"o": {"a": "x", "b": "y"}}),
			json!([
				["a", ["o", "a"], "1"],
				["s", ["o", "b"], "q"],
				["s", ["o"], "flat"],
				["a", ["o"], "!"]
			]),
		);
		assert_eq!(
			folded,
			ops(json!([
				["a", ["o", "a"], "1"],
				["s", ["o", "b"], "q"],
				["s", ["o"], "flat!"]
			]))
		);
		let folded = fold_one_by_one(
			json!({"a": {"b": 0}}),
			json!([["s", ["a", "b"], 1], ["s", ["a"], null], ["s", ["a"], "x"]]),
		);
		assert_eq!(folded, ops(json!([["s", ["a", "b"], 1], ["s", ["a"], "x"]])));
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > deletes, moves and splices shift what a path names
	#[test]
	fn deletes_moves_and_splices_shift_what_a_path_names() {
		let deletes = json!([["s", ["arr", 2], "C"], ["d", ["arr", 0]], ["s", ["arr", 1], "Z"]]);
		assert_eq!(fold_one_by_one(json!({"arr": ["a", "b", "c"]}), deletes).len(), 3);
		let moves = json!([["s", ["arr", 0], 10], ["m", ["arr"], [2, 1, 0]], ["s", ["arr", 0], 20]]);
		assert_eq!(fold_one_by_one(json!({"arr": [1, 2, 3]}), moves).len(), 3);
		let splices = json!([
			["s", ["arr", 0, "x"], 1], ["p", ["arr"], 1, 0, [{"x": 0}]], ["s", ["arr", 0, "x"], 2], ["s", ["arr", 1, "x"], 3],
		]);
		assert_eq!(fold_one_by_one(json!({"arr": [{"x": 0}]}), splices).len(), 4);
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > keys of one parent never collide: `a.b` and
	// `a`/`b`, `1` on an object and on an array
	#[test]
	fn keys_of_one_parent_never_collide() {
		fold_one_by_one(
			json!({"a": {"b": "x"}, "a.b": "z"}),
			json!([["a", ["a", "b"], "1"], ["s", ["a.b"], "y"], ["a", ["a", "b"], "2"]]),
		);
		fold_one_by_one(
			json!({"o": {"1": "a"}, "arr": ["x", "y"]}),
			json!([
				["s", ["o", "1"], "b"],
				["s", ["arr", 1], "z"],
				["s", ["o", "1"], "c"],
				["s", ["arr", 1], "w"]
			]),
		);
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > an array set at the end appends, and a later set
	// of that index folds into it
	#[test]
	fn an_array_set_at_the_end_appends_and_a_later_set_of_that_index_folds_into_it() {
		let folded = fold_one_by_one(
			json!({"arr": [1]}),
			json!([["s", ["arr", 1], 2], ["s", ["arr", 0], 9], ["s", ["arr", 1], 3]]),
		);
		assert_eq!(folded, ops(json!([["s", ["arr", 1], 3], ["s", ["arr", 0], 9]])));
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > `r` drops everything before it
	#[test]
	fn r_drops_everything_before_it() {
		let folded = fold_one_by_one(
			json!({"a": 1}),
			json!([["s", ["a"], 2], ["r", {"b": "x"}], ["a", ["b"], "y"]]),
		);
		assert_eq!(folded, ops(json!([["r", {"b": "x"}], ["a", ["b"], "y"]])));
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > joins appends, folds appends into a string set,
	// and keeps split surrogates
	#[test]
	fn joins_appends_folds_appends_into_a_string_set_and_keeps_split_surrogates() {
		let folded = fold_one_by_one(
			json!({"s": ""}),
			json!([
				["s", ["s"], "ab"],
				["a", ["s"], "c"],
				["s", ["t"], 1],
				["a", ["s"], "d"]
			]),
		);
		assert_eq!(folded, ops(json!([["s", ["s"], "abcd"], ["s", ["t"], 1]])));
		// Decided divergence (rust/docs/slices.md): `pi_json::parse` reads each half of a split pair as U+FFFD, so the
		// halves join to two U+FFFD where TS joins them to the emoji.
		let split =
			pi_json::parse(r#"[["a",["s"],"\ud83d"],["s",["n"],1],["a",["s"],"\ude00"],["a",["s"],"é"]]"#).unwrap();
		let folded = fold_one_by_one(json!({"s": ""}), split);
		assert_eq!(folded, ops(json!([["a", ["s"], "\u{FFFD}\u{FFFD}é"], ["s", ["n"], 1]])));
	}

	// TS: delta-pending.test.ts > PendingOps adversarial sequences > interleaved leaves, a structural op, then the same
	// leaves again
	#[test]
	fn interleaved_leaves_a_structural_op_then_the_same_leaves_again() {
		let folded = fold_one_by_one(
			json!({"a": "", "b": "", "list": [0]}),
			json!([
				["a", ["a"], "1"],
				["a", ["b"], "1"],
				["a", ["a"], "2"],
				["p", ["list"], 1, 0, [1]],
				["a", ["b"], "2"],
				["a", ["a"], "3"],
			]),
		);
		let expected = json!([
			["a", ["a"], "12"],
			["a", ["b"], "1"],
			["p", ["list"], 1, 0, [1]],
			["a", ["b"], "2"],
			["a", ["a"], "3"],
		]);
		assert_eq!(folded, ops(expected));
	}

	// TS: delta-pending.test.ts > PendingOps cap > keeps 10000 pending ops and replaces past them
	#[test]
	fn keeps_max_pending_ops_and_replaces_past_them() {
		let mut queue = PendingOps::new();
		queue.push(&Arc::from([]));
		queue.push(&distinct_sets(MAX_PENDING_OPS, 0));
		assert_eq!(taken_ops(queue.take()).len(), MAX_PENDING_OPS);

		queue.push(&distinct_sets(MAX_PENDING_OPS / 2, 0));
		queue.push(&distinct_sets(MAX_PENDING_OPS / 2, MAX_PENDING_OPS / 2));
		assert_eq!(taken_ops(queue.take()).len(), MAX_PENDING_OPS);

		queue.push(&distinct_sets(MAX_PENDING_OPS / 2, 0));
		queue.push(&distinct_sets(MAX_PENDING_OPS / 2 + 1, MAX_PENDING_OPS / 2));
		assert_eq!(queue.take(), PendingTake::Replace);
	}

	// TS: delta-pending.test.ts > PendingOps cap > counts ops after folding
	#[test]
	fn counts_ops_after_folding() {
		let mut queue = PendingOps::new();
		let append = batch(json!([["a", ["text"], "x"]]));
		for _ in 0..3 * MAX_PENDING_OPS {
			queue.push(&append);
		}
		let expected = vec![Op::Append(
			NonEmptyPath::new(vec!["text".into()]).unwrap(),
			"x".repeat(3 * MAX_PENDING_OPS),
		)];
		assert_eq!(taken_ops(queue.take()), expected);
	}

	// TS: delta-pending.test.ts > PendingOps cap > accumulates nothing once replaced, until taken or a batch starting
	// with `r`
	#[test]
	fn accumulates_nothing_once_replaced_until_taken_or_a_batch_starting_with_r() {
		let mut queue = PendingOps::new();
		queue.push(&distinct_sets(1, 0));
		queue.push(&distinct_sets(MAX_PENDING_OPS + 1, 0));
		queue.push(&distinct_sets(3, 0));
		assert_eq!(queue.take(), PendingTake::Replace);
		assert_eq!(taken_ops(queue.take()), Vec::new());

		queue.push(&distinct_sets(1, 0));
		queue.push(&distinct_sets(MAX_PENDING_OPS + 1, 0));
		let fresh = batch(json!([["r", {"k": [0]}]]));
		queue.push(&fresh);
		let PendingTake::Ops(taken) = queue.take() else {
			panic!("ops")
		};
		assert!(Arc::ptr_eq(&taken, &fresh));
		queue.push(&distinct_sets(1, 0));
		queue.push(&fresh);
		queue.push(&batch(json!([["s", ["n"], 1]])));
		assert_eq!(
			taken_ops(queue.take()),
			ops(json!([["r", {"k": [0]}], ["s", ["n"], 1]]))
		);
	}

	// TS: delta-pending.test.ts > PendingOps cap > hands one batch over as received, whatever its size
	#[test]
	fn hands_one_batch_over_as_received_whatever_its_size() {
		let mut queue = PendingOps::new();
		let large = distinct_sets(MAX_PENDING_OPS + 1, 0);
		queue.push(&large);
		let PendingTake::Ops(taken) = queue.take() else {
			panic!("ops")
		};
		assert!(Arc::ptr_eq(&taken, &large));
		queue.push(&large);
		queue.push(&batch(json!([["s", ["n"], 1]])));
		assert_eq!(queue.take(), PendingTake::Replace);
	}

	// TS: delta-pending.test.ts > PendingOps ownership > hands over a received batch by identity and never mutates
	// batches or op tuples
	#[test]
	fn hands_over_a_received_batch_by_identity_and_never_mutates_batches() {
		let mut queue = PendingOps::new();
		let first = batch(json!([["a", ["s"], "1"]]));
		queue.push(&first);
		let PendingTake::Ops(alone) = queue.take() else {
			panic!("ops")
		};
		assert!(Arc::ptr_eq(&alone, &first));

		let second = batch(json!([["a", ["s"], "2"]]));
		queue.push(&first);
		queue.push(&second);
		assert_eq!(taken_ops(queue.take()), ops(json!([["a", ["s"], "12"]])));
		assert_eq!(&*first, ops(json!([["a", ["s"], "1"]])).as_slice());
		assert_eq!(&*second, ops(json!([["a", ["s"], "2"]])).as_slice());
	}

	// TS: delta-pending.test.ts > PendingOps size > counts pending ops after folding, the replacement once overflowed,
	// and nothing once taken
	#[test]
	fn size_counts_pending_ops_after_folding() {
		let mut queue = PendingOps::new();
		assert_eq!(queue.size(), 0);
		queue.push(&Arc::from([]));
		assert_eq!(queue.size(), 0);
		queue.push(&batch(json!([["a", ["text"], "a"], ["s", ["n"], 1]])));
		assert_eq!(queue.size(), 2);
		queue.push(&batch(json!([
			["a", ["text"], "b"],
			["s", ["n"], 2],
			["p", ["list"], 0, 0, [1]]
		])));
		assert_eq!(queue.size(), 3);
		queue.take();
		assert_eq!(queue.size(), 0);

		queue.push(&distinct_sets(MAX_PENDING_OPS, 0));
		assert_eq!(queue.size(), MAX_PENDING_OPS);
		queue.push(&batch(json!([["s", ["n"], 1]])));
		assert_eq!(queue.size(), 1);
		assert_eq!(queue.take(), PendingTake::Replace);
		assert_eq!(queue.size(), 0);
	}

	// TS: delta-pending.test.ts > PendingOps cost > keeps one op per leaf path however many commits write it
	#[test]
	fn keeps_one_op_per_leaf_path_however_many_commits_write_it() {
		let mut queue = PendingOps::new();
		for index in 0..50_000 {
			queue.push(&batch(json!([
				["a", ["entries", 0, "text"], "tok "],
				["s", ["meta", "tokens"], index]
			])));
		}
		let expected = json!([
			["a", ["entries", 0, "text"], "tok ".repeat(50_000)],
			["s", ["meta", "tokens"], 49_999]
		]);
		assert_eq!(taken_ops(queue.take()), ops(expected));
	}

	/// A hasher that counts the hashes it starts: one per path key computed.
	#[derive(Clone, Default)]
	struct CountingHasher(Arc<AtomicUsize>);

	impl BuildHasher for CountingHasher {
		type Hasher = DefaultHasher;

		fn build_hasher(&self) -> DefaultHasher {
			self.0.fetch_add(1, Ordering::Relaxed);
			DefaultHasher::new()
		}
	}

	// TS: delta-pending.test.ts > PendingOps cost > computes one path key per leaf op once folding, none for one batch
	// or structural ops
	#[test]
	fn computes_one_path_key_per_leaf_op_once_folding() {
		let hashes = CountingHasher::default();
		let mut queue = PendingOps::with_hasher(hashes.clone());
		let leaves = batch(json!([
			["a", ["entries", 0, "text"], "a"],
			["s", ["meta", "tokens"], 1]
		]));
		queue.push(&leaves);
		queue.take();
		assert_eq!(hashes.0.load(Ordering::Relaxed), 0);

		queue.push(&leaves);
		assert_eq!(hashes.0.load(Ordering::Relaxed), 0);
		queue.push(&batch(json!([
			["a", ["entries", 0, "text"], "a"], ["s", ["meta", "tokens"], 1], ["p", ["entries"], 1, 0, [{"text": ""}]],
			["s", ["meta"], {"tokens": 0}], ["d", ["x"]],
		])));
		assert_eq!(hashes.0.load(Ordering::Relaxed), 4);
	}

	// Rust-only: the D0a review measured joined appends quadratic (10k appends 0.82 s); a fold appends in place.
	#[test]
	fn folding_appends_is_linear_in_the_text() {
		let mut queue = PendingOps::new();
		let chunk = batch(json!([["a", ["text"], "x".repeat(100)]]));
		let started = Instant::now();
		for _ in 0..20_000 {
			queue.push(&chunk);
		}
		let taken = taken_ops(queue.take());
		// Generous for a debug build on a loaded runner: copying the joined text per merge moves about 20 GB here.
		assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
		let [Op::Append(_, text)] = taken.as_slice() else {
			panic!("one append")
		};
		assert_eq!(text.len(), 2_000_000);
	}

	// TS: delta-pending.test.ts > PendingOps references > releases a folded-over op, and the index and everything
	// pending once taken (received batches and handed-over ops stand in for the WeakRef'd objects)
	#[test]
	fn releases_a_folded_over_op_and_everything_pending_once_taken() {
		let mut queue = PendingOps::new();
		let first = batch(json!([["s", ["n"], 1]]));
		let second = batch(json!([["s", ["n"], 2], ["s", ["o"], {"big": "x".repeat(1000)}]]));
		let (first_weak, second_weak) = (Arc::downgrade(&first), Arc::downgrade(&second));
		queue.push(&first);
		queue.push(&second);
		drop((first, second));
		// Folding copied what it keeps: neither received batch stays referenced.
		assert!(first_weak.upgrade().is_none());
		assert!(second_weak.upgrade().is_none());
		let PendingTake::Ops(taken) = queue.take() else {
			panic!("ops")
		};
		let taken_weak = Arc::downgrade(&taken);
		drop(taken);
		assert!(taken_weak.upgrade().is_none());

		// Taken while the folding state holds the index: nothing stays behind in the queue.
		queue.push(&batch(json!([["s", ["n"], 1]])));
		queue.push(&batch(json!([["s", ["n"], 2]])));
		queue.push(&batch(json!([["p", ["list"], 0, 0, [1]]])));
		let PendingTake::Ops(taken) = queue.take() else {
			panic!("ops")
		};
		let taken_weak = Arc::downgrade(&taken);
		drop(taken);
		assert!(taken_weak.upgrade().is_none());
		assert_eq!(queue.size(), 0);
	}
}
