//! Agent session events (`session_event.rs`): every event type, nested tool events and unknown types.

mod common;

use common::{USAGE, assistant, round_trip};
use pi_types::{AgentSessionEvent, CompactionReason, KnownAgentSessionEvent};

fn known(text: &str) -> KnownAgentSessionEvent {
	match round_trip::<AgentSessionEvent>(text) {
		AgentSessionEvent::Known(event) => event,
		AgentSessionEvent::Other(object) => panic!("not known: {object:?}"),
	}
}

#[test]
fn every_event_type_round_trips() {
	let message = assistant();
	let result = r#"{"content":[],"details":null}"#;
	let cases = [
		r#"{"type":"agent_start"}"#.to_owned(),
		format!(r#"{{"type":"agent_end","messages":[{message}],"willRetry":false}}"#),
		r#"{"type":"agent_settled","aborted":true}"#.to_owned(),
		r#"{"type":"turn_start"}"#.to_owned(),
		format!(r#"{{"type":"turn_end","message":{message},"toolResults":[]}}"#),
		format!(r#"{{"type":"message_start","message":{message}}}"#),
		format!(
			r#"{{"type":"message_update","message":{message},"assistantMessageEvent":{{"type":"toolcall_start","contentIndex":1,"partial":{message}}}}}"#
		),
		format!(r#"{{"type":"message_end","message":{message}}}"#),
		r#"{"type":"tool_execution_start","toolCallId":"n1","toolName":"read","args":{},"parentToolCallId":"call_1"}"#.to_owned(),
		format!(
			r#"{{"type":"tool_execution_update","toolCallId":"n1","toolName":"read","args":{{}},"partialResult":{result},"parentToolCallId":"call_1"}}"#
		),
		format!(
			r#"{{"type":"tool_execution_end","toolCallId":"n1","toolName":"read","result":{result},"isError":false,"durationMs":1,"parentToolCallId":"call_1"}}"#
		),
		r#"{"type":"queue_update","steering":["a"],"followUp":[]}"#.to_owned(),
		r#"{"type":"compaction_start","reason":"threshold"}"#.to_owned(),
		format!(
			r#"{{"type":"compaction_end","reason":"overflow","result":{{"summary":"s","firstKeptEntryId":"e1","tokensBefore":100,"usage":{USAGE},"details":{{"readFiles":[],"modifiedFiles":[]}},"estimatedTokensAfter":20}},"aborted":false,"willRetry":true}}"#
		),
		r#"{"type":"compaction_end","reason":"manual","aborted":true,"willRetry":false,"errorMessage":"aborted"}"#.to_owned(),
		r#"{"type":"entry_appended","entry":{"type":"session_info","id":"a1","parentId":null,"timestamp":"t","name":"n"}}"#.to_owned(),
		r#"{"type":"session_info_changed","name":"Refactor"}"#.to_owned(),
		r#"{"type":"session_info_changed"}"#.to_owned(),
		r#"{"type":"thinking_level_changed","level":"off"}"#.to_owned(),
		r#"{"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":2000,"errorMessage":"overloaded"}"#.to_owned(),
		r#"{"type":"auto_retry_end","success":false,"attempt":3,"finalError":"overloaded"}"#.to_owned(),
		r#"{"type":"summarization_retry_scheduled","attempt":1,"maxAttempts":3,"delayMs":1000,"errorMessage":"e"}"#.to_owned(),
		r#"{"type":"summarization_retry_attempt_start","source":"branchSummary"}"#.to_owned(),
		r#"{"type":"summarization_retry_attempt_start","source":"compaction","reason":"manual"}"#.to_owned(),
		r#"{"type":"summarization_retry_finished"}"#.to_owned(),
		r#"{"type":"bash_execution_update","id":"b1","delta":"out"}"#.to_owned(),
		r#"{"type":"bash_execution_update","delta":"out"}"#.to_owned(),
	];
	for case in &cases {
		known(case);
	}
}

#[test]
fn compaction_reasons_are_kept() {
	let KnownAgentSessionEvent::CompactionStart(start) = known(r#"{"type":"compaction_start","reason":"overflow"}"#)
	else {
		panic!("compaction_start")
	};
	assert_eq!(start.reason, CompactionReason::Overflow);
}

#[test]
fn an_unknown_event_type_is_kept() {
	let event: AgentSessionEvent =
		round_trip(r#"{"type":"extension_error","extensionPath":"x","event":"e","error":"m"}"#);
	assert!(matches!(event, AgentSessionEvent::Other(_)));
}
