//! The SSE decoder (`sse.rs`). The decoder rules are those of
//! `iterateSseMessages` in `anthropic-messages.ts`; `anthropic-sse-parsing.test.ts` exercises them through the
//! Anthropic API in C3b, so the rules are checked here directly.

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

use bytes::Bytes;
use pi_ai::sse::{ServerSentEvent, SseDecoder};
use std::convert::Infallible;

async fn decode(chunks: &[&[u8]]) -> Vec<ServerSentEvent> {
	let body = futures_util::stream::iter(
		chunks
			.iter()
			.map(|chunk| Ok::<_, Infallible>(Bytes::copy_from_slice(chunk)))
			.collect::<Vec<_>>(),
	);
	let mut decoder = SseDecoder::new(body);
	let mut events = Vec::new();
	while let Some(event) = decoder.next_event().await.unwrap() {
		events.push(event);
	}
	events
}

fn event(name: Option<&str>, data: &str, raw: &[&str]) -> ServerSentEvent {
	ServerSentEvent {
		event: name.map(str::to_owned),
		data: data.into(),
		raw: raw.iter().map(|line| (*line).to_owned()).collect(),
	}
}

#[tokio::test]
async fn splits_events_on_blank_lines_with_lf_crlf_and_cr_line_ends() {
	for body in [
		"event: a\ndata: 1\n\nevent: b\ndata: 2\n\n",
		"event: a\r\ndata: 1\r\n\r\nevent: b\r\ndata: 2\r\n\r\n",
		"event: a\rdata: 1\r\revent: b\rdata: 2\r\r",
	] {
		assert_eq!(
			decode(&[body.as_bytes()]).await,
			[
				event(Some("a"), "1", &["event: a", "data: 1"]),
				event(Some("b"), "2", &["event: b", "data: 2"])
			],
			"{body:?}"
		);
	}
}

#[tokio::test]
async fn joins_data_lines_ignores_comments_and_unknown_fields_and_strips_one_space() {
	let events = decode(&[b": comment\ndata:first\ndata:  second\nid: 7\nretry\n\n"]).await;
	assert_eq!(
		events,
		[event(
			None,
			"first\n second",
			&[": comment", "data:first", "data:  second", "id: 7", "retry"]
		)]
	);
}

#[tokio::test]
async fn flushes_a_final_unterminated_line_and_event_at_the_end_of_the_body() {
	assert_eq!(
		decode(&[b"event: end\ndata: last"]).await,
		[event(Some("end"), "last", &["event: end", "data: last"])]
	);
}

#[tokio::test]
async fn a_crlf_split_across_chunks_reads_as_an_extra_line_end_as_in_ts() {
	// The CR ends the line as soon as it arrives; the LF of the next chunk ends an empty line, dispatching the event.
	let events = decode(&[b"data: a\r", b"\ndata: b\n\n"]).await;
	assert_eq!(events, [event(None, "a", &["data: a"]), event(None, "b", &["data: b"])]);
}

#[tokio::test]
async fn decodes_utf8_split_across_chunks_drops_a_leading_bom_and_replaces_invalid_bytes() {
	let text = "\u{feff}data: é😀\n\n".as_bytes();
	let events = decode(&[&text[..9], &text[9..12], &text[12..]]).await;
	assert_eq!(events, [event(None, "é😀", &["data: é😀"])]);
	assert_eq!(decode(&[b"data: \xff\n\n"]).await[0].data, "\u{fffd}");
}

#[tokio::test]
async fn an_event_name_without_data_dispatches_and_an_empty_one_does_not() {
	assert_eq!(
		decode(&[b"event: ping\n\n"]).await,
		[event(Some("ping"), "", &["event: ping"])]
	);
	assert!(decode(&[b"event:\n\n"]).await.is_empty());
}

#[tokio::test]
async fn decodes_a_4_mb_single_chunk_body_of_100k_events_in_linear_time() {
	// Rust-only: the decoder must not rescan or shift the buffer per line.
	let body: String = (0..100_000)
		.map(|index| format!("event: e\ndata: {index:030}\n\n"))
		.collect();
	assert!(body.len() > 4_000_000);
	let started = std::time::Instant::now();
	let events = decode(&[body.as_bytes()]).await;
	assert_eq!(events.len(), 100_000);
	assert!(started.elapsed().as_millis() < 1000, "{:?}", started.elapsed());
	// One unterminated 4 MB line arriving in small chunks is scanned once.
	let long = format!("data: {}\n\n", "x".repeat(4_000_000));
	let chunks: Vec<&[u8]> = long.as_bytes().chunks(1024).collect();
	let started = std::time::Instant::now();
	assert_eq!(decode(&chunks).await[0].data.len(), 4_000_000);
	assert!(started.elapsed().as_millis() < 1000, "{:?}", started.elapsed());
}
