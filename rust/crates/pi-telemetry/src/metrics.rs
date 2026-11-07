//! Counters, up-down counters and histograms (ports `packages/telemetry/src/metrics.ts`), and the schema-bound
//! instrument lookup `createTypedMetrics` becomes in Rust.

use crate::context::{SpanAttributes, TelemetryContext};
use crate::schema::{MetricDefinition, TelemetrySchema};
use std::sync::Arc;

/// The kind of an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InstrumentKind {
	/// Monotonic sums of non-negative increments.
	Counter,
	/// Sums of signed increments.
	UpDownCounter,
	/// Distributions of measurements.
	Histogram,
}

/// An instrument's description and unit.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InstrumentOptions<'a> {
	/// What it measures.
	pub description: Option<&'a str>,
	/// The unit, such as `ms` or `By`.
	pub unit: Option<&'a str>,
}

/// A histogram's description, unit and bucket hints.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HistogramOptions<'a> {
	/// What it measures.
	pub description: Option<&'a str>,
	/// The unit.
	pub unit: Option<&'a str>,
	/// Bucket hints: finite and strictly increasing, supplied by the caller.
	pub boundaries: Option<&'a [f64]>,
}

impl<'a> From<InstrumentOptions<'a>> for HistogramOptions<'a> {
	fn from(options: InstrumentOptions<'a>) -> Self {
		Self {
			description: options.description,
			unit: options.unit,
			boundaries: None,
		}
	}
}

/// A counter: callers add non-negative increments.
pub trait TelemetryCounter: Send + Sync {
	/// Adds `value`.
	fn add(&self, value: f64, attributes: &SpanAttributes);
}

/// An up-down counter: callers add signed increments.
pub trait TelemetryUpDownCounter: Send + Sync {
	/// Adds `value`, which may be negative.
	fn add(&self, value: f64, attributes: &SpanAttributes);
}

/// A histogram: callers record finite measurements, optionally correlated with a span (an exemplar).
pub trait TelemetryHistogram: Send + Sync {
	/// Records `value`; `context` names the span it belongs to.
	fn record(&self, value: f64, attributes: &SpanAttributes, context: Option<&dyn TelemetryContext>);
}

/// An adapter's instrument factories. Factories and recording are synchronous and trust typed inputs.
pub trait TelemetryMetrics: Send + Sync {
	/// A counter called `name`.
	fn counter(&self, name: &str, options: InstrumentOptions<'_>) -> Arc<dyn TelemetryCounter>;

	/// An up-down counter called `name`.
	fn up_down_counter(&self, name: &str, options: InstrumentOptions<'_>) -> Arc<dyn TelemetryUpDownCounter>;

	/// A histogram called `name`.
	fn histogram(&self, name: &str, options: HistogramOptions<'_>) -> Arc<dyn TelemetryHistogram>;
}

/// Instruments looked up by the names a set of schemas declares, created with the declared metadata.
pub struct SchemaMetrics<'a> {
	metrics: &'a dyn TelemetryMetrics,
	schemas: &'a [&'a TelemetrySchema],
}

impl<'a> SchemaMetrics<'a> {
	/// Binds `metrics` to the metric definitions of `schemas`; a later schema's definition of a name wins, as the TS
	/// map does at runtime (TS rejects duplicates at compile time).
	pub fn new(metrics: &'a dyn TelemetryMetrics, schemas: &'a [&'a TelemetrySchema]) -> Self {
		Self { metrics, schemas }
	}

	/// The counter called `name`, with its declared description and unit.
	pub fn counter(&self, name: &str) -> Arc<dyn TelemetryCounter> {
		self.metrics.counter(name, self.instrument_options(name))
	}

	/// The up-down counter called `name`, with its declared description and unit.
	pub fn up_down_counter(&self, name: &str) -> Arc<dyn TelemetryUpDownCounter> {
		self.metrics.up_down_counter(name, self.instrument_options(name))
	}

	/// The histogram called `name`, with its declared description, unit and boundaries.
	pub fn histogram(&self, name: &str) -> Arc<dyn TelemetryHistogram> {
		let definition = self.definition(name);
		self.metrics.histogram(
			name,
			HistogramOptions {
				description: definition.map(|definition| definition.description),
				unit: definition.and_then(|definition| definition.unit),
				boundaries: definition.and_then(|definition| definition.boundaries),
			},
		)
	}

	fn instrument_options(&self, name: &str) -> InstrumentOptions<'static> {
		let definition = self.definition(name);
		InstrumentOptions {
			description: definition.map(|definition| definition.description),
			unit: definition.and_then(|definition| definition.unit),
		}
	}

	fn definition(&self, name: &str) -> Option<&'static MetricDefinition> {
		// TS parity: an undeclared name gets an instrument without metadata.
		self.schemas
			.iter()
			.rev()
			.find_map(|schema| schema.metrics.iter().find(|definition| definition.name == name))
	}
}
