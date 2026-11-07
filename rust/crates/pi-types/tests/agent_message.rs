//! Agent transcript messages (`agent_message.rs`): every role, unknown roles and unknown fields.

mod common;

use common::{assistant, round_trip};
use pi_json::JsonValue;
use pi_types::{AgentMessage, KnownAgentMessage, Message, UserMessage};

fn known(text: &str) -> KnownAgentMessage {
	match round_trip::<AgentMessage>(text) {
		AgentMessage::Known(message) => message,
		AgentMessage::Other(object) => panic!("not known: {object:?}"),
	}
}

#[test]
fn every_role_round_trips() {
	let cases = [
		(r#"{"role":"system","content":"You are pi.","timestamp":0}"#.to_owned(), "system"),
		(r#"{"role":"user","content":"hi","timestamp":1}"#.to_owned(), "user"),
		(assistant(), "assistant"),
		(
			r#"{"role":"toolResult","toolCallId":"call_1","toolName":"read","content":[{"type":"text","text":"ok"}],"isError":false,"timestamp":2}"#.to_owned(),
			"toolResult",
		),
		(
			r#"{"role":"bashExecution","command":"ls","output":"a","exitCode":0,"cancelled":false,"truncated":true,"fullOutputPath":"/tmp/o","timestamp":3,"excludeFromContext":true}"#.to_owned(),
			"bashExecution",
		),
		(
			r#"{"role":"bashExecution","command":"sleep 9","output":"","cancelled":true,"truncated":false,"timestamp":3}"#.to_owned(),
			"bashExecution",
		),
		(
			r#"{"role":"custom","customType":"note","content":[{"type":"text","text":"n"}],"display":true,"details":{"a":1},"timestamp":4}"#.to_owned(),
			"custom",
		),
		(
			r#"{"role":"branchSummary","summary":"s","fromId":null,"timestamp":5}"#.to_owned(),
			"branchSummary",
		),
		(
			r#"{"role":"compactionSummary","summary":"s","tokensBefore":900,"timestamp":6}"#.to_owned(),
			"compactionSummary",
		),
	];
	for (text, role) in &cases {
		let message = known(text);
		let written = serde_json::to_value(&message).unwrap();
		assert_eq!(written.get("role"), Some(&JsonValue::from(*role)));
	}
}

#[test]
fn an_unknown_role_is_kept() {
	let message: AgentMessage = round_trip(r#"{"role":"artifact","title":"plot","timestamp":7,"data":[1,2]}"#);
	assert!(matches!(message, AgentMessage::Other(object) if object["title"] == "plot"));
	// v2's `hookMessage` role becomes `custom` in C8's migration; until then it is kept as is.
	let message: AgentMessage = round_trip(r#"{"role":"hookMessage","customType":"x","content":"c","timestamp":1}"#);
	assert!(matches!(message, AgentMessage::Other(_)));
}

#[test]
fn a_null_field_that_is_missing_keeps_the_message_unchanged() {
	// `fromId: string | null` is required: a branch summary without it is kept as `Other`.
	let message: AgentMessage = round_trip(r#"{"role":"branchSummary","summary":"s","timestamp":5}"#);
	assert!(matches!(message, AgentMessage::Other(_)));
}

#[test]
fn a_message_with_extra_fields_keeps_them_at_the_end() {
	let KnownAgentMessage::User(user) =
		known(r#"{"role":"user","content":"hi","timestamp":1,"source":"rpc","meta":{"k":[]}}"#)
	else {
		panic!("a user message")
	};
	assert_eq!(user.extra.len(), 2);
}

#[test]
fn a_pi_ai_message_converts_into_an_agent_message() {
	let user: UserMessage = common::read(r#"{"role":"user","content":"hi","timestamp":1}"#).unwrap();
	let message = AgentMessage::from(Message::User(user.clone()));
	assert_eq!(message, AgentMessage::Known(KnownAgentMessage::User(user)));
}
