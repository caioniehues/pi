//! Event streams (ports `packages/ai/src/utils/event-stream.ts`): `EventStream<T, R>`, an async queue whose events
//! go to consumers in order and whose result resolves once, and `AssistantMessageEventStream`, the stream of one
//! assistant response, which also times it.
//!
//! The `partial` message. TS hands every event the same live object, mutated as the response grows. Here the
//! producer owns a [`PartialMessage`] and every event carries an `Arc<AssistantMessage>` snapshot taken at push time:
//! pushing clones the `Arc`, and the producer's next edit (`PartialMessage::edit`, `Arc::make_mut`) copies the message
//! only while a queued or in-flight event still holds the previous snapshot. A consumer that keeps up and drops its
//! events costs nothing; one that lags pays one copy of the message per edit while it lags. Measured costs and the
//! shared-block alternative are a C24 concern (rust/docs/slices.md, Carry-over).

use crate::diagnostics::now_ms;
use pi_json::JsonObject;
use pi_types::{
	AssistantMessage, AssistantMessageEvent, ContentDelta, ContentEnd, ContentStart, DoneReason, ErrorReason,
	KnownAssistantMessageEvent, StreamDone, StreamError, StreamStart, ToolCall, ToolCallEnd,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tokio::sync::{Notify, watch};

type IsComplete<T> = dyn Fn(&T) -> bool + Send + Sync;
type ExtractResult<T, R> = dyn Fn(&T) -> R + Send + Sync;

/// An async event queue with a final result (`EventStream`). Clones share the queue: each event goes to one
/// consumer, in push order. Events stay queued until a consumer's `next()` takes them, so dropping a waiting
/// `next()` future (`select!`, `timeout`) loses nothing; waiting consumers are woken in the order they started.
pub struct EventStream<T, R> {
	shared: Arc<Shared<T, R>>,
}

struct Shared<T, R> {
	state: Mutex<State<T>>,
	/// Wakes waiting consumers: one per pushed event, all at the end.
	wake: Notify,
	result: watch::Sender<Option<R>>,
	is_complete: Box<IsComplete<T>>,
	extract_result: Box<ExtractResult<T, R>>,
}

struct State<T> {
	queue: VecDeque<T>,
	done: bool,
}

impl<T, R> Clone for EventStream<T, R> {
	fn clone(&self) -> Self {
		Self {
			shared: Arc::clone(&self.shared),
		}
	}
}

impl<T: Send + 'static, R: Clone + Send + Sync + 'static> EventStream<T, R> {
	/// A stream that completes on the first event `is_complete` accepts, with `extract_result` of it as the result.
	pub fn new(
		is_complete: impl Fn(&T) -> bool + Send + Sync + 'static,
		extract_result: impl Fn(&T) -> R + Send + Sync + 'static,
	) -> Self {
		Self {
			shared: Arc::new(Shared {
				state: Mutex::new(State {
					queue: VecDeque::new(),
					done: false,
				}),
				wake: Notify::new(),
				result: watch::channel(None).0,
				is_complete: Box::new(is_complete),
				extract_result: Box::new(extract_result),
			}),
		}
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, State<T>> {
		// Nothing under the lock panics, so a poisoned lock still holds consistent state.
		self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Queues an event and wakes one waiting consumer; dropped once the stream is done.
	pub fn push(&self, event: T) {
		let mut state = self.lock();
		if state.done {
			return;
		}
		if (self.shared.is_complete)(&event) {
			state.done = true;
			self.resolve((self.shared.extract_result)(&event));
		}
		state.queue.push_back(event);
		drop(state);
		// A woken consumer whose future is dropped before it runs passes the wake-up on (tokio's `Notified`).
		self.shared.wake.notify_one();
	}

	/// Ends the stream: waiting consumers see the end, and `result` resolves with it unless already resolved.
	pub fn end(&self, result: Option<R>) {
		let mut state = self.lock();
		state.done = true;
		if let Some(result) = result {
			self.resolve(result);
		}
		drop(state);
		self.shared.wake.notify_waiters();
	}

	fn resolve(&self, result: R) {
		self.shared.result.send_if_modified(|current| {
			if current.is_some() {
				return false;
			}
			*current = Some(result);
			true
		});
	}

	/// Whether the stream is done: completed by an event or ended.
	pub fn is_done(&self) -> bool {
		self.lock().done
	}

	/// The next event; `None` once the stream is done and drained.
	pub async fn next(&self) -> Option<T> {
		loop {
			let woken = self.shared.wake.notified();
			tokio::pin!(woken);
			// Registered before the queue is checked, so a push between the check and the wait is not missed.
			woken.as_mut().enable();
			{
				let mut state = self.lock();
				if let Some(event) = state.queue.pop_front() {
					return Some(event);
				}
				if state.done {
					return None;
				}
			}
			woken.await;
		}
	}

	/// The result, if it is already known.
	pub fn try_result(&self) -> Option<R> {
		self.shared.result.borrow().clone()
	}

	/// The final result. TS parity: a stream ended without one never resolves.
	pub async fn result(&self) -> R {
		let mut receiver = self.shared.result.subscribe();
		loop {
			if let Some(result) = receiver.borrow_and_update().clone() {
				return result;
			}
			if receiver.changed().await.is_err() {
				std::future::pending::<()>().await;
			}
		}
	}
}

/// The response so far, owned by the producer; events carry snapshots of it (see the module comment).
#[derive(Debug, Clone)]
pub struct PartialMessage {
	message: Arc<AssistantMessage>,
}

impl PartialMessage {
	/// The partial of a response that starts as `message`.
	pub fn new(message: AssistantMessage) -> Self {
		Self {
			message: Arc::new(message),
		}
	}

	/// The response so far.
	pub fn get(&self) -> &AssistantMessage {
		&self.message
	}

	/// The response so far, to change; copies it only while an event still holds the previous snapshot.
	pub fn edit(&mut self) -> &mut AssistantMessage {
		Arc::make_mut(&mut self.message)
	}

	/// A snapshot for an event.
	pub fn snapshot(&self) -> Arc<AssistantMessage> {
		Arc::clone(&self.message)
	}

	/// The message, without a copy when no event holds it.
	pub fn into_message(self) -> AssistantMessage {
		Arc::unwrap_or_clone(self.message)
	}
}

/// The event stream of one assistant response (`AssistantMessageEventStream`). The final message (of `done`,
/// `error` or `end`) gets `duration_ms` from the stream's creation on a monotonic clock, unless it has one, its
/// `timestamp` predates the stream (a deferred result fetched later), or the stream is already done.
#[derive(Clone)]
pub struct AssistantMessageEventStream {
	inner: EventStream<AssistantMessageEvent, AssistantMessage>,
	started_at: u64,
	started: Instant,
}

impl Default for AssistantMessageEventStream {
	fn default() -> Self {
		Self::new()
	}
}

fn terminal_message(event: &AssistantMessageEvent) -> Option<&AssistantMessage> {
	match event {
		AssistantMessageEvent::Known(KnownAssistantMessageEvent::Done(done)) => Some(&done.message),
		AssistantMessageEvent::Known(KnownAssistantMessageEvent::Error(error)) => Some(&error.error),
		_ => None,
	}
}

impl AssistantMessageEventStream {
	/// A new stream, timed from now (`createAssistantMessageEventStream`).
	pub fn new() -> Self {
		Self {
			inner: EventStream::new(
				|event| terminal_message(event).is_some(),
				|event| terminal_message(event).cloned().unwrap_or_else(unreachable_message),
			),
			started_at: now_ms(),
			started: Instant::now(),
		}
	}

	/// Pushes an event, timing a final message.
	pub fn push(&self, mut event: AssistantMessageEvent) {
		match &mut event {
			AssistantMessageEvent::Known(KnownAssistantMessageEvent::Done(done)) => self.time(&mut done.message),
			AssistantMessageEvent::Known(KnownAssistantMessageEvent::Error(error)) => self.time(&mut error.error),
			_ => {}
		}
		self.inner.push(event);
	}

	/// Ends the stream with a final message, timed.
	pub fn end(&self, result: Option<AssistantMessage>) {
		let result = result.map(|mut message| {
			self.time(&mut message);
			message
		});
		self.inner.end(result);
	}

	fn time(&self, message: &mut AssistantMessage) {
		if self.inner.is_done() || message.duration_ms.is_some() || message.timestamp < self.started_at {
			return;
		}
		let elapsed = self.started.elapsed().as_secs_f64() * 1000.0;
		#[allow(
			clippy::cast_possible_truncation,
			clippy::cast_sign_loss,
			reason = "a rounded non-negative duration"
		)]
		let rounded = elapsed.round() as u64;
		message.duration_ms = Some(rounded);
	}

	/// The next event; `None` at the end.
	pub async fn next(&self) -> Option<AssistantMessageEvent> {
		self.inner.next().await
	}

	/// The final message.
	pub async fn result(&self) -> AssistantMessage {
		self.inner.result().await
	}

	/// Whether the stream is done.
	pub fn is_done(&self) -> bool {
		self.inner.is_done()
	}

	/// The final message, if it is already known.
	pub fn try_result(&self) -> Option<AssistantMessage> {
		self.inner.try_result()
	}

	/// `start`.
	pub fn push_start(&self, partial: &PartialMessage) {
		self.push_known(KnownAssistantMessageEvent::Start(StreamStart {
			partial: partial.snapshot(),
			extra: JsonObject::new(),
		}));
	}

	/// `text_start`, `thinking_start` or `toolcall_start`.
	pub fn push_block_start(&self, kind: BlockKind, content_index: usize, partial: &PartialMessage) {
		let start = ContentStart {
			content_index: index(content_index),
			partial: partial.snapshot(),
			extra: JsonObject::new(),
		};
		self.push_known(match kind {
			BlockKind::Text => KnownAssistantMessageEvent::TextStart(start),
			BlockKind::Thinking => KnownAssistantMessageEvent::ThinkingStart(start),
			BlockKind::ToolCall => KnownAssistantMessageEvent::ToolCallStart(start),
		});
	}

	/// `text_delta`, `thinking_delta` or `toolcall_delta`.
	pub fn push_block_delta(&self, kind: BlockKind, content_index: usize, delta: &str, partial: &PartialMessage) {
		let delta = ContentDelta {
			content_index: index(content_index),
			delta: delta.to_owned(),
			partial: partial.snapshot(),
			extra: JsonObject::new(),
		};
		self.push_known(match kind {
			BlockKind::Text => KnownAssistantMessageEvent::TextDelta(delta),
			BlockKind::Thinking => KnownAssistantMessageEvent::ThinkingDelta(delta),
			BlockKind::ToolCall => KnownAssistantMessageEvent::ToolCallDelta(delta),
		});
	}

	/// `text_end` (`thinking` false) or `thinking_end` with the block's authoritative content.
	pub fn push_text_end(&self, kind: TextKind, content_index: usize, content: &str, partial: &PartialMessage) {
		let end = ContentEnd {
			content_index: index(content_index),
			content: content.to_owned(),
			partial: partial.snapshot(),
			extra: JsonObject::new(),
		};
		self.push_known(match kind {
			TextKind::Text => KnownAssistantMessageEvent::TextEnd(end),
			TextKind::Thinking => KnownAssistantMessageEvent::ThinkingEnd(end),
		});
	}

	/// `toolcall_end` with the complete call.
	pub fn push_tool_call_end(&self, content_index: usize, tool_call: ToolCall, partial: &PartialMessage) {
		self.push_known(KnownAssistantMessageEvent::ToolCallEnd(ToolCallEnd {
			content_index: index(content_index),
			tool_call,
			partial: partial.snapshot(),
			extra: JsonObject::new(),
		}));
	}

	/// `done` with the final message.
	pub fn push_done(&self, reason: DoneReason, message: AssistantMessage) {
		self.push_known(KnownAssistantMessageEvent::Done(StreamDone {
			reason,
			message,
			extra: JsonObject::new(),
		}));
	}

	/// `error` with the final message.
	pub fn push_error(&self, reason: ErrorReason, error: AssistantMessage) {
		self.push_known(KnownAssistantMessageEvent::Error(StreamError {
			reason,
			error,
			extra: JsonObject::new(),
		}));
	}

	fn push_known(&self, event: KnownAssistantMessageEvent) {
		self.push(AssistantMessageEvent::Known(event));
	}
}

/// The block kinds of start and delta events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
	/// A text block.
	Text,
	/// A thinking block.
	Thinking,
	/// A tool call.
	ToolCall,
}

/// The block kinds of `*_end` events that carry content text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
	/// `text_end`.
	Text,
	/// `thinking_end`.
	Thinking,
}

fn index(content_index: usize) -> u64 {
	u64::try_from(content_index).unwrap_or(u64::MAX)
}

/// invariant: `extract_result` runs only for events `is_complete` accepted, which are terminal.
fn unreachable_message() -> AssistantMessage {
	unreachable!("only terminal events complete an assistant message stream")
}
