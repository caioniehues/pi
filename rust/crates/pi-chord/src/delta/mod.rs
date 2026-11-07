//! Chord Delta (TS `delta/`): the op vocabulary, its validator, the in-place and persistent appliers, the wire codec,
//! the tracker and its drafts, revision diffs, and the pending queue of a slow consumer.

mod apply;
mod diff;
mod op;
mod pending;
mod revision_validator;
mod snapshot;
mod tracker;
mod validate;
mod wire;

pub use apply::{ApplyError, PathError, UnresolvedPath, apply, apply_batches};
pub use diff::diff_revisions;
pub use op::{NonEmptyPath, Op, Path, PathRef, Replaces, Seg, WireOp, is_base, overlap};
pub use pending::{MAX_PENDING_OPS, PendingOps, PendingTake};
pub use revision_validator::JsonRevisionValidator;
pub use snapshot::{Snapshot, SnapshotObject, apply_immutable, apply_immutable_batches, apply_immutable_trusted};
pub use tracker::{Change, Draft, DraftError, Prepared, Tracker, TrackerError, track};
pub use validate::{
	OpError, RESERVED_SEGMENTS, UnsafePathError, assert_index_in_range, assert_permutation, assert_safe_path,
	assert_valid_op, assert_valid_wire_op, validate_op,
};
pub use wire::{DecodeError, Decoder, Encoder};
