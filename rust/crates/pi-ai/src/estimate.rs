//! Context token estimates (ports `packages/ai/src/utils/estimate.ts`): the last applicable usage block plus about
//! 3.5 characters per token for everything after it. Characters are UTF-16 code units, as in TS.

use crate::text::get_system_message_text;
use crate::utf16::utf16_len;
use pi_types::{AssistantContentBlock, AssistantMessage, ContentBlock, Message, MessageContent, StopReason, Usage};
use serde::Serialize;

/// An estimate of a context's tokens (`ContextUsageEstimate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
	/// Estimated total context tokens.
	pub tokens: u64,
	/// Tokens the most recent applicable usage block reported.
	pub usage_tokens: u64,
	/// Estimated tokens after that usage block.
	pub trailing_tokens: u64,
	/// Index of the message whose usage applied, `None` when none did.
	pub last_usage_index: Option<usize>,
}

const CHARS_PER_TOKEN: f64 = 3.5;
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// The tokens a usage block covers (`calculateContextTokens`): `totalTokens`, or the sum of its parts when that is
/// absent or 0.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
	let total = usage
		.total_tokens
		.filter(|total| *total != 0)
		.unwrap_or(usage.input + usage.output + usage.cache_read + usage.cache_write);
	u64::try_from(total).unwrap_or(0)
}

fn tokens_of_chars(chars: usize) -> u64 {
	#[allow(
		clippy::cast_precision_loss,
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "character counts are far below 2^52; the quotient is non-negative"
	)]
	let tokens = (chars as f64 / CHARS_PER_TOKEN).ceil() as u64;
	tokens
}

/// Estimated tokens of a text (`estimateTextTokens`).
pub fn estimate_text_tokens(text: &str) -> u64 {
	tokens_of_chars(utf16_len(text))
}

fn blocks_chars(blocks: &[ContentBlock]) -> usize {
	blocks
		.iter()
		.map(|block| match block {
			ContentBlock::Text(text) => utf16_len(&text.text),
			ContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
		})
		.sum()
}

/// Estimated tokens of user content: text, and 4800 characters per image (`estimateTextAndImageContentTokens`).
pub fn estimate_text_and_image_content_tokens(content: &MessageContent) -> u64 {
	match content {
		MessageContent::Text(text) => estimate_text_tokens(text),
		MessageContent::Blocks(blocks) => tokens_of_chars(blocks_chars(blocks)),
	}
}

/// `JSON.stringify(value)`; TS falls back to `[unserializable]`, which typed values never need.
fn json_text<T: Serialize>(value: &T) -> String {
	serde_json::to_value(value).map_or_else(|_| "[unserializable]".to_owned(), |value| pi_json::to_js_string(&value))
}

/// Estimated tokens of one message (`estimateMessageTokens`).
pub fn estimate_message_tokens(message: &Message) -> u64 {
	match message {
		Message::System(system) => {
			estimate_text_tokens(&get_system_message_text(system))
				+ estimate_tools_tokens(system.tools_added.as_deref())
				+ estimate_tools_tokens(system.tools_removed.as_deref())
		}
		Message::User(user) => estimate_text_and_image_content_tokens(&user.content),
		Message::ToolResult(result) => tokens_of_chars(blocks_chars(&result.content)),
		Message::Assistant(assistant) => {
			let chars = assistant
				.content
				.iter()
				.map(|block| match block {
					AssistantContentBlock::Text(text) => utf16_len(&text.text),
					AssistantContentBlock::Thinking(thinking) => utf16_len(&thinking.thinking),
					AssistantContentBlock::ToolCall(call) => {
						utf16_len(&call.name) + utf16_len(&json_text(&call.arguments))
					}
				})
				.sum();
			tokens_of_chars(chars)
		}
	}
}

fn estimate_tools_tokens<T: Serialize>(tools: Option<&[T]>) -> u64 {
	match tools {
		Some(tools) if !tools.is_empty() => estimate_text_tokens(&json_text(&tools)),
		_ => 0,
	}
}

fn message_timestamp(message: &Message) -> u64 {
	match message {
		Message::System(message) => message.timestamp,
		Message::User(message) => message.timestamp,
		Message::Assistant(message) => message.timestamp,
		Message::ToolResult(message) => message.timestamp,
	}
}

fn last_assistant_usage(messages: &[Message]) -> Option<(&AssistantMessage, usize)> {
	let mut latest_prefix_timestamp: Option<u64> = None;
	let mut found = None;
	for (index, message) in messages.iter().enumerate() {
		if let Message::Assistant(assistant) = message {
			// A newer prefix message was inserted after this response (a compaction summary), so its usage cannot
			// describe the current prefix.
			let applies = latest_prefix_timestamp.is_none_or(|latest| assistant.timestamp >= latest);
			if applies
				&& !matches!(assistant.stop_reason, StopReason::Aborted | StopReason::Error)
				&& calculate_context_tokens(&assistant.usage) > 0
			{
				found = Some((assistant, index));
			}
		}
		let timestamp = message_timestamp(message);
		latest_prefix_timestamp = Some(latest_prefix_timestamp.map_or(timestamp, |latest| latest.max(timestamp)));
	}
	found
}

/// Estimated tokens of a context (`estimateContextTokens`).
pub fn estimate_context_tokens(messages: &[Message]) -> ContextUsageEstimate {
	if let Some((assistant, index)) = last_assistant_usage(messages) {
		let usage_tokens = calculate_context_tokens(&assistant.usage);
		let trailing_tokens = messages[index + 1..].iter().map(estimate_message_tokens).sum();
		return ContextUsageEstimate {
			tokens: usage_tokens + trailing_tokens,
			usage_tokens,
			trailing_tokens,
			last_usage_index: Some(index),
		};
	}
	let tokens = messages.iter().map(estimate_message_tokens).sum();
	ContextUsageEstimate {
		tokens,
		usage_tokens: 0,
		trailing_tokens: tokens,
		last_usage_index: None,
	}
}
