//! Agent loop events (`agent_event.rs`): every event type.

mod common;

use common::{assistant, round_trip};
use pi_types::AgentEvent;

#[test]
fn every_event_type_round_trips() {
	let message = assistant();
	let result = r#"{"content":[{"type":"text","text":"ok"}],"details":{}}"#;
	let tool_result =
		r#"{"role":"toolResult","toolCallId":"call_1","toolName":"read","content":[],"isError":false,"timestamp":2}"#;
	let cases = [
		r#"{"type":"agent_start"}"#.to_owned(),
		format!(r#"{{"type":"agent_end","messages":[{message},{tool_result}]}}"#),
		r#"{"type":"turn_start"}"#.to_owned(),
		format!(r#"{{"type":"turn_end","message":{message},"toolResults":[{tool_result}]}}"#),
		format!(r#"{{"type":"message_start","message":{message}}}"#),
		format!(
			r#"{{"type":"message_update","message":{message},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"R","partial":{message}}}}}"#
		),
		format!(r#"{{"type":"message_end","message":{message}}}"#),
		r#"{"type":"tool_execution_start","toolCallId":"call_1","toolName":"read","args":{"path":"a"}}"#.to_owned(),
		format!(
			r#"{{"type":"tool_execution_update","toolCallId":"call_1","toolName":"read","args":{{"path":"a"}},"partialResult":{result}}}"#
		),
		format!(
			r#"{{"type":"tool_execution_end","toolCallId":"call_1","toolName":"read","result":{result},"isError":false,"durationMs":12}}"#
		),
		format!(
			r#"{{"type":"tool_execution_end","toolCallId":"call_1","toolName":"read","result":{result},"isError":true}}"#
		),
	];
	for case in &cases {
		round_trip::<AgentEvent>(case);
	}
}
