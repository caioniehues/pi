//! pi-ai messages (`message.rs`): every role with its optional fields, standalone messages carrying their role.

mod common;

use common::{USAGE, assistant, round_trip};
use pi_json::JsonValue;
use pi_types::{AssistantMessage, Message, NestedToolCallStatus, SystemMessage, ToolResultMessage};

#[test]
fn system_messages_round_trip_with_sections_and_tool_changes() {
	let message: SystemMessage = round_trip(
		r#"{"role":"system","content":[{"type":"text","text":"Base."}],"sections":{"rules":"<rules/>","old":null},"toolsAdded":[{"name":"read","description":"Read a file","parameters":{"type":"object","properties":{}},"constrainedSampling":false}],"toolsRemoved":[{"name":"write"}],"timestamp":0}"#,
	);
	let sections = message.sections.unwrap();
	assert_eq!(sections.get("old"), Some(&None));
}

#[test]
fn assistant_messages_round_trip_with_every_optional_field() {
	let message: AssistantMessage = round_trip(&format!(
		r#"{{"role":"assistant","content":[{{"type":"thinking","thinking":"","thinkingSignature":"enc","redacted":true}}],"api":"openai-responses","provider":"openai","model":"gpt-5","usage":{USAGE},"stopReason":"deferred","timestamp":10,"providerThinkingLevel":"high","responseId":"resp_1","responseModel":"gpt-5-2025","diagnostics":[{{"type":"retry","timestamp":11,"error":{{"name":"Error","message":"reset","code":"ECONNRESET"}}}},{{"type":"status","timestamp":12,"error":{{"message":"teapot","code":418}},"details":{{"attempt":2}}}}],"deferred":{{"provider":"openai","modelId":"gpt-5","api":"openai-responses","id":"resp_1","expiresAt":1000,"pollAfterMs":500,"data":null}},"rawStopReason":"incomplete","endTurn":false,"errorMessage":"none","durationMs":1234,"thinkingLevel":"xhigh"}}"#
	));
	assert_eq!(message.deferred.unwrap().data, Some(JsonValue::Null));
}

#[test]
fn tool_results_round_trip_with_nested_calls() {
	let message: ToolResultMessage = round_trip(&format!(
		r#"{{"role":"toolResult","toolCallId":"c","toolName":"code","content":[{{"type":"image","data":"AA==","mimeType":"image/png"}}],"details":null,"usage":{USAGE},"isError":true,"durationMs":40,"timestamp":2,"nestedCalls":{{"calls":[{{"id":"n1","name":"read","status":"ok","arguments":{{"path":"a"}},"durationMs":3}},{{"id":"n2","name":"write","status":"unfinished","argumentsBytes":99999}},{{"id":"n3","name":"bash","status":"error","arguments":{{}},"durationMs":1,"error":"boom"}}],"complete":false}}}}"#
	));
	assert_eq!(message.details, Some(JsonValue::Null));
	assert_eq!(
		message.nested_calls.unwrap().calls[1].status,
		NestedToolCallStatus::Unfinished
	);
}

#[test]
fn messages_dispatch_on_their_role() {
	for text in [
		r#"{"role":"system","content":"s","timestamp":0}"#.to_owned(),
		r#"{"role":"user","content":[{"type":"text","text":"t"},{"type":"image","data":"AA==","mimeType":"image/jpeg"}],"timestamp":1}"#.to_owned(),
		assistant(),
		r#"{"role":"toolResult","toolCallId":"c","toolName":"t","content":[],"isError":false,"timestamp":2}"#.to_owned(),
	] {
		round_trip::<Message>(&text);
	}
	assert!(
		common::read::<Message>(r#"{"role":"custom","customType":"x","content":"c","display":true,"timestamp":1}"#)
			.is_err()
	);
}
