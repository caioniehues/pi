//! pi-telemetry: the vendor-neutral telemetry contract every crate instruments against (ports
//! `packages/telemetry`): explicit span contexts, links and propagation, metrics, schemas as data, a no-op and an
//! in-memory reference adapter with a store, trace rendering, and the adapter conformance suite.

mod context;
mod memory;
mod metrics;
mod noop;
mod propagation;
mod render;
mod schema;
mod store;
#[cfg(feature = "tracing")]
mod tracing_bridge;

/// Reusable checks for adapters.
pub mod testing {
	pub mod conformance;
}

pub use context::{
	AttributeValue, SpanAttributes, SpanError, SpanLink, SpanOptions, SpanStatus, TelemetryContext, TelemetrySpan,
};
pub use memory::{
	InMemoryTelemetryContext, RecordedTelemetryEvent, RecordedTelemetryLink, RecordedTelemetryMeasurement,
	RecordedTelemetrySpan, telemetry_now,
};
pub use metrics::{
	HistogramOptions, InstrumentKind, InstrumentOptions, SchemaMetrics, TelemetryCounter, TelemetryHistogram,
	TelemetryMetrics, TelemetryUpDownCounter,
};
pub use noop::{NOOP_TELEMETRY_CONTEXT, NOOP_TELEMETRY_METRICS, NOOP_TELEMETRY_PROPAGATION, NoopTelemetry};
pub use propagation::{TelemetryCarrier, TelemetryPropagation};
pub use render::render_trace;
pub use schema::{
	AttributeDefinition, AttributeKind, Cardinality, EventDefinition, MetricDefinition, ParentRule, SchemaProblem,
	SchemaViolation, SpanDefinition, TelemetrySchema, validate_against_schema,
};
pub use store::{
	MetricQuery, StoredEvent, StoredLink, StoredMetric, StoredSpan, StoredTrace, TelemetryStore, TelemetryStoreError,
	TracePage, TraceQuery,
};
#[cfg(feature = "tracing")]
pub use tracing_bridge::TracingTelemetryContext;
