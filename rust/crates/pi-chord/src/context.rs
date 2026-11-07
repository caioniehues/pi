//! The explicit invocation context (TS `context/index.ts`): an immutable, cheaply clonable value carrying caller
//! cancellation, the telemetry span work runs under, and typed application values, passed as the last argument.

use crate::json::JsonObject;
use std::any::Any;
use std::fmt;
use std::future::{Future, poll_fn};
use std::marker::PhantomData;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::{Context as TaskContext, Poll};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// The span work runs under: the part of `pi-telemetry`'s span the context needs (replaced by
/// `pi_telemetry::TelemetrySpan` once C1 lands).
pub trait TelemetrySpan: Send + Sync {
	/// Records an event on the span.
	fn add_event(&self, name: &str, attributes: &JsonObject);

	/// Sets attributes on the span.
	fn set_attributes(&self, attributes: &JsonObject);

	/// Starts a child span.
	fn start_child(&self, name: &str) -> Arc<dyn TelemetrySpan>;
}

/// The span of a context without telemetry: records nothing, and its children are itself.
pub static NOOP_SPAN: LazyLock<Arc<dyn TelemetrySpan>> = LazyLock::new(|| Arc::new(NoopSpan));

/// Typed identity for one value carried by a `Context`; two keys are distinct even with one description.
pub struct ContextKey<T> {
	id: u64,
	description: &'static str,
	value_type: PhantomData<fn(T) -> T>,
}

/// Caller cancellation: aborted once any of its tokens is cancelled (TS `AbortSignal.any` of the chain).
#[derive(Clone, Debug)]
pub struct AbortSignal {
	tokens: Arc<[CancellationToken]>,
}

/// Immutable invocation-scoped values passed explicitly through operations (TS `Context`).
#[derive(Clone)]
pub struct Context(Scope);

/// Why a wait on behalf of a context ended without the awaited value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
	/// The context's abort signal fired first.
	#[error("The operation was aborted")]
	Aborted,
}

/// The root of long-lived work: no cancellation, no telemetry, no values.
pub static BACKGROUND_CONTEXT: Context = Context(Scope::Root("[Context BACKGROUND_CONTEXT]"));

/// A root for call sites that do not have a context yet; distinct from `BACKGROUND_CONTEXT` so they can be found.
pub static TODO_CONTEXT: Context = Context(Scope::Root("[Context TODO_CONTEXT]"));

#[derive(Clone)]
enum Scope {
	Root(&'static str),
	Derived(Arc<Derived>),
}

/// One link of the chain: the entry it adds, and the signal and span in effect, resolved once at derivation.
struct Derived {
	parent: Context,
	entry: Entry,
	abort_signal: Option<AbortSignal>,
	telemetry: Arc<dyn TelemetrySpan>,
}

enum Entry {
	AbortSignal,
	Telemetry,
	Value {
		key: u64,
		description: &'static str,
		value: Arc<dyn Any + Send + Sync>,
	},
}

/// Derives a context carrying `value` under `key`, replacing a value of `key` further up the chain.
pub fn with_value<T: Send + Sync + 'static>(key: &ContextKey<T>, value: T, context: &Context) -> Context {
	let entry = Entry::Value {
		key: key.id,
		description: key.description,
		value: Arc::new(value),
	};
	context.derive(entry, context.abort_signal().cloned(), context.telemetry().clone())
}

/// Derives a context whose telemetry parent is `telemetry`, keeping cancellation and values.
pub fn with_telemetry(telemetry: Arc<dyn TelemetrySpan>, context: &Context) -> Context {
	context.derive(Entry::Telemetry, context.abort_signal().cloned(), telemetry)
}

/// Derives a context cancelled by either the parent's signal or `signal`; the parent is unchanged.
pub fn with_abort_signal(signal: CancellationToken, context: &Context) -> Context {
	let tokens: Arc<[CancellationToken]> = match context.abort_signal() {
		None => Arc::from([signal]),
		Some(parent) => parent.tokens.iter().cloned().chain([signal]).collect(),
	};
	context.derive(
		Entry::AbortSignal,
		Some(AbortSignal { tokens }),
		context.telemetry().clone(),
	)
}

/// Derives a context keeping every value but caller cancellation; for mandatory cleanup only.
pub fn without_abort_signal(context: &Context) -> Context {
	context.derive(Entry::AbortSignal, None, context.telemetry().clone())
}

/// Derives an independently cancellable child: cancelling the token cancels the child, never the parent.
pub fn with_cancel(context: &Context) -> (Context, CancellationToken) {
	let token = CancellationToken::new();
	(with_abort_signal(token.clone(), context), token)
}

/// Awaits `future` unless the context is cancelled first; cancellation ends only this wait, never the work itself
/// (dropping the returned future drops `future`, so pass work that must go on as a handle to it).
pub fn await_with_context<F: Future>(
	future: F,
	context: &Context,
) -> impl Future<Output = Result<F::Output, ContextError>> {
	let signal = context.abort_signal().cloned();
	async move {
		let Some(signal) = signal else {
			return Ok(future.await);
		};
		let mut aborted = signal.aborted();
		let mut future = pin!(future);
		poll_fn(move |cx| {
			if Pin::new(&mut aborted).poll(cx).is_ready() {
				return Poll::Ready(Err(ContextError::Aborted));
			}
			future.as_mut().poll(cx).map(Ok)
		})
		.await
	}
}

impl Context {
	/// The caller's cancellation, if any.
	pub fn abort_signal(&self) -> Option<&AbortSignal> {
		match &self.0 {
			Scope::Root(_) => None,
			Scope::Derived(derived) => derived.abort_signal.as_ref(),
		}
	}

	/// The span work under this context runs in; `NOOP_SPAN` unless one was set.
	pub fn telemetry(&self) -> &Arc<dyn TelemetrySpan> {
		match &self.0 {
			Scope::Root(_) => &NOOP_SPAN,
			Scope::Derived(derived) => &derived.telemetry,
		}
	}

	/// The value of `key` nearest to this context in the chain.
	pub fn value<T: Send + Sync + 'static>(&self, key: &ContextKey<T>) -> Option<&T> {
		let mut context = self;
		while let Scope::Derived(derived) = &context.0 {
			if let Entry::Value { key: id, value, .. } = &derived.entry
				&& *id == key.id
			{
				return value.downcast_ref();
			}
			context = &derived.parent;
		}
		None
	}

	fn derive(&self, entry: Entry, abort_signal: Option<AbortSignal>, telemetry: Arc<dyn TelemetrySpan>) -> Context {
		Context(Scope::Derived(Arc::new(Derived {
			parent: self.clone(),
			entry,
			abort_signal,
			telemetry,
		})))
	}
}

impl fmt::Display for Context {
	/// The chain as TS `toString()` prints it: `[Context BACKGROUND_CONTEXT].WithValue(name)...`.
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match &self.0 {
			Scope::Root(name) => f.write_str(name),
			Scope::Derived(derived) => {
				let description = match &derived.entry {
					Entry::AbortSignal => "chord.abortSignal",
					Entry::Telemetry => "chord.telemetry",
					Entry::Value { description, .. } => description,
				};
				write!(f, "{}.WithValue({description})", derived.parent)
			}
		}
	}
}

impl fmt::Debug for Context {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Display::fmt(self, f)
	}
}

/// Ids of context keys; a counter, so every key is distinct.
static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

impl<T> ContextKey<T> {
	/// A new key; `description` names it in `Context`'s `Display`.
	pub fn new(description: &'static str) -> Self {
		Self {
			id: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
			description,
			value_type: PhantomData,
		}
	}
}

impl AbortSignal {
	/// True once any token of the signal is cancelled.
	pub fn is_aborted(&self) -> bool {
		self.tokens.iter().any(CancellationToken::is_cancelled)
	}

	/// Resolves once the signal is aborted.
	pub fn aborted(&self) -> WaitForAbort {
		WaitForAbort {
			waits: self
				.tokens
				.iter()
				.map(|token| Box::pin(token.clone().cancelled_owned()))
				.collect(),
		}
	}

	/// True when both are the same signal (one derivation's, shared down the chain).
	pub fn ptr_eq(&self, other: &AbortSignal) -> bool {
		Arc::ptr_eq(&self.tokens, &other.tokens)
	}
}

/// The future of `AbortSignal::aborted`.
pub struct WaitForAbort {
	waits: Vec<Pin<Box<WaitForCancellationFutureOwned>>>,
}

impl Future for WaitForAbort {
	type Output = ();

	fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
		let any = self
			.get_mut()
			.waits
			.iter_mut()
			.any(|wait| wait.as_mut().poll(cx).is_ready());
		if any { Poll::Ready(()) } else { Poll::Pending }
	}
}

struct NoopSpan;

impl TelemetrySpan for NoopSpan {
	fn add_event(&self, _name: &str, _attributes: &JsonObject) {}

	fn set_attributes(&self, _attributes: &JsonObject) {}

	fn start_child(&self, _name: &str) -> Arc<dyn TelemetrySpan> {
		NOOP_SPAN.clone()
	}
}

#[cfg(test)]
mod tests {
	use super::{
		BACKGROUND_CONTEXT, ContextError, ContextKey, NOOP_SPAN, TODO_CONTEXT, TelemetrySpan, await_with_context,
		with_abort_signal, with_cancel, with_telemetry, with_value, without_abort_signal,
	};
	use crate::json::JsonObject;
	use std::future::Future;
	use std::pin::pin;
	use std::sync::{Arc, Mutex};
	use std::task::{Context as TaskContext, Poll, Wake, Waker};
	use std::thread::{self, Thread};
	use tokio_util::sync::CancellationToken;

	/// Runs a future to completion on this thread, parking between polls.
	fn block_on<F: Future>(future: F) -> F::Output {
		struct Unpark(Thread);
		impl Wake for Unpark {
			fn wake(self: Arc<Self>) {
				self.0.unpark();
			}
		}
		let waker = Waker::from(Arc::new(Unpark(thread::current())));
		let mut cx = TaskContext::from_waker(&waker);
		let mut future = pin!(future);
		loop {
			if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
				return output;
			}
			thread::park();
		}
	}

	/// A span that records the names of its events.
	#[derive(Default)]
	struct RecordingSpan {
		events: Mutex<Vec<String>>,
	}

	impl TelemetrySpan for RecordingSpan {
		fn add_event(&self, name: &str, _attributes: &JsonObject) {
			self.events.lock().unwrap().push(name.to_owned());
		}

		fn set_attributes(&self, _attributes: &JsonObject) {}

		fn start_child(&self, _name: &str) -> Arc<dyn TelemetrySpan> {
			Arc::new(RecordingSpan::default())
		}
	}

	// TS: context.test.ts > Context > provides distinct empty root contexts
	#[test]
	fn provides_distinct_empty_root_contexts() {
		let key = ContextKey::<String>::new("value");
		assert!(TODO_CONTEXT.abort_signal().is_none());
		assert!(Arc::ptr_eq(TODO_CONTEXT.telemetry(), &NOOP_SPAN));
		assert!(Arc::ptr_eq(BACKGROUND_CONTEXT.telemetry(), &NOOP_SPAN));
		assert!(TODO_CONTEXT.value(&key).is_none());
		assert_eq!(BACKGROUND_CONTEXT.to_string(), "[Context BACKGROUND_CONTEXT]");
		assert_eq!(TODO_CONTEXT.to_string(), "[Context TODO_CONTEXT]");
	}

	// TS: context.test.ts > Context > layers typed values without modifying parents
	#[test]
	fn layers_typed_values_without_modifying_parents() {
		let first_key = ContextKey::<String>::new("first");
		let second_key = ContextKey::<u32>::new("second");
		let first = with_value(&first_key, "one".to_owned(), &BACKGROUND_CONTEXT);
		let second = with_value(&second_key, 2, &first);
		let replaced = with_value(&first_key, "updated".to_owned(), &second);

		assert!(BACKGROUND_CONTEXT.value(&first_key).is_none());
		assert_eq!(first.value(&first_key).map(String::as_str), Some("one"));
		assert!(first.value(&second_key).is_none());
		assert_eq!(second.value(&first_key).map(String::as_str), Some("one"));
		assert_eq!(second.value(&second_key), Some(&2));
		assert_eq!(replaced.value(&first_key).map(String::as_str), Some("updated"));
		assert_eq!(second.value(&first_key).map(String::as_str), Some("one"));
		assert_eq!(
			replaced.to_string(),
			"[Context BACKGROUND_CONTEXT].WithValue(first).WithValue(second).WithValue(first)"
		);
		let same_description = ContextKey::<String>::new("first");
		assert!(replaced.value(&same_description).is_none());
	}

	// TS: context.test.ts > Context > inherits parent cancellation and isolates child cancellation (tokens carry no
	// reason, so the reason checks are not ported)
	#[test]
	fn inherits_parent_cancellation_and_isolates_child_cancellation() {
		let parent_token = CancellationToken::new();
		let parent = with_abort_signal(parent_token.clone(), &BACKGROUND_CONTEXT);
		let (child, cancel_child) = with_cancel(&parent);
		let (sibling, _cancel_sibling) = with_cancel(&parent);

		cancel_child.cancel();
		assert!(child.abort_signal().unwrap().is_aborted());
		assert!(!sibling.abort_signal().unwrap().is_aborted());
		assert!(!parent.abort_signal().unwrap().is_aborted());
		block_on(child.abort_signal().unwrap().aborted());

		parent_token.cancel();
		assert!(sibling.abort_signal().unwrap().is_aborted());
	}

	// TS: context.test.ts > Context > masks caller cancellation for mandatory cleanup
	#[test]
	fn masks_caller_cancellation_for_mandatory_cleanup() {
		let token = CancellationToken::new();
		let key = ContextKey::<&str>::new("value");
		let context = with_value(
			&key,
			"preserved",
			&with_abort_signal(token.clone(), &BACKGROUND_CONTEXT),
		);
		let cleanup = without_abort_signal(&context);

		token.cancel();
		assert!(context.abort_signal().unwrap().is_aborted());
		assert!(cleanup.abort_signal().is_none());
		assert_eq!(cleanup.value(&key), Some(&"preserved"));
	}

	// TS: context.test.ts > Context > derives telemetry independently from values and cancellation (the traceIn half
	// waits for pi-telemetry, C1)
	#[test]
	fn derives_telemetry_independently_from_values_and_cancellation() {
		let parent: Arc<dyn TelemetrySpan> = Arc::new(RecordingSpan::default());
		let other: Arc<dyn TelemetrySpan> = Arc::new(RecordingSpan::default());
		let key = ContextKey::<&str>::new("principal");
		let token = CancellationToken::new();
		let base = with_value(&key, "owner", &with_abort_signal(token.clone(), &BACKGROUND_CONTEXT));
		let context = with_telemetry(parent.clone(), &base);
		let replaced = with_telemetry(other.clone(), &context);

		assert!(Arc::ptr_eq(base.telemetry(), &NOOP_SPAN));
		assert!(Arc::ptr_eq(context.telemetry(), &parent));
		assert!(Arc::ptr_eq(replaced.telemetry(), &other));
		assert_eq!(replaced.value(&key), Some(&"owner"));
		assert!(replaced.abort_signal().unwrap().ptr_eq(base.abort_signal().unwrap()));
		assert!(Arc::ptr_eq(without_abort_signal(&context).telemetry(), &parent));
		context.telemetry().add_event("started", &JsonObject::new());
		token.cancel();
		assert!(context.abort_signal().unwrap().is_aborted());
		assert_eq!(
			context.to_string(),
			"[Context BACKGROUND_CONTEXT].WithValue(chord.abortSignal).WithValue(principal).WithValue(chord.telemetry)"
		);
	}

	// TS: context.test.ts > Context > stops waiting when the invocation is cancelled
	#[test]
	fn stops_waiting_when_the_invocation_is_cancelled() {
		let token = CancellationToken::new();
		let context = with_abort_signal(token.clone(), &BACKGROUND_CONTEXT);
		let work = CancellationToken::new();
		let canceller = {
			let token = token.clone();
			thread::spawn(move || token.cancel())
		};
		let waiting = block_on(await_with_context(work.clone().cancelled_owned(), &context));
		canceller.join().unwrap();
		assert_eq!(waiting, Err(ContextError::Aborted));
		assert_eq!(ContextError::Aborted.to_string(), "The operation was aborted");

		assert_eq!(
			block_on(await_with_context(async { "completed" }, &BACKGROUND_CONTEXT)),
			Ok("completed")
		);
		assert_eq!(
			block_on(await_with_context(async { "completed" }, &context)),
			Err(ContextError::Aborted)
		);
		let live = with_abort_signal(CancellationToken::new(), &BACKGROUND_CONTEXT);
		assert_eq!(
			block_on(await_with_context(async { "completed" }, &live)),
			Ok("completed")
		);
	}
}
