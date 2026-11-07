//! Text of message content and system messages (ports `packages/ai/src/utils/text.ts`).

use pi_types::{
	AssistantContentBlock, ContentBlock, MessageContent, Record, SystemContent, SystemContentBlock, SystemMessage,
};

/// A content block that may hold text: every block type of every message role.
pub trait TextBlock {
	/// The text of a text block, `None` for other blocks.
	fn block_text(&self) -> Option<&str>;
}

impl TextBlock for ContentBlock {
	fn block_text(&self) -> Option<&str> {
		match self {
			Self::Text(block) => Some(&block.text),
			Self::Image(_) => None,
		}
	}
}

impl TextBlock for AssistantContentBlock {
	fn block_text(&self) -> Option<&str> {
		match self {
			Self::Text(block) => Some(&block.text),
			Self::Thinking(_) | Self::ToolCall(_) => None,
		}
	}
}

impl TextBlock for SystemContentBlock {
	fn block_text(&self) -> Option<&str> {
		match self {
			Self::Text(block) => Some(&block.text),
		}
	}
}

/// The text blocks of `blocks` joined with `separator` (`contentText` of an array).
pub fn blocks_text<B: TextBlock>(blocks: &[B], separator: &str) -> String {
	blocks
		.iter()
		.filter_map(TextBlock::block_text)
		.collect::<Vec<_>>()
		.join(separator)
}

/// User or custom message content as text, text blocks joined with newlines (`contentText`).
pub fn message_content_text(content: &MessageContent) -> String {
	match content {
		MessageContent::Text(text) => text.clone(),
		MessageContent::Blocks(blocks) => blocks_text(blocks, "\n"),
	}
}

/// System message content as text, text blocks joined with newlines (`contentText`).
pub fn system_content_text(content: &SystemContent) -> String {
	match content {
		SystemContent::Text(text) => text.clone(),
		SystemContent::Blocks(blocks) => blocks_text(blocks, "\n"),
	}
}

/// A system message as a complete prompt: its content followed by its sections (`getSystemMessageText`).
pub fn get_system_message_text(message: &SystemMessage) -> String {
	let mut parts = vec![system_content_text(&message.content)];
	for (_, text) in message.sections.iter().flat_map(Record::iter) {
		if let Some(text) = text {
			parts.push(text.clone());
		}
	}
	parts.retain(|part| !part.is_empty());
	parts.join("\n\n")
}

/// A later system message for APIs that accept system messages mid-conversation, section changes framed by name
/// (`renderSystemMessageUpdate`).
pub fn render_system_message_update(message: &SystemMessage) -> String {
	let mut parts = Vec::new();
	let text = system_content_text(&message.content);
	if !text.is_empty() {
		parts.push(text);
	}
	for (name, value) in message.sections.iter().flat_map(Record::iter) {
		parts.push(match value {
			None => format!("Removed system prompt section \"{name}\"."),
			Some(value) => format!("Updated system prompt section \"{name}\":\n\n{value}"),
		});
	}
	parts.join("\n\n")
}
