//! Reading settled telemetry back (ports `StoredSpan`, `StoredTrace`, `StoredMetric`, `TraceQuery`, `MetricQuery`
//! and `TelemetryStore` of `packages/telemetry/src/index.ts`), independent of the recording adapter.

use crate::context::{SpanAttributes, SpanStatus};
use crate::metrics::InstrumentKind;

/// A settled span. Times are milliseconds at microsecond resolution; event times are relative to the start.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSpan {
	/// The globally unique span id.
	pub id: String,
	/// The trace id: the root's span id, inherited across processes.
	pub trace_id: String,
	/// The parent span's id, local or remote.
	pub parent_id: Option<String>,
	/// The span name.
	pub name: String,
	/// Wall-clock start, milliseconds since the epoch (`telemetry_now`).
	pub started_ms: f64,
	/// Wall-clock end; the duration comes from a monotonic clock.
	pub ended_ms: f64,
	/// The merged attributes.
	pub attributes: SpanAttributes,
	/// The events in order.
	pub events: Vec<StoredEvent>,
	/// The links in order.
	pub links: Vec<StoredLink>,
	/// The final status.
	pub status: SpanStatus,
}

/// An event of a stored span.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
	/// The event name.
	pub name: String,
	/// Milliseconds after the span's start.
	pub time_ms: f64,
	/// The event's attributes.
	pub attributes: SpanAttributes,
}

/// A link of a stored span.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredLink {
	/// The target's trace.
	pub trace_id: String,
	/// The target span.
	pub span_id: String,
	/// The link's attributes; `None` where the store keeps none.
	pub attributes: Option<SpanAttributes>,
}

/// The locally stored spans of one trace.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredTrace {
	/// The trace id.
	pub trace_id: String,
	/// Its spans, earliest start first.
	pub spans: Vec<StoredSpan>,
}

/// Filters of [`TelemetryStore::find_traces`]; every filter set must match.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TraceQuery {
	/// The name of the earliest stored span.
	pub name: Option<String>,
	/// Equalities one span of the trace must all match; array values never match (TS compares with `===`).
	pub attributes: Option<SpanAttributes>,
	/// Only traces with an error span.
	pub errors: bool,
	/// Inclusive lower bound on the earliest stored span's start.
	pub start_min: Option<f64>,
	/// Inclusive upper bound on the earliest stored span's start.
	pub start_max: Option<f64>,
	/// The page size; 20 when `None`.
	pub limit: Option<usize>,
	/// The continuation of the previous page, with the same filters.
	pub cursor: Option<String>,
}

/// One page of traces, newest start first.
#[derive(Debug, Clone, PartialEq)]
pub struct TracePage {
	/// The traces.
	pub traces: Vec<StoredTrace>,
	/// The continuation, when more traces match.
	pub cursor: Option<String>,
}

/// Filters of [`TelemetryStore::find_metrics`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetricQuery {
	/// The instrument name.
	pub name: String,
	/// Inclusive lower bound on the interval start, epoch milliseconds.
	pub start_min: Option<u64>,
	/// Inclusive upper bound on the interval start, epoch milliseconds.
	pub start_max: Option<u64>,
	/// Equalities the measurement attributes must match.
	pub attributes: Option<SpanAttributes>,
}

/// A metric row of one series and interval.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMetric {
	/// The interval start, epoch milliseconds.
	pub interval_ms: u64,
	/// The instrument name.
	pub name: String,
	/// The instrument kind.
	pub kind: InstrumentKind,
	/// The unit.
	pub unit: Option<String>,
	/// The series attributes.
	pub attributes: SpanAttributes,
	/// The number of measurements.
	pub count: u64,
	/// Their sum.
	pub sum: f64,
	/// The smallest.
	pub min: f64,
	/// The largest.
	pub max: f64,
}

/// Why a store query failed.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryStoreError {
	/// The cursor is not one this store returned.
	#[error("invalid trace cursor: {0}")]
	InvalidCursor(String),
	/// The backend failed.
	#[error("telemetry store backend failed: {0}")]
	Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Reads settled traces and metrics without depending on a storage backend.
pub trait TelemetryStore: Send + Sync {
	/// Complete locally stored traces matching `query`, newest earliest-start first, equal starts by trace id.
	fn find_traces(&self, query: &TraceQuery) -> Result<TracePage, TelemetryStoreError>;

	/// The trace with `trace_id`, if any of its spans is stored.
	fn get_trace(&self, trace_id: &str) -> Result<Option<StoredTrace>, TelemetryStoreError>;

	/// Metric rows of the instrument `query.name` matching the filters.
	fn find_metrics(&self, query: &MetricQuery) -> Result<Vec<StoredMetric>, TelemetryStoreError>;
}
