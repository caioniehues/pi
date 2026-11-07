//! The events of an agent session (ports `AgentSessionEvent` of `packages/coding-agent/src/core/agent-session.ts`
//! and `CompactionResult` of `core/compaction/compaction.ts`): the agent events plus retries, compaction, queues
//! and session state.

use crate::agent_event::{
	MessageEvent, MessageUpdateEvent, ToolExecutionEndEvent, ToolExecutionStartEvent, ToolExecutionUpdateEvent,
	TurnEndEvent,
};
use crate::agent_message::AgentMessage;
use crate::model::ModelThinkingLevel;
use crate::session_entry::SessionEntry;
use crate::usage::Usage;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};

/// A session event: a known type, or a type this port does not know kept as JSON (the `Other` pattern of the
/// crate root).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentSessionEvent {
	/// A type this port knows.
	Known(KnownAgentSessionEvent),
	/// Any other object, kept as it was read.
	Other(JsonObject),
}

/// The session event types, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KnownAgentSessionEvent {
	/// A low-level agent run started.
	AgentStart,
	/// That run ended; retries, compaction recovery or queued work may still follow.
	AgentEnd(SessionAgentEndEvent),
	/// The session has no automatic work left for the run.
	AgentSettled(AgentSettledEvent),
	/// A turn started.
	TurnStart,
	/// A turn ended.
	TurnEnd(TurnEndEvent),
	/// A message started.
	MessageStart(MessageEvent),
	/// A streamed assistant message changed.
	MessageUpdate(MessageUpdateEvent),
	/// A message completed.
	MessageEnd(MessageEvent),
	/// A tool started executing.
	ToolExecutionStart(ToolExecutionStartEvent),
	/// A tool reported a partial result.
	ToolExecutionUpdate(ToolExecutionUpdateEvent),
	/// A tool finished.
	ToolExecutionEnd(ToolExecutionEndEvent),
	/// The steering or follow-up queue changed.
	QueueUpdate(QueueUpdateEvent),
	/// Compaction started.
	CompactionStart(CompactionStartEvent),
	/// Compaction ended.
	CompactionEnd(CompactionEndEvent),
	/// An entry was appended to the session.
	EntryAppended(EntryAppendedEvent),
	/// The session's display name changed.
	SessionInfoChanged(SessionInfoChangedEvent),
	/// The effective thinking level changed.
	ThinkingLevelChanged(ThinkingLevelChangedEvent),
	/// An automatic retry was scheduled.
	AutoRetryStart(AutoRetryStartEvent),
	/// Automatic retrying ended.
	AutoRetryEnd(AutoRetryEndEvent),
	/// A summarization retry was scheduled.
	SummarizationRetryScheduled(SummarizationRetryScheduledEvent),
	/// A summarization retry attempt started.
	SummarizationRetryAttemptStart(SummarizationRetryAttemptStartEvent),
	/// Summarization retrying finished.
	SummarizationRetryFinished,
	/// Output of a running `!` command.
	BashExecutionUpdate(BashExecutionUpdateEvent),
}

/// `agent_end` of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgentEndEvent {
	/// The messages the run generated.
	pub messages: Vec<AgentMessage>,
	/// Whether an automatic retry follows.
	pub will_retry: bool,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `agent_settled`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSettledEvent {
	/// Whether the run ended because it was aborted.
	pub aborted: bool,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `queue_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueUpdateEvent {
	/// Queued steering messages.
	pub steering: Vec<String>,
	/// Queued follow-up messages.
	pub follow_up: Vec<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Why compaction ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
	/// The user asked for it.
	Manual,
	/// The context passed the threshold.
	Threshold,
	/// The provider reported a context overflow.
	Overflow,
}

/// `compaction_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionStartEvent {
	/// Why compaction runs.
	pub reason: CompactionReason,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `compaction_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEndEvent {
	/// Why compaction ran.
	pub reason: CompactionReason,
	/// The result; `CompactionResult | undefined` in TS, omitted when compaction produced none.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub result: Option<CompactionResult>,
	/// Whether it was aborted.
	pub aborted: bool,
	/// Whether the request that overflowed is retried.
	pub will_retry: bool,
	/// The error text of a failed compaction.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error_message: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `entry_appended`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryAppendedEvent {
	/// The appended entry.
	pub entry: SessionEntry,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `session_info_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfoChangedEvent {
	/// The new name; `string | undefined` in TS, omitted when cleared.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `thinking_level_changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingLevelChangedEvent {
	/// The new level.
	pub level: ModelThinkingLevel,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `auto_retry_start`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRetryStartEvent {
	/// The attempt about to run, from 1.
	pub attempt: u64,
	/// The most attempts the settings allow.
	pub max_attempts: u64,
	/// Milliseconds until the attempt.
	pub delay_ms: u64,
	/// The error being retried.
	pub error_message: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `auto_retry_end`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRetryEndEvent {
	/// Whether a retry succeeded.
	pub success: bool,
	/// The last attempt.
	pub attempt: u64,
	/// The final error when retrying gave up.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub final_error: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `summarization_retry_scheduled`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummarizationRetryScheduledEvent {
	/// The attempt about to run.
	pub attempt: u64,
	/// The most attempts allowed.
	pub max_attempts: u64,
	/// Milliseconds until the attempt.
	pub delay_ms: u64,
	/// The error being retried.
	pub error_message: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// What a summarization retry summarizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SummarizationSource {
	/// A branch summary.
	BranchSummary,
	/// A compaction.
	Compaction,
}

/// `summarization_retry_attempt_start`: TS has one shape per source; `reason` is set exactly for `compaction`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummarizationRetryAttemptStartEvent {
	/// What is summarized.
	pub source: SummarizationSource,
	/// Why compaction runs; only with `source: "compaction"`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reason: Option<CompactionReason>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `bash_execution_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BashExecutionUpdateEvent {
	/// The execution id, when the caller gave one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// The new output.
	pub delta: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The result of a compaction, before the session stores it as an entry. Field order is `compact()`'s; the session
/// adds `estimatedTokensAfter` afterwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
	/// The summary text.
	pub summary: String,
	/// The first entry kept after the summary.
	pub first_kept_entry_id: String,
	/// The context tokens before compaction.
	pub tokens_before: u64,
	/// Usage of the summarizing calls.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub usage: Option<Usage>,
	/// Extension data; any JSON, `null` included.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub details: Option<JsonValue>,
	/// The estimated context tokens after compaction.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub estimated_tokens_after: Option<u64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
