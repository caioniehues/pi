//! Transcript normalization and system-message replay (ports `packages/ai/src/utils/transcript.ts`): the leading
//! system message carries the prompt and tools, later system messages change them, and replaying them in order gives
//! the current prompt and tools.

use crate::text::system_content_text;
use pi_json::JsonObject;
use pi_types::{Message, Record, SystemContent, SystemMessage, SystemRole, Tool, ToolReference};
use std::collections::{HashMap, HashSet};

/// The request input of the public stream entry points: `system_prompt` and `tools` are shorthand for a leading
/// system message (`Context`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
	/// The system prompt.
	pub system_prompt: Option<String>,
	/// The transcript.
	pub messages: Vec<Message>,
	/// The tools.
	pub tools: Option<Vec<Tool>>,
}

/// A normalized transcript: the prompt and tools live in its system messages (`TranscriptContext`). Only
/// [`normalize_context`] and the replay functions here make one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TranscriptContext {
	messages: Vec<Message>,
}

impl TranscriptContext {
	/// The messages, system messages included.
	pub fn messages(&self) -> &[Message] {
		&self.messages
	}

	/// The messages, by value.
	pub fn into_messages(self) -> Vec<Message> {
		self.messages
	}
}

/// The leading system message of a prompt and tool set; `None` when both are empty (`createInitialSystemMessage`).
pub fn create_initial_system_message(system_prompt: Option<&str>, tools: Option<&[Tool]>) -> Option<SystemMessage> {
	let has_prompt = system_prompt.is_some_and(|prompt| !prompt.is_empty());
	let has_tools = tools.is_some_and(|tools| !tools.is_empty());
	if !has_prompt && !has_tools {
		return None;
	}
	Some(SystemMessage {
		role: SystemRole::System,
		content: SystemContent::Text(system_prompt.unwrap_or_default().to_owned()),
		sections: None,
		tools_added: has_tools.then(|| tools.unwrap_or_default().to_vec()),
		tools_removed: None,
		timestamp: 0,
		extra: JsonObject::new(),
	})
}

/// Folds `system_prompt` and `tools` into a leading system message (`normalizeContext`).
pub fn normalize_context(context: Context) -> TranscriptContext {
	let initial = create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
	let mut messages = Vec::with_capacity(context.messages.len() + 1);
	messages.extend(initial.map(Message::System));
	messages.extend(context.messages);
	TranscriptContext { messages }
}

/// A transcript already normalized by its producer, such as a session's replayed messages.
pub fn transcript_from_messages(messages: Vec<Message>) -> TranscriptContext {
	TranscriptContext { messages }
}

fn system_messages(messages: &[Message]) -> impl Iterator<Item = &SystemMessage> {
	messages.iter().filter_map(|message| match message {
		Message::System(system) => Some(system),
		_ => None,
	})
}

/// The leading system message, if the transcript starts with one (`getInitialSystemMessage`).
pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
	match messages.first() {
		Some(Message::System(system)) => Some(system),
		_ => None,
	}
}

/// The messages without the leading system message (`withoutInitialSystemMessage`).
pub fn without_initial_system_message(messages: &[Message]) -> &[Message] {
	if get_initial_system_message(messages).is_some() {
		&messages[1..]
	} else {
		messages
	}
}

/// The tools available after applying every transcript change in order (`getCurrentTools`).
pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
	current_tools(system_messages(messages))
}

fn current_tools<'a>(systems: impl Iterator<Item = &'a SystemMessage>) -> Vec<Tool> {
	// TS parity: a Map keeps a re-added name at its first position unless it was deleted first.
	let mut tools: Vec<Tool> = Vec::new();
	for message in systems {
		for removed in message.tools_removed.iter().flatten() {
			tools.retain(|tool| tool.name != removed.name);
		}
		for added in message.tools_added.iter().flatten() {
			match tools.iter_mut().find(|tool| tool.name == added.name) {
				Some(slot) => *slot = added.clone(),
				None => tools.push(added.clone()),
			}
		}
	}
	tools
}

/// Every system message replayed into one leading system message (`getCurrentSystemMessage`).
pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
	let mut content = Vec::new();
	// A JS Map: a deleted name set again goes to the end.
	let mut sections: Vec<(String, String)> = Vec::new();
	let mut timestamp = None;
	for message in system_messages(messages) {
		timestamp.get_or_insert(message.timestamp);
		let text = system_content_text(&message.content);
		if !text.is_empty() {
			content.push(text);
		}
		for (name, value) in message.sections.iter().flat_map(Record::iter) {
			let existing = sections.iter_mut().find(|(key, _)| key == name);
			match (value, existing) {
				(None, _) => sections.retain(|(key, _)| key != name),
				(Some(value), Some((_, slot))) => slot.clone_from(value),
				(Some(value), None) => sections.push((name.to_owned(), value.clone())),
			}
		}
	}
	let tools = get_current_tools(messages);
	if timestamp.is_none() && tools.is_empty() {
		return None;
	}
	let sections: Record<Option<String>> = sections.into_iter().map(|(name, value)| (name, Some(value))).collect();
	Some(SystemMessage {
		role: SystemRole::System,
		content: SystemContent::Text(content.join("\n\n")),
		sections: (!sections.is_empty()).then_some(sections),
		tools_added: (!tools.is_empty()).then_some(tools),
		tools_removed: None,
		timestamp: timestamp.unwrap_or(0),
		extra: JsonObject::new(),
	})
}

/// The current system prompt text after replaying every system message (`getCurrentSystemPrompt`).
pub fn get_current_system_prompt(messages: &[Message]) -> String {
	get_current_system_message(messages)
		.map(|message| crate::text::get_system_message_text(&message))
		.unwrap_or_default()
}

/// The transcript for APIs without mid-conversation system messages: the replayed system message leads, every
/// later one is dropped (`collapseSystemMessages`).
pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
	let head = get_current_system_message(&context.messages);
	let mut messages: Vec<Message> = head.into_iter().map(Message::System).collect();
	messages.extend(
		context
			.messages
			.iter()
			.filter(|message| !matches!(message, Message::System(_)))
			.cloned(),
	);
	TranscriptContext { messages }
}

/// Keeps later system messages in place when the model accepts them, else collapses them (`resolveTranscript`).
pub fn resolve_transcript(
	context: &TranscriptContext,
	supports_mid_convo_system_messages: Option<bool>,
) -> TranscriptContext {
	if supports_mid_convo_system_messages == Some(true) {
		context.clone()
	} else {
		collapse_system_messages(context)
	}
}

/// A tool without executable or display-only fields (`toToolDeclaration`).
pub fn to_tool_declaration(tool: &Tool) -> Tool {
	Tool {
		name: tool.name.clone(),
		description: tool.description.clone(),
		parameters: tool.parameters.clone(),
		constrained_sampling: tool.constrained_sampling.clone(),
		extra: JsonObject::new(),
	}
}

/// Whether two tools declare the same interface to the model: their declarations serialize equally
/// (`declarationsEqual`).
pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
	let text = |tool: &Tool| {
		serde_json::to_value(to_tool_declaration(tool))
			.map(|value| pi_json::to_js_string(&value))
			.unwrap_or_default()
	};
	text(left) == text(right)
}

/// The changes between two complete tool states; a changed definition is a removal and an addition
/// (`ToolStateChanges`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolStateChanges {
	/// Tools that are new or changed.
	pub tools_added: Vec<Tool>,
	/// Tools that are gone or changed.
	pub tools_removed: Vec<ToolReference>,
}

/// Compares two complete tool states (`getToolStateChanges`).
pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
	let previous_by_name: HashMap<&str, &Tool> = previous.iter().map(|tool| (tool.name.as_str(), tool)).collect();
	let current_by_name: HashMap<&str, &Tool> = current.iter().map(|tool| (tool.name.as_str(), tool)).collect();
	ToolStateChanges {
		tools_added: current
			.iter()
			.filter(|tool| {
				previous_by_name
					.get(tool.name.as_str())
					.is_none_or(|old| !declarations_equal(old, tool))
			})
			.map(to_tool_declaration)
			.collect(),
		tools_removed: previous
			.iter()
			.filter(|tool| {
				current_by_name
					.get(tool.name.as_str())
					.is_none_or(|new| !declarations_equal(tool, new))
			})
			.map(|tool| ToolReference {
				name: tool.name.clone(),
				extra: JsonObject::new(),
			})
			.collect(),
	}
}

/// Every definition referenced by transcript tool state, in first-declaration order, the last definition of each
/// name winning (`getDeclaredTools`).
pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
	let mut definitions: Vec<Tool> = Vec::new();
	for message in system_messages(messages) {
		for tool in message.tools_added.iter().flatten() {
			match definitions.iter_mut().find(|existing| existing.name == tool.name) {
				Some(slot) => *slot = tool.clone(),
				None => definitions.push(tool.clone()),
			}
		}
	}
	definitions
}

/// Whether a tool name was declared twice with different definitions (`hasToolRedefinitions`, deprecated in TS).
pub fn has_tool_redefinitions(messages: &[Message]) -> bool {
	let mut declared: HashMap<&str, &Tool> = HashMap::new();
	for message in system_messages(messages) {
		for tool in message.tools_added.iter().flatten() {
			if declared
				.get(tool.name.as_str())
				.is_some_and(|previous| !declarations_equal(previous, tool))
			{
				return true;
			}
			declared.insert(&tool.name, tool);
		}
	}
	false
}

/// Whether tool history holds a removal or same-name redeclaration (`hasNonAdditiveToolChanges`).
pub fn has_non_additive_tool_changes(messages: &[Message]) -> bool {
	let systems: Vec<&SystemMessage> = system_messages(messages).collect();
	find_last_non_additive_tool_change(&systems).is_some()
}

/// Index of the last system message with a removal or same-name redeclaration.
fn find_last_non_additive_tool_change(systems: &[&SystemMessage]) -> Option<usize> {
	// A name declared before counts as a redeclaration even after its removal: Kimi K3 ignores a removed tool that a
	// later tool message declares again (verified October 2026).
	let mut declared: HashSet<&str> = HashSet::new();
	let mut last = None;
	for (index, message) in systems.iter().enumerate() {
		if message
			.tools_removed
			.as_ref()
			.is_some_and(|removed| !removed.is_empty())
		{
			last = Some(index);
		}
		for tool in message.tools_added.iter().flatten() {
			if !declared.insert(&tool.name) {
				last = Some(index);
			}
		}
	}
	last
}

/// How a transport can express tool changes after the first request (`TranscriptToolSupport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptToolSupport {
	/// Only the top-level tool list exists; it always holds the current tools.
	None,
	/// Tools can be added at a later message, not removed or redefined.
	Additions,
	/// Every tool is declared in place; removals are announced as text.
	Changes,
}

/// How tool declarations split between the top-level request field and in-place changes (`TranscriptTools`).
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptTools {
	/// Tools sent in the top-level request field.
	pub request_tools: Vec<Tool>,
	/// The number of leading system messages whose tool changes `request_tools` already holds.
	pub folded_system_messages: usize,
}

/// Splits tool declarations between the top-level field and in-place changes (`resolveTranscriptTools`).
pub fn resolve_transcript_tools(messages: &[Message], support: TranscriptToolSupport) -> TranscriptTools {
	let systems: Vec<&SystemMessage> = system_messages(messages).collect();
	match support {
		TranscriptToolSupport::None => TranscriptTools {
			request_tools: current_tools(systems.iter().copied()),
			folded_system_messages: systems.len(),
		},
		TranscriptToolSupport::Changes => TranscriptTools {
			request_tools: Vec::new(),
			folded_system_messages: 0,
		},
		TranscriptToolSupport::Additions => {
			let initial = usize::from(get_initial_system_message(messages).is_some());
			let folded = initial.max(find_last_non_additive_tool_change(&systems).map_or(0, |index| index + 1));
			TranscriptTools {
				request_tools: current_tools(systems[..folded.min(systems.len())].iter().copied()),
				folded_system_messages: folded,
			}
		}
	}
}
