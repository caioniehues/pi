//! A stream returned at once while async setup (auth resolution, provider dispatch) runs behind it (ports
//! `lazyStream` of `packages/ai/src/api/lazy.ts`). Setup failures end the stream with an error event.
//!
//! The setup runs on a spawned task, so these functions must be called inside a tokio runtime.

use crate::diagnostics::now_ms;
use crate::error::ModelsError;
use crate::event_stream::AssistantMessageEventStream;
use pi_json::JsonObject;
use pi_types::{
	AssistantMessage, AssistantMessageEvent, AssistantRole, ErrorReason, KnownAssistantMessageEvent, Model, StopReason,
	Usage,
};
use std::future::Future;
use tokio_util::sync::CancellationToken;

/// Synchronous observation of the forwarded events and the final result (`LazyStreamObserver`).
pub trait LazyStreamObserver: Send {
	/// Each event, as it is forwarded.
	fn on_event(&mut self, _event: &AssistantMessageEvent) {}

	/// The final message once, `None` when the source gave none.
	fn on_result(&mut self, _result: Option<&AssistantMessage>) {}
}

/// An observer that observes nothing.
pub struct NoObserver;

impl LazyStreamObserver for NoObserver {}

/// Runs `setup` behind a returned stream and forwards the stream it produces (`lazyStream`); a failed setup ends
/// the stream with an `error` event, `aborted` when `cancel` fired.
pub fn lazy_stream<F>(
	model: &Model,
	setup: F,
	mut observer: Box<dyn LazyStreamObserver>,
	cancel: Option<CancellationToken>,
) -> AssistantMessageEventStream
where
	F: Future<Output = Result<AssistantMessageEventStream, ModelsError>> + Send + 'static,
{
	let started_at = now_ms();
	let outer = AssistantMessageEventStream::new();
	let target = outer.clone();
	let (api, provider, id) = (model.api.clone(), model.provider.clone(), model.id.clone());
	tokio::spawn(async move {
		match setup.await {
			Ok(inner) => forward(&target, &inner, observer.as_mut()).await,
			Err(error) => {
				let reason = if cancel.is_some_and(|cancel| cancel.is_cancelled()) {
					ErrorReason::Aborted
				} else {
					ErrorReason::Error
				};
				let message = setup_error_message(api, provider, id, &error.message, started_at, reason);
				let event = AssistantMessageEvent::Known(KnownAssistantMessageEvent::Error(pi_types::StreamError {
					reason,
					error: message.clone(),
					extra: JsonObject::new(),
				}));
				observer.on_event(&event);
				target.push(event);
				target.end(Some(message.clone()));
				observer.on_result(Some(&message));
			}
		}
	});
	outer
}

async fn forward(
	target: &AssistantMessageEventStream,
	source: &AssistantMessageEventStream,
	observer: &mut dyn LazyStreamObserver,
) {
	let mut terminal = None;
	while let Some(event) = source.next().await {
		if let AssistantMessageEvent::Known(KnownAssistantMessageEvent::Done(done)) = &event {
			terminal = Some(done.message.clone());
		} else if let AssistantMessageEvent::Known(KnownAssistantMessageEvent::Error(error)) = &event {
			terminal = Some(error.error.clone());
		}
		// Observed before the push, which takes the event: no copy per event. The observer is synchronous, so
		// nothing tells this order from TS's (push, then observe).
		observer.on_event(&event);
		target.push(event);
	}
	// A source that ended without a result has none to await (TS would wait forever); its last terminal event is it.
	let result = source.try_result().or(terminal);
	target.end(result.clone());
	observer.on_result(result.as_ref());
}

fn setup_error_message(
	api: String,
	provider: String,
	model: String,
	error: &str,
	timestamp: u64,
	reason: ErrorReason,
) -> AssistantMessage {
	let stop_reason = match reason {
		ErrorReason::Aborted => StopReason::Aborted,
		ErrorReason::Error => StopReason::Error,
	};
	let mut message = assistant_message(api, provider, model, stop_reason, timestamp);
	message.error_message = Some(error.to_owned());
	message
}

/// An assistant message without content, with zero usage (no `totalFromProvider`), as providers start one.
pub fn assistant_message(
	api: String,
	provider: String,
	model: String,
	stop_reason: StopReason,
	timestamp: u64,
) -> AssistantMessage {
	AssistantMessage {
		role: AssistantRole::Assistant,
		content: Vec::new(),
		api,
		provider,
		model,
		usage: Usage::zero(),
		stop_reason,
		timestamp,
		provider_thinking_level: None,
		response_id: None,
		response_model: None,
		diagnostics: None,
		deferred: None,
		raw_stop_reason: None,
		end_turn: None,
		error_message: None,
		duration_ms: None,
		thinking_level: None,
		extra: JsonObject::new(),
	}
}
