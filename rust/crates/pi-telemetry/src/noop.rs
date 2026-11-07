//! The context used when telemetry is off (ports `packages/telemetry/src/noop.ts`): it records nothing and
//! inspects nothing, and its spans, instruments and propagation are shared values.

use crate::context::{SpanAttributes, SpanLink, SpanOptions, SpanStatus, TelemetryContext, TelemetrySpan};
use crate::metrics::{
	HistogramOptions, InstrumentOptions, TelemetryCounter, TelemetryHistogram, TelemetryMetrics, TelemetryUpDownCounter,
};
use crate::propagation::{TelemetryCarrier, TelemetryPropagation};
use std::sync::{Arc, LazyLock};

/// The no-op context, span, instruments and propagation in one stateless value.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopTelemetry;

/// The shared no-op context: its spans, metrics and propagation are no-ops too.
pub static NOOP_TELEMETRY_CONTEXT: NoopTelemetry = NoopTelemetry;

/// The shared no-op metrics: every instrument is one shared no-op.
pub static NOOP_TELEMETRY_METRICS: NoopTelemetry = NoopTelemetry;

/// The shared no-op propagation: injection gives `None`, extraction the no-op context.
pub static NOOP_TELEMETRY_PROPAGATION: NoopTelemetry = NoopTelemetry;

/// One shared handle and instrument, so no-op calls never allocate after the first.
static NOOP_SHARED: LazyLock<Arc<NoopTelemetry>> = LazyLock::new(|| Arc::new(NoopTelemetry));

impl TelemetryContext for NoopTelemetry {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		Some(&NOOP_TELEMETRY_METRICS)
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		Some(&NOOP_TELEMETRY_PROPAGATION)
	}

	fn start_span(&self, _options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		// A box of a zero-sized value does not allocate.
		Box::new(NoopTelemetry)
	}
}

impl TelemetrySpan for NoopTelemetry {
	fn add_link(&self, _link: SpanLink<'_>) {}

	fn add_event(&self, _name: &str, _attributes: SpanAttributes) {}

	fn set_attributes(&self, _attributes: SpanAttributes) {}

	fn set_status(&self, _status: SpanStatus) {}

	fn handle(&self) -> Arc<dyn TelemetryContext> {
		NOOP_SHARED.clone()
	}

	fn end(self: Box<Self>) {}
}

impl TelemetryMetrics for NoopTelemetry {
	fn counter(&self, _name: &str, _options: InstrumentOptions<'_>) -> Arc<dyn TelemetryCounter> {
		NOOP_SHARED.clone()
	}

	fn up_down_counter(&self, _name: &str, _options: InstrumentOptions<'_>) -> Arc<dyn TelemetryUpDownCounter> {
		NOOP_SHARED.clone()
	}

	fn histogram(&self, _name: &str, _options: HistogramOptions<'_>) -> Arc<dyn TelemetryHistogram> {
		NOOP_SHARED.clone()
	}
}

impl TelemetryCounter for NoopTelemetry {
	fn add(&self, _value: f64, _attributes: &SpanAttributes) {}
}

impl TelemetryUpDownCounter for NoopTelemetry {
	fn add(&self, _value: f64, _attributes: &SpanAttributes) {}
}

impl TelemetryHistogram for NoopTelemetry {
	fn record(&self, _value: f64, _attributes: &SpanAttributes, _context: Option<&dyn TelemetryContext>) {}
}

impl TelemetryPropagation for NoopTelemetry {
	fn inject(&self, _context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
		None
	}

	fn extract(&self, _carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
		NOOP_SHARED.clone()
	}
}
