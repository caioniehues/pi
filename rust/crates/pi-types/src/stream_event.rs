//! The event protocol of one assistant response stream (ports `AssistantMessageEvent`, `StopReason` and
//! `DeferredHandle` of `packages/ai/src/types.ts`, and `AssistantMessageDiagnostic` of `utils/diagnostics.ts`).

use crate::content::ToolCall;
use crate::message::AssistantMessage;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Why a response stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
	/// Still streaming.
	Pending,
	/// The model finished.
	Stop,
	/// The output token limit was reached.
	Length,
	/// The model called tools.
	ToolUse,
	/// The request failed.
	Error,
	/// The request was aborted.
	Aborted,
	/// The provider continues the request asynchronously; see `AssistantMessage::deferred`.
	Deferred,
}

/// The stop reasons of a `done` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneReason {
	/// The model finished.
	Stop,
	/// The output token limit was reached.
	Length,
	/// The model called tools.
	ToolUse,
	/// The provider continues the request asynchronously.
	Deferred,
}

/// The stop reasons of an `error` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
	/// The request was aborted.
	Aborted,
	/// The request failed.
	Error,
}

/// One event of a response stream: a known type, or a type this port does not know kept as JSON (the `Other`
/// pattern of the crate root), so a `message_update` carrying it still reads as one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantMessageEvent {
	/// A type this port knows.
	Known(KnownAssistantMessageEvent),
	/// Any other object, kept as it was read.
	Other(JsonObject),
}

/// The stream event types, tagged by `type`. `start` comes first, updates follow, `done` or `error` ends the stream;
/// `error` may also come first when the request fails before generation starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownAssistantMessageEvent {
	/// The stream started.
	Start(StreamStart),
	/// A text block started, empty.
	TextStart(ContentStart),
	/// Text appended to a block.
	TextDelta(ContentDelta),
	/// A text block ended with its authoritative content.
	TextEnd(ContentEnd),
	/// A thinking block started, empty unless redacted.
	ThinkingStart(ContentStart),
	/// Thinking appended to a block.
	ThinkingDelta(ContentDelta),
	/// A thinking block ended with its authoritative content.
	ThinkingEnd(ContentEnd),
	/// A tool call block started; its arguments so far are provider-specific.
	#[serde(rename = "toolcall_start")]
	ToolCallStart(ContentStart),
	/// Serialized arguments appended to a tool call.
	#[serde(rename = "toolcall_delta")]
	ToolCallDelta(ContentDelta),
	/// A tool call ended with its complete arguments.
	#[serde(rename = "toolcall_end")]
	ToolCallEnd(ToolCallEnd),
	/// The response completed.
	Done(StreamDone),
	/// The response failed or was aborted.
	Error(StreamError),
}

/// `start`: the response so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamStart {
	/// The response so far: a live object in TS, a snapshot taken at push time here (see `pi-ai`'s event stream).
	pub partial: Arc<AssistantMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `text_start`, `thinking_start`, `toolcall_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentStart {
	/// The index of the block in `partial.content`.
	pub content_index: u64,
	/// The response so far, a snapshot taken at push time.
	pub partial: Arc<AssistantMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `text_delta`, `thinking_delta`, `toolcall_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDelta {
	/// The index of the block in `partial.content`.
	pub content_index: u64,
	/// The appended text.
	pub delta: String,
	/// The response so far, a snapshot taken at push time.
	pub partial: Arc<AssistantMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `text_end`, `thinking_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentEnd {
	/// The index of the block in `partial.content`.
	pub content_index: u64,
	/// The block's authoritative content.
	pub content: String,
	/// The response so far, a snapshot taken at push time.
	pub partial: Arc<AssistantMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `toolcall_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEnd {
	/// The index of the block in `partial.content`.
	pub content_index: u64,
	/// The complete tool call.
	pub tool_call: ToolCall,
	/// The response so far, a snapshot taken at push time.
	pub partial: Arc<AssistantMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `done`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamDone {
	/// Why the response stopped.
	pub reason: DoneReason,
	/// The final response.
	pub message: AssistantMessage,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamError {
	/// Aborted or failed.
	pub reason: ErrorReason,
	/// The final response, with `error_message` set.
	pub error: AssistantMessage,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A durable handle of a response the provider continues asynchronously.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model_id: String,
	/// The wire API.
	pub api: String,
	/// The provider token, such as a response id or a batch id plus row id.
	pub id: String,
	/// Epoch milliseconds after which the provider drops the response.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub expires_at: Option<u64>,
	/// Milliseconds to wait before polling.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub poll_after_ms: Option<u64>,
	/// Provider conversion data needed to rebuild the final message; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub data: Option<JsonValue>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A redacted provider or runtime diagnostic attached to a response. `type` is an open string here, not a tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageDiagnostic {
	/// What happened, such as `retry`.
	#[serde(rename = "type")]
	pub kind: String,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// The error, when there was one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error: Option<DiagnosticErrorInfo>,
	/// Extra structured details.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub details: Option<JsonObject>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The parts of a thrown value a diagnostic keeps. Field order is `extractDiagnosticError`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticErrorInfo {
	/// The error's name, such as `TypeError`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// The error's message.
	pub message: String,
	/// The stack trace.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stack: Option<String>,
	/// A string or numeric error code.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub code: Option<DiagnosticErrorCode>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `string | number`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticErrorCode {
	/// A code such as `ECONNRESET`.
	Text(String),
	/// A numeric code.
	Number(f64),
}
