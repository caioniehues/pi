//! Public types (TS `types.ts`): the options of `compile()`, the errors a failed value produces, and the error a
//! schema that cannot compile produces.
//!
//! Decided difference from TS: `schemas` is a list of `(uri, document)` pairs instead of a record, and the formats
//! are a `FormatSet` value (`formats::standard()`), never a global.

use pi_json::JsonValue;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// A JSON Schema: `true`, `false` or a schema object. Schemas come from tool definitions, MCP servers and other
/// untrusted sources, so `compile()` reads every keyword defensively instead of trusting a static shape.
pub type JsonSchema = JsonValue;

/// The draft whose semantics apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Draft {
	/// Draft 2020-12 (the default).
	#[default]
	V2020_12,
	/// Draft 7: `$ref` makes its siblings inert, `items` arrays with `additionalItems`, `dependencies`.
	V7,
}

/// What a `$ref` that names nothing (a missing definition, a document not in `schemas`) does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnresolvedRefs {
	/// `compile()` fails (the default).
	#[default]
	Throw,
	/// The reference compiles as `false`: only values at its place fail, with "unresolved reference".
	Reject,
}

/// A format check: whether a string has the format.
pub type FormatCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Format checks by name. Empty, `format` is an annotation, as draft 2020-12 specifies.
#[derive(Clone, Default)]
pub struct FormatSet {
	checks: HashMap<String, FormatCheck>,
}

impl FormatSet {
	/// An empty set: `format` is an annotation.
	pub fn new() -> Self {
		Self::default()
	}

	/// Adds (or replaces) the check of the format `name`.
	pub fn insert(&mut self, name: impl Into<String>, check: impl Fn(&str) -> bool + Send + Sync + 'static) {
		self.checks.insert(name.into(), Arc::new(check));
	}

	/// The check of the format `name`, if the set has one.
	pub fn get(&self, name: &str) -> Option<&FormatCheck> {
		self.checks.get(name)
	}

	/// The number of formats in the set.
	pub fn len(&self) -> usize {
		self.checks.len()
	}

	/// Whether the set checks no format.
	pub fn is_empty(&self) -> bool {
		self.checks.is_empty()
	}
}

impl fmt::Debug for FormatSet {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let mut names: Vec<&String> = self.checks.keys().collect();
		names.sort();
		f.debug_tuple("FormatSet").field(&names).finish()
	}
}

/// Options of `compile()`.
#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
	/// The draft whose semantics apply.
	pub draft: Draft,
	/// The format checks; empty, `format` is an annotation.
	pub formats: FormatSet,
	/// Schema documents by URI, for `$ref`s to other documents.
	pub schemas: Vec<(String, JsonValue)>,
	/// Whether the validator also coerces loosely typed input (`coerce()` and `parse()`).
	pub coerce: bool,
	/// What a `$ref` that names nothing does.
	pub unresolved_refs: UnresolvedRefs,
}

/// One failure of a value: where it is (a JSON Pointer, "" for the value itself), the failing keyword and a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationError {
	/// The JSON Pointer of the failing value.
	pub path: String,
	/// The keyword that failed.
	pub keyword: String,
	/// What is wrong, in the words of the TS implementation.
	pub message: String,
}

impl ValidationError {
	/// An error at `path`.
	pub fn new(path: impl Into<String>, keyword: impl Into<String>, message: impl Into<String>) -> Self {
		Self {
			path: path.into(),
			keyword: keyword.into(),
			message: message.into(),
		}
	}
}

/// The result of `parse()`: the coerced value when it is valid, else the errors of the coerced value.
pub type ParseResult = Result<JsonValue, Vec<ValidationError>>;

/// Why a schema does not compile; the messages are the TS ones.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum CompileError {
	/// A subschema is neither an object nor a boolean.
	#[error("A schema must be an object or a boolean, got {0}")]
	Malformed(String),
	/// A `$ref` names nothing (with `UnresolvedRefs::Throw`).
	#[error("{0}")]
	UnresolvedRef(String),
	/// A `$ref` fragment has a malformed percent-encoding (`decodeURIComponent` throws).
	#[error("URI malformed")]
	MalformedUri,
	/// A `pattern` or `patternProperties` key is not an ECMAScript regular expression.
	#[error("Invalid regular expression: /{pattern}/: {reason}")]
	InvalidPattern {
		/// The pattern source.
		pattern: String,
		/// The parser's reason.
		reason: String,
	},
}
