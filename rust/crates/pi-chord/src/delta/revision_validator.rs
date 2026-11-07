//! The replica revision validator (TS `delta/revision-validator.ts`), which in Rust has nothing left to check.
//!
//! The TS validator walks a replicated revision once per container and rejects what JS values can hold and JSON cannot:
//! cycles, non-finite numbers, class instances, sparse arrays, accessors and symbol keys. A `Snapshot` is built from a
//! `JsonValue` and holds none of them by construction, so every TS rejection is unrepresentable and validation is the
//! identity; the type stays so that replica code ported from TS keeps its call site and its cost (none).

use crate::delta::snapshot::Snapshot;

/// Validates immutable replica revisions; every `Snapshot` is strict JSON, so each one is accepted unchanged.
#[derive(Debug, Default)]
pub struct JsonRevisionValidator;

impl JsonRevisionValidator {
	/// A validator.
	pub fn new() -> Self {
		Self
	}

	/// The revision, accepted: a `Snapshot` cannot hold a value the TS validator rejects.
	pub fn validate(&self, value: Snapshot) -> Snapshot {
		value
	}
}
