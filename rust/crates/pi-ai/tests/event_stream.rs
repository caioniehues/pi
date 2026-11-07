//! Event streams (`event_stream.rs`): ports `event-stream.test.ts` (regression tests for #9055) and the partial
//! snapshot rule of the Rust stream.

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

mod common;

use pi_ai::event_stream::{AssistantMessageEventStream, BlockKind, EventStream, PartialMessage};
use pi_types::{
	AssistantMessage, AssistantMessageEvent, DoneReason, ErrorReason, KnownAssistantMessageEvent, StopReason,
};
use serde_json::json;
use std::time::Duration;

fn numbers(complete_at: Option<u32>) -> EventStream<u32, u32> {
	EventStream::new(move |event| Some(*event) == complete_at, |event| *event)
}

async fn drain(stream: &EventStream<u32, u32>) -> Vec<u32> {
	let mut events = Vec::new();
	while let Some(event) = stream.next().await {
		events.push(event);
	}
	events
}

// TS: event-stream.test.ts
#[tokio::test]
async fn drains_buffered_events_in_order_and_ignores_events_pushed_after_completion() {
	let stream = numbers(Some(3));
	for event in 1..=4 {
		stream.push(event);
	}
	assert_eq!(stream.result().await, 3);
	assert_eq!(drain(&stream).await, [1, 2, 3]);
}

// TS: event-stream.test.ts
#[tokio::test]
async fn preserves_order_when_events_arrive_after_buffered_draining_starts() {
	let stream = numbers(None);
	stream.push(1);
	stream.push(2);
	assert_eq!(stream.next().await, Some(1));
	stream.push(3);
	assert_eq!(stream.next().await, Some(2));
	assert_eq!(stream.next().await, Some(3));
	stream.end(Some(3));
	assert_eq!(stream.next().await, None);
}

// TS: event-stream.test.ts
#[tokio::test]
async fn delivers_events_to_waiting_consumers_in_registration_order() {
	let stream = numbers(None);
	let (first, second) = (stream.clone(), stream.clone());
	let first = tokio::spawn(async move { first.next().await });
	tokio::task::yield_now().await;
	let second = tokio::spawn(async move { second.next().await });
	tokio::task::yield_now().await;
	stream.push(1);
	stream.push(2);
	assert_eq!(first.await.unwrap(), Some(1));
	assert_eq!(second.await.unwrap(), Some(2));
}

// TS: event-stream.test.ts
#[tokio::test]
async fn drains_buffered_events_after_end_and_resolves_the_explicit_result() {
	let stream: EventStream<u32, String> = EventStream::new(|_: &u32| false, |event: &u32| event.to_string());
	stream.push(1);
	stream.push(2);
	stream.end(Some("complete".into()));
	assert_eq!(stream.result().await, "complete");
	assert_eq!(stream.next().await, Some(1));
	assert_eq!(stream.next().await, Some(2));
	assert_eq!(stream.next().await, None);
}

// TS: event-stream.test.ts
#[tokio::test]
async fn wakes_all_waiting_consumers_when_ended_without_a_result() {
	let stream = numbers(None);
	let (first, second) = (stream.clone(), stream.clone());
	let first = tokio::spawn(async move { first.next().await });
	let second = tokio::spawn(async move { second.next().await });
	tokio::task::yield_now().await;
	stream.end(None);
	assert_eq!(first.await.unwrap(), None);
	assert_eq!(second.await.unwrap(), None);
}

fn message(timestamp: u64, duration_ms: Option<u64>) -> AssistantMessage {
	let mut message = common::assistant(json!({ "timestamp": timestamp }));
	message.duration_ms = duration_ms;
	message
}

fn now() -> u64 {
	pi_ai::diagnostics::now_ms()
}

// TS: event-stream.test.ts
#[tokio::test]
async fn sets_duration_ms_on_the_final_done_or_error_message_of_a_response_it_saw_start() {
	let done = AssistantMessageEventStream::new();
	let answer = message(now(), None);
	tokio::time::sleep(Duration::from_millis(20)).await;
	done.push_done(DoneReason::Stop, answer);
	assert!(done.result().await.duration_ms.unwrap() >= 15);

	let failed = AssistantMessageEventStream::new();
	let mut error = message(now(), None);
	error.stop_reason = StopReason::Error;
	failed.push_error(ErrorReason::Error, error);
	assert!(failed.result().await.duration_ms.is_some());

	let ended = AssistantMessageEventStream::new();
	ended.end(Some(message(now(), None)));
	assert!(ended.result().await.duration_ms.is_some());
}

// TS: event-stream.test.ts
#[tokio::test]
async fn keeps_an_existing_duration_so_a_forwarding_stream_keeps_the_inner_measurement() {
	let outer = AssistantMessageEventStream::new();
	tokio::time::sleep(Duration::from_millis(20)).await;
	let inner = AssistantMessageEventStream::new();
	inner.push_done(DoneReason::Stop, message(now(), None));
	let measured = inner.result().await;
	outer.push_done(DoneReason::Stop, measured.clone());
	assert_eq!(outer.result().await.duration_ms, measured.duration_ms);
	assert!(measured.duration_ms.unwrap() < 20);

	let preset = AssistantMessageEventStream::new();
	preset.push_done(DoneReason::Stop, message(now(), Some(1234)));
	assert_eq!(preset.result().await.duration_ms, Some(1234));
}

// TS: event-stream.test.ts
#[tokio::test]
async fn leaves_a_message_untimed_when_it_started_before_the_stream_such_as_a_fetched_deferred_result() {
	let stream = AssistantMessageEventStream::new();
	stream.push_done(DoneReason::Stop, message(now() - 60_000, None));
	assert_eq!(stream.result().await.duration_ms, None);
}

// TS: event-stream.test.ts
#[tokio::test]
async fn does_not_time_a_message_pushed_after_the_stream_completed() {
	let stream = AssistantMessageEventStream::new();
	stream.push_done(DoneReason::Stop, message(now(), None));
	stream.push_done(DoneReason::Stop, message(now(), None));
	// The late event is dropped, so the only terminal event is the first one.
	let mut terminals = 0;
	while let Some(event) = stream.next().await {
		if matches!(event, AssistantMessageEvent::Known(KnownAssistantMessageEvent::Done(_))) {
			terminals += 1;
		}
	}
	assert_eq!(terminals, 1);
}

// Rust-only.
#[tokio::test]
async fn partial_snapshots_are_taken_at_push_time_and_copied_only_while_held() {
	let stream = AssistantMessageEventStream::new();
	let mut partial = PartialMessage::new(message(now(), None));
	stream.push_start(&partial);
	partial
		.edit()
		.content
		.push(pi_types::AssistantContentBlock::Text(pi_types::TextContent::new("a")));
	stream.push_block_start(BlockKind::Text, 0, &partial);
	let Some(AssistantMessageEvent::Known(KnownAssistantMessageEvent::Start(start))) = stream.next().await else {
		panic!("start")
	};
	// The start event's snapshot was taken before the edit, so the edit copied the message.
	assert!(start.partial.content.is_empty());
	let Some(AssistantMessageEvent::Known(KnownAssistantMessageEvent::TextStart(text))) = stream.next().await else {
		panic!("text_start")
	};
	assert_eq!(text.partial.content.len(), 1);
	drop(text);
	drop(start);
	// No event holds the current snapshot now, so editing does not copy.
	let before = std::sync::Arc::as_ptr(&partial.snapshot());
	partial.edit().stop_reason = StopReason::Stop;
	assert_eq!(std::sync::Arc::as_ptr(&partial.snapshot()), before);
}

#[tokio::test]
async fn keeps_an_event_whose_waiting_next_future_is_dropped() {
	// Rust-only: cancel safety of `next()` under `select!` and `timeout`.
	let stream = numbers(None);
	{
		let next = stream.next();
		tokio::pin!(next);
		let pending = std::future::poll_fn(|cx| {
			std::task::Poll::Ready(std::future::Future::poll(next.as_mut(), cx).is_pending())
		})
		.await;
		assert!(pending);
		stream.push(1);
	}
	stream.push(2);
	stream.end(None);
	assert_eq!(drain(&stream).await, [1, 2]);
}
