//! Transcript preparation for a target model (ports `packages/ai/src/api/transform-messages.ts`): images the model
//! cannot see become notes, other models' thinking and signatures are dropped, tool call ids are normalized, failed
//! turns are skipped, and orphaned tool calls get synthetic results.

use crate::diagnostics::now_ms;
use pi_json::JsonObject;
use pi_types::{
	AssistantContentBlock, AssistantMessage, ContentBlock, ImageContent, Message, MessageContent, Model,
	ModelInputModality, StopReason, TextContent, ToolCall, ToolResultMessage, ToolResultRole,
};
use std::collections::{HashMap, HashSet};

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";
const ERROR_TOOL_IMAGE_PLACEHOLDER: &str = "(image attached after the tool results)";

/// An error tool result split into text and the images to send after the tool results (`SplitErrorToolResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct SplitErrorToolResult {
	/// The content with each run of images replaced by a note.
	pub content: Vec<TextContent>,
	/// Text naming the tool call, sent before `images`.
	pub caption: String,
	/// The images.
	pub images: Vec<ImageContent>,
}

/// Splits the images out of an error tool result, for APIs that reject images there (#10794); `None` when no split
/// is needed (`splitErrorToolResultImages`).
pub fn split_error_tool_result_images(message: &ToolResultMessage) -> Option<SplitErrorToolResult> {
	if !message.is_error {
		return None;
	}
	let images: Vec<ImageContent> = message
		.content
		.iter()
		.filter_map(|block| match block {
			ContentBlock::Image(image) => Some(image.clone()),
			ContentBlock::Text(_) => None,
		})
		.collect();
	if images.is_empty() {
		return None;
	}
	Some(SplitErrorToolResult {
		content: replace_images_with_placeholder(&message.content, ERROR_TOOL_IMAGE_PLACEHOLDER),
		caption: format!(
			"Images from the error result of tool call {} ({}):",
			message.tool_call_id, message.tool_name
		),
		images,
	})
}

fn replace_images_with_placeholder(content: &[ContentBlock], placeholder: &str) -> Vec<TextContent> {
	let mut result = Vec::new();
	let mut previous_was_placeholder = false;
	for block in content {
		match block {
			ContentBlock::Image(_) => {
				if !previous_was_placeholder {
					result.push(TextContent::new(placeholder));
				}
				previous_was_placeholder = true;
			}
			ContentBlock::Text(text) => {
				previous_was_placeholder = text.text == placeholder;
				result.push(text.clone());
			}
		}
	}
	result
}

fn as_blocks(texts: Vec<TextContent>) -> Vec<ContentBlock> {
	texts.into_iter().map(ContentBlock::Text).collect()
}

fn downgrade_unsupported_images(messages: Vec<Message>, model: &Model) -> Vec<Message> {
	if model.input.contains(&ModelInputModality::Image) {
		return messages;
	}
	messages
		.into_iter()
		.map(|message| match message {
			Message::User(mut user) => {
				if let MessageContent::Blocks(blocks) = &user.content {
					user.content = MessageContent::Blocks(as_blocks(replace_images_with_placeholder(
						blocks,
						NON_VISION_USER_IMAGE_PLACEHOLDER,
					)));
				}
				Message::User(user)
			}
			Message::ToolResult(mut result) => {
				result.content = as_blocks(replace_images_with_placeholder(
					&result.content,
					NON_VISION_TOOL_IMAGE_PLACEHOLDER,
				));
				Message::ToolResult(result)
			}
			other => other,
		})
		.collect()
}

/// Normalizes a tool call id from another model for the target model.
pub type NormalizeToolCallId<'a> = &'a dyn Fn(&str, &Model, &AssistantMessage) -> String;

/// Prepares a transcript for `model` (`transformMessages`).
pub fn transform_messages(
	messages: &[Message],
	model: &Model,
	normalize_tool_call_id: Option<NormalizeToolCallId<'_>>,
) -> Vec<Message> {
	let mut id_map: HashMap<String, String> = HashMap::new();
	let messages = downgrade_unsupported_images(messages.to_vec(), model);
	let transformed: Vec<Message> = messages
		.into_iter()
		.map(|message| match message {
			Message::ToolResult(mut result) => {
				if let Some(id) = id_map.get(&result.tool_call_id) {
					result.tool_call_id = id.clone();
				}
				Message::ToolResult(result)
			}
			Message::Assistant(assistant) => Message::Assistant(transform_assistant(
				assistant,
				model,
				normalize_tool_call_id,
				&mut id_map,
			)),
			other => other,
		})
		.collect();
	insert_synthetic_tool_results(transformed)
}

fn transform_assistant(
	mut assistant: AssistantMessage,
	model: &Model,
	normalize: Option<NormalizeToolCallId<'_>>,
	id_map: &mut HashMap<String, String>,
) -> AssistantMessage {
	let same_model = assistant.provider == model.provider && assistant.api == model.api && assistant.model == model.id;
	let source = assistant.clone();
	assistant.content = std::mem::take(&mut assistant.content)
		.into_iter()
		.filter_map(|block| match block {
			// Thinking from another model is dropped: signatures only fit their model, and readable thinking sent as
			// text would read as part of the visible reply.
			AssistantContentBlock::Thinking(thinking) => {
				let keep = same_model
					&& (thinking.redacted == Some(true)
						|| thinking
							.thinking_signature
							.as_ref()
							.is_some_and(|signature| !signature.is_empty())
						|| !thinking.thinking.trim().is_empty());
				keep.then_some(AssistantContentBlock::Thinking(thinking))
			}
			AssistantContentBlock::Text(text) if !same_model => {
				Some(AssistantContentBlock::Text(TextContent::new(text.text)))
			}
			AssistantContentBlock::ToolCall(call) => Some(AssistantContentBlock::ToolCall(transform_tool_call(
				call, same_model, model, &source, normalize, id_map,
			))),
			text @ AssistantContentBlock::Text(_) => Some(text),
		})
		.collect();
	assistant
}

fn transform_tool_call(
	mut call: ToolCall,
	same_model: bool,
	model: &Model,
	source: &AssistantMessage,
	normalize: Option<NormalizeToolCallId<'_>>,
	id_map: &mut HashMap<String, String>,
) -> ToolCall {
	if same_model {
		return call;
	}
	call.thought_signature = None;
	if let Some(normalize) = normalize {
		let normalized = normalize(&call.id, model, source);
		if normalized != call.id {
			id_map.insert(call.id.clone(), normalized.clone());
			call.id = normalized;
		}
	}
	call
}

/// Skips failed turns and closes orphaned tool calls with synthetic error results; system messages between a call
/// and its results are held back until the results are out.
fn insert_synthetic_tool_results(messages: Vec<Message>) -> Vec<Message> {
	let mut result = Vec::new();
	let mut pending: Vec<ToolCall> = Vec::new();
	let mut answered: HashSet<String> = HashSet::new();
	let mut held: Vec<Message> = Vec::new();
	let close = |result: &mut Vec<Message>,
	             pending: &mut Vec<ToolCall>,
	             answered: &mut HashSet<String>,
	             held: &mut Vec<Message>| {
		for call in pending.drain(..) {
			if !answered.contains(&call.id) {
				result.push(Message::ToolResult(ToolResultMessage {
					role: ToolResultRole::ToolResult,
					tool_call_id: call.id,
					tool_name: call.name,
					content: vec![ContentBlock::Text(TextContent::new("No result provided"))],
					details: None,
					usage: None,
					is_error: true,
					duration_ms: None,
					timestamp: now_ms(),
					nested_calls: None,
					extra: JsonObject::new(),
				}));
			}
		}
		answered.clear();
		result.append(held);
	};
	for message in messages {
		match message {
			Message::Assistant(assistant) => {
				close(&mut result, &mut pending, &mut answered, &mut held);
				// Failed or aborted turns may be incomplete; replaying them makes APIs fail.
				if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) {
					continue;
				}
				let calls: Vec<ToolCall> = assistant
					.content
					.iter()
					.filter_map(|block| match block {
						AssistantContentBlock::ToolCall(call) => Some(call.clone()),
						_ => None,
					})
					.collect();
				if !calls.is_empty() {
					pending = calls;
					answered.clear();
				}
				result.push(Message::Assistant(assistant));
			}
			Message::ToolResult(tool_result) => {
				answered.insert(tool_result.tool_call_id.clone());
				result.push(Message::ToolResult(tool_result));
			}
			Message::System(system) => {
				if pending.is_empty() {
					result.push(Message::System(system));
				} else {
					held.push(Message::System(system));
				}
			}
			Message::User(user) => {
				close(&mut result, &mut pending, &mut answered, &mut held);
				result.push(Message::User(user));
			}
		}
	}
	close(&mut result, &mut pending, &mut answered, &mut held);
	result
}
