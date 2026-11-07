//! Revision diffs (TS `delta/diff.ts`, `diffRevisions`): the op batch that turns one immutable revision into another,
//! and the same engine with a draft's hints for the tracker (`delta/tracker.rs`).
//!
//! Identity is the TS notion: two containers are the same when they are the same `Arc` (a revision shares every
//! subtree it did not change with the one it came from), two primitives when JS `===` holds (numbers compare as
//! doubles, strings by content). The heuristics that decide op minimality are the TS ones: prefix append and `overlap`
//! for strings; common prefix and suffix, positional matches, identity subsequences, identity anchors (a longest
//! increasing subsequence) and a bounded semantic LCS for arrays; a permutation for a pure reorder.
//!
//! `diff_revisions` ignores key order, as TS does: its callers recompute values, whose key order may change on every
//! run, and replaying it would cost ops for no contractual gain. The tracker's diff (`diff_draft`) adds what the TS
//! tracker's overlay knows, through the draft's hints (`DraftHints`): appends recorded by `append_text`; the base
//! position each element of a structurally edited array came from (aligned on them, an element edited in place diffs
//! in place, and a reorder is a permutation plus the edits); arrays never edited structurally (dense element edits fold
//! into one region splice, as TS `buildDenseRegions` does); and, as the TS tracker emits for a key it deleted and set
//! again, `d` and `s` of keys whose order moved, so the replica keeps the draft's key order. A hint only chooses
//! between correct batches.

use crate::delta::op::{NonEmptyPath, Op, Path, Seg, overlap};
use crate::delta::snapshot::{Snapshot, SnapshotObject};
use crate::delta::validate::RESERVED_SEGMENTS;
use pi_json::{JsonValue, js_number_to_string};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// What a draft knows about how its tree changed; read by the tracker's diff.
#[derive(Debug, Default)]
pub(crate) struct DraftHints {
	/// Appends recorded by `append_text`, keyed by the string they produced (`string_key`).
	pub(crate) appends: HashMap<usize, AppendHint>,
	/// For each array edited structurally (insertions, removals, reorders), by path: the base position each element
	/// came from, `None` for an inserted one. Such arrays never fold element edits into dense regions.
	pub(crate) origins: HashMap<Path, Vec<Option<usize>>>,
	/// Objects placed whole by `set`: deeply equal to what they replace, they keep its key order and cost no op, while
	/// an object edited in place replays a key it deleted and set again (TS `readded`).
	pub(crate) assigned: HashSet<Path>,
}

/// Text appended to a string: `result` is `origin` followed by `text`, which the diff trusts only when both pointers
/// match the strings it compares, so a hint that no longer describes its path costs a comparison, never correctness.
#[derive(Debug)]
pub(crate) struct AppendHint {
	pub(crate) origin: Arc<str>,
	pub(crate) result: Arc<str>,
	pub(crate) text: String,
}

/// The key of an append hint: the address of the string it produced, which the hint keeps alive.
pub(crate) fn string_key(text: &Arc<str>) -> usize {
	Arc::as_ptr(text).cast::<u8>() as usize
}

/// A draft's diff: the ops, and whether a deeply equal object was kept in the base's key order although the draft's
/// differs, in which case the prepared value must be the replay, not the draft.
pub(crate) struct DraftDiff {
	pub(crate) ops: Vec<Op>,
	pub(crate) kept_base_order: bool,
}

/// The ops that turn `before` into `after`; `[["r", after]]` past 4,096 ops, or when the ops would cost more bytes
/// than `after` itself once they pass 64 KiB.
pub fn diff_revisions(before: &Snapshot, after: &Snapshot) -> Vec<Op> {
	let mut diff = Diff::new(None);
	diff.value(before, after);
	if diff.overflowed {
		return vec![Op::Replace(after.to_value())];
	}
	let ops = diff.ops;
	if ops.is_empty() || matches!(ops[0], Op::Replace(_)) {
		return ops;
	}
	let delta_cost = 2 + ops.iter().map(|op| operation_cost(op) + 1).sum::<usize>();
	if delta_cost < DELTA_COST_FLOOR {
		return ops;
	}
	let replacement = after.to_value();
	if delta_cost >= json_cost(&replacement) + 6 {
		vec![Op::Replace(replacement)]
	} else {
		ops
	}
}

/// Below this many bytes a batch is kept however large `after` is (TS parity).
const DELTA_COST_FLOOR: usize = 65_536;

/// The tracker's diff of a draft against its base: `diff_revisions` with the draft's hints and without the byte-cost
/// fallback (the TS tracker replaces the root only past the op count).
pub(crate) fn diff_draft(before: &Snapshot, after: &Snapshot, hints: &DraftHints) -> DraftDiff {
	let mut diff = Diff::new(Some(hints));
	diff.value(before, after);
	if diff.overflowed {
		return DraftDiff {
			ops: vec![Op::Replace(after.to_value())],
			kept_base_order: false,
		};
	}
	DraftDiff {
		ops: diff.ops,
		kept_base_order: diff.kept_base_order,
	}
}

/// Past this many ops a batch becomes `[["r", after]]` (TS `MAX_DELTA_OPERATIONS`).
const MAX_DELTA_OPERATIONS: usize = 4_096;

/// How far `overlap` looks back into the old string for a rolling window (TS `DEFAULT_OVERLAP_SCAN`).
const OVERLAP_SCAN: usize = 65_536;

/// One diff in progress: the ops so far and the path of the values being compared.
struct Diff<'h> {
	ops: Vec<Op>,
	overflowed: bool,
	hints: Option<&'h DraftHints>,
	path: Path,
	kept_base_order: bool,
}

impl<'h> Diff<'h> {
	fn new(hints: Option<&'h DraftHints>) -> Self {
		Self {
			ops: Vec::new(),
			overflowed: false,
			hints,
			path: Vec::new(),
			kept_base_order: false,
		}
	}

	fn emit(&mut self, op: Op) {
		if self.overflowed {
			return;
		}
		if self.ops.len() >= MAX_DELTA_OPERATIONS {
			self.overflowed = true;
			return;
		}
		self.ops.push(op);
	}

	/// The current path, which the callers below only use once a segment was pushed.
	fn leaf_path(&self) -> NonEmptyPath {
		// invariant: called only below a pushed segment (object members, array elements, non-root strings).
		NonEmptyPath::new(self.path.clone()).unwrap_or_else(|| unreachable!("leaf path below the root"))
	}

	/// `s` of `value` at the current path, or `r` at the root.
	fn set(&mut self, value: &Snapshot) {
		let op = match NonEmptyPath::new(self.path.clone()) {
			Some(path) => Op::Set(path, value.to_value()),
			None => Op::Replace(value.to_value()),
		};
		self.emit(op);
	}

	/// Runs `compare` with `segment` pushed onto the path.
	fn below(&mut self, segment: Seg, compare: impl FnOnce(&mut Self)) {
		self.path.push(segment);
		compare(self);
		self.path.pop();
	}

	fn value(&mut self, before: &Snapshot, after: &Snapshot) {
		if self.overflowed || identical(before, after) {
			return;
		}
		match (before, after) {
			(Snapshot::String(old), Snapshot::String(new)) if !self.path.is_empty() => self.string(old, new),
			(Snapshot::Array(old), Snapshot::Array(new)) => self.array(old, new),
			(Snapshot::Object(old), Snapshot::Object(new)) => self.object(old, new),
			_ => self.set(after),
		}
	}

	/// An append, a front cut and an append (a rolling window), or a set. O(length): it reads both strings, unless the
	/// draft recorded the append.
	fn string(&mut self, before: &Arc<str>, after: &Arc<str>) {
		if before == after {
			return;
		}
		let hint = self.hints.and_then(|hints| hints.appends.get(&string_key(after)));
		if let Some(hint) = hint.filter(|hint| Arc::ptr_eq(&hint.origin, before) && Arc::ptr_eq(&hint.result, after)) {
			let text = hint.text.clone();
			self.emit(Op::Append(self.leaf_path(), text));
			return;
		}
		if after.len() > before.len() && after.starts_with(&**before) {
			self.emit(Op::Append(self.leaf_path(), after[before.len()..].to_owned()));
			return;
		}
		let shared = overlap(before, after, OVERLAP_SCAN, 64, 8);
		if shared == 0 {
			self.emit(Op::Set(self.leaf_path(), JsonValue::String(after.to_string())));
			return;
		}
		// `overlap` counts UTF-16 code units, as the `t` count does; the appended rest starts at the byte where the
		// shared units end, which is a character boundary of `after` (a shared head cannot end inside a pair).
		let units = before.encode_utf16().count();
		self.emit(Op::Truncate(self.leaf_path(), (units - shared) as u64));
		let offset = utf16_offset(after, shared);
		if offset < after.len() {
			self.emit(Op::Append(self.leaf_path(), after[offset..].to_owned()));
		}
	}

	fn array(&mut self, before: &Arc<Vec<Snapshot>>, after: &Arc<Vec<Snapshot>>) {
		if Arc::ptr_eq(before, after) || before == after {
			return;
		}
		if let Some(hints) = self.hints {
			let handled = match hints.origins.get(&self.path) {
				Some(origins) => self.aligned(before, after, origins),
				None => before.len() == after.len() && self.dense(before, after),
			};
			if handled {
				return;
			}
		}
		let (first, last) = (before.len().saturating_sub(1), after.len().saturating_sub(1));
		if before.len() == after.len()
			&& before.len() > 1
			&& !same_value(&before[0], &after[0])
			&& !same_value(&before[first], &after[last])
			&& let Some(order) = permutation(before, after)
		{
			self.emit(Op::Move(self.path.clone(), order));
			return;
		}
		self.array_region(before, after, Region::whole(before, after));
	}

	/// Diffs an array the draft edited structurally along its element origins: a reorder of every base element is a
	/// permutation followed by the edits of the elements, anything else the longest run of elements in base order
	/// matched to their base positions, the rest spliced. False when the origins align nothing (a stale hint).
	fn aligned(&mut self, before: &[Snapshot], after: &[Snapshot], origins: &[Option<usize>]) -> bool {
		if origins.len() != after.len() {
			return false;
		}
		if let Some(order) = full_permutation(origins, before.len()) {
			if order.iter().enumerate().any(|(index, &from)| from != index as u64) {
				self.emit(Op::Move(self.path.clone(), order.clone()));
			}
			for (index, &from) in order.iter().enumerate() {
				// invariant: `full_permutation` checked every origin is below `before.len()`.
				let old = &before[usize::try_from(from).unwrap_or_default()];
				if !identical(old, &after[index]) {
					self.below(Seg::Index(index as u64), |diff| diff.value(old, &after[index]));
				}
			}
			return true;
		}
		let matches = increasing_origins(origins, before.len());
		if matches.is_empty() {
			return false;
		}
		self.process_matches(before, after, &Region::whole(before, after), &matches);
		true
	}

	/// Folds element edits of an array the draft never edited structurally into one splice per dense region (TS
	/// `buildDenseRegions`), and diffs the elements outside them; false when no region is dense enough.
	fn dense(&mut self, before: &[Snapshot], after: &[Snapshot]) -> bool {
		let changed: Vec<bool> = before
			.iter()
			.zip(after)
			.map(|(old, new)| !identical(old, new))
			.collect();
		let regions = dense_regions(&changed);
		if regions.is_empty() {
			return false;
		}
		let mut index = 0;
		let mut regions = regions.into_iter().peekable();
		while index < after.len() && !self.overflowed {
			if let Some(&(start, length)) = regions.peek()
				&& start == index
			{
				let items = after[start..start + length].iter().map(Snapshot::to_value).collect();
				self.emit(Op::Splice(self.path.clone(), start as u64, length as u64, items));
				regions.next();
				index += length;
				continue;
			}
			if changed[index] {
				self.below(Seg::Index(index as u64), |diff| {
					diff.value(&before[index], &after[index]);
				});
			}
			index += 1;
		}
		true
	}

	/// Diffs `before[region.before]` into `after[region.after]`, which start at `region.output` in the array as the ops
	/// before this region left it (TS `diffArrayRegion`).
	fn array_region(&mut self, before: &[Snapshot], after: &[Snapshot], region: Region) {
		if self.overflowed {
			return;
		}
		let Region {
			before_start: mut bs,
			before_end: mut be,
			after_start: mut as_,
			after_end: mut ae,
			output: mut out,
		} = region;
		while bs < be && as_ < ae && same_value(&before[bs], &after[as_]) {
			bs += 1;
			as_ += 1;
			out += 1;
		}
		while bs < be && as_ < ae && same_value(&before[be - 1], &after[ae - 1]) {
			be -= 1;
			ae -= 1;
		}
		let trimmed = Region {
			before_start: bs,
			before_end: be,
			after_start: as_,
			after_end: ae,
			output: out,
		};
		let (before_count, after_count) = (be - bs, ae - as_);
		if before_count == 0 && after_count == 0 {
			return;
		}
		if before_count == 0 || after_count == 0 {
			self.splice(out, before_count, &after[as_..ae]);
			return;
		}
		if let Some(matches) = Self::region_matches(before, after, &trimmed) {
			self.process_matches(before, after, &trimmed, &matches);
			return;
		}
		if before_count == 1 && after_count == 1 {
			self.below(Seg::Index(out as u64), |diff| diff.value(&before[bs], &after[as_]));
			return;
		}
		self.splice(out, before_count, &after[as_..ae]);
	}

	/// The first non-empty alignment of a trimmed region, tried in the TS order: positional matches of an equal-count
	/// region, an identity subsequence, identity anchors, then a semantic LCS of bounded size.
	fn region_matches(before: &[Snapshot], after: &[Snapshot], region: &Region) -> Option<Vec<Match>> {
		let (bs, be, as_, ae) = (
			region.before_start,
			region.before_end,
			region.after_start,
			region.after_end,
		);
		if be - bs == ae - as_ {
			let positional: Vec<Match> = (0..be - bs)
				.filter(|offset| same_value(&before[bs + offset], &after[as_ + offset]))
				.map(|offset| (bs + offset, as_ + offset))
				.collect();
			if !positional.is_empty() {
				return Some(positional);
			}
		}
		if let Some(subsequence) = identity_subsequence(before, after, region)
			&& !subsequence.is_empty()
		{
			return Some(subsequence);
		}
		let anchors = identity_anchors(before, after, region);
		if !anchors.is_empty() {
			return Some(anchors);
		}
		let semantic = lcs_matches(
			&before[bs..be],
			&after[as_..ae],
			semantically_aligned,
			MAX_SEMANTIC_CELLS,
		)?;
		if semantic.is_empty() {
			return None;
		}
		Some(semantic.into_iter().map(|(old, new)| (bs + old, as_ + new)).collect())
	}

	/// Diffs the gaps between matched pairs and each matched pair that is not the same value (TS
	/// `processArrayMatches`).
	fn process_matches(&mut self, before: &[Snapshot], after: &[Snapshot], region: &Region, matches: &[Match]) {
		let (mut before_at, mut after_at, mut output_at) = (region.before_start, region.after_start, region.output);
		for &(before_match, after_match) in matches {
			if self.overflowed {
				return;
			}
			let gap = Region {
				before_start: before_at,
				before_end: before_match,
				after_start: after_at,
				after_end: after_match,
				output: output_at,
			};
			self.array_region(before, after, gap);
			output_at += after_match - after_at;
			if !same_value(&before[before_match], &after[after_match]) {
				self.below(Seg::Index(output_at as u64), |diff| {
					diff.value(&before[before_match], &after[after_match]);
				});
			}
			output_at += 1;
			before_at = before_match + 1;
			after_at = after_match + 1;
		}
		if !self.overflowed {
			let rest = Region {
				before_start: before_at,
				before_end: region.before_end,
				after_start: after_at,
				after_end: region.after_end,
				output: output_at,
			};
			self.array_region(before, after, rest);
		}
	}

	fn splice(&mut self, index: usize, remove: usize, items: &[Snapshot]) {
		let items = items.iter().map(Snapshot::to_value).collect();
		self.emit(Op::Splice(self.path.clone(), index as u64, remove as u64, items));
	}

	fn object(&mut self, before: &Arc<SnapshotObject>, after: &Arc<SnapshotObject>) {
		if Arc::ptr_eq(before, after) {
			return;
		}
		let reserved = |object: &SnapshotObject| object.iter().any(|(key, _)| RESERVED_SEGMENTS.contains(&key));
		// TS parity: no path may name a reserved key, so an object holding one is set whole.
		if reserved(before) || reserved(after) {
			if before != after {
				self.set(&Snapshot::Object(after.clone()));
			}
			return;
		}
		// `diff_revisions` ignores key order (TS parity); only the tracker replays it.
		let moved_from = if self.hints.is_some() {
			moved_from(before, after)
		} else {
			after.len()
		};
		let moves_kept_keys = after
			.iter()
			.skip(moved_from)
			.any(|(key, _)| !is_index_key(key) && before.position(key).is_some());
		// Key order is not part of equality: a deeply equal object placed whole keeps the base's order and costs no op.
		let placed = self.hints.is_some_and(|hints| hints.assigned.contains(&self.path));
		if moves_kept_keys && placed && before == after {
			self.kept_base_order = true;
			return;
		}
		for (position, (key, value)) in after.iter().enumerate() {
			if self.overflowed {
				return;
			}
			let moved = position >= moved_from && !is_index_key(key);
			self.below(Seg::Key(key.to_owned()), |diff| match before.get(key) {
				Some(_) if moved => {
					diff.emit(Op::Delete(diff.leaf_path()));
					diff.set(value);
				}
				Some(old) => diff.value(old, value),
				None => diff.set(value),
			});
		}
		for (key, _) in before.iter() {
			if self.overflowed {
				return;
			}
			if after.get(key).is_none() {
				self.below(Seg::Key(key.to_owned()), |diff| diff.emit(Op::Delete(diff.leaf_path())));
			}
		}
	}
}

/// A pair of aligned indices: `(before, after)`.
type Match = (usize, usize);

/// Part of an array diff: `before[before_start..before_end]` becomes `after[after_start..after_end]`, at `output` in
/// the array the ops before it produced.
#[derive(Clone, Copy)]
struct Region {
	before_start: usize,
	before_end: usize,
	after_start: usize,
	after_end: usize,
	output: usize,
}

impl Region {
	fn whole(before: &[Snapshot], after: &[Snapshot]) -> Self {
		Self {
			before_start: 0,
			before_end: before.len(),
			after_start: 0,
			after_end: after.len(),
			output: 0,
		}
	}
}

/// JS `===` on revision values: the same container, or equal primitives (numbers as doubles, strings by content).
fn identical(left: &Snapshot, right: &Snapshot) -> bool {
	match (left, right) {
		(Snapshot::Null, Snapshot::Null) => true,
		(Snapshot::Bool(a), Snapshot::Bool(b)) => a == b,
		(Snapshot::Number(a), Snapshot::Number(b)) => a.as_f64() == b.as_f64(),
		(Snapshot::String(a), Snapshot::String(b)) => a == b,
		_ => left.ptr_eq(right),
	}
}

/// TS `sameValue`: identical, or deeply equal.
fn same_value(left: &Snapshot, right: &Snapshot) -> bool {
	identical(left, right) || left == right
}

/// What a JS `Map` keys a revision value by: containers by reference, primitives by value.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Identity<'a> {
	Container(usize),
	Null,
	Bool(bool),
	Number(u64),
	Text(&'a str),
}

fn identity(value: &Snapshot) -> Identity<'_> {
	match value {
		Snapshot::Null => Identity::Null,
		Snapshot::Bool(flag) => Identity::Bool(*flag),
		// `-0` and `0` are one key, as in a JS `Map` (SameValueZero).
		Snapshot::Number(number) => Identity::Number((number.as_f64().unwrap_or(0.0) + 0.0).to_bits()),
		Snapshot::String(text) => Identity::Text(text),
		Snapshot::Array(items) => Identity::Container(Arc::as_ptr(items) as usize),
		Snapshot::Object(object) => Identity::Container(Arc::as_ptr(object) as usize),
	}
}

/// `new[i] = old[permutation[i]]` when `after` holds exactly the elements of `before` (by identity), reordered.
fn permutation(before: &[Snapshot], after: &[Snapshot]) -> Option<Vec<u64>> {
	if before.len() != after.len() {
		return None;
	}
	let mut positions: HashMap<Identity<'_>, (Vec<usize>, usize)> = HashMap::new();
	for (index, value) in before.iter().enumerate() {
		positions.entry(identity(value)).or_default().0.push(index);
	}
	after
		.iter()
		.map(|value| {
			let (indices, used) = positions.get_mut(&identity(value))?;
			let index = *indices.get(*used)?;
			*used += 1;
			Some(index as u64)
		})
		.collect()
}

/// The origins as a permutation of `0..length`, when every element came from a distinct base position and none is
/// missing.
fn full_permutation(origins: &[Option<usize>], length: usize) -> Option<Vec<u64>> {
	if origins.len() != length {
		return None;
	}
	let mut seen = vec![false; length];
	origins
		.iter()
		.map(|origin| {
			let from = (*origin)?;
			let slot = seen.get_mut(from)?;
			if *slot {
				return None;
			}
			*slot = true;
			Some(from as u64)
		})
		.collect()
}

/// The longest run of elements whose origins increase, as `(base position, index)` pairs: the elements that kept
/// their base order, which the diff matches in place.
fn increasing_origins(origins: &[Option<usize>], length: usize) -> Vec<Match> {
	let candidates: Vec<Match> = origins
		.iter()
		.enumerate()
		.filter_map(|(index, origin)| origin.filter(|&from| from < length).map(|from| (from, index)))
		.collect();
	// Patience sorting on the base positions, each candidate linked to the one it extends.
	let mut tails: Vec<usize> = Vec::new();
	let mut previous: Vec<Option<usize>> = vec![None; candidates.len()];
	for (at, &(from, _)) in candidates.iter().enumerate() {
		let slot = tails.partition_point(|&tail| candidates[tail].0 < from);
		previous[at] = slot.checked_sub(1).map(|slot| tails[slot]);
		if slot == tails.len() {
			tails.push(at);
		} else {
			tails[slot] = at;
		}
	}
	let mut matches = Vec::new();
	let mut next = tails.last().copied();
	while let Some(at) = next {
		matches.push(candidates[at]);
		next = previous[at];
	}
	matches.reverse();
	matches
}

/// Elements of a dense region at least, and at least half of the region's elements (TS `buildDenseRegions`).
const DENSE_REGION_MIN: usize = 256;

/// Runs of changed elements with gaps of at most one, holding at least `DENSE_REGION_MIN` changes that are at least
/// half of the run, as `(start, length)`.
fn dense_regions(changed: &[bool]) -> Vec<(usize, usize)> {
	if changed.iter().filter(|&&flag| flag).count() < DENSE_REGION_MIN {
		return Vec::new();
	}
	let mut regions = Vec::new();
	let mut at = 0;
	while at < changed.len() {
		while at < changed.len() && !changed[at] {
			at += 1;
		}
		if at == changed.len() {
			break;
		}
		let (start, mut end, mut count, mut gap) = (at, at, 0, 0);
		while at < changed.len() {
			if changed[at] {
				count += 1;
				end = at;
				gap = 0;
			} else {
				gap += 1;
				if gap > 1 {
					break;
				}
			}
			at += 1;
		}
		let length = end - start + 1;
		if count >= DENSE_REGION_MIN && count * 2 >= length {
			regions.push((start, length));
		}
	}
	regions
}

/// Matches of the shorter side in order inside the longer one, by identity; `None` when the counts are equal or one
/// element of the shorter side is missing (TS `identitySubsequence`).
fn identity_subsequence(before: &[Snapshot], after: &[Snapshot], region: &Region) -> Option<Vec<Match>> {
	let (bs, be, as_, ae) = (
		region.before_start,
		region.before_end,
		region.after_start,
		region.after_end,
	);
	let mut matches = Vec::new();
	if ae - as_ < be - bs {
		let mut before_index = bs;
		for (after_index, new) in after.iter().enumerate().take(ae).skip(as_) {
			while before_index < be && !identical(&before[before_index], new) {
				before_index += 1;
			}
			if before_index == be {
				return None;
			}
			matches.push((before_index, after_index));
			before_index += 1;
		}
		return Some(matches);
	}
	if be - bs < ae - as_ {
		let mut after_index = as_;
		for (before_index, old) in before.iter().enumerate().take(be).skip(bs) {
			while after_index < ae && !identical(old, &after[after_index]) {
				after_index += 1;
			}
			if after_index == ae {
				return None;
			}
			matches.push((before_index, after_index));
			after_index += 1;
		}
		return Some(matches);
	}
	None
}

/// Candidate pairs past which identity anchors fall back to a greedy scan (TS `MAX_IDENTITY_CANDIDATES`).
const MAX_IDENTITY_CANDIDATES: usize = 200_000;

/// The longest increasing run of identity matches of a region (TS `identityAnchors`).
fn identity_anchors(before: &[Snapshot], after: &[Snapshot], region: &Region) -> Vec<Match> {
	let mut positions: HashMap<Identity<'_>, Vec<usize>> = HashMap::new();
	for (index, value) in before
		.iter()
		.enumerate()
		.take(region.before_end)
		.skip(region.before_start)
	{
		positions.entry(identity(value)).or_default().push(index);
	}
	let after_range = region.after_start..region.after_end;
	let mut candidate_count = 0;
	for value in &after[after_range.clone()] {
		candidate_count += positions.get(&identity(value)).map_or(0, Vec::len);
		if candidate_count > MAX_IDENTITY_CANDIDATES {
			return greedy_identity_anchors(&positions, after, region);
		}
	}
	if candidate_count == 0 {
		return Vec::new();
	}
	// Patience sorting over the before positions, each candidate linked to the tail it extends.
	let mut candidates: Vec<(Match, Option<usize>)> = Vec::new();
	let mut tails: Vec<usize> = Vec::new();
	let mut tail_values: Vec<usize> = Vec::new();
	for after_index in after_range {
		let Some(before_positions) = positions.get(&identity(&after[after_index])) else {
			continue;
		};
		for &before_index in before_positions.iter().rev() {
			let at = tail_values.partition_point(|&value| value < before_index);
			let previous = at.checked_sub(1).map(|at| tails[at]);
			candidates.push(((before_index, after_index), previous));
			let candidate = candidates.len() - 1;
			if at == tails.len() {
				tails.push(candidate);
				tail_values.push(before_index);
			} else {
				tails[at] = candidate;
				tail_values[at] = before_index;
			}
		}
	}
	let mut matches = Vec::new();
	let mut next = tails.last().copied();
	while let Some(candidate) = next {
		let (pair, previous) = candidates[candidate];
		matches.push(pair);
		next = previous;
	}
	matches.reverse();
	matches
}

/// Each after element matched to the first unused later before position (TS `greedyIdentityAnchors`).
fn greedy_identity_anchors(
	positions: &HashMap<Identity<'_>, Vec<usize>>,
	after: &[Snapshot],
	region: &Region,
) -> Vec<Match> {
	let mut matches = Vec::new();
	let mut next_before = 0;
	for (after_index, value) in after.iter().enumerate().take(region.after_end).skip(region.after_start) {
		let Some(candidates) = positions.get(&identity(value)) else {
			continue;
		};
		let at = candidates.partition_point(|&value| value < next_before);
		if let Some(&before_index) = candidates.get(at) {
			matches.push((before_index, after_index));
			next_before = before_index + 1;
		}
	}
	matches
}

/// Cells of the semantic LCS table past which it is not computed (TS `MAX_SEMANTIC_CELLS`).
const MAX_SEMANTIC_CELLS: usize = 65_536;

/// The longest common subsequence of `before` and `after` under `equal`, as index pairs; `None` past `max_cells`.
fn lcs_matches(
	before: &[Snapshot],
	after: &[Snapshot],
	equal: fn(&Snapshot, &Snapshot) -> bool,
	max_cells: usize,
) -> Option<Vec<Match>> {
	if before.is_empty() || after.is_empty() {
		return Some(Vec::new());
	}
	if before.len() * after.len() > max_cells {
		return None;
	}
	let width = after.len() + 1;
	let mut lengths = vec![0_u32; (before.len() + 1) * width];
	for left in (0..before.len()).rev() {
		for right in (0..after.len()).rev() {
			lengths[left * width + right] = if equal(&before[left], &after[right]) {
				lengths[(left + 1) * width + right + 1] + 1
			} else {
				lengths[(left + 1) * width + right].max(lengths[left * width + right + 1])
			};
		}
	}
	let mut matches = Vec::new();
	let (mut left, mut right) = (0, 0);
	while left < before.len() && right < after.len() {
		if equal(&before[left], &after[right])
			&& lengths[left * width + right] == lengths[(left + 1) * width + right + 1] + 1
		{
			matches.push((left, right));
			left += 1;
			right += 1;
		} else if lengths[(left + 1) * width + right] >= lengths[left * width + right + 1] {
			left += 1;
		} else {
			right += 1;
		}
	}
	Some(matches)
}

/// Two values that are the same, or containers of one kind sharing a child container at the same key or index (TS
/// `semanticallyAligned`).
fn semantically_aligned(left: &Snapshot, right: &Snapshot) -> bool {
	if same_value(left, right) {
		return true;
	}
	let is_container = |value: &Snapshot| matches!(value, Snapshot::Array(_) | Snapshot::Object(_));
	match (left, right) {
		(Snapshot::Array(left), Snapshot::Array(right)) => {
			left.len() == right.len()
				&& left
					.iter()
					.zip(right.iter())
					.any(|(old, new)| is_container(old) && old.ptr_eq(new))
		}
		(Snapshot::Object(left), Snapshot::Object(right)) => left
			.iter()
			.any(|(key, old)| is_container(old) && right.get(key).is_some_and(|new| old.ptr_eq(new))),
		_ => false,
	}
}

/// Where the order of `after`'s keys stops following `before`: the first non-index key that is new or that comes
/// earlier in `before` than a key before it. Keys from there on are appended by the replay in `after`'s order, which
/// is where `s` puts a new key and where `d` then `s` moves an existing one.
fn moved_from(before: &SnapshotObject, after: &SnapshotObject) -> usize {
	let mut last = None;
	for (position, (key, _)) in after.iter().enumerate() {
		if is_index_key(key) {
			continue;
		}
		match before.position(key) {
			Some(at) if last.is_none_or(|last| at > last) => last = Some(at),
			_ => return position,
		}
	}
	after.len()
}

/// The largest array index is 2^32 - 2 (ECMA-262 6.1.7).
const MAX_ARRAY_INDEX: u64 = u32::MAX as u64 - 1;

/// A key JS orders before all others whatever its insertion order: a canonical array index.
fn is_index_key(key: &str) -> bool {
	let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
	canonical && key.len() <= 10 && key.parse::<u64>().is_ok_and(|index| index <= MAX_ARRAY_INDEX)
}

/// The byte offset in `text` after its first `units` UTF-16 code units (rounded up to a character boundary).
fn utf16_offset(text: &str, units: usize) -> usize {
	let mut counted = 0;
	for (offset, ch) in text.char_indices() {
		if counted >= units {
			return offset;
		}
		counted += ch.len_utf16();
	}
	text.len()
}

/// Bytes the JSON of `value` takes, counting strings in UTF-16 units as TS does (TS `jsonCost`).
fn json_cost(value: &JsonValue) -> usize {
	match value {
		JsonValue::Null | JsonValue::Bool(true) => 4,
		JsonValue::Bool(false) => 5,
		JsonValue::Number(number) => js_number_to_string(number.as_f64().unwrap_or(0.0)).len(),
		JsonValue::String(text) => text.encode_utf16().count() + 2,
		JsonValue::Array(items) => 2 + items.iter().map(json_cost).sum::<usize>() + items.len().saturating_sub(1),
		JsonValue::Object(members) => {
			let entries: usize = members
				.iter()
				.map(|(key, value)| key.encode_utf16().count() + 3 + json_cost(value))
				.sum();
			2 + entries + members.len().saturating_sub(1)
		}
	}
}

fn digits(number: u64) -> usize {
	number.to_string().len()
}

fn path_cost(path: &[Seg]) -> usize {
	let segments: usize = path
		.iter()
		.map(|segment| match segment {
			Seg::Key(key) => key.encode_utf16().count() + 2,
			Seg::Index(index) => digits(*index),
		})
		.sum();
	2 + segments + path.len().saturating_sub(1)
}

/// Bytes one op's tuple takes (TS `operationCost`).
fn operation_cost(op: &Op) -> usize {
	match op {
		Op::Replace(value) => 6 + json_cost(value),
		Op::Set(path, value) => 7 + path_cost(path) + json_cost(value),
		Op::Delete(path) => 6 + path_cost(path),
		Op::Append(path, text) => 7 + path_cost(path) + text.encode_utf16().count() + 2,
		Op::Truncate(path, count) => 7 + path_cost(path) + digits(*count),
		Op::Splice(path, index, remove, items) => {
			let items_cost = 2 + items.iter().map(json_cost).sum::<usize>() + items.len().saturating_sub(1);
			10 + path_cost(path) + digits(*index) + digits(*remove) + items_cost
		}
		Op::Move(path, permutation) => {
			let entries: usize = permutation.iter().map(|&index| digits(index)).sum();
			7 + path_cost(path) + 2 + entries + permutation.len().saturating_sub(1)
		}
	}
}
