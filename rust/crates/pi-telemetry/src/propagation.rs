//! Cross-process parent identity (ports `TelemetryCarrier` and `TelemetryPropagation` of
//! `packages/telemetry/src/index.ts`).

use crate::context::TelemetryContext;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Opaque adapter-owned propagation data: a JSON-safe string map. Transports send the carrier, never a context.
pub type TelemetryCarrier = BTreeMap<String, String>;

/// An adapter's optional transport capability. Trace metadata is diagnostics, never authorization.
pub trait TelemetryPropagation: Send + Sync {
	/// A detached carrier for a span, a span handle or an extracted parent; `None` for roots, no-op and foreign
	/// contexts.
	fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier>;

	/// A receiver-owned parent for a carrier; absent or unrecognized carriers give the adapter's root.
	fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext>;
}
