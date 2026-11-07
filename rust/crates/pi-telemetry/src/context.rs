//! The span contract every instrumented crate uses (ports `TelemetryContext`, `TelemetrySpan`, `SpanOptions`,
//! `SpanLink`, `SpanStatus`, `AttributeValue` and `SpanAttributes` of `packages/telemetry/src/index.ts`).
//!
//! TS starts a span around a callback and settles it when the callback settles. Rust starts a span and hands it to
//! the caller: it settles when [`TelemetrySpan::end`] is called or when it is dropped, so a span cannot be used after
//! it settled. A span dropped while its thread panics records an error (unless a status was set), as a throw does in
//! TS. [`TelemetrySpan::handle`] gives a context that outlives the span, for links and injection after settlement.

use crate::metrics::TelemetryMetrics;
use crate::propagation::TelemetryPropagation;
use pi_json::JsonValue;
use std::any::Any;
use std::sync::Arc;

/// A value of an attribute: a scalar or a homogeneous array of scalars.
#[derive(Debug, Clone, PartialEq)]
pub enum AttributeValue {
	/// A string.
	String(String),
	/// A number.
	Number(f64),
	/// A boolean.
	Bool(bool),
	/// Strings.
	Strings(Vec<String>),
	/// Numbers.
	Numbers(Vec<f64>),
	/// Booleans.
	Bools(Vec<bool>),
}

/// Named attributes in insertion order. Setting a name again replaces its value in place, as spreading a later
/// object over an earlier one does in TS; TS's `undefined` values are simply not set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpanAttributes {
	entries: Vec<(String, AttributeValue)>,
}

/// Where new work belongs in the span tree: it starts spans and may expose its adapter's capabilities. Adapters
/// trust typed inputs; malformed input behavior is unspecified.
pub trait TelemetryContext: Any + Send + Sync {
	/// The adapter's metrics capability, the same object on every context of the adapter.
	fn metrics(&self) -> Option<&dyn TelemetryMetrics>;

	/// The adapter's propagation capability, the same object on every context of the adapter.
	fn propagation(&self) -> Option<&dyn TelemetryPropagation>;

	/// Starts a child span of this context; it settles on [`TelemetrySpan::end`] or drop.
	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan>;
}

/// An open span: it records attributes, events, links and a status, and is the parent context of its children.
/// Recording methods are synchronous.
pub trait TelemetrySpan: TelemetryContext {
	/// Links this span to another context without changing its parent; unknown and root contexts are ignored.
	fn add_link(&self, link: SpanLink<'_>);

	/// Records a named point in time.
	fn add_event(&self, name: &str, attributes: SpanAttributes);

	/// Merges attributes: later values replace earlier ones of the same name.
	fn set_attributes(&self, attributes: SpanAttributes);

	/// Sets the outcome; the last call wins, and an explicit status is never overwritten automatically.
	fn set_status(&self, status: SpanStatus);

	/// A context that refers to this span and outlives it: a link target and an injection source after the span
	/// settled; children started from it after settlement are inert (not recorded).
	fn handle(&self) -> Arc<dyn TelemetryContext>;

	/// Settles the span with its current status, `ok` unless set.
	fn end(self: Box<Self>);
}

/// What a new span is called and starts with.
#[derive(Clone, Default)]
pub struct SpanOptions<'a> {
	/// The span name.
	pub name: String,
	/// The start attributes.
	pub attributes: SpanAttributes,
	/// Links known at creation.
	pub links: Vec<SpanLink<'a>>,
}

/// A relation from a span to another context, with its own attributes.
#[derive(Clone)]
pub struct SpanLink<'a> {
	/// The target: a span, a span's handle or an extracted remote parent.
	pub context: &'a dyn TelemetryContext,
	/// The link's attributes.
	pub attributes: SpanAttributes,
}

/// The outcome of a span.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SpanStatus {
	/// It succeeded (the default).
	#[default]
	Ok,
	/// It failed, optionally with the error's name and message.
	Error(Option<SpanError>),
}

/// The name and message of the error a span failed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanError {
	/// The error's name, such as `TypeError`.
	pub name: String,
	/// The error's message.
	pub message: String,
}

impl SpanAttributes {
	/// No attributes.
	pub fn new() -> Self {
		Self::default()
	}

	/// These attributes with `name` set to `value`, for building attribute sets inline.
	#[must_use]
	pub fn with(mut self, name: impl Into<String>, value: impl Into<AttributeValue>) -> Self {
		self.set(name, value);
		self
	}

	/// Sets `name`; an existing name keeps its position.
	pub fn set(&mut self, name: impl Into<String>, value: impl Into<AttributeValue>) {
		let (name, value) = (name.into(), value.into());
		match self.entries.iter_mut().find(|(existing, _)| *existing == name) {
			Some((_, slot)) => *slot = value,
			None => self.entries.push((name, value)),
		}
	}

	/// Sets every attribute of `other`, in its order.
	pub fn merge(&mut self, other: SpanAttributes) {
		for (name, value) in other.entries {
			self.set(name, value);
		}
	}

	/// The value of `name`, if set.
	pub fn get(&self, name: &str) -> Option<&AttributeValue> {
		self.entries
			.iter()
			.find_map(|(existing, value)| (existing == name).then_some(value))
	}

	/// The attributes in insertion order.
	pub fn iter(&self) -> impl Iterator<Item = (&str, &AttributeValue)> {
		self.entries.iter().map(|(name, value)| (name.as_str(), value))
	}

	/// Whether there are none.
	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

impl<N: Into<String>, V: Into<AttributeValue>> FromIterator<(N, V)> for SpanAttributes {
	fn from_iter<I: IntoIterator<Item = (N, V)>>(iter: I) -> Self {
		let mut attributes = Self::new();
		for (name, value) in iter {
			attributes.set(name, value);
		}
		attributes
	}
}

impl AttributeValue {
	/// The value as JSON, as `JSON.stringify` writes it (a non-finite number becomes `null`).
	pub fn to_json(&self) -> JsonValue {
		match self {
			Self::String(text) => JsonValue::from(text.as_str()),
			Self::Number(number) => JsonValue::from(*number),
			Self::Bool(flag) => JsonValue::from(*flag),
			Self::Strings(items) => JsonValue::from(items.clone()),
			Self::Numbers(items) => JsonValue::from(items.clone()),
			Self::Bools(items) => JsonValue::from(items.clone()),
		}
	}
}

impl From<&str> for AttributeValue {
	fn from(value: &str) -> Self {
		Self::String(value.to_owned())
	}
}

impl From<String> for AttributeValue {
	fn from(value: String) -> Self {
		Self::String(value)
	}
}

impl From<f64> for AttributeValue {
	fn from(value: f64) -> Self {
		Self::Number(value)
	}
}

impl From<i32> for AttributeValue {
	fn from(value: i32) -> Self {
		Self::Number(f64::from(value))
	}
}

impl From<u32> for AttributeValue {
	fn from(value: u32) -> Self {
		Self::Number(f64::from(value))
	}
}

impl From<bool> for AttributeValue {
	fn from(value: bool) -> Self {
		Self::Bool(value)
	}
}

impl From<Vec<String>> for AttributeValue {
	fn from(value: Vec<String>) -> Self {
		Self::Strings(value)
	}
}

impl From<Vec<&str>> for AttributeValue {
	fn from(value: Vec<&str>) -> Self {
		Self::Strings(value.into_iter().map(str::to_owned).collect())
	}
}

impl From<Vec<f64>> for AttributeValue {
	fn from(value: Vec<f64>) -> Self {
		Self::Numbers(value)
	}
}

impl From<Vec<bool>> for AttributeValue {
	fn from(value: Vec<bool>) -> Self {
		Self::Bools(value)
	}
}

impl<'a> SpanOptions<'a> {
	/// Options for a span called `name`, without attributes or links.
	pub fn new(name: impl Into<String>) -> Self {
		Self {
			name: name.into(),
			attributes: SpanAttributes::new(),
			links: Vec::new(),
		}
	}

	/// These options with start attributes.
	#[must_use]
	pub fn with_attributes(mut self, attributes: SpanAttributes) -> Self {
		self.attributes = attributes;
		self
	}

	/// These options with one more link.
	#[must_use]
	pub fn with_link(mut self, link: SpanLink<'a>) -> Self {
		self.links.push(link);
		self
	}
}

impl<'a> SpanLink<'a> {
	/// A link to `context` without attributes.
	pub fn new(context: &'a dyn TelemetryContext) -> Self {
		Self {
			context,
			attributes: SpanAttributes::new(),
		}
	}

	/// This link with attributes.
	#[must_use]
	pub fn with_attributes(mut self, attributes: SpanAttributes) -> Self {
		self.attributes = attributes;
		self
	}
}

impl SpanStatus {
	/// An error status with a name and message.
	pub fn error(name: impl Into<String>, message: impl Into<String>) -> Self {
		Self::Error(Some(SpanError {
			name: name.into(),
			message: message.into(),
		}))
	}
}
