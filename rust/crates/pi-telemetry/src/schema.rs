//! Telemetry schemas as runtime data (ports the schema definitions of `packages/telemetry/src/index.ts` and
//! `TelemetryMetricDefinition` of `metrics.ts`), and the check of recorded telemetry against them that tests use.
//!
//! TS infers exact attribute types from schemas at compile time; here a crate declares its schema as a `static` and
//! writes plain starter functions for its spans, and its tests run [`validate_against_schema`] on what the
//! in-memory adapter recorded.

use crate::context::{AttributeValue, SpanAttributes};
use crate::memory::{RecordedTelemetryMeasurement, RecordedTelemetrySpan};
use crate::metrics::InstrumentKind;
use std::fmt;

/// A versioned vocabulary of spans and metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetrySchema {
	/// The schema version.
	pub version: u32,
	/// The spans.
	pub spans: &'static [SpanDefinition],
	/// The metrics.
	pub metrics: &'static [MetricDefinition],
}

/// One span of a schema.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanDefinition {
	/// The span name.
	pub name: &'static str,
	/// What the span covers.
	pub description: &'static str,
	/// Which parents it may have.
	pub parents: ParentRule,
	/// Attributes known when the span starts; `required` is honoured.
	pub start_attributes: &'static [AttributeDefinition],
	/// Attributes known later; always optional (`required` is ignored).
	pub end_attributes: &'static [AttributeDefinition],
	/// The events it records.
	pub events: &'static [EventDefinition],
	/// When its status is an error (the default is ok).
	pub error_when: &'static str,
}

/// The parents a span may have (descriptive in TS; checked by [`validate_against_schema`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParentRule {
	/// A root or any span.
	Any,
	/// A root or a span outside the schema.
	RootOrExternal,
	/// Only the listed spans of the schema.
	Spans(&'static [&'static str]),
}

/// One event of a span.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDefinition {
	/// The event name.
	pub name: &'static str,
	/// What it marks.
	pub description: &'static str,
	/// Its attributes; `required` is honoured.
	pub attributes: &'static [AttributeDefinition],
}

/// One instrument of a schema.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricDefinition {
	/// The instrument name.
	pub name: &'static str,
	/// What it measures.
	pub description: &'static str,
	/// The unit.
	pub unit: Option<&'static str>,
	/// The instrument kind.
	pub kind: InstrumentKind,
	/// Histogram bucket hints; only histograms declare them.
	pub boundaries: Option<&'static [f64]>,
	/// Its attributes, low cardinality only; `required` is honoured.
	pub attributes: &'static [AttributeDefinition],
}

/// One attribute of a span, event or metric.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeDefinition {
	/// The attribute name.
	pub name: &'static str,
	/// What it means.
	pub description: &'static str,
	/// Its type and allowed values.
	pub kind: AttributeKind,
	/// Whether it must be present (start, event and metric attributes).
	pub required: bool,
	/// Whether the value needs special handling.
	pub sensitive: bool,
	/// The expected cardinality, when stated.
	pub cardinality: Option<Cardinality>,
}

/// An attribute type with its optional closed set of values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttributeKind {
	/// `string`.
	String {
		/// The allowed values, when closed.
		values: Option<&'static [&'static str]>,
	},
	/// `number`.
	Number {
		/// The allowed values, when closed.
		values: Option<&'static [f64]>,
	},
	/// `boolean`.
	Bool {
		/// The allowed values, when closed.
		values: Option<&'static [bool]>,
	},
	/// `string[]`.
	Strings {
		/// The allowed elements, when closed.
		element_values: Option<&'static [&'static str]>,
	},
	/// `number[]`.
	Numbers {
		/// The allowed elements, when closed.
		element_values: Option<&'static [f64]>,
	},
	/// `boolean[]`.
	Bools {
		/// The allowed elements, when closed.
		element_values: Option<&'static [bool]>,
	},
}

/// The expected number of distinct values of an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
	/// Bounded, such as a direction or method.
	Low,
	/// Unbounded, such as a request id; never on metrics.
	High,
}

/// One way recorded telemetry departs from its schemas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaViolation {
	/// What departs: `span <name>`, `span <name> event <name>` or `metric <name>`.
	pub subject: String,
	/// How it departs.
	pub problem: SchemaProblem,
}

/// How recorded telemetry departs from a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaProblem {
	/// No schema declares the span, event or metric.
	Undeclared,
	/// A required attribute is missing.
	MissingAttribute(String),
	/// An attribute is not declared.
	UndeclaredAttribute(String),
	/// An attribute has the wrong type or a value outside its closed set.
	InvalidValue(String),
	/// A high-cardinality attribute is declared on a metric.
	HighCardinalityMetricAttribute(String),
	/// The span's parent breaks its parent rule.
	ParentNotAllowed,
	/// The measurement's instrument kind is not the declared one.
	WrongInstrumentKind,
}

impl AttributeDefinition {
	/// An optional attribute of `kind`, not sensitive, without a cardinality.
	pub const fn new(name: &'static str, description: &'static str, kind: AttributeKind) -> Self {
		Self {
			name,
			description,
			kind,
			required: false,
			sensitive: false,
			cardinality: None,
		}
	}

	/// This attribute, required.
	#[must_use]
	pub const fn required(mut self) -> Self {
		self.required = true;
		self
	}

	/// This attribute, marked sensitive.
	#[must_use]
	pub const fn sensitive(mut self) -> Self {
		self.sensitive = true;
		self
	}

	/// This attribute with a stated cardinality.
	#[must_use]
	pub const fn cardinality(mut self, cardinality: Cardinality) -> Self {
		self.cardinality = Some(cardinality);
		self
	}
}

impl fmt::Display for SchemaViolation {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		let subject = &self.subject;
		match &self.problem {
			SchemaProblem::Undeclared => write!(formatter, "{subject}: not declared"),
			SchemaProblem::MissingAttribute(name) => write!(formatter, "{subject}: missing attribute {name}"),
			SchemaProblem::UndeclaredAttribute(name) => write!(formatter, "{subject}: undeclared attribute {name}"),
			SchemaProblem::InvalidValue(name) => write!(formatter, "{subject}: invalid value of {name}"),
			SchemaProblem::HighCardinalityMetricAttribute(name) => {
				write!(formatter, "{subject}: high-cardinality attribute {name}")
			}
			SchemaProblem::ParentNotAllowed => write!(formatter, "{subject}: parent not allowed"),
			SchemaProblem::WrongInstrumentKind => write!(formatter, "{subject}: wrong instrument kind"),
		}
	}
}

/// Every way `spans` and `measurements` (as the in-memory adapter records them) depart from `schemas`: undeclared
/// names, missing required or undeclared attributes, wrong types and closed-set values, parents that break a span's
/// rule (a remote parent counts as external), and wrong instrument kinds.
pub fn validate_against_schema(
	spans: &[RecordedTelemetrySpan],
	measurements: &[RecordedTelemetryMeasurement],
	schemas: &[&TelemetrySchema],
) -> Vec<SchemaViolation> {
	let mut violations = Vec::new();
	for span in spans {
		check_span(span, spans, schemas, &mut violations);
	}
	for measurement in measurements {
		check_measurement(measurement, schemas, &mut violations);
	}
	violations
}

fn check_span(
	span: &RecordedTelemetrySpan,
	spans: &[RecordedTelemetrySpan],
	schemas: &[&TelemetrySchema],
	violations: &mut Vec<SchemaViolation>,
) {
	let subject = format!("span {}", span.name);
	let Some(definition) = find_span(schemas, &span.name) else {
		violations.push(violation(subject, SchemaProblem::Undeclared));
		return;
	};
	let declared = [definition.start_attributes, definition.end_attributes];
	check_attributes(
		&subject,
		&span.attributes,
		&declared,
		definition.start_attributes,
		violations,
	);
	let parent = span
		.parent_id
		.and_then(|parent_id| spans.iter().find(|candidate| candidate.id == parent_id));
	let allowed = match definition.parents {
		ParentRule::Any => true,
		ParentRule::RootOrExternal => parent.is_none_or(|parent| find_span(schemas, &parent.name).is_none()),
		ParentRule::Spans(names) => parent.is_some_and(|parent| names.contains(&parent.name.as_str())),
	};
	if !allowed {
		violations.push(violation(subject.clone(), SchemaProblem::ParentNotAllowed));
	}
	for event in &span.events {
		let event_subject = format!("{subject} event {}", event.name);
		match definition.events.iter().find(|declared| declared.name == event.name) {
			Some(declared) => check_attributes(
				&event_subject,
				&event.attributes,
				&[declared.attributes],
				declared.attributes,
				violations,
			),
			None => violations.push(violation(event_subject, SchemaProblem::Undeclared)),
		}
	}
}

fn find_span<'a>(schemas: &[&'a TelemetrySchema], name: &str) -> Option<&'a SpanDefinition> {
	schemas
		.iter()
		.find_map(|schema| schema.spans.iter().find(|definition| definition.name == name))
}

fn check_measurement(
	measurement: &RecordedTelemetryMeasurement,
	schemas: &[&TelemetrySchema],
	violations: &mut Vec<SchemaViolation>,
) {
	let subject = format!("metric {}", measurement.name);
	let definition = schemas.iter().find_map(|schema| {
		schema
			.metrics
			.iter()
			.find(|definition| definition.name == measurement.name)
	});
	let Some(definition) = definition else {
		violations.push(violation(subject, SchemaProblem::Undeclared));
		return;
	};
	if definition.kind != measurement.kind {
		violations.push(violation(subject.clone(), SchemaProblem::WrongInstrumentKind));
	}
	for attribute in definition.attributes {
		if attribute.cardinality == Some(Cardinality::High) {
			let problem = SchemaProblem::HighCardinalityMetricAttribute(attribute.name.to_owned());
			violations.push(violation(subject.clone(), problem));
		}
	}
	check_attributes(
		&subject,
		&measurement.attributes,
		&[definition.attributes],
		definition.attributes,
		violations,
	);
}

/// Checks recorded `attributes` against the `declared` ones, of which those in `requiring` honour `required`.
fn check_attributes(
	subject: &str,
	attributes: &SpanAttributes,
	declared: &[&[AttributeDefinition]],
	requiring: &[AttributeDefinition],
	violations: &mut Vec<SchemaViolation>,
) {
	for definition in requiring.iter().filter(|definition| definition.required) {
		if attributes.get(definition.name).is_none() {
			let problem = SchemaProblem::MissingAttribute(definition.name.to_owned());
			violations.push(violation(subject.to_owned(), problem));
		}
	}
	for (name, value) in attributes.iter() {
		let definition = declared
			.iter()
			.flat_map(|set| set.iter())
			.find(|definition| definition.name == name);
		let problem = match definition {
			None => SchemaProblem::UndeclaredAttribute(name.to_owned()),
			Some(definition) if !value_allowed(&definition.kind, value) => SchemaProblem::InvalidValue(name.to_owned()),
			Some(_) => continue,
		};
		violations.push(violation(subject.to_owned(), problem));
	}
}

fn value_allowed(kind: &AttributeKind, value: &AttributeValue) -> bool {
	match (kind, value) {
		(AttributeKind::String { values }, AttributeValue::String(value)) => in_set(*values, &value.as_str()),
		(AttributeKind::Number { values }, AttributeValue::Number(value)) => in_set(*values, value),
		(AttributeKind::Bool { values }, AttributeValue::Bool(value)) => in_set(*values, value),
		(AttributeKind::Strings { element_values }, AttributeValue::Strings(items)) => {
			items.iter().all(|item| in_set(*element_values, &item.as_str()))
		}
		(AttributeKind::Numbers { element_values }, AttributeValue::Numbers(items)) => {
			items.iter().all(|item| in_set(*element_values, item))
		}
		(AttributeKind::Bools { element_values }, AttributeValue::Bools(items)) => {
			items.iter().all(|item| in_set(*element_values, item))
		}
		_ => false,
	}
}

fn in_set<T: PartialEq>(values: Option<&[T]>, value: &T) -> bool {
	values.is_none_or(|values| values.contains(value))
}

fn violation(subject: String, problem: SchemaProblem) -> SchemaViolation {
	SchemaViolation { subject, problem }
}
