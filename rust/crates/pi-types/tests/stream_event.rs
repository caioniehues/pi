//! Response stream events (`stream_event.rs`): every event type, standalone handles and diagnostics.

mod common;

use common::{assistant, round_trip};
use pi_types::{AssistantMessageEvent, DiagnosticErrorCode, KnownAssistantMessageEvent, StopReason};

#[test]
fn every_event_type_round_trips() {
	let partial = assistant();
	let tool_call = r#"{"type":"toolCall","id":"call_1","name":"read","arguments":{"path":"a.txt"}}"#;
	let cases = [
		format!(r#"{{"type":"start","partial":{partial}}}"#),
		format!(r#"{{"type":"text_start","contentIndex":0,"partial":{partial}}}"#),
		format!(r#"{{"type":"text_delta","contentIndex":0,"delta":"Re","partial":{partial}}}"#),
		format!(r#"{{"type":"text_end","contentIndex":0,"content":"Reading.","partial":{partial}}}"#),
		format!(r#"{{"type":"thinking_start","contentIndex":0,"partial":{partial}}}"#),
		format!(r#"{{"type":"thinking_delta","contentIndex":0,"delta":"h","partial":{partial}}}"#),
		format!(r#"{{"type":"thinking_end","contentIndex":0,"content":"hm","partial":{partial}}}"#),
		format!(r#"{{"type":"toolcall_start","contentIndex":1,"partial":{partial}}}"#),
		format!(r#"{{"type":"toolcall_delta","contentIndex":1,"delta":"{{\"pa","partial":{partial}}}"#),
		format!(r#"{{"type":"toolcall_end","contentIndex":1,"toolCall":{tool_call},"partial":{partial}}}"#),
		format!(r#"{{"type":"done","reason":"toolUse","message":{partial}}}"#),
		format!(r#"{{"type":"error","reason":"aborted","error":{partial}}}"#),
	];
	for case in &cases {
		let event = round_trip::<AssistantMessageEvent>(case);
		assert!(matches!(event, AssistantMessageEvent::Known(_)), "{case}");
	}
	let unknown = format!(r#"{{"type":"citation_delta","contentIndex":0,"partial":{partial}}}"#);
	assert!(matches!(
		round_trip::<AssistantMessageEvent>(&unknown),
		AssistantMessageEvent::Other(_)
	));
}

#[test]
fn done_and_error_accept_only_their_stop_reasons() {
	let partial = assistant();
	for text in [
		format!(r#"{{"type":"done","reason":"error","message":{partial}}}"#),
		format!(r#"{{"type":"error","reason":"stop","error":{partial}}}"#),
	] {
		assert!(common::read::<KnownAssistantMessageEvent>(&text).is_err());
		// Malformed known events are kept, like unknown ones.
		assert!(matches!(
			round_trip::<AssistantMessageEvent>(&text),
			AssistantMessageEvent::Other(_)
		));
	}
	for (text, reason) in [
		(r#""pending""#, StopReason::Pending),
		(r#""stop""#, StopReason::Stop),
		(r#""length""#, StopReason::Length),
		(r#""toolUse""#, StopReason::ToolUse),
		(r#""error""#, StopReason::Error),
		(r#""aborted""#, StopReason::Aborted),
		(r#""deferred""#, StopReason::Deferred),
	] {
		assert_eq!(round_trip::<StopReason>(text), reason);
	}
}

#[test]
fn diagnostic_codes_are_strings_or_numbers() {
	assert_eq!(
		round_trip::<DiagnosticErrorCode>(r#""EPIPE""#),
		DiagnosticErrorCode::Text("EPIPE".into())
	);
	assert_eq!(
		round_trip::<DiagnosticErrorCode>("-32"),
		DiagnosticErrorCode::Number(-32.0)
	);
}
