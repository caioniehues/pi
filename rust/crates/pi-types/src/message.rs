//! pi-ai's messages (ports `SystemMessage`, `UserMessage`, `AssistantMessage`, `ToolResultMessage`, `Message` and
//! the nested tool call records of `packages/ai/src/types.ts`).

use crate::content::{AssistantContentBlock, ContentBlock, MessageContent, SystemContent};
use crate::literal::tag_type;
use crate::model::ModelThinkingLevel;
use crate::record::Record;
use crate::stream_event::{AssistantMessageDiagnostic, DeferredHandle, StopReason};
use crate::tool::{Tool, ToolReference};
use crate::usage::Usage;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};

/// A message of an LLM transcript; each message carries its `role`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
	/// `role: "system"`.
	System(SystemMessage),
	/// `role: "user"`.
	User(UserMessage),
	/// `role: "assistant"`.
	Assistant(AssistantMessage),
	/// `role: "toolResult"`.
	ToolResult(ToolResultMessage),
}

tag_type!(
	/// `role: "system"`.
	SystemRole::System = "system"
);
tag_type!(
	/// `role: "user"`.
	UserRole::User = "user"
);
tag_type!(
	/// `role: "assistant"`.
	AssistantRole::Assistant = "assistant"
);
tag_type!(
	/// `role: "toolResult"`.
	ToolResultRole::ToolResult = "toolResult"
);

/// System instructions and tool declarations from this point of the transcript on. Field order is
/// `utils/transcript.ts`'s; the agent loop's tool-change message (`withToolChanges`) writes `toolsAdded` and
/// `toolsRemoved` after `timestamp` (a second writer order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
	/// Always `system`.
	pub role: SystemRole,
	/// The base prompt on the leading message; additional instructions later.
	pub content: SystemContent,
	/// Named prompt sections; later messages replace a section by name, `null` removes it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sections: Option<Record<Option<String>>>,
	/// Tools that become available here.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tools_added: Option<Vec<Tool>>,
	/// Tools that stop being available here.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tools_removed: Option<Vec<ToolReference>>,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A user turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
	/// Always `user`.
	pub role: UserRole,
	/// Text, or text and image blocks.
	pub content: MessageContent,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A model response. Providers build the fields up to `timestamp` first and add the optional ones by mutation in
/// provider-specific order; the order here is `anthropic-messages`', then the stream's `durationMs` and the agent
/// loop's `thinkingLevel`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
	/// Always `assistant`.
	pub role: AssistantRole,
	/// Text, reasoning and tool call blocks.
	pub content: Vec<AssistantContentBlock>,
	/// The wire API that produced it.
	pub api: String,
	/// The provider id.
	pub provider: String,
	/// The requested model id.
	pub model: String,
	/// Tokens and cost.
	pub usage: Usage,
	/// Why generation stopped.
	pub stop_reason: StopReason,
	/// Epoch milliseconds when the request started.
	pub timestamp: u64,
	/// The provider-native effort level used.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub provider_thinking_level: Option<String>,
	/// The provider's response or message id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub response_id: Option<String>,
	/// The concrete model the provider reported, when reported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub response_model: Option<String>,
	/// Redacted provider and runtime diagnostics for failures and recoveries.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
	/// The handle of a deferred response.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub deferred: Option<DeferredHandle>,
	/// The provider's own stop reason.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub raw_stop_reason: Option<String>,
	/// Whether the provider said the model ended its turn; kept for debugging.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub end_turn: Option<bool>,
	/// The error text of an `error` or `aborted` response.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error_message: Option<String>,
	/// Milliseconds from `timestamp` to the end of the response, on a monotonic clock.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration_ms: Option<u64>,
	/// The pi thinking level the agent loop requested.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thinking_level: Option<ModelThinkingLevel>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The result of a tool call, sent back to the model. Field order is the agent loop's
/// (`createToolResultMessage`); the session adds `nestedCalls` afterwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
	/// Always `toolResult`.
	pub role: ToolResultRole,
	/// The id of the call this answers.
	pub tool_call_id: String,
	/// The tool's name.
	pub tool_name: String,
	/// Text and image blocks for the model.
	pub content: Vec<ContentBlock>,
	/// The tool's structured details, for logs and UI; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub details: Option<JsonValue>,
	/// Usage of the tool's own execution; not part of the main context accounting.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub usage: Option<Usage>,
	/// Whether the result is an error.
	pub is_error: bool,
	/// Milliseconds the execution took, on a monotonic clock.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration_ms: Option<u64>,
	/// Epoch milliseconds when the result was created.
	pub timestamp: u64,
	/// Calls this tool made to other tools; kept for the session record, not sent to the model.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub nested_calls: Option<NestedToolCalls>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The bounded record of the nested calls a tool made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedToolCalls {
	/// The recorded calls.
	pub calls: Vec<NestedToolCallRecord>,
	/// False when calls were dropped, arguments omitted, or calls had not finished.
	pub complete: bool,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// One nested tool call; results are not recorded. Field order is `NestedToolCallRecorder`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
	/// The call id.
	pub id: String,
	/// The tool's name.
	pub name: String,
	/// How the call ended.
	pub status: NestedToolCallStatus,
	/// The arguments; omitted when over the size limits.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub arguments: Option<JsonObject>,
	/// The UTF-8 size of the arguments as JSON, set when `arguments` is omitted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub arguments_bytes: Option<u64>,
	/// Milliseconds the call took.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub duration_ms: Option<u64>,
	/// The error text, truncated.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `"ok" | "error" | "unfinished"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedToolCallStatus {
	/// The call succeeded.
	Ok,
	/// The call failed.
	Error,
	/// The call was still running when the calling tool finished.
	Unfinished,
}
