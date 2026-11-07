//! The messages of an agent transcript: pi-ai's four roles plus the coding agent's custom roles (ports `AgentMessage`
//! of `packages/agent/src/types.ts` with the declaration merging of `packages/coding-agent/src/core/messages.ts`).

use crate::content::MessageContent;
use crate::literal::tag_type;
use crate::message::{AssistantMessage, Message, SystemMessage, ToolResultMessage, UserMessage};
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};

/// A transcript message: a known role, or a message of a role this port does not know (an extension's), kept as
/// JSON (the `Other` pattern of the crate root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
	/// A role this port knows.
	Known(KnownAgentMessage),
	/// Any other object, kept as it was read.
	Other(JsonObject),
}

/// The roles this port knows; each message carries its `role`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KnownAgentMessage {
	/// `role: "system"`.
	System(SystemMessage),
	/// `role: "user"`.
	User(UserMessage),
	/// `role: "assistant"`.
	Assistant(AssistantMessage),
	/// `role: "toolResult"`.
	ToolResult(ToolResultMessage),
	/// `role: "bashExecution"`: a `!` command the user ran.
	BashExecution(BashExecutionMessage),
	/// `role: "custom"`: a message an extension injected.
	Custom(CustomMessage),
	/// `role: "branchSummary"`: the summary of a branch the conversation came back from.
	BranchSummary(BranchSummaryMessage),
	/// `role: "compactionSummary"`: the summary of compacted history.
	CompactionSummary(CompactionSummaryMessage),
}

tag_type!(
	/// `role: "bashExecution"`.
	BashExecutionRole::BashExecution = "bashExecution"
);
tag_type!(
	/// `role: "custom"`.
	CustomRole::Custom = "custom"
);
tag_type!(
	/// `role: "branchSummary"`.
	BranchSummaryRole::BranchSummary = "branchSummary"
);
tag_type!(
	/// `role: "compactionSummary"`.
	CompactionSummaryRole::CompactionSummary = "compactionSummary"
);

/// A bash execution via `!` (`!!` excludes it from the model's context). Field order is `recordBashResult`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
	/// Always `bashExecution`.
	pub role: BashExecutionRole,
	/// The command line.
	pub command: String,
	/// Combined stdout and stderr, sanitized and possibly truncated.
	pub output: String,
	/// The exit code; `number | undefined` in TS, omitted when the command was killed or cancelled.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub exit_code: Option<i64>,
	/// Whether the command was cancelled.
	pub cancelled: bool,
	/// Whether `output` was truncated.
	pub truncated: bool,
	/// The temp file holding the full output, when truncated.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub full_output_path: Option<String>,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Whether the message stays out of the model's context (`!!`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub exclude_from_context: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A message an extension injected with `sendMessage()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
	/// Always `custom`.
	pub role: CustomRole,
	/// The extension's message type.
	pub custom_type: String,
	/// Text, or text and image blocks.
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
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The summary of an abandoned branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
	/// Always `branchSummary`.
	pub role: BranchSummaryRole,
	/// The summary text.
	pub summary: String,
	/// The entry the branch was left from; `string | null` in TS, written as `null`.
	#[serde(deserialize_with = "Option::deserialize")]
	pub from_id: Option<String>,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The summary of compacted history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
	/// Always `compactionSummary`.
	pub role: CompactionSummaryRole,
	/// The summary text.
	pub summary: String,
	/// The context tokens before compaction.
	pub tokens_before: u64,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

impl From<Message> for AgentMessage {
	fn from(message: Message) -> Self {
		Self::Known(match message {
			Message::System(message) => KnownAgentMessage::System(message),
			Message::User(message) => KnownAgentMessage::User(message),
			Message::Assistant(message) => KnownAgentMessage::Assistant(message),
			Message::ToolResult(message) => KnownAgentMessage::ToolResult(message),
		})
	}
}
