//! Content blocks of messages (ports `TextContent`, `ThinkingContent`, `ImageContent` and `ToolCall` of
//! `packages/ai/src/types.ts`) and the content shapes the message roles allow.

use crate::literal::tag_type;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

tag_type!(
	/// `type: "text"`.
	TextType::Text = "text"
);
tag_type!(
	/// `type: "thinking"`.
	ThinkingType::Thinking = "thinking"
);
tag_type!(
	/// `type: "image"`.
	ImageType::Image = "image"
);
tag_type!(
	/// `type: "toolCall"`.
	ToolCallType::ToolCall = "toolCall"
);

/// A text block (`{ type: "text" }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
	/// Always `text`.
	#[serde(rename = "type")]
	pub kind: TextType,
	/// The text.
	pub text: String,
	/// Provider message metadata: a legacy id string or a `TextSignatureV1` as JSON (OpenAI Responses).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub text_signature: Option<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A reasoning block (`{ type: "thinking" }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
	/// Always `thinking`.
	#[serde(rename = "type")]
	pub kind: ThinkingType,
	/// The reasoning text; empty when redacted.
	pub thinking: String,
	/// Provider-specific opaque or serialized reasoning replay data; the encrypted payload when redacted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thinking_signature: Option<String>,
	/// Whether safety filters redacted the reasoning.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub redacted: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// An image block (`{ type: "image" }`), base64 encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
	/// Always `image`.
	#[serde(rename = "type")]
	pub kind: ImageType,
	/// Base64 encoded image bytes.
	pub data: String,
	/// The media type, such as `image/png`.
	pub mime_type: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A tool call the model made (`{ type: "toolCall" }`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
	/// Always `toolCall`.
	#[serde(rename = "type")]
	pub kind: ToolCallType,
	/// The provider's call id; tool results refer to it.
	pub id: String,
	/// The tool's name.
	pub name: String,
	/// The parsed arguments; `{}` when `arguments_error` is set.
	pub arguments: JsonObject,
	/// Google: opaque signature for reusing thought context.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thought_signature: Option<String>,
	/// OpenAI Responses namespace of a dynamically loaded or namespaced tool.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub namespace: Option<String>,
	/// Why the model's arguments text was not a JSON object, with that text; argument validation fails with it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub arguments_error: Option<String>,
	/// Fields this port does not know (old sessions carry `partialJson`), kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A text or image block: the content of user messages, tool results and custom messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentBlock {
	/// `{ type: "text" }`.
	Text(TextContent),
	/// `{ type: "image" }`.
	Image(ImageContent),
}

/// A block of an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantContentBlock {
	/// `{ type: "text" }`.
	Text(TextContent),
	/// `{ type: "thinking" }`.
	Thinking(ThinkingContent),
	/// `{ type: "toolCall" }`.
	ToolCall(ToolCall),
}

/// A block of a system message: text only, but tagged like every block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContentBlock {
	/// `{ type: "text" }`.
	Text(TextContent),
}

/// `string | (TextContent | ImageContent)[]`: user and custom message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
	/// Plain text.
	Text(String),
	/// Text and image blocks.
	Blocks(Vec<ContentBlock>),
}

/// `string | TextContent[]`: system message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
	/// Plain text.
	Text(String),
	/// Text blocks.
	Blocks(Vec<SystemContentBlock>),
}

impl TextContent {
	/// A text block with no signature.
	pub fn new(text: impl Into<String>) -> Self {
		Self {
			kind: TextType::Text,
			text: text.into(),
			text_signature: None,
			extra: JsonObject::new(),
		}
	}
}
