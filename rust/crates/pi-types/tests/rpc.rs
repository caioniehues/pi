//! The RPC protocol (`rpc.rs`): every command, every response, failures, extension UI requests and responses.

mod common;

use common::{USAGE, assistant, round_trip};
use pi_types::{
	ExtensionUiAnswer, ExtensionUiMethod, RpcCommand, RpcExtensionUiRequest, RpcExtensionUiResponse, RpcResponse,
	RpcResult, RpcSuccess,
};

#[test]
fn every_command_round_trips() {
	let cases = [
		r#"{"type":"prompt","message":"Hello","images":[{"type":"image","data":"AA==","mimeType":"image/png"}],"streamingBehavior":"followUp","id":"req-1"}"#,
		r#"{"type":"prompt","message":"Hello"}"#,
		r#"{"type":"steer","message":"Stop","id":"r"}"#,
		r#"{"type":"follow_up","message":"Then","images":[]}"#,
		r#"{"type":"abort"}"#,
		r#"{"type":"clear_queue","id":"r"}"#,
		r#"{"type":"new_session","parentSession":"/s/p.jsonl"}"#,
		r#"{"type":"get_state"}"#,
		r#"{"type":"set_model","provider":"anthropic","modelId":"claude-sonnet-4-20250514"}"#,
		r#"{"type":"cycle_model"}"#,
		r#"{"type":"get_available_models"}"#,
		r#"{"type":"set_thinking_level","level":"high"}"#,
		r#"{"type":"cycle_thinking_level"}"#,
		r#"{"type":"get_available_thinking_levels"}"#,
		r#"{"type":"set_steering_mode","mode":"one-at-a-time"}"#,
		r#"{"type":"set_follow_up_mode","mode":"all"}"#,
		r#"{"type":"compact","customInstructions":"Focus on code changes"}"#,
		r#"{"type":"set_auto_compaction","enabled":true}"#,
		r#"{"type":"set_auto_retry","enabled":false}"#,
		r#"{"type":"abort_retry"}"#,
		r#"{"type":"bash","command":"ls -la","excludeFromContext":true,"id":"req-1"}"#,
		r#"{"type":"abort_bash"}"#,
		r#"{"type":"get_session_stats"}"#,
		r#"{"type":"export_html","outputPath":"/tmp/session.html"}"#,
		r#"{"type":"switch_session","sessionPath":"/path/to/session.jsonl"}"#,
		r#"{"type":"fork","entryId":"abc123"}"#,
		r#"{"type":"clone"}"#,
		r#"{"type":"get_fork_messages"}"#,
		r#"{"type":"get_entries","since":"abc123"}"#,
		r#"{"type":"get_tree"}"#,
		r#"{"type":"get_last_assistant_text"}"#,
		r#"{"type":"set_session_name","name":"Refactor"}"#,
		r#"{"type":"get_messages"}"#,
		r#"{"type":"get_commands"}"#,
	];
	for case in cases {
		round_trip::<RpcCommand>(case);
	}
	assert!(common::read::<RpcCommand>(r#"{"type":"teleport"}"#).is_err());
}

const MODEL: &str = r#"{"id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5","api":"anthropic-messages","provider":"anthropic","baseUrl":"https://api.anthropic.com","reasoning":true,"input":["text","image"],"cost":{"input":3,"output":15,"cacheRead":0.3,"cacheWrite":3.75},"contextWindow":200000,"maxTokens":64000}"#;

#[test]
fn every_success_response_round_trips() {
	let tree_entry = r#"{"type":"label","id":"a1","parentId":null,"timestamp":"t","targetId":"b2","label":"x"}"#;
	let source_info = r#"{"path":"/p/ext.ts","source":"npm:ext","scope":"user","origin":"package","baseDir":"/p"}"#;
	let data = [
		("prompt", r#"{"disposition":"started"}"#.to_owned()),
		("steer", r#"{"disposition":"queued"}"#.to_owned()),
		("follow_up", r#"{"disposition":"handled"}"#.to_owned()),
		("clear_queue", r#"{"steering":["a"],"followUp":["b"]}"#.to_owned()),
		("new_session", r#"{"cancelled":false}"#.to_owned()),
		(
			"get_state",
			format!(
				r#"{{"model":{MODEL},"thinkingLevel":"medium","isStreaming":false,"isCompacting":false,"steeringMode":"all","followUpMode":"one-at-a-time","sessionFile":"/s/a.jsonl","sessionId":"019a","sessionName":"n","autoCompactionEnabled":true,"messageCount":4,"pendingMessageCount":0}}"#
			),
		),
		("set_model", MODEL.to_owned()),
		("cycle_model", format!(r#"{{"model":{MODEL},"thinkingLevel":"high","isScoped":false}}"#)),
		("cycle_model", "null".to_owned()),
		("get_available_models", format!(r#"{{"models":[{MODEL}]}}"#)),
		("cycle_thinking_level", r#"{"level":"low"}"#.to_owned()),
		("cycle_thinking_level", "null".to_owned()),
		("get_available_thinking_levels", r#"{"levels":["off","minimal","low"]}"#.to_owned()),
		(
			"compact",
			format!(r#"{{"summary":"s","firstKeptEntryId":"e","tokensBefore":10,"usage":{USAGE}}}"#),
		),
		(
			"bash",
			r#"{"output":"total 0","exitCode":0,"cancelled":false,"truncated":true,"fullOutputPath":"/tmp/b"}"#.to_owned(),
		),
		(
			"get_session_stats",
			r#"{"sessionFile":"/s/a.jsonl","sessionId":"019a","userMessages":1,"assistantMessages":2,"toolCalls":3,"toolResults":3,"totalMessages":9,"tokens":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"total":10},"cost":0.25,"contextUsage":{"tokens":null,"contextWindow":200000,"percent":null}}"#.to_owned(),
		),
		("export_html", r#"{"path":"/tmp/session.html"}"#.to_owned()),
		("switch_session", r#"{"cancelled":true}"#.to_owned()),
		("fork", r#"{"text":"Hello","cancelled":false}"#.to_owned()),
		("clone", r#"{"cancelled":false}"#.to_owned()),
		("get_fork_messages", r#"{"messages":[{"entryId":"e1","text":"Hello"}]}"#.to_owned()),
		("get_entries", format!(r#"{{"entries":[{tree_entry}],"leafId":"a1"}}"#)),
		("get_tree", format!(r#"{{"tree":[{{"entry":{tree_entry},"children":[]}}],"leafId":null}}"#)),
		("get_last_assistant_text", r#"{"text":null}"#.to_owned()),
		("get_messages", format!(r#"{{"messages":[{}]}}"#, assistant())),
		(
			"get_commands",
			format!(r#"{{"commands":[{{"name":"review","description":"Review","source":"extension","sourceInfo":{source_info}}}]}}"#),
		),
	];
	for (command, data) in &data {
		let response: RpcResponse = round_trip(&format!(
			r#"{{"id":"req-1","type":"response","command":"{command}","success":true,"data":{data}}}"#
		));
		assert!(matches!(response.result, RpcResult::Success(_)), "{command}");
	}
	for command in [
		"abort",
		"set_thinking_level",
		"set_steering_mode",
		"set_follow_up_mode",
		"set_auto_compaction",
		"set_auto_retry",
		"abort_retry",
		"abort_bash",
		"set_session_name",
	] {
		let response: RpcResponse = round_trip(&format!(
			r#"{{"type":"response","command":"{command}","success":true}}"#
		));
		assert!(matches!(response.result, RpcResult::Success(_)), "{command}");
	}
}

#[test]
fn failures_of_any_command_round_trip() {
	let response: RpcResponse = round_trip(
		r#"{"id":"req-2","type":"response","command":"prompt","success":false,"error":"No model selected"}"#,
	);
	assert!(matches!(response.result, RpcResult::Failure(failure) if failure.error == "No model selected"));
	let response: RpcResponse =
		round_trip(r#"{"type":"response","command":"teleport","success":false,"error":"Unknown command: teleport"}"#);
	assert!(matches!(response.result, RpcResult::Failure(failure) if failure.command == "teleport"));
	let response: RpcResponse = round_trip(r#"{"type":"response","command":"cycle_model","success":true,"data":null}"#);
	assert!(matches!(response.result, RpcResult::Success(RpcSuccess::CycleModel(data)) if data.data.is_none()));
}

#[test]
fn extension_ui_requests_round_trip() {
	let cases = [
		r#"{"type":"extension_ui_request","id":"u1","method":"select","title":"Pick","options":["a","b"],"timeout":5000}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"confirm","title":"Sure?","message":"Delete it"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"input","title":"Name","placeholder":"n"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"editor","title":"Edit","prefill":"text"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"notify","message":"Done","notifyType":"warning"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"setStatus","statusKey":"k","statusText":"busy"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"setStatus","statusKey":"k"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"setWidget","widgetKey":"w","widgetLines":["l"],"widgetPlacement":"belowEditor"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"setTitle","title":"pi"}"#,
		r#"{"type":"extension_ui_request","id":"u1","method":"set_editor_text","text":"t"}"#,
	];
	for case in cases {
		round_trip::<RpcExtensionUiRequest>(case);
	}
	let request: RpcExtensionUiRequest =
		round_trip(r#"{"type":"extension_ui_request","id":"u1","method":"setWidget","widgetKey":"w"}"#);
	assert!(matches!(request.request, ExtensionUiMethod::SetWidget(widget) if widget.widget_lines.is_none()));
}

#[test]
fn extension_ui_responses_round_trip() {
	let value: RpcExtensionUiResponse = round_trip(r#"{"type":"extension_ui_response","id":"u1","value":"a"}"#);
	assert_eq!(
		value.answer,
		ExtensionUiAnswer::Value {
			value: Some("a".into())
		}
	);
	let confirmed: RpcExtensionUiResponse =
		round_trip(r#"{"type":"extension_ui_response","id":"u1","confirmed":false}"#);
	assert!(matches!(
		confirmed.answer,
		ExtensionUiAnswer::Confirmed { confirmed: false }
	));
	let cancelled: RpcExtensionUiResponse =
		round_trip(r#"{"type":"extension_ui_response","id":"u1","cancelled":true}"#);
	assert!(matches!(cancelled.answer, ExtensionUiAnswer::Cancelled { .. }));
}

/// The answer a response body reads as.
fn answer(body: &str) -> ExtensionUiAnswer {
	let text = format!(r#"{{"type":"extension_ui_response","id":"u1"{body}}}"#);
	common::read::<RpcExtensionUiResponse>(&text)
		.unwrap_or_else(|error| panic!("{error}: {text}"))
		.answer
}

#[test]
fn extension_ui_answers_follow_the_ts_check_order() {
	// `rpc-mode.ts` checks a truthy `cancelled` first, then `confirmed`, then `value`.
	assert!(matches!(
		answer(r#","cancelled":true,"value":"x""#),
		ExtensionUiAnswer::Cancelled { .. }
	));
	assert!(matches!(
		answer(r#","confirmed":true,"cancelled":true"#),
		ExtensionUiAnswer::Cancelled { .. }
	));
	assert_eq!(answer(""), ExtensionUiAnswer::Value { value: None });
	assert_eq!(
		answer(r#","cancelled":false"#),
		ExtensionUiAnswer::Value { value: None }
	);
	assert_eq!(
		answer(r#","confirmed":true,"value":"x""#),
		ExtensionUiAnswer::Confirmed { confirmed: true }
	);
}
