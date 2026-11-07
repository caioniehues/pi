//! Catalog models and their metadata (ports `Model`, `ImageModel`, `ClassifierModel` of `packages/ai/src/types.ts`
//! and `packages/ai/src/providers/model-schema.ts`). Field order follows the catalog writer
//! (`packages/ai/catalog/define.ts`): the fields it sets first, then the ones provider maps add.

use crate::literal::tag_type;
use crate::record::Record;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

/// A pi thinking level of a reasoning request (`ThinkingLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
	/// `minimal`.
	Minimal,
	/// `low`.
	Low,
	/// `medium`.
	Medium,
	/// `high`.
	High,
	/// `xhigh`: selected model families only.
	Xhigh,
	/// `max`: selected model families only.
	Max,
}

/// A thinking level including `off` (`ModelThinkingLevel`; pi-agent's `ThinkingLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
	/// `off`.
	Off,
	/// `minimal`.
	Minimal,
	/// `low`.
	Low,
	/// `medium`.
	Medium,
	/// `high`.
	High,
	/// `xhigh`.
	Xhigh,
	/// `max`.
	Max,
}

/// A content modality, text or images: what a model accepts (`ModelInputModality`) and what an image model returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelInputModality {
	/// Text.
	Text,
	/// Images.
	Image,
}

/// Prompt cache retention preference of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
	/// No caching.
	None,
	/// The provider's short tier (the default).
	Short,
	/// The provider's long tier.
	Long,
}

/// The provider value of one thinking level: a string, or `null` for unsupported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThinkingLevelValue {
	/// The value sent to the provider.
	Value(String),
	/// `null`: the level is unsupported.
	Unsupported(()),
}

/// Provider values per thinking level; an absent level uses the provider's default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingLevelMap {
	/// `off`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub off: Option<ThinkingLevelValue>,
	/// `minimal`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub minimal: Option<ThinkingLevelValue>,
	/// `low`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub low: Option<ThinkingLevelValue>,
	/// `medium`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub medium: Option<ThinkingLevelValue>,
	/// `high`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub high: Option<ThinkingLevelValue>,
	/// `xhigh`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub xhigh: Option<ThinkingLevelValue>,
	/// `max`.
	#[serde(
		default,
		skip_serializing_if = "Option::is_none",
		deserialize_with = "crate::present"
	)]
	pub max: Option<ThinkingLevelValue>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Best-effort prompt cache lifetimes in seconds per retention tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPromptCache {
	/// The short tier's lifetime.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub short: Option<f64>,
	/// The long tier's lifetime.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub long: Option<f64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Prices in USD per million tokens, with optional request-wide tiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
	/// Input price.
	pub input: f64,
	/// Output price.
	pub output: f64,
	/// Cache-read price.
	pub cache_read: f64,
	/// Cache-write price.
	pub cache_write: f64,
	/// Tiers by total request input; the highest matching threshold prices the whole request.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tiers: Option<Vec<ModelCostTier>>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Prices for requests whose input exceeds a token count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
	/// The tier applies when total request input exceeds this many tokens.
	pub input_tokens_above: f64,
	/// Input price.
	pub input: f64,
	/// Output price.
	pub output: f64,
	/// Cache-read price.
	pub cache_read: f64,
	/// Cache-write price.
	pub cache_write: f64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Provider input limits and cache-safe preprocessing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInputLimits {
	/// Largest serialized request in bytes.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_request_bytes: Option<u64>,
	/// Image limits.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub images: Option<ModelImageInputLimits>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Image input limits of a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageInputLimits {
	/// The resize applied before a new image enters the conversation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub resize: Option<ModelImageResizeOptions>,
	/// Most images in one provider message.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_per_message: Option<u64>,
	/// Most images in one request.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_per_request: Option<u64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A cache-safe image resize profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelImageResizeOptions {
	/// Largest width in pixels.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_width: Option<u64>,
	/// Largest height in pixels.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_height: Option<u64>,
	/// Largest base64 payload in bytes.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_bytes: Option<u64>,
	/// JPEG quality, 1 to 100.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub jpeg_quality: Option<u64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

tag_type!(
	/// `type: "chat"`, optional on chat models.
	ChatModelType::Chat = "chat"
);

/// A chat model: usable with the stream functions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
	/// The provider's model id.
	pub id: String,
	/// Display name.
	pub name: String,
	/// The wire API, such as `anthropic-messages`.
	pub api: String,
	/// The provider id, such as `anthropic`.
	pub provider: String,
	/// The API base URL.
	pub base_url: String,
	/// `chat` or absent: models without a type are chat models.
	#[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
	pub model_type: Option<ChatModelType>,
	/// Whether the model reasons.
	pub reasoning: bool,
	/// Accepted inputs.
	pub input: Vec<ModelInputModality>,
	/// Prices.
	pub cost: ModelCost,
	/// Context window in tokens.
	pub context_window: u64,
	/// Largest output in tokens.
	pub max_tokens: u64,
	/// Extra HTTP headers.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub headers: Option<Record<String>>,
	/// API compatibility overrides; typed per API by `pi-ai` (C3).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub compat: Option<JsonObject>,
	/// Provider values per thinking level.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thinking_level_map: Option<ThinkingLevelMap>,
	/// Prompt cache lifetimes; absent when unknown.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prompt_cache: Option<ModelPromptCache>,
	/// Input limits.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub input_limits: Option<ModelInputLimits>,
	/// Default sampling parameters merged into requests.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sampling_params: Option<JsonObject>,
	/// Sampling parameter overrides per effective thinking level (`Partial<Record<ModelThinkingLevel, ...>>`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sampling_params_by_thinking_level: Option<Record<JsonObject>>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

tag_type!(
	/// `type: "image"`.
	ImageModelType::Image = "image"
);

/// An image-generation model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageModel {
	/// The provider's model id.
	pub id: String,
	/// Display name.
	pub name: String,
	/// The image API, such as `openrouter-images`.
	pub api: String,
	/// The provider id.
	pub provider: String,
	/// The API base URL.
	pub base_url: String,
	/// Always `image`.
	#[serde(rename = "type")]
	pub model_type: ImageModelType,
	/// Accepted inputs.
	pub input: Vec<ModelInputModality>,
	/// Output modalities; always includes `image`, `text` when it can also return text.
	pub output: Vec<ModelInputModality>,
	/// Prices.
	pub cost: ModelCost,
	/// Input limits.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub input_limits: Option<ModelInputLimits>,
	/// Extra HTTP headers.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub headers: Option<Record<String>>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

tag_type!(
	/// `type: "classifier"`.
	ClassifierModelType::Classifier = "classifier"
);

/// A structured classifier model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierModel {
	/// The provider's model id.
	pub id: String,
	/// Display name.
	pub name: String,
	/// The classifier API, such as `typesafe-system-one`.
	pub api: String,
	/// The provider id.
	pub provider: String,
	/// The API base URL.
	pub base_url: String,
	/// Always `classifier`.
	#[serde(rename = "type")]
	pub model_type: ClassifierModelType,
	/// Accepted inputs.
	pub input: Vec<ModelInputModality>,
	/// Prices.
	pub cost: ModelCost,
	/// Context window in tokens.
	pub context_window: u64,
	/// Input limits.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub input_limits: Option<ModelInputLimits>,
	/// Extra HTTP headers.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub headers: Option<Record<String>>,
	/// System One compatibility overrides; typed by `pi-ai` (C14).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub compat: Option<JsonObject>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
