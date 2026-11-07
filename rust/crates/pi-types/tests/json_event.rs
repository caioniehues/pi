//! The wire form of session events (`json_event.rs`); ports the cases of
//! `packages/coding-agent/test/suite/regressions/{7925,7290,7911}-*.test.ts` and the two TS errors.

mod common;

use common::{assistant, write};
use pi_json::JsonValue;
use pi_types::{
	AgentSessionEvent, JsonAgentSessionEvent, JsonAssistantMessageEvent, JsonEventError, KnownAgentSessionEvent,
	KnownJsonAssistantMessageEvent, to_json_event,
};

fn event(text: &str) -> AgentSessionEvent {
	common::read(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

fn update(stream_event: &str) -> AgentSessionEvent {
	let message = assistant();
	let stream_event = stream_event.replace("PARTIAL", &message);
	event(&format!(
		r#"{{"type":"message_update","message":{message},"assistantMessageEvent":{stream_event}}}"#
	))
}

fn json(text: &str) -> JsonValue {
	common::read(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

#[test]
fn issue_7925_toolcall_start_carries_the_call_id_and_tool_name() {
	let wire = to_json_event(update(
		r#"{"type":"toolcall_start","contentIndex":1,"partial":PARTIAL}"#,
	))
	.unwrap();
	assert_eq!(
		json(&write(&wire)),
		json(&format!(
			r#"{{"type":"message_update","usage":{},"assistantMessageEvent":{{"type":"toolcall_start","contentIndex":1,"id":"call_1","toolName":"read"}}}}"#,
			common::USAGE
		))
	);
	assert!(write(&wire).ends_with(r#""contentIndex":1,"id":"call_1","toolName":"read"}}"#));
}

/// The bytes of the wire updates of a response that streams `text` one character per delta.
fn streamed_update_bytes(text: &str) -> usize {
	let mut message: JsonValue = json(&assistant());
	message["content"] = json(r#"[{"type":"text","text":""}]"#);
	let (mut bytes, mut streamed) = (0, String::new());
	for character in text.chars() {
		streamed.push(character);
		message["content"][0]["text"] = JsonValue::from(streamed.as_str());
		let session_event = event(&format!(
			r#"{{"type":"message_update","message":{message},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"{character}","partial":{message}}}}}"#
		));
		let Ok(JsonAgentSessionEvent::MessageUpdate(update)) = to_json_event(session_event) else {
			panic!("a wire message_update")
		};
		let written = write(&update);
		assert!(
			!written.contains(r#""message""#) && !written.contains(r#""partial""#),
			"{written}"
		);
		bytes += written.len();
	}
	bytes
}

#[test]
fn issue_7290_message_updates_stay_linear() {
	let small = streamed_update_bytes(&"x".repeat(2_000));
	let large = streamed_update_bytes(&"x".repeat(4_000));
	assert!(large > small);
	#[expect(clippy::cast_precision_loss, reason = "byte counts far below 2^52")]
	let ratio = large as f64 / small as f64;
	assert!(ratio < 2.2, "{ratio}");
}

#[test]
fn issue_7911_message_updates_keep_the_cumulative_usage() {
	let wire = to_json_event(update(
		r#"{"type":"text_delta","contentIndex":0,"delta":"R","partial":PARTIAL}"#,
	))
	.unwrap();
	let JsonAgentSessionEvent::MessageUpdate(update) = wire else {
		panic!("a wire message_update")
	};
	assert_eq!(update.usage.total_tokens, Some(18));
	assert!(
		matches!(update.assistant_message_event, JsonAssistantMessageEvent::Known(KnownJsonAssistantMessageEvent::TextDelta(delta)) if delta.delta == "R")
	);
}

#[test]
fn every_stream_event_loses_only_its_snapshot() {
	let tool_call = r#"{"type":"toolCall","id":"call_1","name":"read","arguments":{"path":"a.txt"}}"#;
	let cases = [
		(
			r#"{"type":"start","partial":PARTIAL}"#.to_owned(),
			r#"{"type":"start"}"#.to_owned(),
		),
		(
			r#"{"type":"text_start","contentIndex":0,"partial":PARTIAL}"#.to_owned(),
			r#"{"type":"text_start","contentIndex":0}"#.to_owned(),
		),
		(
			r#"{"type":"text_end","contentIndex":0,"content":"Reading.","partial":PARTIAL}"#.to_owned(),
			r#"{"type":"text_end","contentIndex":0,"content":"Reading."}"#.to_owned(),
		),
		(
			r#"{"type":"thinking_start","contentIndex":0,"partial":PARTIAL}"#.to_owned(),
			r#"{"type":"thinking_start","contentIndex":0}"#.to_owned(),
		),
		(
			r#"{"type":"thinking_delta","contentIndex":0,"delta":"h","partial":PARTIAL}"#.to_owned(),
			r#"{"type":"thinking_delta","contentIndex":0,"delta":"h"}"#.to_owned(),
		),
		(
			r#"{"type":"thinking_end","contentIndex":0,"content":"hm","partial":PARTIAL}"#.to_owned(),
			r#"{"type":"thinking_end","contentIndex":0,"content":"hm"}"#.to_owned(),
		),
		(
			r#"{"type":"toolcall_delta","contentIndex":1,"delta":"{","partial":PARTIAL}"#.to_owned(),
			r#"{"type":"toolcall_delta","contentIndex":1,"delta":"{"}"#.to_owned(),
		),
		(
			format!(r#"{{"type":"toolcall_end","contentIndex":1,"toolCall":{tool_call},"partial":PARTIAL}}"#),
			format!(r#"{{"type":"toolcall_end","contentIndex":1,"toolCall":{tool_call}}}"#),
		),
		(
			r#"{"type":"done","reason":"stop","message":PARTIAL}"#.to_owned(),
			r#"{"type":"done","reason":"stop","message":PARTIAL}"#.to_owned(),
		),
		(
			r#"{"type":"error","reason":"error","error":PARTIAL}"#.to_owned(),
			r#"{"type":"error","reason":"error","error":PARTIAL}"#.to_owned(),
		),
	];
	for (input, expected) in &cases {
		let JsonAgentSessionEvent::MessageUpdate(update) = to_json_event(update(input)).unwrap() else {
			panic!("a wire message_update")
		};
		assert_eq!(
			write(&update.assistant_message_event),
			expected.replace("PARTIAL", &assistant())
		);
	}
}

#[test]
fn unknown_fields_of_the_stream_event_stay_in_place() {
	let wire = to_json_event(update(
		r#"{"type":"toolcall_start","contentIndex":1,"partial":PARTIAL,"seq":4}"#,
	))
	.unwrap();
	assert!(
		write(&wire)
			.ends_with(r#"{"type":"toolcall_start","contentIndex":1,"seq":4,"id":"call_1","toolName":"read"}}"#)
	);
}

#[test]
fn an_unknown_stream_event_loses_its_partial_and_gains_the_usage() {
	let session_event = update(r#"{"type":"citation_delta","contentIndex":0,"partial":PARTIAL,"url":"https://x"}"#);
	assert!(matches!(
		&session_event,
		AgentSessionEvent::Known(KnownAgentSessionEvent::MessageUpdate(update))
			if matches!(update.assistant_message_event, pi_types::AssistantMessageEvent::Other(_))
	));
	let wire = to_json_event(session_event).unwrap();
	assert_eq!(
		write(&wire),
		format!(
			r#"{{"type":"message_update","usage":{},"assistantMessageEvent":{{"type":"citation_delta","contentIndex":0,"url":"https://x"}}}}"#,
			common::USAGE
		)
	);
}

#[test]
fn other_events_pass_through_unchanged() {
	let settled = event(r#"{"type":"agent_settled","aborted":false}"#);
	assert_eq!(
		to_json_event(settled.clone()).unwrap(),
		JsonAgentSessionEvent::Event(settled)
	);
	let JsonAgentSessionEvent::Event(AgentSessionEvent::Known(KnownAgentSessionEvent::MessageEnd(_))) =
		to_json_event(event(&format!(r#"{{"type":"message_end","message":{}}}"#, assistant()))).unwrap()
	else {
		panic!("message_end passes through")
	};
}

#[test]
fn updates_that_ts_rejects_are_errors() {
	let user = r#"{"role":"user","content":"hi","timestamp":1}"#;
	let not_assistant = event(&format!(
		r#"{{"type":"message_update","message":{user},"assistantMessageEvent":{{"type":"start","partial":{}}}}}"#,
		assistant()
	));
	assert_eq!(to_json_event(not_assistant), Err(JsonEventError::NotAssistant));
	let not_tool_call = update(r#"{"type":"toolcall_start","contentIndex":0,"partial":PARTIAL}"#);
	assert_eq!(to_json_event(not_tool_call), Err(JsonEventError::NotToolCall(0)));
	assert_eq!(
		JsonEventError::NotToolCall(3).to_string(),
		"toolcall_start content at index 3 is not a tool call"
	);
	let past_the_end = update(r#"{"type":"toolcall_start","contentIndex":9,"partial":PARTIAL}"#);
	assert_eq!(to_json_event(past_the_end), Err(JsonEventError::NotToolCall(9)));
}

#[test]
fn wire_events_read_back() {
	let wire = to_json_event(update(
		r#"{"type":"text_delta","contentIndex":0,"delta":"R","partial":PARTIAL}"#,
	))
	.unwrap();
	let read: JsonAgentSessionEvent = common::round_trip(&write(&wire));
	assert_eq!(read, wire);
	let read: JsonAgentSessionEvent = common::round_trip(r#"{"type":"agent_settled","aborted":true}"#);
	assert!(matches!(read, JsonAgentSessionEvent::Event(_)));
}
