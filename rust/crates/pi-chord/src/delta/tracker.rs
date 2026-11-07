//! The tracker (TS `delta/tracker.ts`, `track`, `appendText`): an immutable revision sequence edited through explicit
//! drafts, each prepared into the next revision and the ops that replay it.
//!
//! Decided rule (rust/docs/slices.md, D0b): the TS tracker is a `Proxy` over a piece-table overlay; Rust has no
//! proxies, and the README says op shape is not contractual ("Only the resulting value is contractual"). So a `Draft`
//! is an explicit API over a copy-on-write `Snapshot` (editing a path copies only the containers on it), and
//! `prepare` diffs the draft against its base with the draft's hints (`delta/diff.rs`). The ops must (1) replay to the
//! prepared value under the TS `apply` and `applyImmutable` (the cross-apply corpus checks it), (2) be valid ops, (3)
//! come from the same value the TS draft reaches for the same edits, and (4) be reasonably minimal: `append_text` gives
//! one `a`, a single `set` one `s`, a `push` one `p`, and only `prepare_replace` (or more than 4,096 ops, as in TS)
//! replaces the root. Batches are not byte-identical to the TS tracker's.
//!
//! The hints are what the TS overlay knows and a diff of two trees cannot: the text each `append_text` added (so the
//! op needs no string comparison), the base position each element of a structurally edited array came from (so an
//! element edited in place, an appended block of streamed text say, still diffs in place after the array was spliced,
//! sorted or reversed, as TS pieces do), and which objects were placed whole.
//!
//! Lifecycle, as in TS: several changes may be open or prepared from one revision; adopting one makes every other
//! stale and releases its trees at once. Drafts are values, so where TS revokes held proxies, Rust drafts refuse every
//! call once settled; Rust has no handles that follow an element through reindexing (paths name positions at the time
//! of the call).

use crate::delta::apply::{NodeMut, Tree, TreeObject};
use crate::delta::diff::{AppendHint, DraftHints, diff_draft, string_key};
use crate::delta::op::{Op, Path, Seg};
use crate::delta::snapshot::{Snapshot, SnapshotObject, apply_immutable_trusted};
use pi_json::{JsonValue, to_js_string};
use std::cmp::Ordering as CompareOrdering;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

/// One revision sequence: the latest adopted revision and the open changes adoption makes stale.
#[derive(Debug)]
pub struct Tracker {
	id: u64,
	value: Snapshot,
	revision: u64,
	/// Changes begun since the last adoption; dropped changes leave dead entries, pruned as they pile up.
	open: Vec<OpenChange>,
	prune_at: usize,
}

/// An open transaction on a tracker's revision: its draft, then the preparation.
#[derive(Debug)]
pub struct Change {
	draft: Draft,
	settled: bool,
	/// The status of the preparation this change made, which `abort` after `prepare` aborts.
	prepared: Option<Arc<StatusCell>>,
}

/// A prepared next revision: the base it was made from, the candidate value and the ops that turn one into the other.
#[derive(Clone, Debug)]
pub struct Prepared {
	tracker: u64,
	status: Arc<StatusCell>,
	base_revision: u64,
	base: Snapshot,
	value: Snapshot,
	ops: Arc<[Op]>,
}

/// The mutable view of one change: edits by path, refused once the change is settled.
#[derive(Debug)]
pub struct Draft {
	status: Arc<StatusCell>,
	tracker: u64,
	base_revision: u64,
	/// The base, the edited tree and the hints; `None` once the change is prepared, aborted or made stale. Shared with
	/// the tracker (weakly), so an adoption releases a competitor's trees at once, as TS `releaseOverlayReferences`.
	trees: Arc<Mutex<Option<Trees>>>,
}

/// Why a lifecycle call failed; messages are the TS ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrackerError {
	/// `prepare` on a change that was already prepared or aborted.
	#[error("Change has already been settled")]
	AlreadySettled,
	/// A change made stale by an adoption, or a draft used after its change settled.
	#[error("Cannot use a settled overlay")]
	Settled,
	/// `adopt` of a preparation another tracker made.
	#[error("Prepared change belongs to a different tracker")]
	ForeignTracker,
	/// `adopt` of a preparation already adopted.
	#[error("Prepared change has already been used")]
	Consumed,
	/// `adopt` of an aborted preparation.
	#[error("Prepared change has been aborted")]
	Aborted,
	/// `adopt` of a preparation made from an older revision, or made stale by another adoption.
	#[error("Prepared change is stale")]
	Stale,
}

/// Why a draft edit failed; the draft is unchanged. Messages are the TS ones where TS has the case.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DraftError {
	/// The change was prepared, aborted, or made stale by an adoption.
	#[error("Cannot use a settled overlay")]
	Settled,
	/// The path does not lead to a container that can hold the edit.
	#[error("unresolvable path: {}", path_text(.0))]
	Path(Path),
	/// `set` and `delete` need a key or index: the root is replaced with `Tracker::prepare_replace`.
	#[error("path is empty")]
	RootPath,
	/// An index past the end, or a deleted element: arrays stay dense.
	#[error("Overlay arrays cannot contain holes")]
	Holes,
	/// A key on an array (its length is set with `set_len`).
	#[error("Only array indices and length can be written")]
	ArrayKey,
	/// `None` placed in an array, where JS `undefined` is not JSON.
	#[error("Value contains a non-JSON undefined; expected strict JSON")]
	Undefined,
	/// `append_text` on a missing key, a value that is not a string, or an array element.
	#[error("appendText() needs a string at {0}")]
	NotAString(String),
	/// An array edit on a value that is not an array.
	#[error("Array mutator called on incompatible receiver")]
	NotAnArray,
}

/// What a draft edits: the revision it began from, the edited tree, and what it knows about the edits.
#[derive(Debug)]
struct Trees {
	base: Snapshot,
	root: Snapshot,
	hints: DraftHints,
}

/// The tracker's references to one open change.
#[derive(Debug)]
struct OpenChange {
	status: Weak<StatusCell>,
	trees: Weak<Mutex<Option<Trees>>>,
}

/// Tracker ids, so `adopt` can tell its own preparations from another tracker's.
static NEXT_TRACKER: AtomicU64 = AtomicU64::new(0);

/// Takes ownership of `initial` as revision 0, in O(1).
pub fn track(initial: Snapshot) -> Tracker {
	Tracker {
		id: NEXT_TRACKER.fetch_add(1, Ordering::Relaxed),
		value: initial,
		revision: 0,
		open: Vec::new(),
		prune_at: PRUNE_MIN,
	}
}

/// Dead entries a tracker keeps before pruning them (TS `#pruneBudget`).
const PRUNE_MIN: usize = 256;

impl Tracker {
	/// The latest adopted revision.
	pub fn value(&self) -> &Snapshot {
		&self.value
	}

	/// The number of adoptions so far.
	pub fn revision(&self) -> u64 {
		self.revision
	}

	/// Opens a change on the current revision; it does not touch the revision, and may stay open across `await`.
	pub fn begin_change(&mut self) -> Change {
		let status = Arc::new(StatusCell::new(Status::Open));
		let trees = Arc::new(Mutex::new(Some(Trees {
			base: self.value.clone(),
			root: self.value.clone(),
			hints: DraftHints::default(),
		})));
		if self.open.len() >= self.prune_at {
			self.open.retain(|change| change.status.strong_count() > 0);
			self.prune_at = PRUNE_MIN.max(self.open.len() * 2);
		}
		self.open.push(OpenChange {
			status: Arc::downgrade(&status),
			trees: Arc::downgrade(&trees),
		});
		Change {
			draft: Draft {
				status,
				tracker: self.id,
				base_revision: self.revision,
				trees,
			},
			settled: false,
			prepared: None,
		}
	}

	/// Prepares the whole root `value`: no ops and the current revision when it is deeply equal to it (the comparison
	/// may read both trees), else `[["r", value]]` with `value` itself as the candidate.
	pub fn prepare_replace(&self, value: Snapshot) -> Prepared {
		let (value, ops): (Snapshot, Arc<[Op]>) = if value == self.value {
			(self.value.clone(), Arc::from([]))
		} else {
			let ops = Arc::from([Op::Replace(value.to_value())]);
			(value, ops)
		};
		Prepared {
			tracker: self.id,
			status: Arc::new(StatusCell::new(Status::Prepared)),
			base_revision: self.revision,
			base: self.value.clone(),
			value,
			ops,
		}
	}

	/// Makes `prepared` the current revision (a pointer swap) and every other open or prepared change stale, releasing
	/// their trees.
	pub fn adopt(&mut self, prepared: &Prepared) -> Result<(), TrackerError> {
		if prepared.tracker != self.id {
			return Err(TrackerError::ForeignTracker);
		}
		match prepared.status.get() {
			Status::Consumed => return Err(TrackerError::Consumed),
			Status::Aborted => return Err(TrackerError::Aborted),
			Status::Stale => return Err(TrackerError::Stale),
			// A preparation is never open: it is made prepared.
			Status::Open | Status::Prepared => {}
		}
		if prepared.base_revision != self.revision {
			prepared.status.transition(&[Status::Prepared], Status::Stale);
			return Err(TrackerError::Stale);
		}
		if !prepared.status.transition(&[Status::Prepared], Status::Consumed) {
			// A concurrent abort won the cell between the check above and here.
			return Err(TrackerError::Aborted);
		}
		self.value = prepared.value.clone();
		self.revision += 1;
		for change in self.open.drain(..) {
			if let Some(status) = change.status.upgrade() {
				status.transition(&[Status::Open, Status::Prepared], Status::Stale);
			}
			if let Some(trees) = change.trees.upgrade() {
				lock(&trees).take();
			}
		}
		self.prune_at = PRUNE_MIN;
		Ok(())
	}
}

fn lock(trees: &Mutex<Option<Trees>>) -> MutexGuard<'_, Option<Trees>> {
	// Every update of the trees is complete or not started when a panic unwinds through it.
	trees.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Change {
	/// The change's draft; every call on it fails with `DraftError::Settled` once the change settled or went stale.
	pub fn state(&mut self) -> &mut Draft {
		&mut self.draft
	}

	/// Computes the candidate revision and its ops; the draft is unusable from here on, the tracker is unchanged.
	pub fn prepare(&mut self) -> Result<Prepared, TrackerError> {
		if self.settled {
			return Err(TrackerError::AlreadySettled);
		}
		let draft = &self.draft;
		let trees = {
			let mut slot = lock(&draft.trees);
			if slot.is_none() || !draft.status.transition(&[Status::Open], Status::Prepared) {
				return Err(TrackerError::Settled);
			}
			slot.take()
		};
		let Some(Trees { base, root, hints }) = trees else {
			return Err(TrackerError::Settled);
		};
		let diff = diff_draft(&base, &root, &hints);
		let value = if diff.ops.is_empty() {
			base.clone()
		} else if diff.kept_base_order {
			// invariant: the ops come from diffing `root` against `base`, so every path resolves in `base`.
			#[expect(
				clippy::expect_used,
				reason = "the diff's ops resolve in the revision they were diffed from"
			)]
			apply_immutable_trusted(&base, &diff.ops).expect("diff ops replay on their base")
		} else {
			root
		};
		self.settled = true;
		self.prepared = Some(draft.status.clone());
		Ok(Prepared {
			tracker: draft.tracker,
			status: draft.status.clone(),
			base_revision: draft.base_revision,
			base,
			value,
			ops: Arc::from(diff.ops),
		})
	}

	/// Abandons the change, or, after `prepare`, its preparation; idempotent.
	pub fn abort(&mut self) {
		if self.settled {
			if let Some(prepared) = self.prepared.take() {
				prepared.transition(&[Status::Prepared], Status::Aborted);
			}
			return;
		}
		self.settled = true;
		self.draft
			.status
			.transition(&[Status::Open, Status::Prepared], Status::Aborted);
		lock(&self.draft.trees).take();
	}
}

impl Prepared {
	/// The revision the change was made from.
	pub fn base(&self) -> &Snapshot {
		&self.base
	}

	/// The candidate revision; readable whatever becomes of the preparation.
	pub fn value(&self) -> &Snapshot {
		&self.value
	}

	/// The ops that turn `base` into `value`; empty when they are deeply equal.
	pub fn ops(&self) -> &Arc<[Op]> {
		&self.ops
	}

	/// The tracker revision `base` is.
	pub fn base_revision(&self) -> u64 {
		self.base_revision
	}

	/// Prevents adoption; idempotent.
	pub fn abort(&self) {
		self.status.transition(&[Status::Prepared], Status::Aborted);
	}
}

impl Draft {
	/// The node at `path` (the root for `[]`), shared with the draft (an O(1) clone); `None` when nothing is there.
	pub fn get(&self, path: &[Seg]) -> Result<Option<Snapshot>, DraftError> {
		self.edit(|trees| Ok(trees.root.at(path).cloned()))
	}

	/// Sets the member or element at `path`; `None` deletes a member (JS `= undefined`) and is refused in an array.
	/// Index `len` appends; a larger index is refused.
	pub fn set(&mut self, path: &[Seg], value: Option<Snapshot>) -> Result<(), DraftError> {
		self.edit(|trees| trees.set(path, value))
	}

	/// Deletes the member at `path`; a missing member is a no-op, an array element is refused (use `splice`).
	pub fn delete(&mut self, path: &[Seg]) -> Result<(), DraftError> {
		self.edit(|trees| trees.delete(path))
	}

	/// Appends `text` to the string member at `path` (TS `appendText`): the change records one `a` op per path however
	/// many appends it makes, without comparing the old and new strings. A plain `set` of the same path in the change
	/// falls back to that comparison, as does an append after one.
	pub fn append_text(&mut self, path: &[Seg], text: &str) -> Result<(), DraftError> {
		self.edit(|trees| trees.append_text(path, text))
	}

	/// Appends `items` to the array at `path`; returns its new length.
	pub fn push(&mut self, path: &[Seg], items: Vec<Snapshot>) -> Result<usize, DraftError> {
		self.edit(|trees| {
			let length = trees.array(path)?.len();
			trees.splice(path, length, 0, items)?;
			trees.array(path).map(<[Snapshot]>::len)
		})
	}

	/// Removes and returns the last element of the array at `path`.
	pub fn pop(&mut self, path: &[Seg]) -> Result<Option<Snapshot>, DraftError> {
		self.edit(|trees| {
			let Some(last) = trees.array(path)?.len().checked_sub(1) else {
				return Ok(None);
			};
			Ok(trees.splice(path, last, 1, Vec::new())?.pop())
		})
	}

	/// Removes and returns the first element of the array at `path`.
	pub fn shift(&mut self, path: &[Seg]) -> Result<Option<Snapshot>, DraftError> {
		self.edit(|trees| {
			if trees.array(path)?.is_empty() {
				return Ok(None);
			}
			Ok(trees.splice(path, 0, 1, Vec::new())?.pop())
		})
	}

	/// Inserts `items` at the front of the array at `path`; returns its new length.
	pub fn unshift(&mut self, path: &[Seg], items: Vec<Snapshot>) -> Result<usize, DraftError> {
		self.edit(|trees| {
			trees.splice(path, 0, 0, items)?;
			trees.array(path).map(<[Snapshot]>::len)
		})
	}

	/// JS `splice(start, remove, ...items)` on the array at `path`: a negative `start` counts from the end, both clamp
	/// to the array, and `None` removes to the end. Returns the removed elements.
	pub fn splice(
		&mut self,
		path: &[Seg],
		start: i64,
		remove: Option<usize>,
		items: Vec<Snapshot>,
	) -> Result<Vec<Snapshot>, DraftError> {
		self.edit(|trees| {
			let length = trees.array(path)?.len();
			let start = clamp_index(start, length);
			let remove = remove.map_or(length - start, |remove| remove.min(length - start));
			trees.splice(path, start, remove, items)
		})
	}

	/// Reverses the array at `path` in place.
	pub fn reverse(&mut self, path: &[Seg]) -> Result<(), DraftError> {
		self.edit(|trees| {
			if trees.array(path)?.len() < 2 {
				return Ok(());
			}
			let (array, origins) = trees.structural(path)?;
			array.reverse();
			origins.reverse();
			Ok(())
		})
	}

	/// Sorts the array at `path` with `compare`, stably as JS `sort` does.
	pub fn sort_by(
		&mut self,
		path: &[Seg],
		mut compare: impl FnMut(&Snapshot, &Snapshot) -> CompareOrdering,
	) -> Result<(), DraftError> {
		self.edit(|trees| {
			if trees.array(path)?.len() < 2 {
				return Ok(());
			}
			let (array, origins) = trees.structural(path)?;
			// The origins travel with their elements.
			let mut pairs: Vec<(Snapshot, Option<usize>)> = array.drain(..).zip(origins.drain(..)).collect();
			pairs.sort_by(|left, right| compare(&left.0, &right.0));
			(*array, *origins) = pairs.into_iter().unzip();
			Ok(())
		})
	}

	/// JS `fill(value, start, end)` on the array at `path`: relative indices clamp, `None` is the end.
	pub fn fill(&mut self, path: &[Seg], value: &Snapshot, start: i64, end: Option<i64>) -> Result<(), DraftError> {
		self.edit(|trees| {
			let length = trees.array(path)?.len();
			let start = clamp_index(start, length);
			let end = end.map_or(length, |end| clamp_index(end, length));
			if end <= start {
				return Ok(());
			}
			let items = vec![value.clone(); end - start];
			trees.splice(path, start, end - start, items).map(drop)
		})
	}

	/// JS `copyWithin(target, start, end)` on the array at `path`: copies by value, overlapping ranges included.
	pub fn copy_within(&mut self, path: &[Seg], target: i64, start: i64, end: Option<i64>) -> Result<(), DraftError> {
		self.edit(|trees| {
			let array = trees.array(path)?;
			let length = array.len();
			let target = clamp_index(target, length);
			let start = clamp_index(start, length);
			let end = end.map_or(length, |end| clamp_index(end, length));
			let count = end.saturating_sub(start).min(length - target);
			if count == 0 {
				return Ok(());
			}
			let copied: Vec<Snapshot> = array[start..start + count].to_vec();
			trees.splice(path, target, count, copied).map(drop)
		})
	}

	/// Sets element `index` of the array at `path`; `index` equal to the length appends, a larger one is refused.
	pub fn set_index(&mut self, path: &[Seg], index: usize, value: Snapshot) -> Result<(), DraftError> {
		self.edit(|trees| trees.set_index(path, index, value))
	}

	/// Sets the length of the array at `path` (JS `length = n`): shrinking removes elements, growing appends `null`.
	pub fn set_len(&mut self, path: &[Seg], length: usize) -> Result<(), DraftError> {
		self.edit(|trees| {
			let current = trees.array(path)?.len();
			if length < current {
				trees.splice(path, length, current - length, Vec::new())?;
			} else if length > current {
				trees.splice(path, current, 0, vec![Snapshot::Null; length - current])?;
			}
			Ok(())
		})
	}

	/// Runs `edit` on the trees while the change is open.
	fn edit<R>(&self, edit: impl FnOnce(&mut Trees) -> Result<R, DraftError>) -> Result<R, DraftError> {
		let mut slot = lock(&self.trees);
		match slot.as_mut() {
			Some(trees) if self.status.get() == Status::Open => edit(trees),
			_ => Err(DraftError::Settled),
		}
	}
}

impl Trees {
	fn set(&mut self, path: &[Seg], value: Option<Snapshot>) -> Result<(), DraftError> {
		let (parent, leaf) = split(path)?;
		match (self.container(parent, path)?, leaf, value) {
			(Container::Array, Seg::Key(_), _) => Err(DraftError::ArrayKey),
			(Container::Array, Seg::Index(_), None) => Err(DraftError::Undefined),
			(Container::Array, Seg::Index(index), Some(value)) => {
				let index = usize::try_from(*index).map_err(|_| DraftError::Holes)?;
				self.set_index(parent, index, value)
			}
			(Container::Object, _, None) => self.delete(path),
			(Container::Object, leaf, Some(value)) => {
				let key = leaf.to_string();
				self.placed(path, &value);
				self.object_mut(parent)?.insert(&key, value);
				Ok(())
			}
		}
	}

	fn delete(&mut self, path: &[Seg]) -> Result<(), DraftError> {
		let (parent, leaf) = split(path)?;
		let Container::Object = self.container(parent, path)? else {
			return Err(DraftError::Holes);
		};
		let key = leaf.to_string();
		if self.root.at(parent).and_then(|object| object.get(&key)).is_some() {
			self.forget_at_and_below(path);
			self.object_mut(parent)?.remove(&key);
		}
		Ok(())
	}

	fn append_text(&mut self, path: &[Seg], text: &str) -> Result<(), DraftError> {
		let (parent, leaf) = split(path)?;
		let key = leaf.to_string();
		let not_a_string = || DraftError::NotAString(to_js_string(&JsonValue::String(key.clone())));
		let current = match self.container(parent, path)? {
			Container::Object => match self.root.at(parent).and_then(|object| object.get(&key)) {
				Some(Snapshot::String(current)) => current.clone(),
				_ => return Err(not_a_string()),
			},
			Container::Array => return Err(not_a_string()),
		};
		if text.is_empty() {
			return Ok(());
		}
		let mut joined = String::with_capacity(current.len() + text.len());
		joined.push_str(&current);
		joined.push_str(text);
		let result: Arc<str> = Arc::from(joined);
		self.object_mut(parent)?.insert(&key, Snapshot::String(result.clone()));
		// Hints are keyed by the string they produced, not by path: a structural edit of an ancestor moves the string,
		// and the diff finds the hint wherever the string ends up.
		let hint = match self.hints.appends.remove(&string_key(&current)) {
			Some(mut hint) => {
				hint.text.push_str(text);
				hint.result = result;
				hint
			}
			None => AppendHint {
				origin: current,
				result,
				text: text.to_owned(),
			},
		};
		self.hints.appends.insert(string_key(&hint.result), hint);
		Ok(())
	}

	/// Replaces `remove` elements at `start` of the array at `path` with `items`, keeping the element origins in step
	/// (an inserted element has none); returns the removed elements.
	fn splice(
		&mut self,
		path: &[Seg],
		start: usize,
		remove: usize,
		items: Vec<Snapshot>,
	) -> Result<Vec<Snapshot>, DraftError> {
		if remove == 0 && items.is_empty() {
			return Ok(Vec::new());
		}
		let inserted = items.len();
		let (array, origins) = self.structural(path)?;
		let removed = array.splice(start..start + remove, items).collect();
		origins.splice(start..start + remove, std::iter::repeat_n(None, inserted));
		Ok(removed)
	}

	fn set_index(&mut self, path: &[Seg], index: usize, value: Snapshot) -> Result<(), DraftError> {
		let length = self.array(path)?.len();
		if index > length {
			return Err(DraftError::Holes);
		}
		if index == length {
			return self.splice(path, length, 0, vec![value]).map(drop);
		}
		let mut element = path.to_vec();
		element.push(Seg::Index(index as u64));
		self.placed(&element, &value);
		// A replaced element came from nowhere: it is a placement, not the base element edited.
		if let Some(origin) = self
			.hints
			.origins
			.get_mut(path)
			.and_then(|origins| origins.get_mut(index))
		{
			*origin = None;
		}
		self.array_mut(path)?[index] = value;
		Ok(())
	}

	/// Records a whole placement at `path`: hints about what was there before no longer apply.
	fn placed(&mut self, path: &[Seg], value: &Snapshot) {
		self.forget_at_and_below(path);
		if matches!(value, Snapshot::Object(_)) && !self.hints.assigned.contains(path) {
			self.hints.assigned.insert(path.to_vec());
		}
	}

	/// Drops the element origins of arrays at or below `path`, which no longer name the base positions they did.
	fn forget_at_and_below(&mut self, path: &[Seg]) {
		if !self.hints.origins.is_empty() {
			self.hints.origins.retain(|array, _| !array.starts_with(path));
		}
	}

	/// Drops the element origins of arrays strictly below `path`, whose paths a structural edit at `path` shifts.
	fn forget_below(&mut self, path: &[Seg]) {
		if self.hints.origins.len() > 1 {
			self.hints
				.origins
				.retain(|array, _| array.len() <= path.len() || !array.starts_with(path));
		}
	}

	/// The kind of container at `parent`, which an edit of `path` writes into.
	fn container(&self, parent: &[Seg], path: &[Seg]) -> Result<Container, DraftError> {
		match self.root.at(parent) {
			Some(Snapshot::Array(_)) => Ok(Container::Array),
			Some(Snapshot::Object(_)) => Ok(Container::Object),
			_ => Err(DraftError::Path(path.to_vec())),
		}
	}

	/// The array at `path`, read-only: edits check it first, so a refused edit copies nothing.
	fn array(&self, path: &[Seg]) -> Result<&[Snapshot], DraftError> {
		match self.root.at(path) {
			Some(Snapshot::Array(items)) => Ok(items),
			Some(_) => Err(DraftError::NotAnArray),
			None => Err(DraftError::Path(path.to_vec())),
		}
	}

	/// The array at `path` and its element origins (base positions, `None` for an inserted element), for a structural
	/// edit: the first one records every element at its own position.
	fn structural(&mut self, path: &[Seg]) -> Result<(&mut Vec<Snapshot>, &mut Vec<Option<usize>>), DraftError> {
		let length = self.array(path)?.len();
		self.forget_below(path);
		let origins = self
			.hints
			.origins
			.entry(path.to_vec())
			.or_insert_with(|| (0..length).map(Some).collect());
		match node_mut(&mut self.root, path)?.node_mut() {
			NodeMut::Array(items) => Ok((items, origins)),
			_ => Err(DraftError::NotAnArray),
		}
	}

	/// The array at `path`, made exclusively the draft's, after `array(path)` accepted it.
	fn array_mut(&mut self, path: &[Seg]) -> Result<&mut Vec<Snapshot>, DraftError> {
		match node_mut(&mut self.root, path)?.node_mut() {
			NodeMut::Array(items) => Ok(items),
			_ => Err(DraftError::NotAnArray),
		}
	}

	/// The object at `path`, made exclusively the draft's, after `container` accepted it.
	fn object_mut(&mut self, path: &[Seg]) -> Result<&mut SnapshotObject, DraftError> {
		match node_mut(&mut self.root, path)?.node_mut() {
			NodeMut::Object(object) => Ok(object),
			_ => Err(DraftError::Path(path.to_vec())),
		}
	}
}

/// The node at an existing `path`, copying each shared container on the way (`Arc::make_mut`).
fn node_mut<'a>(root: &'a mut Snapshot, path: &[Seg]) -> Result<&'a mut Snapshot, DraftError> {
	let unresolved = || DraftError::Path(path.to_vec());
	let mut node = root;
	for segment in path {
		node = match (node.node_mut(), segment) {
			(NodeMut::Array(items), Seg::Index(index)) => usize::try_from(*index)
				.ok()
				.and_then(|index| items.get_mut(index))
				.ok_or_else(unresolved)?,
			(NodeMut::Object(object), segment) => object.get_mut(&segment.to_string()).ok_or_else(unresolved)?,
			_ => return Err(unresolved()),
		};
	}
	Ok(node)
}

/// The container an edit writes into.
enum Container {
	Array,
	Object,
}

/// The parent path and the leaf segment of a non-root path.
fn split(path: &[Seg]) -> Result<(&[Seg], &Seg), DraftError> {
	match path.split_last() {
		Some((leaf, parent)) => Ok((parent, leaf)),
		None => Err(DraftError::RootPath),
	}
}

/// JS relative index clamping (`splice`, `fill`, `copyWithin`): negative counts from the end.
fn clamp_index(value: i64, length: usize) -> usize {
	let length_i64 = i64::try_from(length).unwrap_or(i64::MAX);
	let clamped = if value < 0 {
		(length_i64 + value).max(0)
	} else {
		value.min(length_i64)
	};
	usize::try_from(clamped).unwrap_or(length)
}

fn path_text(path: &Path) -> String {
	// invariant: a path serializes to a JSON array of strings and integers.
	to_js_string(&serde_json::to_value(path).unwrap_or(JsonValue::Null))
}

/// Where one change or preparation stands; shared by the change, its preparation and the tracker that stales it.
#[derive(Debug)]
struct StatusCell(AtomicU8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Status {
	Open,
	Prepared,
	Consumed,
	Aborted,
	Stale,
}

impl StatusCell {
	fn new(status: Status) -> Self {
		Self(AtomicU8::new(status as u8))
	}

	fn get(&self) -> Status {
		match self.0.load(Ordering::Acquire) {
			0 => Status::Open,
			1 => Status::Prepared,
			2 => Status::Consumed,
			3 => Status::Aborted,
			_ => Status::Stale,
		}
	}

	/// Moves to `next` from one of `from`; false when the status was another.
	fn transition(&self, from: &[Status], next: Status) -> bool {
		from.iter().any(|&from| {
			self.0
				.compare_exchange(from as u8, next as u8, Ordering::AcqRel, Ordering::Acquire)
				.is_ok()
		})
	}
}
