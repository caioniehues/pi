//! The session events as the JSON and RPC stdout protocols write them (ports
//! `packages/coding-agent/src/modes/json-event.ts`): `message_update` loses its cumulative snapshots so the stream
//! stays linear in the response size, and keeps the fixed-size usage and tool-call identity.

use crate::agent_event::MessageUpdateEvent;
use crate::agent_message::{AgentMessage, KnownAgentMessage};
use crate::content::{AssistantContentBlock, ToolCall};
use crate::literal::tag_type;
use crate::session_event::{AgentSessionEvent, KnownAgentSessionEvent};
use crate::stream_event::{
	AssistantMessageEvent, ContentDelta, ContentEnd, ContentStart, KnownAssistantMessageEvent, StreamDone, StreamError,
	ToolCallEnd,
};
use crate::usage::Usage;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

/// A session event on the JSON wire: `message_update` in its delta-only form, every other event unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonAgentSessionEvent {
	/// `message_update` without snapshots.
	MessageUpdate(JsonMessageUpdateEvent),
	/// Any other session event, as the session emitted it.
	Event(AgentSessionEvent),
}

tag_type!(
	/// `type: "message_update"`, the tag of [`JsonMessageUpdateEvent`].
	MessageUpdateType::MessageUpdate = "message_update"
);

/// `message_update` on the wire: the cumulative usage and the stream event without `partial`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonMessageUpdateEvent {
	/// Always `message_update`.
	#[serde(rename = "type")]
	pub kind: MessageUpdateType,
	/// The latest cumulative usage of the response.
	pub usage: Usage,
	/// The stream event without its snapshot.
	pub assistant_message_event: JsonAssistantMessageEvent,
}

/// A stream event without `partial`: a known type, or an unknown one with its `partial` removed (as `json-event.ts`
/// removes `partial` from any event).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonAssistantMessageEvent {
	/// A type this port knows.
	Known(KnownJsonAssistantMessageEvent),
	/// Any other event, without `partial`.
	Other(JsonObject),
}

/// The known stream event types on the wire; `toolcall_start` also names the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownJsonAssistantMessageEvent {
	/// The stream started.
	Start(JsonEventFields),
	/// A text block started.
	TextStart(JsonContentStart),
	/// Text appended to a block.
	TextDelta(JsonContentDelta),
	/// A text block ended.
	TextEnd(JsonContentEnd),
	/// A thinking block started.
	ThinkingStart(JsonContentStart),
	/// Thinking appended to a block.
	ThinkingDelta(JsonContentDelta),
	/// A thinking block ended.
	ThinkingEnd(JsonContentEnd),
	/// A tool call started, with its id and tool name.
	#[serde(rename = "toolcall_start")]
	ToolCallStart(JsonToolCallStart),
	/// Serialized arguments appended to a tool call.
	#[serde(rename = "toolcall_delta")]
	ToolCallDelta(JsonContentDelta),
	/// A tool call ended.
	#[serde(rename = "toolcall_end")]
	ToolCallEnd(JsonToolCallEnd),
	/// The response completed (it carries no snapshot).
	Done(StreamDone),
	/// The response failed (it carries no snapshot).
	Error(StreamError),
}

/// The fields of an event that has none of its own but may carry unknown ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonEventFields {
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `text_start`, `thinking_start` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonContentStart {
	/// The index of the block.
	pub content_index: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `text_delta`, `thinking_delta`, `toolcall_delta` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonContentDelta {
	/// The index of the block.
	pub content_index: u64,
	/// The appended text.
	pub delta: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `text_end`, `thinking_end` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonContentEnd {
	/// The index of the block.
	pub content_index: u64,
	/// The block's authoritative content.
	pub content: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `toolcall_start` on the wire: the block's index and the call's identity, written after the event's own fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonToolCallStart {
	/// The index of the block.
	pub content_index: u64,
	/// Fields of the source event this port does not know, written where TS spreads them.
	#[serde(flatten)]
	pub extra: JsonObject,
	/// The tool call id.
	pub id: String,
	/// The tool's name.
	pub tool_name: String,
}

/// `toolcall_end` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonToolCallEnd {
	/// The index of the block.
	pub content_index: u64,
	/// The complete tool call.
	pub tool_call: ToolCall,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Why a `message_update` has no wire form; TS throws these.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JsonEventError {
	/// The updated message is not an assistant message.
	#[error("message_update message is not an assistant message")]
	NotAssistant,
	/// The block a `toolcall_start` points at is not a tool call.
	#[error("toolcall_start content at index {0} is not a tool call")]
	NotToolCall(u64),
}

/// The wire form of a session event: `message_update` without its cumulative `message` and `partial` snapshots,
/// with the response's usage, and with the tool call's `id` and `toolName` on `toolcall_start`.
pub fn to_json_event(event: AgentSessionEvent) -> Result<JsonAgentSessionEvent, JsonEventError> {
	match event {
		AgentSessionEvent::Known(KnownAgentSessionEvent::MessageUpdate(update)) => {
			to_json_message_update(update).map(JsonAgentSessionEvent::MessageUpdate)
		}
		event => Ok(JsonAgentSessionEvent::Event(event)),
	}
}

fn to_json_message_update(update: MessageUpdateEvent) -> Result<JsonMessageUpdateEvent, JsonEventError> {
	let AgentMessage::Known(KnownAgentMessage::Assistant(message)) = update.message else {
		return Err(JsonEventError::NotAssistant);
	};
	Ok(JsonMessageUpdateEvent {
		kind: MessageUpdateType::MessageUpdate,
		usage: message.usage,
		assistant_message_event: to_json_assistant_message_event(update.assistant_message_event)?,
	})
}

fn to_json_assistant_message_event(event: AssistantMessageEvent) -> Result<JsonAssistantMessageEvent, JsonEventError> {
	match event {
		AssistantMessageEvent::Known(event) => to_json_known_event(event).map(JsonAssistantMessageEvent::Known),
		AssistantMessageEvent::Other(mut object) => {
			object.shift_remove("partial");
			Ok(JsonAssistantMessageEvent::Other(object))
		}
	}
}

fn to_json_known_event(event: KnownAssistantMessageEvent) -> Result<KnownJsonAssistantMessageEvent, JsonEventError> {
	use KnownAssistantMessageEvent as Event;
	use KnownJsonAssistantMessageEvent as Json;
	Ok(match event {
		Event::Start(start) => Json::Start(JsonEventFields { extra: start.extra }),
		Event::TextStart(start) => Json::TextStart(content_start(start)),
		Event::TextDelta(delta) => Json::TextDelta(content_delta(delta)),
		Event::TextEnd(end) => Json::TextEnd(content_end(end)),
		Event::ThinkingStart(start) => Json::ThinkingStart(content_start(start)),
		Event::ThinkingDelta(delta) => Json::ThinkingDelta(content_delta(delta)),
		Event::ThinkingEnd(end) => Json::ThinkingEnd(content_end(end)),
		Event::ToolCallStart(start) => Json::ToolCallStart(tool_call_start(start)?),
		Event::ToolCallDelta(delta) => Json::ToolCallDelta(content_delta(delta)),
		Event::ToolCallEnd(ToolCallEnd {
			content_index,
			tool_call,
			extra,
			..
		}) => Json::ToolCallEnd(JsonToolCallEnd {
			content_index,
			tool_call,
			extra,
		}),
		Event::Done(done) => Json::Done(done),
		Event::Error(error) => Json::Error(error),
	})
}

fn content_start(start: ContentStart) -> JsonContentStart {
	JsonContentStart {
		content_index: start.content_index,
		extra: start.extra,
	}
}

fn content_delta(delta: ContentDelta) -> JsonContentDelta {
	JsonContentDelta {
		content_index: delta.content_index,
		delta: delta.delta,
		extra: delta.extra,
	}
}

fn content_end(end: ContentEnd) -> JsonContentEnd {
	JsonContentEnd {
		content_index: end.content_index,
		content: end.content,
		extra: end.extra,
	}
}

fn tool_call_start(start: ContentStart) -> Result<JsonToolCallStart, JsonEventError> {
	let block = usize::try_from(start.content_index)
		.ok()
		.and_then(|index| start.partial.content.get(index));
	let Some(AssistantContentBlock::ToolCall(tool_call)) = block else {
		return Err(JsonEventError::NotToolCall(start.content_index));
	};
	Ok(JsonToolCallStart {
		content_index: start.content_index,
		id: tool_call.id.clone(),
		tool_name: tool_call.name.clone(),
		extra: start.extra,
	})
}
