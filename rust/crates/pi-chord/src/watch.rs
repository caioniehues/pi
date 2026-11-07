//! Watch handles (TS `watch.ts`): the watch types and the one implementation of their lifecycle, for JSON values
//! (`Watch`, over `Snapshot` revisions and their ops) and in-process values (`NativeWatch`).
//!
//! A watch delivers one listener call at a time; frames a source hands it meanwhile wait, folded, for the next call.
//! TS schedules delivery on the microtask queue; Rust has no runtime here, so `start` returns the delivery future and
//! the caller spawns it: the listener is only ever called from that future, never inline from `advance`. Abort is
//! observed by the futures that wait (delivery and `closed`): a `CancellationToken` has no callback to end the watch
//! the moment it is cancelled, so an aborted watch nobody waits on ends when one of them next polls.
//!
//! What the delivery future does when its owner does not run it to the end:
//! - never spawned, then dropped: the watch ends `Stopped` (nothing can deliver to the listener any more) and detaches;
//! - dropped while a listener call runs: the call is dropped with it, and the watch ends `Stopped`;
//! - the listener panics: the panic is caught at the delivery boundary and handled like a returned error: reported,
//!   and the watch ends `ListenerError` with the panic message.

use crate::context::{AbortSignal, Context, WaitForAbort, without_abort_signal};
use crate::delta::{Op, PendingOps, PendingTake, Snapshot};
use std::collections::VecDeque;
use std::fmt;
use std::future::{Future, poll_fn};
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context as TaskContext, Poll, Waker};

/// An error a listener returned or a source failed with.
pub type WatchError = Arc<dyn std::error::Error + Send + Sync>;

/// Terminal result of one watch. A watch ends only for good; a failure its source recovers from is a
/// `WatchCall::Failed`, and the watch stays open.
#[derive(Clone, Debug)]
pub enum WatchEnd {
	/// `stop()` was called.
	Stopped,
	/// The context the watch was acquired with was aborted.
	Aborted,
	/// The source closed (its owner ended or closed).
	Ended,
	/// The listener returned this error.
	ListenerError(WatchError),
	/// The source failed for good with this error.
	Failed(WatchError),
}

/// One call of a JSON watch's listener, and one frame a source hands its watch.
#[derive(Clone, Debug)]
pub enum WatchCall {
	/// The newest value, the ops that turn the previous value into it, and whether the document the watch follows was
	/// retired since the previous value call (the watch goes on: `value` is what the address shows after it, and a
	/// document created there later arrives as `[["r", value]]`).
	Value {
		/// The newest value.
		value: Snapshot,
		/// The ops from the previously delivered value.
		ops: Arc<[Op]>,
		/// A retirement happened since the previous value call.
		retired: bool,
	},
	/// The source cannot compute its value now; the next value arrives as `[["r", value]]`.
	Failed(WatchError),
}

/// One call of a native watch's listener: the new value, or a failure its source may recover from.
#[derive(Clone, Debug)]
pub enum NativeWatchCall<T> {
	/// The new value.
	Value(T),
	/// The source cannot compute its value now.
	Failed(WatchError),
}

/// Why `start` or `observe_abort` refused; messages are the TS ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WatchUsageError {
	/// `start` was called before.
	#[error("Watch is already started")]
	AlreadyStarted,
	/// The watch already ended.
	#[error("Watch is stopped")]
	Stopped,
	/// `observe_abort` was called before.
	#[error("Watch abort is already installed")]
	AbortInstalled,
}

/// The future that delivers a started watch's calls; spawn it. It returns once the watch ended and no call runs;
/// dropped before that, it ends the watch `Stopped`.
pub struct WatchDelivery {
	/// `None` once it returned or caught a panic.
	future: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
	lifecycle: Arc<dyn Closing>,
}

/// Serialized observation of an immutable JSON value: one listener call at a time, changes meanwhile folded.
pub trait WatchHandle: Send + Sync {
	/// The acquisition revision before start; the latest delivered revision afterward (a failed call keeps it).
	fn value(&self) -> Snapshot;

	/// Installs the sole listener and returns the future that calls it.
	fn start<L, F>(&self, listener: L) -> Result<WatchDelivery, WatchUsageError>
	where
		L: FnMut(WatchCall, Context) -> F + Send + 'static,
		F: Future<Output = Result<(), WatchError>> + Send + 'static;

	/// Ends the watch now (idempotently) and resolves to its end; a running call keeps running.
	fn stop(&self) -> WatchClosed;

	/// Resolves to the watch's end once it ended.
	fn closed(&self) -> WatchClosed;
}

/// Serialized observation of an in-process value: as `WatchHandle`, but each new value is its own call, in order.
pub trait NativeWatchHandle<T>: Send + Sync {
	/// The acquisition value before start; the latest delivered value afterward (a failed call keeps it).
	fn value(&self) -> T;

	/// Installs the sole listener and returns the future that calls it.
	fn start<L, F>(&self, listener: L) -> Result<WatchDelivery, WatchUsageError>
	where
		L: FnMut(NativeWatchCall<T>, Context) -> F + Send + 'static,
		F: Future<Output = Result<(), WatchError>> + Send + 'static;

	/// Ends the watch now (idempotently) and resolves to its end; a running call keeps running.
	fn stop(&self) -> WatchClosed;

	/// Resolves to the watch's end once it ended.
	fn closed(&self) -> WatchClosed;
}

/// A JSON value that changes and can be watched (documents, derives, members of references).
pub trait Watchable {
	/// The handle `watch` returns.
	type Handle: WatchHandle;

	/// The current value.
	fn value(&self) -> Snapshot;

	/// A watch holding the current value, then its changes; aborting `context` ends it.
	fn watch(&self, context: &Context) -> impl Future<Output = Result<Self::Handle, WatchError>> + Send;
}

/// An in-process value that changes and can be watched (it may hold anything): a change is a new value, without ops.
pub trait NativeWatchable<T> {
	/// The handle `watch` returns.
	type Handle: NativeWatchHandle<T>;

	/// The current value.
	fn value(&self) -> T;

	/// A watch holding the current value, then one call per new value; aborting `context` ends it.
	fn watch(&self, context: &Context) -> impl Future<Output = Result<Self::Handle, WatchError>> + Send;
}

/// The watch handle of a JSON source. Frames that arrive before `start` or while the listener runs wait in one
/// `PendingOps` queue; the next call gets the newest value, the folded ops of every frame since the previous call (or
/// `[["r", newest]]` after the queue overflowed), `retired` when any of them was a retirement, and the newest frame's
/// context without its abort signal. A frame delivered alone is passed on as the source made it. A failure drops the
/// pending frames and waits as one failed call; the first value call after it is `[["r", value]]`.
#[derive(Clone)]
pub struct Watch(Arc<Shared<JsonPending>>);

/// The watch handle of a native source: every value in order, one call each; past 100 pending values they collapse to
/// one, `replace()` or by default the newest. A failure drops the pending values and waits as one failed call.
pub struct NativeWatch<T: Clone + Send + 'static>(Arc<Shared<NativePending<T>>>);

/// The future of `stop` and `closed`.
pub struct WatchClosed {
	lifecycle: Arc<dyn Closing>,
	abort: Option<WaitForAbort>,
}

impl Watch {
	/// A watch at `value`; `detach` unsubscribes it from its source when it ends, `report` gets a listener's error,
	/// which also ends it.
	pub fn new(
		value: Snapshot,
		detach: impl FnOnce() + Send + 'static,
		report: impl Fn(&WatchError) + Send + Sync + 'static,
	) -> Self {
		Watch(Shared::new(value, JsonPending::default(), detach, report))
	}

	/// Queues the frame `call`: a value with the ops from the previous one and whether it was a retirement, or a
	/// failure (as `fail`). Ignored once the watch ended or its source ended.
	pub fn advance(&self, call: WatchCall, context: Context) {
		let mut state = self.0.lock();
		if !state.open() {
			return;
		}
		match call {
			WatchCall::Failed(error) => state.pending.fail(error, context),
			WatchCall::Value { value, ops, retired } => state.pending.advance(value, ops, retired, context),
		}
		state.wake_delivery();
	}

	/// Queues a failure the source may recover from: pending frames are dropped (`value` keeps the last delivered
	/// value), the listener gets `WatchCall::Failed`, and the next value arrives as `[["r", value]]`.
	pub fn fail(&self, error: WatchError, context: Context) {
		self.advance(WatchCall::Failed(error), context);
	}

	/// Ends the watch `aborted` when `signal` aborts (observed by the futures that wait on the watch); installed once.
	pub fn observe_abort(&self, signal: AbortSignal) -> Result<(), WatchUsageError> {
		self.0.observe_abort(signal)
	}

	/// Ends the watch `aborted`: its acquisition context was aborted.
	pub fn abort(&self) {
		self.0.terminate(WatchEnd::Aborted);
	}

	/// Ends the watch with `end` at once, dropping what is pending: the source closed or failed for good.
	pub fn end(&self, end: WatchEnd) {
		self.0.terminate(end);
	}

	/// The source sends nothing more: take no more frames, deliver what is pending, then end `ended` after the running
	/// call returns. A watch never started keeps what is pending until `start`, `stop`, `end` or an abort.
	pub fn end_after_delivery(&self) {
		self.0.end_after_delivery();
	}
}

impl WatchHandle for Watch {
	fn value(&self) -> Snapshot {
		self.0.lock().value.clone()
	}

	fn start<L, F>(&self, listener: L) -> Result<WatchDelivery, WatchUsageError>
	where
		L: FnMut(WatchCall, Context) -> F + Send + 'static,
		F: Future<Output = Result<(), WatchError>> + Send + 'static,
	{
		self.0.start(listener)
	}

	fn stop(&self) -> WatchClosed {
		self.0.terminate(WatchEnd::Stopped);
		self.closed()
	}

	fn closed(&self) -> WatchClosed {
		WatchClosed {
			lifecycle: self.0.clone(),
			abort: None,
		}
	}
}

impl fmt::Debug for Watch {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("Watch")
			.field("value", &self.value())
			.finish_non_exhaustive()
	}
}

impl<T: Clone + Send + 'static> NativeWatch<T> {
	/// A watch at `value`, as `Watch::new`; past 100 pending values the newest is kept.
	pub fn new(
		value: T,
		detach: impl FnOnce() + Send + 'static,
		report: impl Fn(&WatchError) + Send + Sync + 'static,
	) -> Self {
		NativeWatch(Shared::new(value, NativePending::new(None), detach, report))
	}

	/// A watch whose overflow keeps `replace()` instead of the newest value.
	pub fn with_replace(
		value: T,
		detach: impl FnOnce() + Send + 'static,
		report: impl Fn(&WatchError) + Send + Sync + 'static,
		replace: impl Fn() -> T + Send + Sync + 'static,
	) -> Self {
		NativeWatch(Shared::new(
			value,
			NativePending::new(Some(Arc::new(replace))),
			detach,
			report,
		))
	}

	/// Queues `value`, the source's new value; it replaces a pending failure.
	pub fn advance(&self, value: T, context: Context) {
		let replace = {
			let state = self.0.lock();
			if !state.open() {
				return;
			}
			state.pending.overflow_replace()
		};
		// `replace` is the source's code: it runs outside the watch's lock, so it may call into the watch.
		let value = replace.map_or(value, |replace| replace());
		let mut state = self.0.lock();
		if !state.open() {
			return;
		}
		state.pending.advance(value, context);
		state.wake_delivery();
	}

	/// Queues a failure the source may recover from: pending values are dropped and the listener gets
	/// `NativeWatchCall::Failed`.
	pub fn fail(&self, error: WatchError, context: Context) {
		let mut state = self.0.lock();
		if !state.open() {
			return;
		}
		state.pending.fail(error, context);
		state.wake_delivery();
	}

	/// As `Watch::observe_abort`.
	pub fn observe_abort(&self, signal: AbortSignal) -> Result<(), WatchUsageError> {
		self.0.observe_abort(signal)
	}

	/// As `Watch::abort`.
	pub fn abort(&self) {
		self.0.terminate(WatchEnd::Aborted);
	}

	/// As `Watch::end`.
	pub fn end(&self, end: WatchEnd) {
		self.0.terminate(end);
	}

	/// As `Watch::end_after_delivery`.
	pub fn end_after_delivery(&self) {
		self.0.end_after_delivery();
	}
}

impl<T: Clone + Send + 'static> Clone for NativeWatch<T> {
	fn clone(&self) -> Self {
		NativeWatch(self.0.clone())
	}
}

impl<T: Clone + Send + 'static> NativeWatchHandle<T> for NativeWatch<T> {
	fn value(&self) -> T {
		self.0.lock().value.clone()
	}

	fn start<L, F>(&self, listener: L) -> Result<WatchDelivery, WatchUsageError>
	where
		L: FnMut(NativeWatchCall<T>, Context) -> F + Send + 'static,
		F: Future<Output = Result<(), WatchError>> + Send + 'static,
	{
		self.0.start(listener)
	}

	fn stop(&self) -> WatchClosed {
		self.0.terminate(WatchEnd::Stopped);
		self.closed()
	}

	fn closed(&self) -> WatchClosed {
		WatchClosed {
			lifecycle: self.0.clone(),
			abort: None,
		}
	}
}

impl Future for WatchDelivery {
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
		let this = self.get_mut();
		let Some(future) = this.future.as_mut() else {
			return Poll::Ready(());
		};
		// A panicking listener leaves the future unusable; it is dropped, never polled again.
		match std::panic::catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
			Ok(Poll::Pending) => Poll::Pending,
			Ok(Poll::Ready(())) => {
				this.future = None;
				Poll::Ready(())
			}
			Err(payload) => {
				this.future = None;
				let message = payload
					.downcast_ref::<&str>()
					.map(|text| (*text).to_owned())
					.or_else(|| payload.downcast_ref::<String>().cloned())
					.unwrap_or_else(|| "a non-string payload".to_owned());
				let error: WatchError = Arc::new(std::io::Error::other(format!("listener panicked: {message}")));
				this.lifecycle.listener_failed(error);
				Poll::Ready(())
			}
		}
	}
}

impl Drop for WatchDelivery {
	fn drop(&mut self) {
		if self.future.take().is_some() {
			self.lifecycle.abandon();
		}
	}
}

impl fmt::Debug for WatchDelivery {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("WatchDelivery")
			.field("running", &self.future.is_some())
			.finish_non_exhaustive()
	}
}

impl Future for WatchClosed {
	type Output = WatchEnd;

	fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<WatchEnd> {
		let this = self.get_mut();
		this.lifecycle.poll_closed(cx, &mut this.abort)
	}
}

/// What is pending behind one watch and how it becomes the next call (TS `pendingContext`, `next`, `release`).
trait Pending: Send + 'static {
	type Value: Clone + Send;
	type Call: Send;

	/// The context of the newest frame the next call covers, or `None` when nothing is pending.
	fn pending_context(&self) -> Option<&Context>;

	/// Takes the next call with the value it delivers, if it is a value call; `None` when nothing is pending.
	fn next(&mut self) -> Option<(Self::Call, Option<Self::Value>)>;

	/// Drops everything pending; called once, on termination.
	fn release(&mut self);
}

/// The lifecycle both handles share (TS `WatchLifecycle`): serialized delivery and termination.
struct Shared<P: Pending> {
	state: Mutex<State<P>>,
}

struct State<P: Pending> {
	value: P::Value,
	pending: P,
	delivery: Delivery,
	/// Set by `end_after_delivery`: the source sends nothing more.
	ending: bool,
	/// Set once, by `terminate`.
	end: Option<WatchEnd>,
	detach: Option<Box<dyn FnOnce() + Send>>,
	report: Option<Report>,
	abort: Abort,
	/// The delivery future, while it waits for a frame.
	delivery_waker: Option<Waker>,
	/// `closed` futures waiting for the end.
	closed: Vec<Waker>,
}

type Report = Arc<dyn Fn(&WatchError) + Send + Sync>;

/// Where delivery stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Delivery {
	Unstarted,
	Idle,
	/// A listener call is running.
	Running,
}

/// The acquisition signal the watch ends `aborted` on.
enum Abort {
	NotInstalled,
	Watching(AbortSignal),
	/// Installed, then dropped when the watch ended.
	Released,
}

impl<P: Pending> State<P> {
	/// The watch takes frames: it has not ended, and its source has not ended.
	fn open(&self) -> bool {
		self.end.is_none() && !self.ending
	}

	fn wake_delivery(&mut self) {
		if let Some(waker) = self.delivery_waker.take() {
			waker.wake();
		}
	}

	fn wake_closed(&mut self) {
		for waker in self.closed.drain(..) {
			waker.wake();
		}
	}
}

/// What `WatchClosed` needs of a lifecycle, whatever its pending queue.
trait Closing: Send + Sync {
	fn poll_closed(&self, cx: &mut TaskContext<'_>, abort: &mut Option<WaitForAbort>) -> Poll<WatchEnd>;

	/// Ends the watch as a listener error: the listener panicked with `error`.
	fn listener_failed(&self, error: WatchError);

	/// Ends the watch `Stopped`: its delivery future was dropped.
	fn abandon(&self);
}

impl<P: Pending> Shared<P> {
	fn new(
		value: P::Value,
		pending: P,
		detach: impl FnOnce() + Send + 'static,
		report: impl Fn(&WatchError) + Send + Sync + 'static,
	) -> Arc<Self> {
		Arc::new(Shared {
			state: Mutex::new(State {
				value,
				pending,
				delivery: Delivery::Unstarted,
				ending: false,
				end: None,
				detach: Some(Box::new(detach)),
				report: Some(Arc::new(report)),
				abort: Abort::NotInstalled,
				delivery_waker: None,
				closed: Vec::new(),
			}),
		})
	}

	fn lock(&self) -> MutexGuard<'_, State<P>> {
		// A panic while the lock is held leaves the state consistent (every update is a single assignment), so a
		// poisoned lock is taken as it is.
		self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
	}

	fn start<L, F>(self: &Arc<Self>, mut listener: L) -> Result<WatchDelivery, WatchUsageError>
	where
		L: FnMut(P::Call, Context) -> F + Send + 'static,
		F: Future<Output = Result<(), WatchError>> + Send + 'static,
	{
		{
			let mut state = self.lock();
			if state.delivery != Delivery::Unstarted {
				return Err(WatchUsageError::AlreadyStarted);
			}
			if state.end.is_some() {
				return Err(WatchUsageError::Stopped);
			}
			state.delivery = Delivery::Idle;
		}
		let shared = self.clone();
		let future = Box::pin(async move {
			let mut abort = None;
			while let Some((call, context)) = poll_fn(|cx| shared.poll_next(cx, &mut abort)).await {
				let result = listener(call, without_abort_signal(&context)).await;
				drop(context);
				shared.finish_call(result);
			}
		});
		Ok(WatchDelivery {
			future: Some(future),
			lifecycle: self.clone(),
		})
	}

	/// The next call once one is pending; `None` once the watch ended (an ending source ends it here, after the rest
	/// was delivered).
	fn poll_next(
		&self,
		cx: &mut TaskContext<'_>,
		abort: &mut Option<WaitForAbort>,
	) -> Poll<Option<(P::Call, Context)>> {
		let mut state = self.lock();
		if state.end.is_some() {
			return Poll::Ready(None);
		}
		if poll_abort(&state, cx, abort) {
			drop(state);
			self.terminate(WatchEnd::Aborted);
			return Poll::Ready(None);
		}
		if let Some(context) = state.pending.pending_context().cloned()
			&& let Some((call, value)) = state.pending.next()
		{
			if let Some(value) = value {
				state.value = value;
			}
			state.delivery = Delivery::Running;
			return Poll::Ready(Some((call, context)));
		}
		if state.ending {
			drop(state);
			self.terminate(WatchEnd::Ended);
			return Poll::Ready(None);
		}
		state.delivery_waker = Some(cx.waker().clone());
		Poll::Pending
	}

	/// After a call returned: a listener error ends the watch and is reported, unless it already ended.
	fn finish_call(&self, result: Result<(), WatchError>) {
		let report = {
			let mut state = self.lock();
			state.delivery = Delivery::Idle;
			match result {
				Err(error) if state.end.is_none() => state.report.clone().map(|report| (report, error)),
				_ => None,
			}
		};
		if let Some((report, error)) = report {
			report(&error);
			self.terminate(WatchEnd::ListenerError(error));
		}
	}

	fn observe_abort(&self, signal: AbortSignal) -> Result<(), WatchUsageError> {
		let aborted = {
			let mut state = self.lock();
			if !matches!(state.abort, Abort::NotInstalled) {
				return Err(WatchUsageError::AbortInstalled);
			}
			if state.end.is_some() {
				state.abort = Abort::Released;
				return Ok(());
			}
			let aborted = signal.is_aborted();
			state.abort = Abort::Watching(signal);
			// The futures that wait pick the signal up on their next poll.
			state.wake_delivery();
			state.wake_closed();
			aborted
		};
		if aborted {
			self.terminate(WatchEnd::Aborted);
		}
		Ok(())
	}

	fn end_after_delivery(&self) {
		let mut state = self.lock();
		if state.end.is_some() || state.ending {
			return;
		}
		state.ending = true;
		if state.delivery != Delivery::Running && state.pending.pending_context().is_none() {
			drop(state);
			self.terminate(WatchEnd::Ended);
			return;
		}
		state.wake_delivery();
	}

	/// Ends the watch, once: drops what is pending and every reference into the source, settles `closed`, then
	/// detaches outside the lock (a source's detach may call back into the watch). A running call keeps running.
	fn terminate(&self, end: WatchEnd) {
		let detach = {
			let mut state = self.lock();
			if state.end.is_some() {
				return;
			}
			state.end = Some(end);
			state.report = None;
			if matches!(state.abort, Abort::Watching(_)) {
				state.abort = Abort::Released;
			}
			state.pending.release();
			state.wake_delivery();
			state.wake_closed();
			state.detach.take()
		};
		if let Some(detach) = detach {
			detach();
		}
	}
}

impl<P: Pending> Closing for Shared<P> {
	fn listener_failed(&self, error: WatchError) {
		self.finish_call(Err(error));
	}

	fn abandon(&self) {
		self.terminate(WatchEnd::Stopped);
	}

	fn poll_closed(&self, cx: &mut TaskContext<'_>, abort: &mut Option<WaitForAbort>) -> Poll<WatchEnd> {
		let mut state = self.lock();
		if let Some(end) = &state.end {
			return Poll::Ready(end.clone());
		}
		if poll_abort(&state, cx, abort) {
			drop(state);
			self.terminate(WatchEnd::Aborted);
			return Poll::Ready(WatchEnd::Aborted);
		}
		state.closed.push(cx.waker().clone());
		Poll::Pending
	}
}

/// True once the installed abort signal fired; otherwise registers `cx` with it (the waiter is the caller's, created
/// on the first poll after the signal was installed).
fn poll_abort<P: Pending>(state: &State<P>, cx: &mut TaskContext<'_>, abort: &mut Option<WaitForAbort>) -> bool {
	let Abort::Watching(signal) = &state.abort else {
		return false;
	};
	let waiter = abort.get_or_insert_with(|| signal.aborted());
	Pin::new(waiter).poll(cx).is_ready()
}

/// The pending frames of a JSON watch (TS `Watch`'s fields).
#[derive(Default)]
struct JsonPending {
	ops: PendingOps,
	/// The source's call while it is the only frame pending: passed on as it is.
	lone: Option<WatchCall>,
	/// The newest frame's value and context while a call is pending; the context also marks that one is.
	newest_value: Option<Snapshot>,
	newest_context: Option<Context>,
	retired: bool,
	/// The newest frame was a failure: the next call is this.
	failure: Option<WatchError>,
	/// A failure came after the last value call: the next value call replaces the root, and ops are not kept.
	replace: bool,
}

impl JsonPending {
	fn advance(&mut self, value: Snapshot, ops: Arc<[Op]>, retired: bool, context: Context) {
		if self.replace {
			self.failure = None;
		} else {
			self.ops.push(&ops);
			self.lone = if self.newest_context.is_none() {
				Some(WatchCall::Value {
					value: value.clone(),
					ops,
					retired,
				})
			} else {
				None
			};
		}
		self.newest_value = Some(value);
		self.newest_context = Some(context);
		self.retired |= retired;
	}

	fn fail(&mut self, error: WatchError, context: Context) {
		self.ops.take();
		self.lone = None;
		self.newest_value = None;
		self.failure = Some(error);
		self.newest_context = Some(context);
		self.replace = true;
	}

	/// The pending failure, or the value after one as a root replacement.
	fn next_after_failure(&mut self) -> (WatchCall, Option<Snapshot>) {
		self.newest_context = None;
		if let Some(error) = self.failure.take() {
			return (WatchCall::Failed(error), None);
		}
		let value = self.newest_value.take().unwrap_or_default();
		let retired = std::mem::take(&mut self.retired);
		self.replace = false;
		let ops = Arc::from([Op::Replace(value.to_value())]);
		(
			WatchCall::Value {
				value: value.clone(),
				ops,
				retired,
			},
			Some(value),
		)
	}
}

impl Pending for JsonPending {
	type Value = Snapshot;
	type Call = WatchCall;

	fn pending_context(&self) -> Option<&Context> {
		self.newest_context.as_ref()
	}

	fn next(&mut self) -> Option<(WatchCall, Option<Snapshot>)> {
		if self.replace {
			return Some(self.next_after_failure());
		}
		// Set with the context, which says a frame is pending.
		let value = self.newest_value.take().unwrap_or_default();
		let batch = self.ops.take();
		let retired = std::mem::take(&mut self.retired);
		self.newest_context = None;
		// One frame never folds or overflows: its call is the listener's.
		if let Some(lone) = self.lone.take() {
			return Some((lone, Some(value)));
		}
		let ops = match batch {
			PendingTake::Ops(ops) => ops,
			PendingTake::Replace => Arc::from([Op::Replace(value.to_value())]),
		};
		Some((
			WatchCall::Value {
				value: value.clone(),
				ops,
				retired,
			},
			Some(value),
		))
	}

	fn release(&mut self) {
		*self = JsonPending::default();
	}
}

/// Values pending behind one native watch past which they collapse to one (TS `MAX_PENDING_NATIVE_VALUES`).
const MAX_PENDING_NATIVE_VALUES: usize = 100;

/// The pending values of a native watch, each with its frame's context, or one failure.
struct NativePending<T> {
	values: VecDeque<(T, Context)>,
	replace: Option<Replace<T>>,
	failure: Option<(WatchError, Context)>,
}

/// What a native watch keeps instead of its newest value on overflow.
type Replace<T> = Arc<dyn Fn() -> T + Send + Sync>;

impl<T> NativePending<T> {
	fn new(replace: Option<Replace<T>>) -> Self {
		Self {
			values: VecDeque::new(),
			replace,
			failure: None,
		}
	}

	/// The replacement to compute for the next value, when it overflows the queue.
	fn overflow_replace(&self) -> Option<Replace<T>> {
		self.replace
			.clone()
			.filter(|_| self.values.len() >= MAX_PENDING_NATIVE_VALUES)
	}

	/// Queues `value`; past the cap the queue collapses to it (the replacement already, when one is set).
	fn advance(&mut self, value: T, context: Context) {
		self.failure = None;
		if self.values.len() >= MAX_PENDING_NATIVE_VALUES {
			self.values.clear();
		}
		self.values.push_back((value, context));
	}

	fn fail(&mut self, error: WatchError, context: Context) {
		self.values.clear();
		self.failure = Some((error, context));
	}
}

impl<T: Clone + Send + 'static> Pending for NativePending<T> {
	type Value = T;
	type Call = NativeWatchCall<T>;

	fn pending_context(&self) -> Option<&Context> {
		match &self.failure {
			Some((_, context)) => Some(context),
			None => self.values.front().map(|(_, context)| context),
		}
	}

	fn next(&mut self) -> Option<(NativeWatchCall<T>, Option<T>)> {
		if let Some((error, _)) = self.failure.take() {
			return Some((NativeWatchCall::Failed(error), None));
		}
		let (value, _) = self.values.pop_front()?;
		Some((NativeWatchCall::Value(value.clone()), Some(value)))
	}

	fn release(&mut self) {
		self.values.clear();
		self.replace = None;
		self.failure = None;
	}
}

#[cfg(test)]
mod tests {
	use super::{NativeWatch, NativeWatchCall, NativeWatchHandle, Watch, WatchCall, WatchEnd, WatchError, WatchHandle};
	use crate::context::{BACKGROUND_CONTEXT, Context, ContextKey, with_abort_signal, with_value};
	use crate::delta::{MAX_PENDING_OPS, Op, Snapshot, apply_immutable, track};
	use crate::test_executor::{Executor, now};
	use pi_json::JsonValue;
	use serde_json::json;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::{Arc, Mutex};
	use tokio_util::sync::CancellationToken;

	fn context() -> Context {
		BACKGROUND_CONTEXT.clone()
	}

	fn snap(value: JsonValue) -> Snapshot {
		Snapshot::from(value)
	}

	fn ops(value: JsonValue) -> Arc<[Op]> {
		let ops: Vec<Op> = serde_json::from_value(value).unwrap();
		Arc::from(ops)
	}

	fn frame(value: JsonValue, batch: JsonValue) -> WatchCall {
		WatchCall::Value {
			value: snap(value),
			ops: ops(batch),
			retired: false,
		}
	}

	fn retirement(value: JsonValue, batch: JsonValue) -> WatchCall {
		WatchCall::Value {
			value: snap(value),
			ops: ops(batch),
			retired: true,
		}
	}

	fn error(message: &str) -> WatchError {
		Arc::new(std::io::Error::other(message.to_owned()))
	}

	/// A watch whose detaches and reports are counted.
	fn recorded_watch(value: JsonValue) -> (Watch, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
		let detached = Arc::new(AtomicUsize::new(0));
		let reported = Arc::new(Mutex::new(Vec::new()));
		let (on_detach, on_report) = (detached.clone(), reported.clone());
		let watch = Watch::new(
			snap(value),
			move || {
				on_detach.fetch_add(1, Ordering::SeqCst);
			},
			move |error| on_report.lock().unwrap().push(error.to_string()),
		);
		(watch, detached, reported)
	}

	type Calls = Arc<Mutex<Vec<WatchCall>>>;

	/// What a listener recorded, shared with the test.
	type Shared<T> = Arc<Mutex<Vec<T>>>;

	/// `ending_while_running`'s watch, detach count, reports, gate and returned-call count.
	type Ending = (
		Watch,
		Arc<AtomicUsize>,
		Shared<String>,
		CancellationToken,
		Arc<AtomicUsize>,
	);

	/// Starts `watch` on `executor` with a listener recording every call, waiting on `gate` in the calls it names.
	fn start_recording(executor: &Executor, watch: &Watch, gated: &[usize], gate: &CancellationToken) -> Calls {
		let calls: Calls = Arc::default();
		let (recorded, gated, gate) = (calls.clone(), gated.to_vec(), gate.clone());
		let delivery = watch
			.start(move |call, _context| {
				let index = {
					let mut calls = recorded.lock().unwrap();
					calls.push(call);
					calls.len()
				};
				let gate = gated.contains(&index).then(|| gate.clone());
				async move {
					if let Some(gate) = gate {
						gate.cancelled_owned().await;
					}
					Ok(())
				}
			})
			.unwrap();
		executor.spawn(delivery);
		calls
	}

	/// The (value, ops) of each value call.
	fn values(calls: &Calls) -> Vec<(JsonValue, JsonValue)> {
		calls
			.lock()
			.unwrap()
			.iter()
			.map(|call| match call {
				WatchCall::Value { value, ops, .. } => (
					value.to_value(),
					JsonValue::Array(ops.iter().map(Op::to_json).collect()),
				),
				WatchCall::Failed(error) => (json!({"failed": error.to_string()}), JsonValue::Null),
			})
			.collect()
	}

	fn end_kind(end: Option<WatchEnd>) -> Option<String> {
		end.map(|end| match end {
			WatchEnd::Stopped => "stopped".to_owned(),
			WatchEnd::Aborted => "aborted".to_owned(),
			WatchEnd::Ended => "ended".to_owned(),
			WatchEnd::ListenerError(error) => format!("listener_error: {error}"),
			WatchEnd::Failed(error) => format!("failed: {error}"),
		})
	}

	/// Advances one frame per element of `{ k: [0, ..., count - 1] }`, each setting its own path; returns the newest.
	fn advance_distinct(watch: &Watch, count: usize) -> JsonValue {
		let mut values = Vec::new();
		for index in 0..count {
			values.push(index);
			watch.advance(
				frame(json!({"k": values}), json!([["s", ["k", index], index]])),
				context(),
			);
		}
		json!({"k": values})
	}

	// TS: watch.test.ts > Watch delivery > keeps the acquisition value until start, then delivers the frames since in
	// one call
	#[test]
	fn keeps_the_acquisition_value_until_start_then_delivers_the_frames_in_one_call() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"n": 0, "text": ""}));
		watch.advance(frame(json!({"n": 1, "text": ""}), json!([["s", ["n"], 1]])), context());
		watch.advance(
			frame(
				json!({"n": 2, "text": "a"}),
				json!([["s", ["n"], 2], ["a", ["text"], "a"]]),
			),
			context(),
		);
		watch.advance(
			frame(json!({"n": 2, "text": "ab"}), json!([["a", ["text"], "b"]])),
			context(),
		);
		executor.run();
		assert_eq!(watch.value().to_value(), json!({"n": 0, "text": ""}));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		assert_eq!(calls.lock().unwrap().len(), 0);
		executor.run();
		assert_eq!(
			values(&calls),
			[(
				json!({"n": 2, "text": "ab"}),
				json!([["s", ["n"], 2], ["a", ["text"], "ab"]])
			)]
		);
		assert_eq!(watch.value().to_value(), json!({"n": 2, "text": "ab"}));
	}

	// TS: watch.test.ts > Watch delivery > passes a frame's ops on by identity to a listener that keeps up (and Watch
	// call objects > passes a frame delivered alone on as the source made it)
	#[test]
	fn passes_a_frames_ops_on_by_identity_to_a_listener_that_keeps_up() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		let (first, second) = (ops(json!([["r", 1]])), ops(json!([["r", 2]])));
		for (value, batch) in [(1, &first), (2, &second)] {
			let call = WatchCall::Value {
				value: snap(json!(value)),
				ops: batch.clone(),
				retired: false,
			};
			watch.advance(call, context());
			executor.run();
		}
		let calls = calls.lock().unwrap();
		let delivered: Vec<&Arc<[Op]>> = calls
			.iter()
			.map(|call| match call {
				WatchCall::Value { ops, .. } => ops,
				WatchCall::Failed(_) => panic!("failed"),
			})
			.collect();
		assert!(Arc::ptr_eq(delivered[0], &first));
		assert!(Arc::ptr_eq(delivered[1], &second));
	}

	// TS: watch.test.ts > Watch delivery > never invokes the listener inline
	#[test]
	fn never_invokes_the_listener_inline() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		watch.advance(frame(json!(1), json!([["r", 1]])), context());
		assert_eq!(calls.lock().unwrap().len(), 0);
		executor.run();
		assert_eq!(values(&calls), [(json!(1), json!([["r", 1]]))]);
	}

	// TS: watch.test.ts > Watch delivery > awaits each callback and folds frames advanced while one is in flight into
	// the next call
	#[test]
	fn awaits_each_callback_and_folds_frames_advanced_while_one_is_in_flight() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"text": ""}));
		let gate = CancellationToken::new();
		let calls = start_recording(&executor, &watch, &[1], &gate);
		watch.advance(frame(json!({"text": "a"}), json!([["a", ["text"], "a"]])), context());
		executor.run();
		watch.advance(frame(json!({"text": "ab"}), json!([["a", ["text"], "b"]])), context());
		watch.advance(frame(json!({"text": "abc"}), json!([["a", ["text"], "c"]])), context());
		executor.run();
		assert_eq!(calls.lock().unwrap().len(), 1);
		gate.cancel();
		executor.run();
		assert_eq!(
			values(&calls),
			[
				(json!({"text": "a"}), json!([["a", ["text"], "a"]])),
				(json!({"text": "abc"}), json!([["a", ["text"], "bc"]]))
			]
		);
	}

	// TS: watch.test.ts > Watch delivery > delivers the newest frame's context, without its abort signal
	#[test]
	fn delivers_the_newest_frames_context_without_its_abort_signal() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let key = ContextKey::<&str>::new("frame");
		let delivered: Arc<Mutex<Vec<Context>>> = Arc::default();
		let recorded = delivered.clone();
		let delivery = watch
			.start(move |_call, frame_context| {
				recorded.lock().unwrap().push(frame_context);
				async { Ok(()) }
			})
			.unwrap();
		executor.spawn(delivery);
		watch.advance(
			frame(json!(1), json!([["r", 1]])),
			with_value(&key, "first", &context()),
		);
		let aborting = with_abort_signal(CancellationToken::new(), &context());
		watch.advance(
			frame(json!(2), json!([["r", 2]])),
			with_value(&key, "second", &aborting),
		);
		executor.run();
		let delivered = delivered.lock().unwrap();
		assert_eq!(delivered.len(), 1);
		assert_eq!(delivered[0].value(&key), Some(&"second"));
		assert!(delivered[0].abort_signal().is_none());
	}

	// TS: watch.test.ts > Watch with a slow listener > gets one call per batch whose ops turn the previous delivered
	// value into the newest (a gate opened once a "second" stands in for the fake timer)
	#[test]
	fn a_slow_listener_gets_one_call_per_batch() {
		let executor = Executor::default();
		let mut tracker = track(snap(json!({"entries": [], "tokens": 0})));
		let (watch, ..) = recorded_watch(json!({"entries": [], "tokens": 0}));
		let replica = Arc::new(Mutex::new(tracker.value().clone()));
		let counts: Arc<Mutex<Vec<usize>>> = Arc::default();
		let gate = Arc::new(Mutex::new(CancellationToken::new()));
		let (on_replica, on_counts, on_gate) = (replica.clone(), counts.clone(), gate.clone());
		let delivery = watch
			.start(move |call, _context| {
				let WatchCall::Value { value, ops, .. } = call else {
					panic!("failed")
				};
				let mut replica = on_replica.lock().unwrap();
				*replica = apply_immutable(&replica, &ops).unwrap();
				assert_eq!(*replica, value);
				on_counts.lock().unwrap().push(ops.len());
				let wait = on_gate.lock().unwrap().clone();
				async move {
					wait.cancelled_owned().await;
					Ok(())
				}
			})
			.unwrap();
		executor.spawn(delivery);
		let entries = [crate::delta::Seg::Key("entries".to_owned())];
		for tick in 0..600_usize {
			let mut change = tracker.begin_change();
			if tick % 60 == 0 {
				change
					.state()
					.push(&entries, vec![snap(json!({"text": "", "status": "streaming"}))])
					.unwrap();
			}
			let last = tick / 60;
			let text = [
				entries[0].clone(),
				crate::delta::Seg::Index(last as u64),
				crate::delta::Seg::Key("text".to_owned()),
			];
			change.state().append_text(&text, "tok ").unwrap();
			change
				.state()
				.set(
					&[crate::delta::Seg::Key("tokens".to_owned())],
					Some(snap(json!(tick + 1))),
				)
				.unwrap();
			let prepared = change.prepare().unwrap();
			tracker.adopt(&prepared).unwrap();
			watch.advance(
				WatchCall::Value {
					value: tracker.value().clone(),
					ops: prepared.ops().clone(),
					retired: false,
				},
				context(),
			);
			if tick % 60 == 59 {
				let open = std::mem::replace(&mut *gate.lock().unwrap(), CancellationToken::new());
				open.cancel();
			}
			executor.run();
		}
		gate.lock().unwrap().cancel();
		executor.run();
		gate.lock().unwrap().cancel();
		executor.run();
		assert_eq!(*replica.lock().unwrap(), *tracker.value());
		let counts = counts.lock().unwrap();
		assert!((10..=12).contains(&counts.len()), "{counts:?}");
		assert!(counts.iter().all(|&ops| ops <= 5), "{counts:?}");
	}

	// TS: watch.test.ts > Watch overflow > keeps 10000 pending ops after folding and replaces the root past them
	#[test]
	fn keeps_max_pending_ops_after_folding_and_replaces_the_root_past_them() {
		let executor = Executor::default();
		let (exact, ..) = recorded_watch(json!({"k": []}));
		let calls = start_recording(&executor, &exact, &[], &CancellationToken::new());
		advance_distinct(&exact, MAX_PENDING_OPS);
		executor.run();
		let calls = calls.lock().unwrap();
		assert_eq!(calls.len(), 1);
		assert!(matches!(&calls[0], WatchCall::Value { ops, .. } if ops.len() == MAX_PENDING_OPS));

		let (over, ..) = recorded_watch(json!({"k": []}));
		let calls = start_recording(&executor, &over, &[], &CancellationToken::new());
		let newest = advance_distinct(&over, MAX_PENDING_OPS + 1);
		executor.run();
		assert_eq!(values(&calls), [(newest.clone(), json!([["r", newest]]))]);
	}

	// TS: watch.test.ts > Watch overflow > never folds the in-flight call into an overflow
	#[test]
	fn never_folds_the_in_flight_call_into_an_overflow() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"k": []}));
		let gate = CancellationToken::new();
		let calls = start_recording(&executor, &watch, &[1], &gate);
		watch.advance(
			frame(json!({"k": [], "first": true}), json!([["s", ["first"], true]])),
			context(),
		);
		executor.run();
		let newest = advance_distinct(&watch, MAX_PENDING_OPS + 1);
		gate.cancel();
		executor.run();
		assert_eq!(
			values(&calls),
			[
				(json!({"k": [], "first": true}), json!([["s", ["first"], true]])),
				(newest.clone(), json!([["r", newest]]))
			]
		);
	}

	// TS: watch.test.ts > Watch overflow > starts folding again after an overflow was delivered
	#[test]
	fn starts_folding_again_after_an_overflow_was_delivered() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"k": []}));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		advance_distinct(&watch, MAX_PENDING_OPS + 1);
		executor.run();
		watch.advance(frame(json!({"n": 1}), json!([["r", {"n": 1}]])), context());
		watch.advance(frame(json!({"n": 2}), json!([["s", ["n"], 2]])), context());
		executor.run();
		assert_eq!(
			values(&calls).last().unwrap(),
			&(json!({"n": 2}), json!([["r", {"n": 1}], ["s", ["n"], 2]]))
		);
	}

	fn retired_flags(calls: &Calls) -> Vec<bool> {
		calls
			.lock()
			.unwrap()
			.iter()
			.map(|call| matches!(call, WatchCall::Value { retired: true, .. }))
			.collect()
	}

	// TS: watch.test.ts > Watch retirement > reports a retirement in the next call with the value the source gives,
	// and goes on (and treats a null value as a value, never as a retirement)
	#[test]
	fn reports_a_retirement_in_the_next_call_and_goes_on() {
		let executor = Executor::default();
		let (watch, detached, _) = recorded_watch(json!(1));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		watch.advance(frame(json!(2), json!([["r", 2]])), context());
		executor.run();
		watch.advance(retirement(json!(2), json!([])), context());
		executor.run();
		watch.advance(frame(json!(null), json!([["r", null]])), context());
		executor.run();
		watch.advance(frame(json!(3), json!([["r", 3]])), context());
		executor.run();
		assert_eq!(
			values(&calls),
			[
				(json!(2), json!([["r", 2]])),
				(json!(2), json!([])),
				(json!(null), json!([["r", null]])),
				(json!(3), json!([["r", 3]]))
			]
		);
		assert_eq!(retired_flags(&calls), [false, true, false, false]);
		assert_eq!(watch.value().to_value(), json!(3));
		assert!(now(watch.closed()).is_none());
		assert_eq!(detached.load(Ordering::SeqCst), 0);
		assert_eq!(end_kind(now(watch.stop())).as_deref(), Some("stopped"));
	}

	// TS: watch.test.ts > Watch retirement > folds a retirement and the frames around it into one call that reports it
	#[test]
	fn folds_a_retirement_and_the_frames_around_it_into_one_call() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"n": 0}));
		watch.advance(frame(json!({"n": 1}), json!([["s", ["n"], 1]])), context());
		watch.advance(retirement(json!({"n": 1}), json!([])), context());
		watch.advance(frame(json!({"m": 1}), json!([["r", {"m": 1}]])), context());
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		executor.run();
		watch.advance(frame(json!({"m": 2}), json!([["s", ["m"], 2]])), context());
		executor.run();
		assert_eq!(
			values(&calls),
			[
				(json!({"m": 1}), json!([["r", {"m": 1}]])),
				(json!({"m": 2}), json!([["s", ["m"], 2]]))
			]
		);
		assert_eq!(retired_flags(&calls), [true, false]);
	}

	// TS: watch.test.ts > Watch retirement > keeps reporting the retirement past an overflow
	#[test]
	fn keeps_reporting_the_retirement_past_an_overflow() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"k": []}));
		watch.advance(retirement(json!({"k": []}), json!([])), context());
		let newest = advance_distinct(&watch, MAX_PENDING_OPS + 1);
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		executor.run();
		assert_eq!(values(&calls), [(newest.clone(), json!([["r", newest]]))]);
		assert_eq!(retired_flags(&calls), [true]);
	}

	// TS: watch.test.ts > Watch retirement > discards a retirement still pending when the source ends
	#[test]
	fn discards_a_retirement_still_pending_when_the_source_ends() {
		let (watch, ..) = recorded_watch(json!(1));
		watch.advance(retirement(json!(1), json!([])), context());
		watch.end(WatchEnd::Ended);
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("ended"));
		assert_eq!(watch.value().to_value(), json!(1));
	}

	// TS: watch.test.ts > Watch failure calls > delivers a failure as a call, stays open, keeps the last value, and
	// replaces the root on recovery (and takes a failure through `advance()` as through `fail()`)
	#[test]
	fn delivers_a_failure_as_a_call_and_replaces_the_root_on_recovery() {
		let executor = Executor::default();
		let (watch, detached, _) = recorded_watch(json!({"n": 0}));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		watch.advance(frame(json!({"n": 1}), json!([["s", ["n"], 1]])), context());
		executor.run();
		watch.fail(error("cannot compute"), context());
		executor.run();
		assert_eq!(watch.value().to_value(), json!({"n": 1}));
		assert!(now(watch.closed()).is_none());
		assert_eq!(detached.load(Ordering::SeqCst), 0);
		watch.advance(frame(json!({"n": 2}), json!([["s", ["n"], 2]])), context());
		executor.run();
		let next = ops(json!([["s", ["n"], 3]]));
		watch.advance(
			WatchCall::Value {
				value: snap(json!({"n": 3})),
				ops: next.clone(),
				retired: false,
			},
			context(),
		);
		executor.run();
		watch.advance(WatchCall::Failed(error("down")), context());
		executor.run();
		assert_eq!(
			values(&calls),
			[
				(json!({"n": 1}), json!([["s", ["n"], 1]])),
				(json!({"failed": "cannot compute"}), JsonValue::Null),
				(json!({"n": 2}), json!([["r", {"n": 2}]])),
				(json!({"n": 3}), json!([["s", ["n"], 3]])),
				(json!({"failed": "down"}), JsonValue::Null)
			]
		);
		assert!(matches!(&calls.lock().unwrap()[3], WatchCall::Value { ops, .. } if Arc::ptr_eq(ops, &next)));
		assert_eq!(watch.value().to_value(), json!({"n": 3}));
	}

	// TS: watch.test.ts > Watch failure calls > folds pending values followed by a failure into the failure
	#[test]
	fn folds_pending_values_followed_by_a_failure_into_the_failure() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"n": 0}));
		watch.advance(frame(json!({"n": 1}), json!([["s", ["n"], 1]])), context());
		watch.advance(frame(json!({"n": 2}), json!([["s", ["n"], 2]])), context());
		watch.fail(error("first"), context());
		watch.fail(error("second"), context());
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		executor.run();
		assert_eq!(values(&calls), [(json!({"failed": "second"}), JsonValue::Null)]);
		assert_eq!(watch.value().to_value(), json!({"n": 0}));
	}

	// TS: watch.test.ts > Watch failure calls > folds a pending failure followed by values into one root replacement
	// with the newest value
	#[test]
	fn folds_a_pending_failure_followed_by_values_into_one_root_replacement() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"n": 0}));
		let key = ContextKey::<&str>::new("frame");
		watch.fail(error("down"), with_value(&key, "failure", &context()));
		watch.advance(frame(json!({"n": 1}), json!([["s", ["n"], 1]])), context());
		watch.advance(retirement(json!({"n": 1}), json!([])), context());
		watch.advance(
			frame(json!({"m": 2}), json!([["r", {"m": 2}]])),
			with_value(&key, "newest", &context()),
		);
		let frames: Shared<(JsonValue, bool, Option<&'static str>)> = Arc::default();
		let recorded = frames.clone();
		let on_key = Arc::new(key);
		executor.spawn(
			watch
				.start(move |call, frame_context| {
					let WatchCall::Value { value, retired, .. } = call else {
						panic!("failed")
					};
					recorded
						.lock()
						.unwrap()
						.push((value.to_value(), retired, frame_context.value(&on_key).copied()));
					async { Ok(()) }
				})
				.unwrap(),
		);
		executor.run();
		assert_eq!(*frames.lock().unwrap(), [(json!({"m": 2}), true, Some("newest"))]);
	}

	// TS: watch.test.ts > Watch failure calls > delivers a failure that arrives during a call after it, then the
	// recovery
	#[test]
	fn delivers_a_failure_that_arrives_during_a_call_after_it() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let gate = CancellationToken::new();
		let calls = start_recording(&executor, &watch, &[1], &gate);
		watch.advance(frame(json!(1), json!([["r", 1]])), context());
		executor.run();
		watch.advance(frame(json!(2), json!([["r", 2]])), context());
		watch.fail(error("down"), context());
		gate.cancel();
		executor.run();
		watch.advance(frame(json!(3), json!([["r", 3]])), context());
		executor.run();
		let delivered: Vec<JsonValue> = values(&calls).into_iter().map(|(value, _)| value).collect();
		assert_eq!(delivered, [json!(1), json!({"failed": "down"}), json!(3)]);
		assert_eq!(watch.value().to_value(), json!(3));
	}

	// TS: watch.test.ts > Watch failure calls > drops a pending failure when the source ends, and ignores one after
	#[test]
	fn drops_a_pending_failure_when_the_source_ends() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		watch.fail(error("down"), context());
		watch.end(WatchEnd::Ended);
		watch.fail(error("later"), context());
		executor.run();
		assert_eq!(calls.lock().unwrap().len(), 0);
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("ended"));
	}

	// TS: watch.test.ts > Watch failure calls > passes every lone value call on by identity, so a value call costs the
	// watch no allocation
	#[test]
	fn passes_every_lone_value_call_on_by_identity() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		let sent: Vec<Arc<[Op]>> = (1..=1_000).map(|n| ops(json!([["r", n]]))).collect();
		for (n, batch) in sent.iter().enumerate() {
			let call = WatchCall::Value {
				value: snap(json!(n + 1)),
				ops: batch.clone(),
				retired: false,
			};
			watch.advance(call, context());
			executor.run();
		}
		let calls = calls.lock().unwrap();
		assert_eq!(calls.len(), sent.len());
		for (call, batch) in calls.iter().zip(&sent) {
			assert!(matches!(call, WatchCall::Value { ops, .. } if Arc::ptr_eq(ops, batch)));
		}
	}

	// TS: watch.test.ts > Watch end after delivery > takes no more frames, delivers what is pending, then ends `ended`
	// and only then detaches
	#[test]
	fn ends_after_delivering_what_is_pending_and_only_then_detaches() {
		let executor = Executor::default();
		let (watch, detached, _) = recorded_watch(json!(1));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		watch.advance(retirement(json!(1), json!([])), context());
		watch.end_after_delivery();
		assert_eq!(detached.load(Ordering::SeqCst), 0);
		watch.advance(frame(json!(2), json!([["r", 2]])), context());
		executor.run();
		assert_eq!(values(&calls), [(json!(1), json!([]))]);
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("ended"));
		assert_eq!(detached.load(Ordering::SeqCst), 1);
	}

	// TS: watch.test.ts > Watch end after delivery > waits for start to deliver, and ends at once with nothing pending
	#[test]
	fn waits_for_start_to_deliver_and_ends_at_once_with_nothing_pending() {
		let executor = Executor::default();
		let (watch, detached, _) = recorded_watch(json!(1));
		watch.advance(frame(json!(2), json!([["r", 2]])), context());
		watch.end_after_delivery();
		executor.run();
		assert!(now(watch.closed()).is_none());
		assert_eq!(detached.load(Ordering::SeqCst), 0);
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		executor.run();
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("ended"));
		assert_eq!(values(&calls), [(json!(2), json!([["r", 2]]))]);
		assert_eq!(detached.load(Ordering::SeqCst), 1);

		let (idle, idle_detached, _) = recorded_watch(json!(1));
		let idle_calls = start_recording(&executor, &idle, &[], &CancellationToken::new());
		idle.end_after_delivery();
		assert_eq!(end_kind(now(idle.closed())).as_deref(), Some("ended"));
		executor.run();
		assert_eq!(idle_calls.lock().unwrap().len(), 0);
		assert_eq!(idle_detached.load(Ordering::SeqCst), 1);
	}

	// TS: watch.test.ts > Watch end after delivery > ends a never-started watch at its source's end, dropping what is
	// pending
	#[test]
	fn ends_a_never_started_watch_at_its_sources_end() {
		let (watch, detached, _) = recorded_watch(json!(1));
		watch.advance(retirement(json!(1), json!([])), context());
		watch.end_after_delivery();
		watch.end(WatchEnd::Failed(error("source failed")));
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("failed: source failed"));
		assert_eq!(detached.load(Ordering::SeqCst), 1);
		let refused = watch.start(|_call, _context| async { Ok(()) }).err();
		assert_eq!(
			refused.map(|error| error.to_string()).as_deref(),
			Some("Watch is stopped")
		);
	}

	/// A started watch whose listener is in a call held by the returned gate, with nothing pending, then
	/// `end_after_delivery()`; the listener returns `result` after the gate.
	fn ending_while_running(executor: &Executor, result: Option<&'static str>) -> Ending {
		let (watch, detached, reported) = recorded_watch(json!(0));
		let gate = CancellationToken::new();
		let returned = Arc::new(AtomicUsize::new(0));
		let (on_gate, on_returned) = (gate.clone(), returned.clone());
		executor.spawn(
			watch
				.start(move |_call, _context| {
					let (gate, returned) = (on_gate.clone(), on_returned.clone());
					async move {
						gate.cancelled_owned().await;
						returned.fetch_add(1, Ordering::SeqCst);
						match result {
							Some(message) => Err(error(message)),
							None => Ok(()),
						}
					}
				})
				.unwrap(),
		);
		watch.advance(retirement(json!(1), json!([["r", 1]])), context());
		executor.run();
		watch.end_after_delivery();
		executor.run();
		(watch, detached, reported, gate, returned)
	}

	// TS: watch.test.ts > Watch end after delivery > ends `ended` only after the running call returns when nothing is
	// pending, detached then
	#[test]
	fn ends_ended_only_after_the_running_call_returns() {
		let executor = Executor::default();
		let (watch, detached, _, gate, returned) = ending_while_running(&executor, None);
		assert!(now(watch.closed()).is_none());
		assert_eq!(detached.load(Ordering::SeqCst), 0);
		gate.cancel();
		executor.run();
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("ended"));
		assert_eq!(returned.load(Ordering::SeqCst), 1);
		assert_eq!(detached.load(Ordering::SeqCst), 1);
	}

	// TS: watch.test.ts > Watch end after delivery > ends `listener_error` and reports it when the running call throws
	// after `endAfterDelivery()`
	#[test]
	fn ends_listener_error_when_the_running_call_fails_after_end_after_delivery() {
		let executor = Executor::default();
		let (watch, detached, reported, gate, _) = ending_while_running(&executor, Some("listener failed"));
		assert!(now(watch.closed()).is_none());
		gate.cancel();
		executor.run();
		assert_eq!(
			end_kind(now(watch.closed())).as_deref(),
			Some("listener_error: listener failed")
		);
		assert_eq!(*reported.lock().unwrap(), ["listener failed"]);
		assert_eq!(detached.load(Ordering::SeqCst), 1);
	}

	// TS: watch.test.ts > Watch end after delivery > ends `stopped` when stopped while ending, without awaiting the
	// running call
	#[test]
	fn ends_stopped_when_stopped_while_ending_without_awaiting_the_running_call() {
		let executor = Executor::default();
		let (watch, detached, _, gate, returned) = ending_while_running(&executor, None);
		assert_eq!(end_kind(now(watch.stop())).as_deref(), Some("stopped"));
		assert_eq!(detached.load(Ordering::SeqCst), 1);
		assert_eq!(returned.load(Ordering::SeqCst), 0);
		gate.cancel();
		assert_eq!(executor.run(), 0);
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("stopped"));
	}

	// TS: watch.test.ts > Watch call objects > keeps no frame context alive through a call a listener keeps
	#[test]
	fn keeps_no_frame_context_alive_through_a_call_a_listener_keeps() {
		let executor = Executor::default();
		let key = ContextKey::<Arc<String>>::new("payload");
		let payload = Arc::new("x".repeat(1000));
		let weak = Arc::downgrade(&payload);
		let (watch, ..) = recorded_watch(json!(0));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		let producer = with_abort_signal(CancellationToken::new(), &with_value(&key, payload, &context()));
		watch.advance(frame(json!(1), json!([["r", 1]])), producer);
		executor.run();
		drop(now(watch.stop()));
		executor.run();
		assert_eq!(calls.lock().unwrap().len(), 1);
		assert!(weak.upgrade().is_none());
	}

	// TS: watch.test.ts > Watch ends > stops idempotently and rejects a repeated or late start
	#[test]
	fn stops_idempotently_and_rejects_a_repeated_or_late_start() {
		let (watch, detached, _) = recorded_watch(json!(0));
		let _delivery = watch.start(|_call, _context| async { Ok(()) }).unwrap();
		let again = watch
			.start(|_call, _context| async { Ok(()) })
			.err()
			.map(|error| error.to_string());
		assert_eq!(again.as_deref(), Some("Watch is already started"));
		assert_eq!(end_kind(now(watch.stop())).as_deref(), Some("stopped"));
		assert_eq!(end_kind(now(watch.stop())).as_deref(), Some("stopped"));
		assert_eq!(detached.load(Ordering::SeqCst), 1);
		let (late, ..) = recorded_watch(json!(0));
		drop(now(late.stop()));
		let refused = late
			.start(|_call, _context| async { Ok(()) })
			.err()
			.map(|error| error.to_string());
		assert_eq!(refused.as_deref(), Some("Watch is stopped"));
	}

	// TS: watch.test.ts > Watch ends > ends `ended` or `failed` from its source, dropping pending frames
	#[test]
	fn ends_ended_or_failed_from_its_source_dropping_pending_frames() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		watch.advance(frame(json!(1), json!([["r", 1]])), context());
		watch.end(WatchEnd::Ended);
		executor.run();
		assert_eq!(calls.lock().unwrap().len(), 0);
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some("ended"));
		let (failed, ..) = recorded_watch(json!(0));
		failed.end(WatchEnd::Failed(error("source failed")));
		failed.end(WatchEnd::Ended);
		assert_eq!(end_kind(now(failed.closed())).as_deref(), Some("failed: source failed"));
	}

	// TS: watch.test.ts > Watch ends > stops future delivery without aborting or awaiting an in-flight callback
	#[test]
	fn stops_future_delivery_without_aborting_an_in_flight_callback() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!(0));
		let gate = CancellationToken::new();
		let calls = start_recording(&executor, &watch, &[1], &gate);
		watch.advance(frame(json!(1), json!([["r", 1]])), context());
		executor.run();
		watch.advance(frame(json!(2), json!([["r", 2]])), context());
		assert_eq!(end_kind(now(watch.stop())).as_deref(), Some("stopped"));
		gate.cancel();
		assert_eq!(executor.run(), 0);
		assert_eq!(values(&calls), [(json!(1), json!([["r", 1]]))]);
	}

	// TS: watch.test.ts > Watch ends > ends `aborted` when its acquisition signal aborts, also when already aborted
	#[test]
	fn ends_aborted_when_its_acquisition_signal_aborts() {
		let token = CancellationToken::new();
		let signal = with_abort_signal(token.clone(), &context())
			.abort_signal()
			.unwrap()
			.clone();
		let (watch, detached, _) = recorded_watch(json!(0));
		watch.observe_abort(signal.clone()).unwrap();
		let again = watch.observe_abort(signal).err().map(|error| error.to_string());
		assert_eq!(again.as_deref(), Some("Watch abort is already installed"));
		let closed = watch.closed();
		token.cancel();
		assert_eq!(end_kind(now(closed)).as_deref(), Some("aborted"));
		assert_eq!(detached.load(Ordering::SeqCst), 1);

		let cancelled = CancellationToken::new();
		cancelled.cancel();
		let (aborted, ..) = recorded_watch(json!(0));
		aborted
			.observe_abort(with_abort_signal(cancelled, &context()).abort_signal().unwrap().clone())
			.unwrap();
		assert_eq!(end_kind(now(aborted.closed())).as_deref(), Some("aborted"));
		let (direct, ..) = recorded_watch(json!(0));
		direct.abort();
		assert_eq!(end_kind(now(direct.closed())).as_deref(), Some("aborted"));
	}

	// TS: watch.test.ts > Watch ends > ends `listener_error` on a throw, reports it, and delivers nothing more (a Rust
	// listener returns its error; "wraps a thrown non-Error" has no Rust counterpart)
	#[test]
	fn ends_listener_error_on_a_listener_error_and_delivers_nothing_more() {
		let executor = Executor::default();
		let (watch, detached, reported) = recorded_watch(json!(0));
		let gate = CancellationToken::new();
		let delivered = Arc::new(AtomicUsize::new(0));
		let (on_gate, on_delivered) = (gate.clone(), delivered.clone());
		executor.spawn(
			watch
				.start(move |_call, _context| {
					on_delivered.fetch_add(1, Ordering::SeqCst);
					let gate = on_gate.clone();
					async move {
						gate.cancelled_owned().await;
						Err(error("listener failed"))
					}
				})
				.unwrap(),
		);
		watch.advance(frame(json!(1), json!([["r", 1]])), context());
		executor.run();
		watch.advance(frame(json!(2), json!([["r", 2]])), context());
		gate.cancel();
		executor.run();
		assert_eq!(
			end_kind(now(watch.closed())).as_deref(),
			Some("listener_error: listener failed")
		);
		assert_eq!(delivered.load(Ordering::SeqCst), 1);
		assert_eq!(*reported.lock().unwrap(), ["listener failed"]);
		assert_eq!(detached.load(Ordering::SeqCst), 1);
	}

	// TS: watch.test.ts > Watch references > retains neither its source, its listener, nor its signal once ended,
	// while still referenced (the source and the listener; an `AbortSignal` is released with the watch's state)
	#[test]
	fn retains_neither_its_source_nor_its_listener_once_ended() {
		type End = fn(&Watch);
		let ends: [End; 4] = [
			|watch| drop(now(watch.stop())),
			Watch::abort,
			|watch| watch.end(WatchEnd::Ended),
			Watch::end_after_delivery,
		];
		for (index, end) in [None].into_iter().chain(ends.into_iter().map(Some)).enumerate() {
			let executor = Executor::default();
			let source = Arc::new(1);
			let (on_detach, on_report) = (source.clone(), source.clone());
			let watch = Watch::new(
				snap(json!(0)),
				move || drop(on_detach),
				move |_| drop(on_report.clone()),
			);
			let listener_state = Arc::new(());
			let held = listener_state.clone();
			executor.spawn(
				watch
					.start(move |_call, _context| {
						let _ = &held;
						async { Ok(()) }
					})
					.unwrap(),
			);
			watch.advance(frame(json!(1), json!([["r", 1]])), context());
			executor.run();
			if let Some(end) = end {
				end(&watch);
			}
			executor.run();
			let (source_weak, listener) = (Arc::downgrade(&source), Arc::downgrade(&listener_state));
			drop((source, listener_state));
			let source = source_weak;
			let released = source.upgrade().is_none() && listener.upgrade().is_none();
			// The open watch (index 0) is the control: it keeps both.
			assert_eq!(released, index > 0, "end {index}");
			drop(now(watch.stop()));
		}
	}

	// TS: watch.test.ts > Watch references > keeps no ops a fold or an overflow dropped, while it waits
	#[test]
	fn keeps_no_ops_a_fold_or_an_overflow_dropped() {
		let executor = Executor::default();
		let members: serde_json::Map<String, JsonValue> =
			(0..16_000).map(|index| (format!("k{index}"), json!(0))).collect();
		let mut tracker = track(snap(JsonValue::Object(members)));
		let (watch, ..) = recorded_watch(tracker.value().to_value());
		let mut batches = Vec::new();
		for round in 0..4 {
			let mut change = tracker.begin_change();
			for index in round * 4_000..(round + 1) * 4_000 {
				change
					.state()
					.set(&[crate::delta::Seg::Key(format!("k{index}"))], Some(snap(json!(1))))
					.unwrap();
			}
			let prepared = change.prepare().unwrap();
			tracker.adopt(&prepared).unwrap();
			batches.push(Arc::downgrade(prepared.ops()));
			watch.advance(
				WatchCall::Value {
					value: tracker.value().clone(),
					ops: prepared.ops().clone(),
					retired: false,
				},
				context(),
			);
		}
		assert!(batches.iter().all(|batch| batch.upgrade().is_none()));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		executor.run();
		let newest = tracker.value().to_value();
		assert_eq!(values(&calls), [(newest.clone(), json!([["r", newest]]))]);
	}

	// TS: watch.test.ts > Watch references > releases its pending queue and leaf index once delivered or ended
	#[test]
	fn releases_its_pending_queue_once_delivered_or_ended() {
		for stop in [false, true] {
			let executor = Executor::default();
			let (watch, ..) = recorded_watch(json!(0));
			let gate = CancellationToken::new();
			start_recording(&executor, &watch, &[1], &gate);
			watch.advance(frame(json!(1), json!([["s", ["n"], 1]])), context());
			executor.run();
			let payload = ops(json!([["s", ["o"], {"big": "x".repeat(1000)}]]));
			let weak = Arc::downgrade(&payload);
			watch.advance(
				WatchCall::Value {
					value: snap(json!(2)),
					ops: payload,
					retired: false,
				},
				context(),
			);
			watch.advance(frame(json!(3), json!([["s", ["n"], 3]])), context());
			if stop {
				drop(now(watch.stop()));
			} else {
				gate.cancel();
			}
			executor.run();
			assert!(weak.upgrade().is_none(), "stop {stop}");
			if !stop {
				assert_eq!(watch.value().to_value(), json!(3));
			}
		}
	}

	fn native() -> NativeWatch<i64> {
		NativeWatch::new(0, || {}, |_| {})
	}

	type NativeCalls = Arc<Mutex<Vec<NativeWatchCall<i64>>>>;

	fn start_native(executor: &Executor, watch: &NativeWatch<i64>) -> NativeCalls {
		let calls: NativeCalls = Arc::default();
		let recorded = calls.clone();
		executor.spawn(
			watch
				.start(move |call, _context| {
					recorded.lock().unwrap().push(call);
					async { Ok(()) }
				})
				.unwrap(),
		);
		calls
	}

	fn native_values(calls: &NativeCalls) -> Vec<String> {
		calls
			.lock()
			.unwrap()
			.iter()
			.map(|call| match call {
				NativeWatchCall::Value(value) => value.to_string(),
				NativeWatchCall::Failed(error) => format!("failed {error}"),
			})
			.collect()
	}

	// TS: watch.test.ts > Watch references > delivers a native backlog that never drains in order
	#[test]
	fn delivers_a_native_backlog_that_never_drains_in_order() {
		let executor = Executor::default();
		let watch = native();
		let values: Arc<Mutex<Vec<i64>>> = Arc::default();
		let next = Arc::new(AtomicUsize::new(50));
		let (recorded, source, counter) = (values.clone(), watch.clone(), next.clone());
		executor.spawn(
			watch
				.start(move |call, _context| {
					if let NativeWatchCall::Value(value) = call {
						recorded.lock().unwrap().push(value);
					}
					let at = counter.fetch_add(1, Ordering::SeqCst);
					if at < 1_000 {
						source.advance(i64::try_from(at).unwrap(), context());
					}
					async { Ok(()) }
				})
				.unwrap(),
		);
		for n in 0..50 {
			watch.advance(n, context());
		}
		executor.run();
		assert_eq!(*values.lock().unwrap(), (0..1_000).collect::<Vec<i64>>());
		drop(now(watch.stop()));
	}

	// TS: watch.test.ts > NativeWatch > delivers values by identity in order with a context, never inline
	#[test]
	fn native_delivers_values_by_identity_in_order_with_a_context() {
		let executor = Executor::default();
		let watch: NativeWatch<Arc<str>> = NativeWatch::new(Arc::from("zero"), || {}, |_| {});
		let key = ContextKey::<&str>::new("frame");
		let delivered: Shared<(Arc<str>, Option<&'static str>)> = Arc::default();
		let (recorded, on_key) = (delivered.clone(), Arc::new(key));
		let frame_key = on_key.clone();
		executor.spawn(
			watch
				.start(move |call, frame_context| {
					if let NativeWatchCall::Value(value) = call {
						recorded
							.lock()
							.unwrap()
							.push((value, frame_context.value(&on_key).copied()));
					}
					async { Ok(()) }
				})
				.unwrap(),
		);
		let (first, second): (Arc<str>, Arc<str>) = (Arc::from("first"), Arc::from("second"));
		watch.advance(first.clone(), with_value(&frame_key, "first", &context()));
		watch.advance(second.clone(), with_value(&frame_key, "second", &context()));
		assert_eq!(delivered.lock().unwrap().len(), 0);
		executor.run();
		let delivered = delivered.lock().unwrap();
		assert!(Arc::ptr_eq(&delivered[0].0, &first) && Arc::ptr_eq(&delivered[1].0, &second));
		assert_eq!((delivered[0].1, delivered[1].1), (Some("first"), Some("second")));
		assert!(Arc::ptr_eq(&watch.value(), &second));
	}

	// TS: watch.test.ts > NativeWatch > collapses 101 pending values to the newest, and to the value of `replace`
	#[test]
	fn native_collapses_101_pending_values() {
		let executor = Executor::default();
		let watch = native();
		for n in 1..=101 {
			watch.advance(n, context());
		}
		let calls = start_native(&executor, &watch);
		executor.run();
		assert_eq!(native_values(&calls), ["101"]);

		let replacing = NativeWatch::with_replace(0, || {}, |_| {}, || -1);
		for n in 1..=101 {
			replacing.advance(n, context());
		}
		let calls = start_native(&executor, &replacing);
		executor.run();
		assert_eq!(native_values(&calls), ["-1"]);
	}

	// TS: watch.test.ts > NativeWatch > ends with every reason of its lifecycle
	#[test]
	fn native_ends_with_every_reason_of_its_lifecycle() {
		let executor = Executor::default();
		assert_eq!(end_kind(now(native().stop())).as_deref(), Some("stopped"));
		let cancelled = CancellationToken::new();
		cancelled.cancel();
		let aborted = native();
		aborted
			.observe_abort(with_abort_signal(cancelled, &context()).abort_signal().unwrap().clone())
			.unwrap();
		assert_eq!(end_kind(now(aborted.closed())).as_deref(), Some("aborted"));
		let ended = native();
		ended.end(WatchEnd::Ended);
		assert_eq!(end_kind(now(ended.closed())).as_deref(), Some("ended"));

		let finishing = native();
		finishing.advance(1, context());
		finishing.advance(2, context());
		finishing.end_after_delivery();
		finishing.advance(3, context());
		let calls = start_native(&executor, &finishing);
		executor.run();
		assert_eq!(end_kind(now(finishing.closed())).as_deref(), Some("ended"));
		assert_eq!(native_values(&calls), ["1", "2"]);

		let failed = native();
		failed.end(WatchEnd::Failed(error("source failed")));
		assert_eq!(end_kind(now(failed.closed())).as_deref(), Some("failed: source failed"));

		let reported: Arc<Mutex<Vec<String>>> = Arc::default();
		let on_report = reported.clone();
		let throwing = NativeWatch::new(0, || {}, move |error| on_report.lock().unwrap().push(error.to_string()));
		executor.spawn(
			throwing
				.start(|_call, _context| async { Err(error("listener failed")) })
				.unwrap(),
		);
		throwing.advance(1, context());
		executor.run();
		assert_eq!(
			end_kind(now(throwing.closed())).as_deref(),
			Some("listener_error: listener failed")
		);
		assert_eq!(*reported.lock().unwrap(), ["listener failed"]);
	}

	// TS: watch.test.ts > NativeWatch > delivers a failure as a call, stays open, and folds it with the values around it
	#[test]
	fn native_delivers_a_failure_as_a_call_and_folds_it_with_the_values_around_it() {
		let executor = Executor::default();
		let watch = native();
		let calls = start_native(&executor, &watch);
		watch.advance(1, context());
		executor.run();
		watch.advance(2, context());
		watch.advance(3, context());
		watch.fail(error("down"), context());
		executor.run();
		assert_eq!(watch.value(), 1);
		watch.fail(error("again"), context());
		watch.advance(4, context());
		watch.advance(5, context());
		executor.run();
		assert_eq!(native_values(&calls), ["1", "failed down", "4", "5"]);
		assert_eq!(watch.value(), 5);
	}

	// TS: watch.test.ts > NativeWatch > retains neither its source nor its listener once stopped, while still
	// referenced
	#[test]
	fn native_retains_neither_its_source_nor_its_listener_once_stopped() {
		let executor = Executor::default();
		let source = Arc::new(1_i64);
		let on_detach = source.clone();
		let watch = NativeWatch::new(0, move || drop(on_detach), |_| {});
		let listener_state = Arc::new(());
		let held = listener_state.clone();
		executor.spawn(
			watch
				.start(move |_call, _context| {
					let _ = &held;
					async { Ok(()) }
				})
				.unwrap(),
		);
		watch.advance(*source, context());
		executor.run();
		drop(now(watch.stop()));
		executor.run();
		let (source_weak, listener) = (Arc::downgrade(&source), Arc::downgrade(&listener_state));
		drop((source, listener_state));
		let source = source_weak;
		assert!(source.upgrade().is_none() && listener.upgrade().is_none());
		assert_eq!(watch.value(), 1);
	}

	// Rust-only (D0b review): a delivery future dropped unspawned, or mid-call, ends the watch `stopped`.
	#[test]
	fn a_dropped_delivery_ends_the_watch_stopped() {
		let (unspawned, detached, _) = recorded_watch(json!(0));
		let delivery = unspawned.start(|_call, _context| async { Ok(()) }).unwrap();
		drop(delivery);
		assert_eq!(end_kind(now(unspawned.closed())).as_deref(), Some("stopped"));
		assert_eq!(detached.load(Ordering::SeqCst), 1);

		let (running, ..) = recorded_watch(json!(0));
		let gate = CancellationToken::new();
		let wait = gate.clone();
		let mut delivery = running
			.start(move |_call, _context| {
				let wait = wait.clone();
				async move {
					wait.cancelled_owned().await;
					Ok(())
				}
			})
			.unwrap();
		running.advance(frame(json!(1), json!([["r", 1]])), context());
		assert!(now(&mut delivery).is_none());
		assert!(now(running.closed()).is_none());
		drop(delivery);
		assert_eq!(end_kind(now(running.closed())).as_deref(), Some("stopped"));
	}

	// Rust-only (D0b review): a panicking listener ends the watch as a listener error, reported, as a TS throw does.
	#[test]
	fn a_panicking_listener_ends_the_watch_as_a_listener_error() {
		let executor = Executor::default();
		let (watch, detached, reported) = recorded_watch(json!(0));
		executor.spawn(
			watch
				.start(|_call, _context| -> std::future::Ready<Result<(), WatchError>> { panic!("listener broke") })
				.unwrap(),
		);
		watch.advance(frame(json!(1), json!([["r", 1]])), context());
		let hook = std::panic::take_hook();
		std::panic::set_hook(Box::new(|_| {}));
		assert_eq!(executor.run(), 0);
		std::panic::set_hook(hook);
		let expected = "listener_error: listener panicked: listener broke";
		assert_eq!(end_kind(now(watch.closed())).as_deref(), Some(expected));
		assert_eq!(*reported.lock().unwrap(), ["listener panicked: listener broke"]);
		assert_eq!(detached.load(Ordering::SeqCst), 1);
	}

	// TS: watch.test.ts > Watch call objects > hands a listener plain data only: `{ kind, value, ops, retired }`,
	// cloneable and serializable (a `WatchCall` holds nothing but those fields by type; this checks a folded call's)
	#[test]
	fn hands_a_listener_plain_data_only() {
		let executor = Executor::default();
		let (watch, ..) = recorded_watch(json!({"n": 0}));
		let calls = start_recording(&executor, &watch, &[], &CancellationToken::new());
		let producer = || with_abort_signal(CancellationToken::new(), &context());
		watch.advance(frame(json!({"n": 1}), json!([["s", ["n"], 1]])), producer());
		executor.run();
		watch.advance(frame(json!({"n": 2}), json!([["s", ["n"], 2]])), producer());
		watch.advance(retirement(json!({"n": 2}), json!([])), producer());
		executor.run();
		assert_eq!(
			values(&calls),
			[
				(json!({"n": 1}), json!([["s", ["n"], 1]])),
				(json!({"n": 2}), json!([["s", ["n"], 2]]))
			]
		);
		assert_eq!(retired_flags(&calls), [false, true]);
		let copy = calls.lock().unwrap()[1].clone();
		assert!(matches!(copy, WatchCall::Value { retired: true, .. }));
	}

	// Rust-only (D0b review): `replace` runs outside the watch's lock, so it may read the watch.
	#[test]
	fn native_replace_may_call_into_the_watch() {
		let slot: Arc<Mutex<Option<NativeWatch<i64>>>> = Arc::default();
		let reader = slot.clone();
		let watch = NativeWatch::with_replace(
			0,
			|| {},
			|_| {},
			move || reader.lock().unwrap().as_ref().map_or(-1, |watch| watch.value() - 1),
		);
		*slot.lock().unwrap() = Some(watch.clone());
		for n in 1..=101 {
			watch.advance(n, context());
		}
		let executor = Executor::default();
		let calls = start_native(&executor, &watch);
		executor.run();
		assert_eq!(native_values(&calls), ["-1"]);
		slot.lock().unwrap().take();
	}
}
