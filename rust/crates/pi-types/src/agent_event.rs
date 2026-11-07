//! The events of the agent loop (ports `AgentEvent` of `packages/agent/src/types.ts`). The payload structs are
//! shared with the session events (`session_event.rs`), which add `parentToolCallId` to the tool events.

use crate::agent_message::AgentMessage;
use crate::message::ToolResultMessage;
use crate::stream_event::AssistantMessageEvent;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};

/// One event of an agent run, tagged by `type`. `agent_end` is the last event of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
	/// The run started.
	AgentStart,
	/// The run ended.
	AgentEnd(AgentEndEvent),
	/// A turn (one assistant response and its tool calls) started.
	TurnStart,
	/// A turn ended.
	TurnEnd(TurnEndEvent),
	/// A message started (every role).
	MessageStart(MessageEvent),
	/// A streamed assistant message changed.
	MessageUpdate(MessageUpdateEvent),
	/// A message completed; the authoritative final message.
	MessageEnd(MessageEvent),
	/// A tool started executing.
	ToolExecutionStart(ToolExecutionStartEvent),
	/// A tool reported a partial result.
	ToolExecutionUpdate(ToolExecutionUpdateEvent),
	/// A tool finished.
	ToolExecutionEnd(ToolExecutionEndEvent),
}

/// `agent_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEndEvent {
	/// The messages the run generated.
	pub messages: Vec<AgentMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `turn_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndEvent {
	/// The assistant message of the turn.
	pub message: AgentMessage,
	/// The tool results of the turn.
	pub tool_results: Vec<ToolResultMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `message_start`, `message_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEvent {
	/// The message.
	pub message: AgentMessage,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `message_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageUpdateEvent {
	/// The assistant message so far.
	pub message: AgentMessage,
	/// The stream event that changed it.
	pub assistant_message_event: AssistantMessageEvent,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `tool_execution_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionStartEvent {
	/// The tool call id.
	pub tool_call_id: String,
	/// The tool's name.
	pub tool_name: String,
	/// The validated arguments.
	pub args: JsonValue,
	/// Session events only: the calling tool's call id, for calls a tool made through `ctx.executeTool()`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub parent_tool_call_id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `tool_execution_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionUpdateEvent {
	/// The tool call id.
	pub tool_call_id: String,
	/// The tool's name.
	pub tool_name: String,
	/// The validated arguments.
	pub args: JsonValue,
	/// The latest partial result (an `AgentToolResult`).
	pub partial_result: JsonValue,
	/// Session events only: the calling tool's call id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub parent_tool_call_id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `tool_execution_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionEndEvent {
	/// The tool call id.
	pub tool_call_id: String,
	/// The tool's name.
	pub tool_name: String,
	/// The final result (an `AgentToolResult`).
	pub result: JsonValue,
	/// Whether the result is an error.
	pub is_error: bool,
	/// Milliseconds `execute()` took on a monotonic clock; absent when the tool did not run.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration_ms: Option<u64>,
	/// Session events only: the calling tool's call id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub parent_tool_call_id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
