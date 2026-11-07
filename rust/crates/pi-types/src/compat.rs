//! Per-API compatibility overrides of catalog models (ports the compat types of
//! `packages/ai/src/providers/compat-schema.ts`). Every struct keeps the keys it does not know in `extra`, as the TS
//! schemas are open objects; every field is optional, absent meaning the API's default or URL-based detection.
//!
//! Fields are read one by one: a field whose value does not fit its type is kept raw in `extra`, so the valid fields of
//! the object still read (TS casts the object and uses even an ill-typed value as it is).
//!
//! The typed fields are written in the schema's order, then `extra`; the catalog writes compat keys in varying orders,
//! so a model re-serialized by this crate can order its `compat` keys differently from the file it was read from.

use crate::model::ModelCost;
use crate::record::Record;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};

/// The compat object of a chat model, typed by its API (`Model.compat` of `types.ts`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ModelCompat {
	/// `openai-completions`.
	OpenAICompletions(OpenAICompletionsCompat),
	/// `openai-responses`, `azure-openai-responses`, `openai-codex-responses`.
	OpenAIResponses(OpenAIResponsesCompat),
	/// `anthropic-messages`.
	AnthropicMessages(AnthropicMessagesCompat),
	/// `bedrock-converse-stream`.
	Bedrock(BedrockCompat),
	/// `mistral-conversations`.
	MistralConversations(MistralConversationsCompat),
	/// An API without typed compat, kept as it was read.
	Other(JsonObject),
}

impl ModelCompat {
	/// Types `object` by `api`; fields that do not fit their type stay raw in the typed object's `extra`.
	pub fn for_api(api: &str, object: JsonObject) -> Self {
		match api {
			"openai-completions" => typed(object, Self::OpenAICompletions),
			"openai-responses" | "azure-openai-responses" | "openai-codex-responses" => {
				typed(object, Self::OpenAIResponses)
			}
			"anthropic-messages" => typed(object, Self::AnthropicMessages),
			"bedrock-converse-stream" => typed(object, Self::Bedrock),
			"mistral-conversations" => typed(object, Self::MistralConversations),
			_ => Self::Other(object),
		}
	}

	/// The `openai-completions` compat, when this is one.
	pub fn openai_completions(&self) -> Option<&OpenAICompletionsCompat> {
		match self {
			Self::OpenAICompletions(compat) => Some(compat),
			_ => None,
		}
	}

	/// The Responses compat, when this is one.
	pub fn openai_responses(&self) -> Option<&OpenAIResponsesCompat> {
		match self {
			Self::OpenAIResponses(compat) => Some(compat),
			_ => None,
		}
	}

	/// The `anthropic-messages` compat, when this is one.
	pub fn anthropic_messages(&self) -> Option<&AnthropicMessagesCompat> {
		match self {
			Self::AnthropicMessages(compat) => Some(compat),
			_ => None,
		}
	}

	/// The `bedrock-converse-stream` compat, when this is one.
	pub fn bedrock(&self) -> Option<&BedrockCompat> {
		match self {
			Self::Bedrock(compat) => Some(compat),
			_ => None,
		}
	}

	/// The `mistral-conversations` compat, when this is one.
	pub fn mistral_conversations(&self) -> Option<&MistralConversationsCompat> {
		match self {
			Self::MistralConversations(compat) => Some(compat),
			_ => None,
		}
	}
}

fn typed<T: CompatObject>(object: JsonObject, wrap: impl FnOnce(T) -> ModelCompat) -> ModelCompat {
	wrap(read_fields(object))
}

/// A compat struct: all fields optional, unknown keys in `extra`.
pub trait CompatObject: serde::de::DeserializeOwned + Default {
	/// The keys this port does not type, and the typed keys whose values did not fit.
	fn extra_mut(&mut self) -> &mut JsonObject;
}

/// Reads a compat object field by field; a field whose value does not fit is kept raw in `extra`.
pub fn read_fields<T: CompatObject>(object: JsonObject) -> T {
	// Trust boundary: catalog files and models.json.
	let mut valid = JsonObject::new();
	let mut invalid = JsonObject::new();
	for (key, value) in object {
		let single = JsonValue::Object(JsonObject::from_iter([(key.clone(), value)]));
		let fits = T::deserialize(&single).is_ok();
		let JsonValue::Object(single) = single else { continue };
		let target = if fits { &mut valid } else { &mut invalid };
		target.extend(single);
	}
	let mut compat = T::deserialize(JsonValue::Object(valid)).unwrap_or_default();
	compat.extra_mut().extend(invalid);
	compat
}

/// Deserializes an optional compat object with [`read_fields`].
pub(crate) fn optional_fields<'de, D: serde::Deserializer<'de>, T: CompatObject>(
	deserializer: D,
) -> Result<Option<T>, D::Error> {
	Option::<JsonObject>::deserialize(deserializer).map(|object| object.map(read_fields))
}

macro_rules! compat_object {
	($($name:ident),*) => {
		$(impl CompatObject for $name {
			fn extra_mut(&mut self) -> &mut JsonObject {
				&mut self.extra
			}
		})*
	};
}

compat_object!(
	OpenAICompletionsCompat,
	OpenAIResponsesCompat,
	AnthropicMessagesCompat,
	BedrockCompat,
	MistralConversationsCompat,
	SystemOneCompat
);

/// `"openai" | "openai-nosession" | "openrouter"`: the session-affinity header format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
	/// `openai`.
	Openai,
	/// `openai-nosession`.
	OpenaiNosession,
	/// `openrouter`.
	Openrouter,
}

/// `"auto" | "enabled" | "disabled"`: whether `prompt_cache_key` is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptCacheKeyMode {
	/// Each API's default.
	Auto,
	/// Whenever caching is active.
	Enabled,
	/// Never.
	Disabled,
}

/// The top-level request field that caps reasoning tokens on OpenAI-compatible servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTokenBudgetField {
	/// `thinking_token_budget` (vLLM).
	ThinkingTokenBudget,
	/// `thinking_budget` (Qwen, Alibaba Cloud's model service, the SGL runtime).
	ThinkingBudget,
	/// `thinking_budget_tokens` (llama.cpp).
	ThinkingBudgetTokens,
}

/// `"max_completion_tokens" | "max_tokens"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
	/// `max_completion_tokens`.
	MaxCompletionTokens,
	/// `max_tokens`.
	MaxTokens,
}

/// The format of reasoning parameters on OpenAI-compatible completions endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
	/// `reasoning_effort`.
	Openai,
	/// `reasoning.effort`.
	Openrouter,
	/// `thinking.type` plus `reasoning_effort`.
	Deepseek,
	/// `reasoning.enabled` plus `reasoning_effort`.
	Together,
	/// `chat_template_args` plus `reasoning_effort`.
	Baseten,
	/// `thinking.type`.
	Zai,
	/// Top-level `enable_thinking`.
	Qwen,
	/// Configurable `chat_template_kwargs`.
	ChatTemplate,
	/// `chat_template_kwargs.enable_thinking` and `preserve_thinking`.
	QwenChatTemplate,
	/// Top-level `thinking`.
	StringThinking,
	/// `reasoning.effort` only when the mapped effort is non-null.
	AntLing,
}

/// `"anthropic"`: the cache control convention of a completions endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlFormat {
	/// Anthropic `cache_control` markers.
	Anthropic,
}

/// A value of `chatTemplateKwargs` or `chatTemplateArgs`: a pi-controlled variable or a literal JSON value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTemplateKwargValue {
	/// `{ $var, omitWhenOff? }`.
	Var(ChatTemplateVar),
	/// A string, number, boolean or `null`.
	Value(JsonValue),
}

/// A pi-controlled chat template value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTemplateVar {
	/// The variable.
	#[serde(rename = "$var")]
	pub var: ChatTemplateVarName,
	/// Whether to omit the key when thinking is off.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub omit_when_off: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The variables a chat template value may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatTemplateVarName {
	/// `thinking.enabled`.
	#[serde(rename = "thinking.enabled")]
	ThinkingEnabled,
	/// `thinking.effort`.
	#[serde(rename = "thinking.effort")]
	ThinkingEffort,
	/// `thinking.budget`.
	#[serde(rename = "thinking.budget")]
	ThinkingBudget,
}

/// Compatibility settings for OpenAI-compatible completions APIs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAICompletionsCompat {
	/// Whether the provider supports the `store` field.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_store: Option<bool>,
	/// Whether the provider supports the `developer` role.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_developer_role: Option<bool>,
	/// Whether the provider supports `reasoning_effort`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_reasoning_effort: Option<bool>,
	/// Whether `stream_options.include_usage` works (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_usage_in_streaming: Option<bool>,
	/// Whether streamed responses include `finish_reason` (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_finish_reason: Option<bool>,
	/// Which field carries the token limit.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_tokens_field: Option<MaxTokensField>,
	/// Whether tool results need the `name` field.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub requires_tool_result_name: Option<bool>,
	/// Whether a user message after tool results needs an assistant message in between.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub requires_assistant_after_tool_result: Option<bool>,
	/// Whether replayed assistant messages without text send `""` instead of `null`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub requires_non_null_assistant_content: Option<bool>,
	/// Whether requests without tools send `"tools": []` when the history has tool calls.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub requires_tools_with_tool_history: Option<bool>,
	/// Whether thinking blocks are sent as `<thinking>` text.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub requires_thinking_as_text: Option<bool>,
	/// Whether replayed assistant messages carry an empty `reasoning_content` when reasoning is on.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub requires_reasoning_content_on_assistant_messages: Option<bool>,
	/// The reasoning parameter format.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thinking_format: Option<ThinkingFormat>,
	/// `chat_template_kwargs` for `chat-template`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub chat_template_kwargs: Option<Record<ChatTemplateKwargValue>>,
	/// `chat_template_args` for `baseten`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub chat_template_args: Option<Record<ChatTemplateKwargValue>>,
	/// OpenRouter routing preferences.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub open_router_routing: Option<OpenRouterRouting>,
	/// Vercel AI Gateway routing preferences.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub vercel_gateway_routing: Option<VercelGatewayRouting>,
	/// Whether z.ai supports top-level `tool_stream`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub zai_tool_stream: Option<bool>,
	/// The field that caps reasoning tokens.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
	/// Alias for `thinking_token_budget_field: thinking_token_budget`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_thinking_token_budget: Option<bool>,
	/// Whether OpenAI grammar tools are supported.
	#[serde(
		rename = "supportsOpenAIGrammarTools",
		default,
		skip_serializing_if = "Option::is_none"
	)]
	pub supports_openai_grammar_tools: Option<bool>,
	/// Whether system messages after the start are accepted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_system_messages: Option<bool>,
	/// Whether system messages may add tools mid-conversation.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_tool_additions: Option<bool>,
	/// Whether tool definitions accept `strict`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_strict_mode: Option<bool>,
	/// The cache control convention.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub cache_control_format: Option<CacheControlFormat>,
	/// Whether session-affinity data is sent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub send_session_affinity_headers: Option<bool>,
	/// The session-affinity header format.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_affinity_format: Option<SessionAffinityFormat>,
	/// Whether `prompt_cache_key` is sent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
	/// Whether long prompt cache retention is supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_long_cache_retention: Option<bool>,
	/// The vLLM scheduler priority.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub vllm_priority: Option<f64>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Compatibility settings for OpenAI Responses APIs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAIResponsesCompat {
	/// Whether the `developer` role is supported (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_developer_role: Option<bool>,
	/// Whether system messages after the start are accepted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_system_messages: Option<bool>,
	/// The session-affinity header format.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_affinity_format: Option<SessionAffinityFormat>,
	/// Whether `prompt_cache_key` is sent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
	/// Whether long prompt cache retention is supported (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_long_cache_retention: Option<bool>,
	/// Whether strict JSON-schema function tools are supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_strict_mode: Option<bool>,
	/// Whether OpenAI grammar tools are sent.
	#[serde(
		rename = "supportsOpenAIGrammarTools",
		default,
		skip_serializing_if = "Option::is_none"
	)]
	pub supports_openai_grammar_tools: Option<bool>,
	/// Whether message-anchored `additional_tools` are supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_additional_tools: Option<bool>,
	/// Whether client-executed tool search is supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_tool_search: Option<bool>,
	/// Whether `prompt_cache_options` is accepted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_explicit_prompt_cache_mode: Option<bool>,
	/// Whether thinking level changes are sent as `configuration_update` items.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_effort: Option<bool>,
	/// Whether `max_output_tokens` is accepted (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_max_output_tokens: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Compatibility settings for Anthropic Messages-compatible APIs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnthropicMessagesCompat {
	/// Whether per-tool `eager_input_streaming` is accepted (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_eager_tool_input_streaming: Option<bool>,
	/// Whether `cache_control.ttl` long retention is supported (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_long_cache_retention: Option<bool>,
	/// Whether `x-session-affinity` is sent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub send_session_affinity_headers: Option<bool>,
	/// `openrouter` sends `x-session-id`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub session_affinity_format: Option<AnthropicSessionAffinityFormat>,
	/// Whether tool definitions accept `cache_control` (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_cache_control_on_tools: Option<bool>,
	/// Whether `temperature` is accepted (default true).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_temperature: Option<bool>,
	/// Whether adaptive thinking is forced.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub force_adaptive_thinking: Option<bool>,
	/// Whether empty thinking signatures are replayed.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub allow_empty_signature: Option<bool>,
	/// Whether strict tool schemas are supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_strict_tools: Option<bool>,
	/// Whether effort-only system messages are supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_effort: Option<bool>,
	/// Whether system messages inside the conversation are accepted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_system_messages: Option<bool>,
	/// Whether `tool_addition` and `tool_removal` blocks are accepted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_tool_changes: Option<bool>,
	/// Whether message threads are supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_message_threads: Option<bool>,
	/// Server-side refusal fallback models with local pricing.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub allowed_fallback_models: Option<Vec<AnthropicAllowedFallbackModel>>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `"openrouter"`: the Anthropic session-affinity format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicSessionAffinityFormat {
	/// `x-session-id`.
	Openrouter,
}

/// An Anthropic refusal fallback model with local pricing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnthropicAllowedFallbackModel {
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model: String,
	/// Its prices.
	pub cost: ModelCost,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Compatibility settings for Amazon Bedrock models.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BedrockCompat {
	/// Whether strict tool schemas are supported.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_strict_mode: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Compatibility settings for the Mistral chat API.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MistralConversationsCompat {
	/// Whether system messages after the start are accepted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub supports_mid_convo_system_messages: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Compatibility settings for System One classifier APIs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemOneCompat {
	/// How the service takes classifier images.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub image_input: Option<SystemOneImageInput>,
	/// Whether requests with images send `TypeSafe-Danger-Zone-Images: true`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub send_image_opt_in_header: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `"assets" | "images" | "state-image-urls"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemOneImageInput {
	/// The assets map of the `typesafe` service.
	Assets,
	/// Clef's images list.
	Images,
	/// `image_url` parts before the state (OpenRouter).
	StateImageUrls,
}

/// OpenRouter provider routing preferences, sent as the request's `provider` field.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OpenRouterRouting {
	/// Whether backup providers may serve requests.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub allow_fallbacks: Option<bool>,
	/// Whether only providers supporting every request parameter are used.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub require_parameters: Option<bool>,
	/// `"deny"` or `"allow"`.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub data_collection: Option<String>,
	/// Zero data retention endpoints only.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub zdr: Option<bool>,
	/// Distillable-text models only.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub enforce_distillable_text: Option<bool>,
	/// Providers to try in order.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub order: Option<Vec<String>>,
	/// Providers to allow exclusively.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub only: Option<Vec<String>>,
	/// Providers to skip.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub ignore: Option<Vec<String>>,
	/// Quantization levels to filter by.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub quantizations: Option<Vec<String>>,
	/// A sort metric name or a sort object; kept as JSON.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sort: Option<JsonValue>,
	/// Maximum prices; kept as JSON.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub max_price: Option<JsonValue>,
	/// Preferred minimum throughput: a number or percentile cutoffs; kept as JSON.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_min_throughput: Option<JsonValue>,
	/// Preferred maximum latency: a number or percentile cutoffs; kept as JSON.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub preferred_max_latency: Option<JsonValue>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Vercel AI Gateway routing preferences.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VercelGatewayRouting {
	/// Providers to use exclusively.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub only: Option<Vec<String>>,
	/// Providers to try in order.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub order: Option<Vec<String>>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
