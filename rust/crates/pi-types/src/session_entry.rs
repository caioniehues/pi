//! The lines of a session file (ports the entry types of `packages/coding-agent/src/core/session-manager.ts`,
//! described in `docs/session-format.md`): the header, the eleven entry kinds, and the tree and listing records
//! built from them. Field order in each entry is its `append*` constructor's.

use crate::agent_message::AgentMessage;
use crate::content::{AssistantContentBlock, MessageContent};
use crate::literal::tag_type;
use crate::message::SystemMessage;
use crate::usage::Usage;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// The current session file version.
pub const CURRENT_SESSION_VERSION: u64 = 3;

/// One line of a session file: the header or an entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FileEntry {
	/// The first line, `type: "session"`.
	Header(SessionHeader),
	/// Every other line.
	Entry(SessionEntry),
}

tag_type!(
	/// `type: "session"`, the header's tag.
	SessionHeaderType::Session = "session"
);

/// The header line of a session file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
	/// Always `session`.
	#[serde(rename = "type")]
	pub kind: SessionHeaderType,
	/// The file version; v1 sessions have none.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub version: Option<u64>,
	/// The session id.
	pub id: String,
	/// ISO time of creation.
	pub timestamp: String,
	/// The working directory the session started in.
	pub cwd: String,
	/// The parent session's file, for forks; `undefined` is omitted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub parent_session: Option<String>,
	/// Fields this port does not know (v1 headers carry `provider`, `modelId`, `thinkingLevel`), kept.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A session entry: a known kind, or a kind this port does not know kept as JSON (the `Other` pattern of the
/// crate root). Entries written before v2 have no `id`/`parentId` and are `Other` until migrated (C8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionEntry {
	/// A kind this port knows.
	Known(KnownSessionEntry),
	/// Any other object, kept as it was read.
	Other(JsonObject),
}

/// The entry kinds, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownSessionEntry {
	/// `message`: a transcript message.
	Message(SessionMessageEntry),
	/// `thinking_level_change`.
	ThinkingLevelChange(ThinkingLevelChangeEntry),
	/// `model_change`.
	ModelChange(ModelChangeEntry),
	/// `usage`: usage outside the model context, such as cache warming.
	Usage(UsageEntry),
	/// `compaction`: a summary replacing the history before `first_kept_entry_id`.
	Compaction(CompactionEntry),
	/// `branch_summary`: the summary of the branch left when moving in the tree.
	BranchSummary(BranchSummaryEntry),
	/// `custom`: extension state, not part of the model context.
	Custom(CustomEntry),
	/// `custom_message`: an extension message that is part of the model context.
	CustomMessage(CustomMessageEntry),
	/// `context_edit`: an append-only edit of an earlier entry's model content.
	ContextEdit(ContextEditEntry),
	/// `label`: a user bookmark on an entry.
	Label(LabelEntry),
	/// `session_info`: session metadata such as the display name.
	SessionInfo(SessionInfoEntry),
}

/// `message`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
	/// The entry id (8 hex digits).
	pub id: String,
	/// The parent entry; `string | null` in TS, `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The message.
	pub message: AgentMessage,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `thinking_level_change`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The new level; an open string in TS.
	pub thinking_level: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `model_change`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model_id: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `usage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// An open usage category, such as `cache_warm`.
	pub kind: String,
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model: String,
	/// Tokens and cost.
	pub usage: Usage,
	/// A human-readable qualifier for usage notices; omitted when empty.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub note: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `compaction`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The summary text.
	pub summary: String,
	/// The first entry kept after the summary (v1 files have `firstKeptEntryIndex` instead).
	pub first_kept_entry_id: String,
	/// The context tokens before compaction.
	pub tokens_before: u64,
	/// Extension data, such as structured compaction markers; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub details: Option<JsonValue>,
	/// Usage of the summarizing calls.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub usage: Option<Usage>,
	/// Whether an extension generated it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub from_hook: Option<bool>,
	/// The complete prompt and tool state at this boundary.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub system_message: Option<SystemMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `branch_summary`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
	/// The entry id.
	pub id: String,
	/// The entry the summary hangs under; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The leaf the branch was left from, or `root`.
	pub from_id: String,
	/// The summary text.
	pub summary: String,
	/// Extension data, not sent to the model; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub details: Option<JsonValue>,
	/// Usage of the summarizing call.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub usage: Option<Usage>,
	/// Whether an extension generated it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub from_hook: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `custom`. Its constructor writes the extension fields before the tree fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
	/// The extension's entry type.
	pub custom_type: String,
	/// The extension's state; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub data: Option<JsonValue>,
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `custom_message`. Its constructor writes the extension fields before the tree fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
	/// The extension's message type.
	pub custom_type: String,
	/// Text, or text and image blocks, for the model.
	pub content: MessageContent,
	/// Whether the TUI shows it.
	pub display: bool,
	/// Extension data, not sent to the model; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub details: Option<JsonValue>,
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `context_edit`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The edited entry.
	pub target_id: String,
	/// The new content; `null` in TS (written as `null`) omits the target from the model context.
	#[serde(deserialize_with = "Option::deserialize")]
	pub replacement: Option<ContextReplacement>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The replacement of a context edit: content only, the message metadata stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextReplacement {
	/// The new content.
	pub content: ContextEditableContent,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The content a context edit may replace: a user, custom, tool result or assistant message's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContextEditableContent {
	/// Text, or text and image blocks (user and custom messages; tool results take the blocks).
	Message(MessageContent),
	/// Assistant blocks.
	Assistant(Vec<AssistantContentBlock>),
}

/// `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The labelled entry.
	pub target_id: String,
	/// The label; `string | undefined` in TS, omitted when the label is cleared.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub label: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `session_info`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
	/// The entry id.
	pub id: String,
	/// The parent entry; `null` for the root.
	#[serde(deserialize_with = "Option::deserialize")]
	pub parent_id: Option<String>,
	/// ISO time.
	pub timestamp: String,
	/// The display name; empty clears it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A node of the entry tree (`getTree()`, RPC `get_tree`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeNode {
	/// The entry.
	pub entry: SessionEntry,
	/// Its children in file order.
	pub children: Vec<SessionTreeNode>,
	/// The entry's resolved label.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub label: Option<String>,
	/// ISO time of the latest label change.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub label_timestamp: Option<String>,
}

/// A session file as session listings describe it. In-process only (TS has `Date` fields), so no serde.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
	/// The session file.
	pub path: String,
	/// The session id.
	pub id: String,
	/// The working directory it started in; empty for old sessions.
	pub cwd: String,
	/// The display name from `session_info` entries.
	pub name: Option<String>,
	/// The parent session's file, for forks.
	pub parent_session_path: Option<String>,
	/// When it was created.
	pub created: SystemTime,
	/// When it was last modified.
	pub modified: SystemTime,
	/// The number of message entries.
	pub message_count: u64,
	/// The first user message's text.
	pub first_message: String,
	/// All message text, for search.
	pub all_messages_text: String,
}
