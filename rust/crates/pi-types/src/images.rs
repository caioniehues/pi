//! Image generation requests and results (ports `ImagesContext`, `AssistantImages` and `ImagesStopReason` of
//! `packages/ai/src/types.ts`).

use crate::content::ContentBlock;
use crate::usage::Usage;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

/// What an image model is asked: text and image blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImagesContext {
	/// The prompt blocks.
	pub input: Vec<ContentBlock>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Why image generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImagesStopReason {
	/// It finished.
	Stop,
	/// It failed.
	Error,
	/// It was aborted.
	Aborted,
}

/// The result of image generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantImages {
	/// The image API.
	pub api: String,
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model: String,
	/// The generated text and image blocks.
	pub output: Vec<ContentBlock>,
	/// The provider's response id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub response_id: Option<String>,
	/// Tokens and cost.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub usage: Option<Usage>,
	/// Why it stopped.
	pub stop_reason: ImagesStopReason,
	/// The error text of a failed request.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error_message: Option<String>,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
