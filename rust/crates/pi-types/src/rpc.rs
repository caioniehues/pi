//! The RPC protocol of headless pi (ports `packages/coding-agent/src/modes/rpc/rpc-types.ts`, documented in
//! `docs/rpc-commands.md`): commands on stdin, responses and extension UI requests on stdout, extension UI responses
//! on stdin, with the payload types they carry (`SessionStats`, `BashResult`, `SourceInfo`, `ContextUsage`).

use crate::agent_message::AgentMessage;
use crate::content::ImageContent;
use crate::literal::tag_type;
use crate::literal::{False, True};
use crate::model::{Model, ModelThinkingLevel};
use crate::session_entry::{SessionEntry, SessionTreeNode};
use crate::session_event::CompactionResult;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

/// A command, tagged by `type`. The RPC client writes `id` after the command's fields (`{ ...command, id }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcCommand {
	/// Send a prompt; while streaming, `streaming_behavior` says how to queue it.
	Prompt(PromptCommand),
	/// Queue a steering message.
	Steer(QueueMessageCommand),
	/// Queue a follow-up message.
	FollowUp(QueueMessageCommand),
	/// Abort the current run.
	Abort(BareCommand),
	/// Clear the steering and follow-up queues.
	ClearQueue(BareCommand),
	/// Start a new session.
	NewSession(NewSessionCommand),
	/// Read the session state.
	GetState(BareCommand),
	/// Switch the model.
	SetModel(SetModelCommand),
	/// Cycle to the next model.
	CycleModel(BareCommand),
	/// List the available models.
	GetAvailableModels(BareCommand),
	/// Set the thinking level.
	SetThinkingLevel(SetThinkingLevelCommand),
	/// Cycle to the next thinking level.
	CycleThinkingLevel(BareCommand),
	/// List the thinking levels the model supports.
	GetAvailableThinkingLevels(BareCommand),
	/// Set how queued steering messages are delivered.
	SetSteeringMode(SetQueueModeCommand),
	/// Set how queued follow-up messages are delivered.
	SetFollowUpMode(SetQueueModeCommand),
	/// Compact the context now.
	Compact(CompactCommand),
	/// Turn automatic compaction on or off.
	SetAutoCompaction(SetEnabledCommand),
	/// Turn automatic retries on or off.
	SetAutoRetry(SetEnabledCommand),
	/// Abort a pending retry.
	AbortRetry(BareCommand),
	/// Run a shell command and record it.
	Bash(BashCommand),
	/// Abort the running shell command.
	AbortBash(BareCommand),
	/// Read token and message statistics.
	GetSessionStats(BareCommand),
	/// Export the session as HTML.
	ExportHtml(ExportHtmlCommand),
	/// Open another session file.
	SwitchSession(SwitchSessionCommand),
	/// Fork the session at a user message.
	Fork(ForkCommand),
	/// Clone the session.
	Clone(BareCommand),
	/// List the messages a fork can start from.
	GetForkMessages(BareCommand),
	/// Read entries, optionally after one.
	GetEntries(GetEntriesCommand),
	/// Read the entry tree.
	GetTree(BareCommand),
	/// Read the last assistant text.
	GetLastAssistantText(BareCommand),
	/// Set the session's display name.
	SetSessionName(SetSessionNameCommand),
	/// Read the transcript.
	GetMessages(BareCommand),
	/// List the commands a prompt can invoke.
	GetCommands(BareCommand),
}

/// A command without fields of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BareCommand {
	/// The request id echoed by the response.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// How a prompt sent while streaming is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
	/// As a steering message.
	Steer,
	/// As a follow-up message.
	FollowUp,
}

/// `prompt`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCommand {
	/// The prompt text.
	pub message: String,
	/// Attached images.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub images: Option<Vec<ImageContent>>,
	/// How to queue it while streaming.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub streaming_behavior: Option<StreamingBehavior>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `steer`, `follow_up`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueMessageCommand {
	/// The message text.
	pub message: String,
	/// Attached images.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub images: Option<Vec<ImageContent>>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `new_session`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionCommand {
	/// The parent session's file.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub parent_session: Option<String>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `set_model`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelCommand {
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model_id: String,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `set_thinking_level`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetThinkingLevelCommand {
	/// The level.
	pub level: ModelThinkingLevel,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// How many queued messages a drain point delivers (pi-agent's `QueueMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
	/// Every queued message.
	All,
	/// The oldest one.
	OneAtATime,
}

/// `set_steering_mode`, `set_follow_up_mode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetQueueModeCommand {
	/// The mode.
	pub mode: QueueMode,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `compact`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactCommand {
	/// Extra instructions for the summary.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub custom_instructions: Option<String>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `set_auto_compaction`, `set_auto_retry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetEnabledCommand {
	/// On or off.
	pub enabled: bool,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `bash`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashCommand {
	/// The command line.
	pub command: String,
	/// Whether the result stays out of the model's context.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub exclude_from_context: Option<bool>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `export_html`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportHtmlCommand {
	/// Where to write; a default path when absent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub output_path: Option<String>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `switch_session`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchSessionCommand {
	/// The session file to open.
	pub session_path: String,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `fork`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkCommand {
	/// The user message entry to fork at.
	pub entry_id: String,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `get_entries`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetEntriesCommand {
	/// Only entries after this one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub since: Option<String>,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `set_session_name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetSessionNameCommand {
	/// The display name.
	pub name: String,
	/// The request id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

tag_type!(
	/// `type: "response"`, the tag of [`RpcResponse`].
	RpcResponseType::Response = "response"
);

/// A response. `rpc-mode.ts` writes `{ id, type, command, success, data | error }`, so `id` (when set) precedes the
/// tag and the response is a struct around the `command`-tagged result rather than a tagged enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
	/// The id of the request answered.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub id: Option<String>,
	/// Always `response`.
	#[serde(rename = "type")]
	pub kind: RpcResponseType,
	/// The command and its outcome.
	#[serde(flatten)]
	pub result: RpcResult,
}

/// A success (tagged by the command) or a failure of any command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcResult {
	/// `success: true` with the command's data.
	Success(RpcSuccess),
	/// `success: false` with the error.
	Failure(RpcFailure),
}

/// `success: false`: any command, including one this port does not know, can fail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcFailure {
	/// The command's `type`.
	pub command: String,
	/// Always `false`.
	pub success: False,
	/// The error text.
	pub error: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `success: true`, tagged by `command`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum RpcSuccess {
	/// `prompt`: what happened to the prompt.
	Prompt(RpcData<DispositionData<PromptDisposition>>),
	/// `steer`.
	Steer(RpcData<DispositionData<QueuedInputDisposition>>),
	/// `follow_up`.
	FollowUp(RpcData<DispositionData<QueuedInputDisposition>>),
	/// `abort`.
	Abort(RpcDone),
	/// `clear_queue`: the messages removed.
	ClearQueue(RpcData<ClearQueueData>),
	/// `new_session`.
	NewSession(RpcData<CancelledData>),
	/// `get_state`.
	GetState(RpcData<RpcSessionState>),
	/// `set_model`: the new model.
	SetModel(RpcData<Model>),
	/// `cycle_model`: the new model, `null` when there is none to cycle to.
	CycleModel(RpcData<Option<CycleModelData>>),
	/// `get_available_models`.
	GetAvailableModels(RpcData<ModelsData>),
	/// `set_thinking_level`.
	SetThinkingLevel(RpcDone),
	/// `cycle_thinking_level`: the new level, `null` when the model does not reason.
	CycleThinkingLevel(RpcData<Option<LevelData>>),
	/// `get_available_thinking_levels`.
	GetAvailableThinkingLevels(RpcData<LevelsData>),
	/// `set_steering_mode`.
	SetSteeringMode(RpcDone),
	/// `set_follow_up_mode`.
	SetFollowUpMode(RpcDone),
	/// `compact`.
	Compact(RpcData<CompactionResult>),
	/// `set_auto_compaction`.
	SetAutoCompaction(RpcDone),
	/// `set_auto_retry`.
	SetAutoRetry(RpcDone),
	/// `abort_retry`.
	AbortRetry(RpcDone),
	/// `bash`.
	Bash(RpcData<BashResult>),
	/// `abort_bash`.
	AbortBash(RpcDone),
	/// `get_session_stats`.
	GetSessionStats(RpcData<SessionStats>),
	/// `export_html`: the written file.
	ExportHtml(RpcData<PathData>),
	/// `switch_session`.
	SwitchSession(RpcData<CancelledData>),
	/// `fork`: the forked message's text.
	Fork(RpcData<ForkData>),
	/// `clone`.
	Clone(RpcData<CancelledData>),
	/// `get_fork_messages`.
	GetForkMessages(RpcData<ForkMessagesData>),
	/// `get_entries`.
	GetEntries(RpcData<EntriesData>),
	/// `get_tree`.
	GetTree(RpcData<TreeData>),
	/// `get_last_assistant_text`.
	GetLastAssistantText(RpcData<LastAssistantTextData>),
	/// `set_session_name`.
	SetSessionName(RpcDone),
	/// `get_messages`.
	GetMessages(RpcData<MessagesData>),
	/// `get_commands`.
	GetCommands(RpcData<CommandsData>),
}

/// A success with data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcData<T> {
	/// Always `true`.
	pub success: True,
	/// The command's data.
	pub data: T,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A success without data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcDone {
	/// Always `true`.
	pub success: True,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// What happened to a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptDisposition {
	/// An extension or command handled it.
	Handled,
	/// It was queued behind the running turn.
	Queued,
	/// A run started.
	Started,
}

/// What happened to a steering or follow-up message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueuedInputDisposition {
	/// An extension or command handled it.
	Handled,
	/// It was queued.
	Queued,
}

/// `{ disposition }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispositionData<T> {
	/// What happened.
	pub disposition: T,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ steering, followUp }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearQueueData {
	/// The removed steering messages.
	pub steering: Vec<String>,
	/// The removed follow-up messages.
	pub follow_up: Vec<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ cancelled }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelledData {
	/// Whether an extension cancelled the operation.
	pub cancelled: bool,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ model, thinkingLevel, isScoped }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleModelData {
	/// The new model.
	pub model: Model,
	/// Its thinking level.
	pub thinking_level: ModelThinkingLevel,
	/// Whether it came from the scoped model list.
	pub is_scoped: bool,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ models }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelsData {
	/// The models.
	pub models: Vec<Model>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ level }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelData {
	/// The level.
	pub level: ModelThinkingLevel,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ levels }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelsData {
	/// The levels.
	pub levels: Vec<ModelThinkingLevel>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ path }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathData {
	/// The path.
	pub path: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ text, cancelled }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForkData {
	/// The text of the message forked at, for the editor.
	pub text: String,
	/// Whether an extension cancelled the fork.
	pub cancelled: bool,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ messages }` of `get_fork_messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForkMessagesData {
	/// The user messages a fork can start from.
	pub messages: Vec<ForkMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A user message a fork can start from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkMessage {
	/// Its entry.
	pub entry_id: String,
	/// Its text.
	pub text: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ entries, leafId }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntriesData {
	/// The entries.
	pub entries: Vec<SessionEntry>,
	/// The current leaf; `string | null` in TS, written as `null`.
	#[serde(deserialize_with = "Option::deserialize")]
	pub leaf_id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ tree, leafId }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeData {
	/// The root nodes.
	pub tree: Vec<SessionTreeNode>,
	/// The current leaf; `string | null` in TS, written as `null`.
	#[serde(deserialize_with = "Option::deserialize")]
	pub leaf_id: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ text }` of `get_last_assistant_text`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastAssistantTextData {
	/// The text; `string | null` in TS, written as `null` when there is none.
	#[serde(deserialize_with = "Option::deserialize")]
	pub text: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ messages }` of `get_messages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesData {
	/// The transcript.
	pub messages: Vec<AgentMessage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ commands }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandsData {
	/// The commands.
	pub commands: Vec<RpcSlashCommand>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The session state of `get_state`. Field order is `rpc-mode.ts`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
	/// The current model.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub model: Option<Model>,
	/// The thinking level.
	pub thinking_level: ModelThinkingLevel,
	/// Whether a run is streaming.
	pub is_streaming: bool,
	/// Whether compaction is running.
	pub is_compacting: bool,
	/// How steering messages are delivered.
	pub steering_mode: QueueMode,
	/// How follow-up messages are delivered.
	pub follow_up_mode: QueueMode,
	/// The session file; `undefined` (omitted) for in-memory sessions.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_file: Option<String>,
	/// The session id.
	pub session_id: String,
	/// The display name.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_name: Option<String>,
	/// Whether automatic compaction is on.
	pub auto_compaction_enabled: bool,
	/// The number of transcript messages.
	pub message_count: u64,
	/// The number of queued messages.
	pub pending_message_count: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// What kind of resource a slash command comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlashCommandSource {
	/// An extension's command.
	Extension,
	/// A prompt template.
	Prompt,
	/// A skill.
	Skill,
}

/// A command a prompt can invoke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
	/// The name, without the slash.
	pub name: String,
	/// What it does.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub description: Option<String>,
	/// What kind of resource it comes from.
	pub source: SlashCommandSource,
	/// The owning resource.
	pub source_info: SourceInfo,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Where a resource is configured (ports `SourceScope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScope {
	/// The user's settings.
	User,
	/// The project's settings.
	Project,
	/// This run only.
	Temporary,
}

/// How a resource was found (ports `SourceOrigin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceOrigin {
	/// Inside a package.
	Package,
	/// Listed directly.
	TopLevel,
}

/// The resource that owns a command, tool or extension (ports `SourceInfo` of `core/source-info.ts`). Field order
/// is `createSourceInfo`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
	/// The resource path, or a synthetic path such as `builtin:read`.
	pub path: String,
	/// The package source or kind.
	pub source: String,
	/// Where it is configured.
	pub scope: SourceScope,
	/// How it was found.
	pub origin: SourceOrigin,
	/// The package's base directory.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub base_dir: Option<String>,
	/// The package's changelog.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub changelog_path: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The result of a shell command (ports `BashResult` of `core/bash-executor.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashResult {
	/// Combined stdout and stderr, sanitized and possibly truncated.
	pub output: String,
	/// The exit code; `number | undefined` in TS, omitted when killed or cancelled.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub exit_code: Option<i64>,
	/// Whether it was cancelled.
	pub cancelled: bool,
	/// Whether `output` was truncated.
	pub truncated: bool,
	/// The temp file with the full output, when truncated.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub full_output_path: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Token and message statistics of a session (ports `SessionStats` of `core/agent-session.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
	/// The session file; `string | undefined` in TS, omitted for in-memory sessions.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_file: Option<String>,
	/// The session id.
	pub session_id: String,
	/// User messages.
	pub user_messages: u64,
	/// Assistant messages.
	pub assistant_messages: u64,
	/// Tool calls.
	pub tool_calls: u64,
	/// Tool results.
	pub tool_results: u64,
	/// All messages.
	pub total_messages: u64,
	/// Token totals.
	pub tokens: TokenTotals,
	/// The total cost in USD.
	pub cost: f64,
	/// The current context usage, when the model's window is known.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub context_usage: Option<ContextUsage>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Token totals of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
	/// Input tokens.
	pub input: i64,
	/// Output tokens.
	pub output: i64,
	/// Cache-read tokens.
	pub cache_read: i64,
	/// Cache-write tokens.
	pub cache_write: i64,
	/// All of them.
	pub total: i64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The context's size against the model's window (ports `ContextUsage` of `core/extensions/types.ts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
	/// Estimated context tokens; `number | null` in TS, `null` when unknown (right after compaction).
	#[serde(deserialize_with = "Option::deserialize")]
	pub tokens: Option<u64>,
	/// The model's context window.
	pub context_window: u64,
	/// `tokens` as a percentage of the window; `null` when unknown.
	#[serde(deserialize_with = "Option::deserialize")]
	pub percent: Option<f64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

tag_type!(
	/// `type: "extension_ui_request"`.
	ExtensionUiRequestType::ExtensionUiRequest = "extension_ui_request"
);

/// An extension's request for user input or UI change, written `{ type, id, method, ... }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcExtensionUiRequest {
	/// Always `extension_ui_request`.
	#[serde(rename = "type")]
	pub kind: ExtensionUiRequestType,
	/// The request id a dialog response answers.
	pub id: String,
	/// The method and its arguments.
	#[serde(flatten)]
	pub request: ExtensionUiMethod,
}

/// The UI methods, tagged by `method`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum ExtensionUiMethod {
	/// Pick one of several options.
	#[serde(rename = "select")]
	Select(UiSelect),
	/// Confirm or decline.
	#[serde(rename = "confirm")]
	Confirm(UiConfirm),
	/// Enter a line of text.
	#[serde(rename = "input")]
	Input(UiInput),
	/// Edit text in an editor.
	#[serde(rename = "editor")]
	Editor(UiEditor),
	/// Show a notification.
	#[serde(rename = "notify")]
	Notify(UiNotify),
	/// Set or clear a status line entry.
	#[serde(rename = "setStatus")]
	SetStatus(UiSetStatus),
	/// Set or clear a widget.
	#[serde(rename = "setWidget")]
	SetWidget(UiSetWidget),
	/// Set the terminal title.
	#[serde(rename = "setTitle")]
	SetTitle(UiSetTitle),
	/// Replace the editor's text.
	#[serde(rename = "set_editor_text")]
	SetEditorText(UiSetEditorText),
}

/// `select`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSelect {
	/// The prompt.
	pub title: String,
	/// The options.
	pub options: Vec<String>,
	/// Milliseconds before the default answer.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<u64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `confirm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiConfirm {
	/// The prompt.
	pub title: String,
	/// The question.
	pub message: String,
	/// Milliseconds before the default answer.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<u64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiInput {
	/// The prompt.
	pub title: String,
	/// The placeholder text.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub placeholder: Option<String>,
	/// Milliseconds before the default answer.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timeout: Option<u64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `editor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiEditor {
	/// The prompt.
	pub title: String,
	/// The initial text.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prefill: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The severity of a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotifyType {
	/// Information.
	Info,
	/// A warning.
	Warning,
	/// An error.
	Error,
}

/// `notify`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNotify {
	/// The text.
	pub message: String,
	/// The severity.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub notify_type: Option<NotifyType>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `setStatus`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSetStatus {
	/// The status entry's key.
	pub status_key: String,
	/// The text; `string | undefined` in TS, omitted to clear the entry.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub status_text: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Where a widget goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WidgetPlacement {
	/// Above the editor.
	AboveEditor,
	/// Below the editor.
	BelowEditor,
}

/// `setWidget`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSetWidget {
	/// The widget's key.
	pub widget_key: String,
	/// The lines; `string[] | undefined` in TS, omitted to remove the widget.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub widget_lines: Option<Vec<String>>,
	/// Where it goes.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub widget_placement: Option<WidgetPlacement>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `setTitle`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSetTitle {
	/// The title.
	pub title: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `set_editor_text`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSetEditorText {
	/// The text.
	pub text: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

tag_type!(
	/// `type: "extension_ui_response"`.
	ExtensionUiResponseType::ExtensionUiResponse = "extension_ui_response"
);

/// The answer to a dialog request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcExtensionUiResponse {
	/// Always `extension_ui_response`.
	#[serde(rename = "type")]
	pub kind: ExtensionUiResponseType,
	/// The request answered.
	pub id: String,
	/// The answer.
	#[serde(flatten)]
	pub answer: ExtensionUiAnswer,
}

/// `{ value } | { confirmed } | { cancelled: true }`, read in the order `rpc-mode.ts` checks them: `cancelled: true`
/// wins over the other fields, then `confirmed`, then `value`; an answer with none of them (or `cancelled: false`
/// alone) is a `Value` without text, which TS resolves to `undefined`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionUiAnswer {
	/// The user dismissed the dialog.
	Cancelled {
		/// Always `true`.
		cancelled: True,
	},
	/// The confirmation.
	Confirmed {
		/// Whether the user confirmed.
		confirmed: bool,
	},
	/// The selected or entered text; `None` when the answer carries none.
	Value {
		/// The text.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		value: Option<String>,
	},
}
