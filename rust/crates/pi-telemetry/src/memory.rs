//! The backend-neutral reference adapter that records in process memory (ports `packages/telemetry/src/memory.ts`):
//! spans, links, events and measurements, readable as normalized snapshots and through [`TelemetryStore`], with
//! its own propagation carrier and the span clock `telemetry_now`.

use crate::context::{
	AttributeValue, SpanAttributes, SpanLink, SpanOptions, SpanStatus, TelemetryContext, TelemetrySpan,
};
use crate::metrics::{
	HistogramOptions, InstrumentKind, InstrumentOptions, TelemetryCounter, TelemetryHistogram, TelemetryMetrics,
	TelemetryUpDownCounter,
};
use crate::propagation::{TelemetryCarrier, TelemetryPropagation};
use crate::store::{
	MetricQuery, StoredEvent, StoredLink, StoredMetric, StoredSpan, StoredTrace, TelemetryStore, TelemetryStoreError,
	TracePage, TraceQuery,
};
use pi_json::{JsonValue, to_js_string};
use std::any::Any;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A recorded span as `get_spans` returns it: deterministic ids, carriers and order, no times.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetrySpan {
	/// The span's number in this recorder, from 1 in start order.
	pub id: u64,
	/// The local parent's number; `None` for roots and parents owned by another recorder.
	pub parent_id: Option<u64>,
	/// The span's carrier.
	pub context: TelemetryCarrier,
	/// The parent's carrier, local or remote; `None` for roots.
	pub parent_context: Option<TelemetryCarrier>,
	/// The links in order.
	pub links: Vec<RecordedTelemetryLink>,
	/// The span name.
	pub name: String,
	/// The merged attributes.
	pub attributes: SpanAttributes,
	/// The events in order.
	pub events: Vec<RecordedTelemetryEvent>,
	/// The current or final status.
	pub status: SpanStatus,
	/// Whether the span ended.
	pub settled: bool,
	/// The span's place in end order, from 1, once settled.
	pub end_sequence: Option<u64>,
}

/// A recorded event.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetryEvent {
	/// The event name.
	pub name: String,
	/// The event's attributes.
	pub attributes: SpanAttributes,
}

/// A recorded link.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetryLink {
	/// The target's carrier.
	pub context: TelemetryCarrier,
	/// The link's attributes.
	pub attributes: SpanAttributes,
}

/// A recorded measurement, in recording order.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetryMeasurement {
	/// The instrument kind.
	pub kind: InstrumentKind,
	/// The instrument name.
	pub name: String,
	/// The instrument's description.
	pub description: Option<String>,
	/// The instrument's unit.
	pub unit: Option<String>,
	/// A histogram's bucket hints.
	pub boundaries: Option<Vec<f64>>,
	/// The measurement.
	pub value: f64,
	/// The measurement's attributes.
	pub attributes: SpanAttributes,
	/// The carrier of the span it was recorded in (histogram exemplars).
	pub context: Option<TelemetryCarrier>,
}

/// The in-memory adapter: a recording root context that is also its own metrics, propagation and store. Storage
/// is unbounded; create one per test or recording scope.
pub struct InMemoryTelemetryContext {
	recorder: Arc<Recorder>,
}

const SCOPE_KEY: &str = "pi-memory-scope";
const SPAN_KEY: &str = "pi-memory-span";
const TRACE_KEY: &str = "pi-memory-trace";

impl InMemoryTelemetryContext {
	/// A fresh recorder with its own random scope.
	pub fn new() -> Self {
		Self::with_clocks(SYSTEM_CLOCKS)
	}

	fn with_clocks(clocks: Clocks) -> Self {
		Self {
			recorder: Arc::new_cyclic(|this| Recorder {
				this: this.clone(),
				scope: pi_json::uuid_v4(),
				clocks,
				state: Mutex::new(State::default()),
			}),
		}
	}

	/// Detached snapshots of every span in start order, open ones included.
	pub fn get_spans(&self) -> Vec<RecordedTelemetrySpan> {
		self.recorder.lock().spans.iter().map(SpanRecord::snapshot).collect()
	}

	/// Detached measurements in recording order, without aggregation.
	pub fn get_metrics(&self) -> Vec<RecordedTelemetryMeasurement> {
		let state = self.recorder.lock();
		state
			.measurements
			.iter()
			.map(|(_, measurement)| measurement.clone())
			.collect()
	}
}

impl Default for InMemoryTelemetryContext {
	fn default() -> Self {
		Self::new()
	}
}

impl TelemetryContext for InMemoryTelemetryContext {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		Some(self.recorder.as_ref())
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		Some(self.recorder.as_ref())
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		self.recorder.start(None, options)
	}
}

impl TelemetryMetrics for InMemoryTelemetryContext {
	fn counter(&self, name: &str, options: InstrumentOptions<'_>) -> Arc<dyn TelemetryCounter> {
		self.recorder.counter(name, options)
	}

	fn up_down_counter(&self, name: &str, options: InstrumentOptions<'_>) -> Arc<dyn TelemetryUpDownCounter> {
		self.recorder.up_down_counter(name, options)
	}

	fn histogram(&self, name: &str, options: HistogramOptions<'_>) -> Arc<dyn TelemetryHistogram> {
		self.recorder.histogram(name, options)
	}
}

impl TelemetryPropagation for InMemoryTelemetryContext {
	fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
		self.recorder.inject(context)
	}

	fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
		self.recorder.extract(carrier)
	}
}

/// The shared state behind every context, span and instrument of one adapter; also its capability object, so
/// every context returns the same metrics and propagation.
struct Recorder {
	/// The recorder itself, for contexts it creates; upgrading succeeds while any caller holds it.
	this: Weak<Recorder>,
	scope: String,
	clocks: Clocks,
	state: Mutex<State>,
}

/// The clocks a recorder reads; the system's, or fixed readings in this module's tests (TS mocks `Date.now` and
/// `performance.now` instead).
#[derive(Clone, Copy)]
struct Clocks {
	/// Span starts: `telemetry_now`.
	now: fn() -> f64,
	/// Event offsets and span ends: `performance.now()`.
	monotonic: fn() -> f64,
	/// Measurement intervals: `Date.now()`.
	wall: fn() -> u64,
}

const SYSTEM_CLOCKS: Clocks = Clocks {
	now: telemetry_now,
	monotonic: monotonic_ms,
	wall: wall_ms,
};

#[derive(Default)]
struct State {
	spans: Vec<SpanRecord>,
	/// Measurements with their ten-second interval start.
	measurements: Vec<(u64, RecordedTelemetryMeasurement)>,
	next_end_sequence: u64,
}

/// A span's recording state; `id` is its index plus one.
struct SpanRecord {
	id: u64,
	parent_id: Option<u64>,
	context: TelemetryCarrier,
	parent_context: Option<TelemetryCarrier>,
	links: Vec<RecordedTelemetryLink>,
	name: String,
	attributes: SpanAttributes,
	events: Vec<(f64, RecordedTelemetryEvent)>,
	status: SpanStatus,
	explicit_status: bool,
	settled: bool,
	end_sequence: Option<u64>,
	started_ms: f64,
	/// The monotonic clock at the start, for event offsets and the end.
	clock: f64,
	ended_ms: f64,
}

/// The parent a new span starts under.
struct Parent {
	context: TelemetryCarrier,
	local_id: Option<u64>,
}

impl Recorder {
	fn lock(&self) -> MutexGuard<'_, State> {
		// Nothing under the lock panics, so a poisoned lock still holds consistent state.
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn arc(&self) -> Arc<Recorder> {
		// invariant: `self` is reachable only through an `Arc<Recorder>` that is alive during this call.
		#[expect(clippy::expect_used, reason = "the recorder is alive while borrowed")]
		self.this.upgrade().expect("recorder alive")
	}

	fn start(&self, parent: Option<Parent>, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		let links = options
			.links
			.into_iter()
			.filter_map(|link| recorded_link(&link))
			.collect();
		let started_ms = (self.clocks.now)();
		let clock = (self.clocks.monotonic)();
		let mut state = self.lock();
		let index = state.spans.len();
		let id = index as u64 + 1;
		let trace = parent
			.as_ref()
			.and_then(|parent| parent.context.get(TRACE_KEY).cloned())
			.unwrap_or_else(|| format!("{}:{id}", self.scope));
		let context = TelemetryCarrier::from([
			(SCOPE_KEY.to_owned(), self.scope.clone()),
			(SPAN_KEY.to_owned(), id.to_string()),
			(TRACE_KEY.to_owned(), trace),
		]);
		state.spans.push(SpanRecord {
			id,
			parent_id: parent.as_ref().and_then(|parent| parent.local_id),
			context: context.clone(),
			parent_context: parent.map(|parent| parent.context),
			links,
			name: options.name,
			attributes: options.attributes,
			events: Vec::new(),
			status: SpanStatus::Ok,
			explicit_status: false,
			settled: false,
			end_sequence: None,
			started_ms,
			clock,
			ended_ms: started_ms,
		});
		drop(state);
		let target = SpanTarget {
			recorder: self.arc(),
			index,
			context,
		};
		Box::new(MemorySpan { target })
	}

	/// An instrument with a copy of its metadata.
	fn instrument(&self, kind: InstrumentKind, name: &str, options: HistogramOptions<'_>) -> Instrument {
		Instrument {
			recorder: self.arc(),
			kind,
			name: name.to_owned(),
			description: options.description.map(str::to_owned),
			unit: options.unit.map(str::to_owned),
			boundaries: options.boundaries.map(<[f64]>::to_vec),
		}
	}
}

impl TelemetryPropagation for Recorder {
	fn inject(&self, context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
		carrier_of(context)
	}

	fn extract(&self, carrier: Option<&TelemetryCarrier>) -> Arc<dyn TelemetryContext> {
		let parts = carrier
			.and_then(|carrier| Some((carrier.get(SCOPE_KEY)?, carrier.get(SPAN_KEY)?, carrier.get(TRACE_KEY)?)));
		let Some((scope, span, trace)) = parts else {
			return Arc::new(InMemoryTelemetryContext { recorder: self.arc() });
		};
		// Trust boundary: the carrier crossed a transport; its identifiers are opaque strings, and only this
		// recorder's own span numbers are read as local parents.
		let local_id = if *scope == self.scope { span.parse().ok() } else { None };
		let context = TelemetryCarrier::from([
			(SCOPE_KEY.to_owned(), scope.clone()),
			(SPAN_KEY.to_owned(), span.clone()),
			(TRACE_KEY.to_owned(), trace.clone()),
		]);
		Arc::new(ExtractedParent {
			recorder: self.arc(),
			context,
			local_id,
		})
	}
}

impl TelemetryMetrics for Recorder {
	fn counter(&self, name: &str, options: InstrumentOptions<'_>) -> Arc<dyn TelemetryCounter> {
		Arc::new(self.instrument(InstrumentKind::Counter, name, options.into()))
	}

	fn up_down_counter(&self, name: &str, options: InstrumentOptions<'_>) -> Arc<dyn TelemetryUpDownCounter> {
		Arc::new(self.instrument(InstrumentKind::UpDownCounter, name, options.into()))
	}

	fn histogram(&self, name: &str, options: HistogramOptions<'_>) -> Arc<dyn TelemetryHistogram> {
		Arc::new(self.instrument(InstrumentKind::Histogram, name, options))
	}
}

/// What a span, a span handle and the recorder share: the record's place and its carrier.
#[derive(Clone)]
struct SpanTarget {
	recorder: Arc<Recorder>,
	index: usize,
	context: TelemetryCarrier,
}

impl SpanTarget {
	/// Runs `change` on the record unless the span settled; calls after settlement are inert.
	fn update(&self, change: impl FnOnce(&mut SpanRecord)) {
		let mut state = self.recorder.lock();
		let record = &mut state.spans[self.index];
		if !record.settled {
			change(record);
		}
	}

	fn settled(&self) -> bool {
		self.recorder.lock().spans[self.index].settled
	}

	fn start_child(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		if self.settled() {
			return Box::new(InertSpan {
				recorder: self.recorder.clone(),
			});
		}
		let parent = Parent {
			context: self.context.clone(),
			local_id: Some(self.index as u64 + 1),
		};
		self.recorder.start(Some(parent), options)
	}

	/// Ends the record; a span dropped while its thread panics ends with an error unless a status was set.
	fn settle(&self, panicking: bool) {
		let mut state = self.recorder.lock();
		let sequence = state.next_end_sequence + 1;
		let record = &mut state.spans[self.index];
		if record.settled {
			return;
		}
		if panicking && !record.explicit_status {
			record.status = SpanStatus::Error(None);
		}
		record.settled = true;
		record.ended_ms = microseconds(record.started_ms + (self.recorder.clocks.monotonic)() - record.clock);
		record.end_sequence = Some(sequence);
		state.next_end_sequence = sequence;
	}
}

/// A recording span; it settles on `end` or drop.
struct MemorySpan {
	target: SpanTarget,
}

impl TelemetryContext for MemorySpan {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		Some(self.target.recorder.as_ref())
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		Some(self.target.recorder.as_ref())
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		self.target.start_child(options)
	}
}

impl TelemetrySpan for MemorySpan {
	fn add_link(&self, link: SpanLink<'_>) {
		if let Some(link) = recorded_link(&link) {
			self.target.update(|record| record.links.push(link));
		}
	}

	fn add_event(&self, name: &str, attributes: SpanAttributes) {
		let now = (self.target.recorder.clocks.monotonic)();
		self.target.update(|record| {
			let event = RecordedTelemetryEvent {
				name: name.to_owned(),
				attributes,
			};
			record.events.push((microseconds(now - record.clock), event));
		});
	}

	fn set_attributes(&self, attributes: SpanAttributes) {
		self.target.update(|record| record.attributes.merge(attributes));
	}

	fn set_status(&self, status: SpanStatus) {
		self.target.update(|record| {
			record.status = status;
			record.explicit_status = true;
		});
	}

	fn handle(&self) -> Arc<dyn TelemetryContext> {
		Arc::new(MemorySpanHandle {
			target: self.target.clone(),
		})
	}

	fn end(self: Box<Self>) {
		self.target.settle(false);
	}
}

impl Drop for MemorySpan {
	fn drop(&mut self) {
		// After `end` this finds the span settled and does nothing.
		self.target.settle(std::thread::panicking());
	}
}

/// A context referring to a span that outlives it: children are inert once the span settled.
struct MemorySpanHandle {
	target: SpanTarget,
}

impl TelemetryContext for MemorySpanHandle {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		Some(self.target.recorder.as_ref())
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		Some(self.target.recorder.as_ref())
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		self.target.start_child(options)
	}
}

/// A parent extracted from a carrier: local when the carrier names this recorder's scope, remote otherwise.
struct ExtractedParent {
	recorder: Arc<Recorder>,
	context: TelemetryCarrier,
	local_id: Option<u64>,
}

impl TelemetryContext for ExtractedParent {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		Some(self.recorder.as_ref())
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		Some(self.recorder.as_ref())
	}

	fn start_span(&self, options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		let parent = Parent {
			context: self.context.clone(),
			local_id: self.local_id,
		};
		self.recorder.start(Some(parent), options)
	}
}

/// A span started from a settled span: it records nothing but keeps the adapter's capabilities.
struct InertSpan {
	recorder: Arc<Recorder>,
}

impl TelemetryContext for InertSpan {
	fn metrics(&self) -> Option<&dyn TelemetryMetrics> {
		Some(self.recorder.as_ref())
	}

	fn propagation(&self) -> Option<&dyn TelemetryPropagation> {
		Some(self.recorder.as_ref())
	}

	fn start_span(&self, _options: SpanOptions<'_>) -> Box<dyn TelemetrySpan> {
		Box::new(InertSpan {
			recorder: self.recorder.clone(),
		})
	}
}

impl TelemetrySpan for InertSpan {
	fn add_link(&self, _link: SpanLink<'_>) {}

	fn add_event(&self, _name: &str, _attributes: SpanAttributes) {}

	fn set_attributes(&self, _attributes: SpanAttributes) {}

	fn set_status(&self, _status: SpanStatus) {}

	fn handle(&self) -> Arc<dyn TelemetryContext> {
		Arc::new(InertSpan {
			recorder: self.recorder.clone(),
		})
	}

	fn end(self: Box<Self>) {}
}

/// The carrier of a context this adapter can identify: one of its spans, span handles or extracted parents, from
/// any recorder (as TS's module-wide registry does); `None` for roots, inert spans and other adapters' contexts.
fn carrier_of(context: &dyn TelemetryContext) -> Option<TelemetryCarrier> {
	let context: &dyn Any = context;
	if let Some(span) = context.downcast_ref::<MemorySpan>() {
		Some(span.target.context.clone())
	} else if let Some(handle) = context.downcast_ref::<MemorySpanHandle>() {
		Some(handle.target.context.clone())
	} else {
		context
			.downcast_ref::<ExtractedParent>()
			.map(|parent| parent.context.clone())
	}
}

fn recorded_link(link: &SpanLink<'_>) -> Option<RecordedTelemetryLink> {
	Some(RecordedTelemetryLink {
		context: carrier_of(link.context)?,
		attributes: link.attributes.clone(),
	})
}

/// An instrument of the recorder; its metadata is copied when it is created.
struct Instrument {
	recorder: Arc<Recorder>,
	kind: InstrumentKind,
	name: String,
	description: Option<String>,
	unit: Option<String>,
	boundaries: Option<Vec<f64>>,
}

/// Measurements are kept per ten-second interval for store queries.
const METRIC_INTERVAL_MS: u64 = 10_000;

impl Instrument {
	fn record(&self, value: f64, attributes: &SpanAttributes, context: Option<&dyn TelemetryContext>) {
		let measurement = RecordedTelemetryMeasurement {
			kind: self.kind,
			name: self.name.clone(),
			description: self.description.clone(),
			unit: self.unit.clone(),
			boundaries: self.boundaries.clone(),
			value,
			attributes: attributes.clone(),
			context: context.and_then(carrier_of),
		};
		let interval = (self.recorder.clocks.wall)() / METRIC_INTERVAL_MS * METRIC_INTERVAL_MS;
		self.recorder.lock().measurements.push((interval, measurement));
	}
}

impl TelemetryCounter for Instrument {
	fn add(&self, value: f64, attributes: &SpanAttributes) {
		self.record(value, attributes, None);
	}
}

impl TelemetryUpDownCounter for Instrument {
	fn add(&self, value: f64, attributes: &SpanAttributes) {
		self.record(value, attributes, None);
	}
}

impl TelemetryHistogram for Instrument {
	fn record(&self, value: f64, attributes: &SpanAttributes, context: Option<&dyn TelemetryContext>) {
		Instrument::record(self, value, attributes, context);
	}
}

impl SpanRecord {
	fn snapshot(&self) -> RecordedTelemetrySpan {
		RecordedTelemetrySpan {
			id: self.id,
			parent_id: self.parent_id,
			context: self.context.clone(),
			parent_context: self.parent_context.clone(),
			links: self.links.clone(),
			name: self.name.clone(),
			attributes: self.attributes.clone(),
			events: self.events.iter().map(|(_, event)| event.clone()).collect(),
			status: self.status.clone(),
			settled: self.settled,
			end_sequence: self.end_sequence,
		}
	}

	fn stored(&self) -> StoredSpan {
		StoredSpan {
			id: carrier_id(&self.context),
			trace_id: self.context.get(TRACE_KEY).cloned().unwrap_or_default(),
			parent_id: self.parent_context.as_ref().map(carrier_id),
			name: self.name.clone(),
			started_ms: self.started_ms,
			ended_ms: self.ended_ms,
			attributes: self.attributes.clone(),
			events: self
				.events
				.iter()
				.map(|(time_ms, event)| StoredEvent {
					name: event.name.clone(),
					time_ms: *time_ms,
					attributes: event.attributes.clone(),
				})
				.collect(),
			links: self
				.links
				.iter()
				.map(|link| StoredLink {
					trace_id: link.context.get(TRACE_KEY).cloned().unwrap_or_default(),
					span_id: carrier_id(&link.context),
					attributes: Some(link.attributes.clone()),
				})
				.collect(),
			status: self.status.clone(),
		}
	}
}

/// The global span id of a carrier: `<scope>:<span>`.
fn carrier_id(carrier: &TelemetryCarrier) -> String {
	let part = |key| carrier.get(key).map_or("", String::as_str);
	format!("{}:{}", part(SCOPE_KEY), part(SPAN_KEY))
}

/// The default page size of `find_traces`.
const DEFAULT_TRACE_LIMIT: usize = 20;

impl TelemetryStore for InMemoryTelemetryContext {
	fn find_traces(&self, query: &TraceQuery) -> Result<TracePage, TelemetryStoreError> {
		let after = query.cursor.as_deref().map(parse_cursor).transpose()?;
		let mut traces: Vec<StoredTrace> = Vec::new();
		for span in self.stored_spans() {
			match traces.iter_mut().find(|trace| trace.trace_id == span.trace_id) {
				Some(trace) => trace.spans.push(span),
				None => traces.push(StoredTrace {
					trace_id: span.trace_id.clone(),
					spans: vec![span],
				}),
			}
		}
		traces.retain(|trace| trace_matches(trace, query, after.as_ref()));
		traces.sort_by(|a, b| {
			b.spans[0]
				.started_ms
				.total_cmp(&a.spans[0].started_ms)
				.then_with(|| a.trace_id.cmp(&b.trace_id))
		});
		let limit = query.limit.unwrap_or(DEFAULT_TRACE_LIMIT);
		let cursor = (traces.len() > limit).then(|| {
			let last = &traces[limit - 1];
			to_js_string(&JsonValue::from(vec![
				JsonValue::from(last.spans[0].started_ms),
				JsonValue::from(last.trace_id.clone()),
			]))
		});
		traces.truncate(limit);
		Ok(TracePage { traces, cursor })
	}

	fn get_trace(&self, trace_id: &str) -> Result<Option<StoredTrace>, TelemetryStoreError> {
		let spans: Vec<StoredSpan> = self
			.stored_spans()
			.into_iter()
			.filter(|span| span.trace_id == trace_id)
			.collect();
		Ok((!spans.is_empty()).then(|| StoredTrace {
			trace_id: trace_id.to_owned(),
			spans,
		}))
	}

	/// Raw measurements as count-one rows, without changing `get_metrics` snapshots.
	fn find_metrics(&self, query: &MetricQuery) -> Result<Vec<StoredMetric>, TelemetryStoreError> {
		let state = self.recorder.lock();
		let rows = state
			.measurements
			.iter()
			.filter(|(interval, measurement)| {
				measurement.name == query.name
					&& query.start_min.is_none_or(|min| *interval >= min)
					&& query.start_max.is_none_or(|max| *interval <= max)
					&& query
						.attributes
						.as_ref()
						.is_none_or(|wanted| scalars_match(wanted, &measurement.attributes))
			})
			.map(|(interval, measurement)| StoredMetric {
				interval_ms: *interval,
				name: measurement.name.clone(),
				kind: measurement.kind,
				unit: measurement.unit.clone(),
				attributes: measurement.attributes.clone(),
				count: 1,
				sum: measurement.value,
				min: measurement.value,
				max: measurement.value,
			})
			.collect();
		Ok(rows)
	}
}

impl InMemoryTelemetryContext {
	/// Settled spans, earliest start first, span id breaking ties.
	fn stored_spans(&self) -> Vec<StoredSpan> {
		let state = self.recorder.lock();
		let mut spans: Vec<StoredSpan> = state
			.spans
			.iter()
			.filter(|record| record.settled)
			.map(SpanRecord::stored)
			.collect();
		drop(state);
		spans.sort_by(|a, b| a.started_ms.total_cmp(&b.started_ms).then_with(|| a.id.cmp(&b.id)));
		spans
	}
}

/// Reads a cursor `find_traces` wrote: `[startedMs, traceId]` as JSON.
fn parse_cursor(cursor: &str) -> Result<(f64, String), TelemetryStoreError> {
	// Trust boundary: the cursor comes back from the caller.
	let invalid = || TelemetryStoreError::InvalidCursor(cursor.to_owned());
	let value = pi_json::parse(cursor).map_err(|_| invalid())?;
	serde_json::from_value(value).map_err(|_| invalid())
}

fn trace_matches(trace: &StoredTrace, query: &TraceQuery, after: Option<&(f64, String)>) -> bool {
	let first = &trace.spans[0];
	query.name.as_ref().is_none_or(|name| first.name == *name)
		&& query.start_min.is_none_or(|min| first.started_ms >= min)
		&& query.start_max.is_none_or(|max| first.started_ms <= max)
		&& after.is_none_or(|(started, trace_id)| {
			// TS parity: the cursor compares start times with `===`; they are stored values, not computed ones.
			#[expect(clippy::float_cmp, reason = "TS parity: `===` on stored start times")]
			let same_start = first.started_ms == *started;
			first.started_ms < *started || (same_start && trace.trace_id > *trace_id)
		}) && (!query.errors
		|| trace
			.spans
			.iter()
			.any(|span| matches!(span.status, SpanStatus::Error(_))))
		&& query
			.attributes
			.as_ref()
			.is_none_or(|wanted| trace.spans.iter().any(|span| scalars_match(wanted, &span.attributes)))
}

/// Whether every wanted scalar equals the attribute of the same name; arrays never match (TS compares with `===`).
fn scalars_match(wanted: &SpanAttributes, attributes: &SpanAttributes) -> bool {
	wanted
		.iter()
		.all(|(name, value)| attributes.get(name).is_some_and(|found| scalar_equal(value, found)))
}

fn scalar_equal(wanted: &AttributeValue, found: &AttributeValue) -> bool {
	match (wanted, found) {
		(AttributeValue::String(wanted), AttributeValue::String(found)) => wanted == found,
		#[expect(clippy::float_cmp, reason = "TS parity: `===` on numbers")]
		(AttributeValue::Number(wanted), AttributeValue::Number(found)) => wanted == found,
		(AttributeValue::Bool(wanted), AttributeValue::Bool(found)) => wanted == found,
		_ => false,
	}
}

/// The tolerance of the span clock: more than `Date.now()`'s whole-millisecond truncation, less than any wall clock
/// step worth following (a clock set, a sleep the monotonic clock did not count).
const TOLERANCE_MS: f64 = 10.0;

/// The wall-clock time of the monotonic clock's zero; `None` until the first reading.
static ANCHOR: Mutex<Option<f64>> = Mutex::new(None);

/// Now as milliseconds since the epoch, rounded to the microsecond: the monotonic clock plus an anchor to the wall
/// clock. While the two agree within 10 ms, times in one process keep their order and spacing to the microsecond;
/// past that the clock follows the wall clock again. Every adapter stamps span starts with it.
pub fn telemetry_now() -> f64 {
	let clock = monotonic_ms();
	#[expect(clippy::cast_precision_loss, reason = "epoch milliseconds stay below 2^53")]
	let wall = wall_ms() as f64;
	let mut anchor = ANCHOR.lock().unwrap_or_else(PoisonError::into_inner);
	anchored_now(&mut anchor, clock, wall)
}

/// `telemetry_now` for given clock readings, re-anchoring `anchor` when the clocks part by more than the tolerance.
fn anchored_now(anchor: &mut Option<f64>, clock: f64, wall: f64) -> f64 {
	let current = match *anchor {
		Some(anchor) if (anchor + clock - wall).abs() <= TOLERANCE_MS => anchor,
		_ => wall - clock,
	};
	*anchor = Some(current);
	microseconds(current + clock)
}

/// `ms` rounded to the microsecond, as span times are.
pub(crate) fn microseconds(ms: f64) -> f64 {
	(ms * 1000.0).round() / 1000.0
}

/// The process's monotonic clock zero (`performance.now()`'s origin).
static MONOTONIC_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

/// `performance.now()`: monotonic milliseconds with sub-millisecond precision.
fn monotonic_ms() -> f64 {
	MONOTONIC_ORIGIN.elapsed().as_secs_f64() * 1000.0
}

/// `Date.now()`: whole milliseconds since the epoch; 0 for a clock before the epoch.
fn wall_ms() -> u64 {
	let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
	u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::{Clocks, InMemoryTelemetryContext, anchored_now};
	use crate::context::{SpanAttributes, SpanOptions, SpanStatus, TelemetryContext};
	use crate::propagation::TelemetryPropagation;
	use crate::store::{StoredEvent, StoredSpan, StoredTrace, TelemetryStore, TraceQuery};
	use std::cell::Cell;

	thread_local! {
		static NOW: Cell<f64> = const { Cell::new(0.0) };
		static MONOTONIC: Cell<f64> = const { Cell::new(0.0) };
	}

	/// A recorder on this thread's fixed readings.
	fn mocked() -> InMemoryTelemetryContext {
		InMemoryTelemetryContext::with_clocks(Clocks {
			now: || NOW.get(),
			monotonic: || MONOTONIC.get(),
			wall: || 0,
		})
	}

	// TS: store.test.ts "records wall-clock boundaries and relative event times without changing normalized event
	// snapshots".
	#[test]
	fn records_wall_clock_boundaries_and_relative_event_times() {
		NOW.set(1000.0);
		MONOTONIC.set(100.0);
		let telemetry = mocked();
		let span = telemetry.start_span(SpanOptions::new("timed"));
		let carrier = telemetry.inject(span.as_ref()).unwrap();
		let id = format!("{}:{}", carrier["pi-memory-scope"], carrier["pi-memory-span"]);
		MONOTONIC.set(110.0);
		span.add_event("event", SpanAttributes::new().with("count", 1));
		MONOTONIC.set(125.0);
		span.end();
		let expected = StoredTrace {
			trace_id: id.clone(),
			spans: vec![StoredSpan {
				id: id.clone(),
				trace_id: id.clone(),
				parent_id: None,
				name: "timed".into(),
				started_ms: 1000.0,
				ended_ms: 1025.0,
				attributes: SpanAttributes::new(),
				events: vec![StoredEvent {
					name: "event".into(),
					time_ms: 10.0,
					attributes: SpanAttributes::new().with("count", 1),
				}],
				links: Vec::new(),
				status: SpanStatus::Ok,
			}],
		};
		assert_eq!(telemetry.get_trace(&id).unwrap(), Some(expected));
		assert_eq!(
			telemetry.get_spans()[0].events[0].attributes,
			SpanAttributes::new().with("count", 1)
		);
	}

	// TS: store-conformance.ts "pages complete traces without duplicates when trace starts tie".
	#[test]
	fn pages_complete_traces_without_duplicates_when_trace_starts_tie() {
		let telemetry = mocked();
		for index in 0..25 {
			NOW.set(20_000.0);
			let root = telemetry
				.start_span(SpanOptions::new("root").with_attributes(SpanAttributes::new().with("index", index)));
			NOW.set(20_001.0);
			root.start_span(SpanOptions::new("child")).end();
			root.end();
		}
		let query = |limit: Option<usize>, cursor: Option<String>| TraceQuery {
			name: Some("root".into()),
			limit,
			cursor,
			..TraceQuery::default()
		};
		let first = telemetry.find_traces(&query(None, None)).unwrap();
		assert_eq!(first.traces.len(), 20);
		let rest = telemetry.find_traces(&query(None, first.cursor.clone())).unwrap();
		assert_eq!(rest.traces.len(), 5);
		assert_eq!(rest.cursor, None);
		let all = telemetry.find_traces(&query(Some(100), None)).unwrap().traces;
		assert_eq!([first.traces, rest.traces].concat(), all);
		assert!(
			all.iter()
				.all(|trace| trace.spans.len() == 2 && trace.spans[0].started_ms.total_cmp(&20_000.0).is_eq())
		);
		let (mut ids, mut cursor) = (Vec::new(), None);
		loop {
			let page = telemetry.find_traces(&query(Some(3), cursor)).unwrap();
			ids.extend(page.traces.into_iter().map(|trace| trace.trace_id));
			cursor = page.cursor;
			if cursor.is_none() {
				break;
			}
		}
		assert_eq!(ids, all.iter().map(|trace| trace.trace_id.clone()).collect::<Vec<_>>());
	}

	// Ports the `telemetryNow` cases of `packages/telemetry/test/store.test.ts` on the pure clock function.
	#[test]
	fn counts_sub_millisecond_steps_of_the_monotonic_clock_while_the_wall_clock_stays_in_its_millisecond() {
		let mut anchor = None;
		let first = anchored_now(&mut anchor, 1000.25, 50_000.0);
		assert!((anchored_now(&mut anchor, 1000.2504, 50_000.0) - first).abs() < 1e-6);
		let later = anchored_now(&mut anchor, 1000.7512, 50_000.0);
		assert!((later - first - 0.501).abs() < 1e-6);
		assert_eq!((later * 1000.0).fract(), 0.0);
	}

	#[test]
	fn follows_a_step_of_the_wall_clock_past_10_ms() {
		let mut anchor = None;
		assert_eq!(anchored_now(&mut anchor, 1000.0, 80_000.0), 80_000.0);
		// Within 10 ms the anchored clock keeps its own spacing.
		assert_eq!(anchored_now(&mut anchor, 1003.5, 80_005.0), 80_003.5);
		assert_eq!(anchored_now(&mut anchor, 1004.0, 3_650_004.0), 3_650_004.0);
		assert_eq!(anchored_now(&mut anchor, 1004.125, 3_650_004.0), 3_650_004.125);
	}
}
