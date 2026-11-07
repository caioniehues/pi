//! Models of every type and the operations on them: model types, cost, thinking levels, equality, and the error
//! results of image and classifier calls (ports `packages/ai/src/utils/model-operations.ts` and the model helpers of
//! `packages/ai/src/models.ts`: `calculateCost`, `getSupportedThinkingLevels`, `clampThinkingLevel`,
//! `modelsAreEqual`, `hasApi`).

use crate::diagnostics::now_ms;
use crate::error::{ModelsError, ModelsErrorCode};
use pi_json::{JsonObject, JsonValue};
use pi_types::{
	AssistantImages, ClassifierContext, ClassifierModel, ClassifierResult, ClassifierStopReason, ImageModel,
	ImagesStopReason, Model, ModelCost, ModelInputModality, ModelThinkingLevel, Record, ThinkingLevelValue, Usage,
	UsageCost,
};
use serde::{Deserialize, Serialize};

/// What a catalog entry is for (`ModelType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelType {
	/// Chat models, usable with the stream functions.
	Chat,
	/// Image-generation models.
	Image,
	/// Structured classifiers.
	Classifier,
}

/// Any model a provider can list (`AnyModel`): read untagged, with `type` deciding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(
	clippy::large_enum_variant,
	reason = "plain catalog data, as in pi-types; boxing costs an allocation per model"
)]
pub enum AnyModel {
	/// An image model (`type: "image"`).
	Image(ImageModel),
	/// A classifier (`type: "classifier"`).
	Classifier(ClassifierModel),
	/// A chat model (`type: "chat"` or none).
	Chat(Model),
	/// A model of a type this version does not know, kept as read.
	Unknown(JsonObject),
}

/// A borrowed model of any type, for callbacks that see the model of a request.
#[derive(Debug, Clone, Copy)]
pub enum ModelRef<'a> {
	/// A chat model.
	Chat(&'a Model),
	/// An image model.
	Image(&'a ImageModel),
	/// A classifier.
	Classifier(&'a ClassifierModel),
}

impl AnyModel {
	/// The model's type (`getModelType`); `None` for an unknown type.
	pub fn model_type(&self) -> Option<ModelType> {
		match self {
			Self::Chat(_) => Some(ModelType::Chat),
			Self::Image(_) => Some(ModelType::Image),
			Self::Classifier(_) => Some(ModelType::Classifier),
			Self::Unknown(_) => None,
		}
	}

	/// The model id.
	pub fn id(&self) -> &str {
		match self {
			Self::Chat(model) => &model.id,
			Self::Image(model) => &model.id,
			Self::Classifier(model) => &model.id,
			Self::Unknown(object) => object.get("id").and_then(JsonValue::as_str).unwrap_or_default(),
		}
	}

	/// The provider id.
	pub fn provider(&self) -> &str {
		match self {
			Self::Chat(model) => &model.provider,
			Self::Image(model) => &model.provider,
			Self::Classifier(model) => &model.provider,
			Self::Unknown(object) => object.get("provider").and_then(JsonValue::as_str).unwrap_or_default(),
		}
	}

	/// The chat model, when this is one.
	pub fn as_chat(&self) -> Option<&Model> {
		match self {
			Self::Chat(model) => Some(model),
			_ => None,
		}
	}

	/// The image model, when this is one.
	pub fn as_image(&self) -> Option<&ImageModel> {
		match self {
			Self::Image(model) => Some(model),
			_ => None,
		}
	}

	/// The classifier, when this is one.
	pub fn as_classifier(&self) -> Option<&ClassifierModel> {
		match self {
			Self::Classifier(model) => Some(model),
			_ => None,
		}
	}

	/// Whether the model has `model_type` (`isModelType`).
	pub fn is_type(&self, model_type: ModelType) -> bool {
		self.model_type() == Some(model_type)
	}

	/// The model's prices; `None` for an unknown type.
	pub fn cost(&self) -> Option<&ModelCost> {
		match self {
			Self::Chat(model) => Some(&model.cost),
			Self::Image(model) => Some(&model.cost),
			Self::Classifier(model) => Some(&model.cost),
			Self::Unknown(_) => None,
		}
	}

	/// A borrowed view, `None` for an unknown type.
	pub fn as_ref(&self) -> Option<ModelRef<'_>> {
		match self {
			Self::Chat(model) => Some(ModelRef::Chat(model)),
			Self::Image(model) => Some(ModelRef::Image(model)),
			Self::Classifier(model) => Some(ModelRef::Classifier(model)),
			Self::Unknown(_) => None,
		}
	}
}

impl From<Model> for AnyModel {
	fn from(model: Model) -> Self {
		Self::Chat(model)
	}
}

impl From<ImageModel> for AnyModel {
	fn from(model: ImageModel) -> Self {
		Self::Image(model)
	}
}

impl From<ClassifierModel> for AnyModel {
	fn from(model: ClassifierModel) -> Self {
		Self::Classifier(model)
	}
}

/// Whether two models have the same type, id and provider (`modelsAreEqual`).
pub fn models_are_equal(a: Option<&AnyModel>, b: Option<&AnyModel>) -> bool {
	match (a, b) {
		(Some(a), Some(b)) => a.model_type() == b.model_type() && a.id() == b.id() && a.provider() == b.provider(),
		_ => false,
	}
}

/// Whether a model is a chat model of `api` (`hasApi`).
pub fn has_api(model: &AnyModel, api: &str) -> bool {
	model.as_chat().is_some_and(|model| model.api == api)
}

/// Fails with code `provider` unless the model is a chat model (`assertChatModel`).
pub fn assert_chat_model(model: &AnyModel) -> Result<&Model, ModelsError> {
	model.as_chat().ok_or_else(|| not_a(model, "chat"))
}

/// Fails unless the model is an image model (`assertImageModel`).
pub fn assert_image_model(model: &AnyModel) -> Result<&ImageModel, ModelsError> {
	model.as_image().ok_or_else(|| not_a(model, "image"))
}

/// Fails unless the model is a classifier (`assertClassifierModel`).
pub fn assert_classifier_model(model: &AnyModel) -> Result<&ClassifierModel, ModelsError> {
	model.as_classifier().ok_or_else(|| not_a(model, "classifier"))
}

fn not_a(model: &AnyModel, kind: &str) -> ModelsError {
	let article = if kind == "image" { "an" } else { "a" };
	ModelsError::new(
		ModelsErrorCode::Provider,
		format!(
			"Model {}/{} is not {article} {kind} model",
			model.provider(),
			model.id()
		),
	)
}

/// Rejects classifier images for models without image input (`assertClassifierInputSupported`).
pub fn assert_classifier_input_supported(
	model: &ClassifierModel,
	context: &ClassifierContext,
) -> Result<(), ModelsError> {
	let has_images = context.images.as_ref().is_some_and(|images| !images.is_empty());
	if has_images && !model.input.contains(&ModelInputModality::Image) {
		return Err(ModelsError::new(
			ModelsErrorCode::Provider,
			format!("Model {}/{} does not accept image input", model.provider, model.id),
		));
	}
	Ok(())
}

/// How a failed operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
	/// It failed.
	Error,
	/// It was aborted.
	Aborted,
}

/// The result of a failed image request (`imageErrorResult`).
pub fn image_error_result(model: &ImageModel, message: &str, failure: Failure) -> AssistantImages {
	AssistantImages {
		api: model.api.clone(),
		provider: model.provider.clone(),
		model: model.id.clone(),
		output: Vec::new(),
		response_id: None,
		usage: None,
		stop_reason: match failure {
			Failure::Error => ImagesStopReason::Error,
			Failure::Aborted => ImagesStopReason::Aborted,
		},
		error_message: Some(message.to_owned()),
		timestamp: now_ms(),
		extra: JsonObject::new(),
	}
}

/// The result of a failed classification (`classifierErrorResult`).
pub fn classifier_error_result(model: &ClassifierModel, message: &str, failure: Failure) -> ClassifierResult {
	ClassifierResult {
		api: model.api.clone(),
		provider: model.provider.clone(),
		model: model.id.clone(),
		answers: Record::new(),
		usage: None,
		stop_reason: match failure {
			Failure::Error => ClassifierStopReason::Error,
			Failure::Aborted => ClassifierStopReason::Aborted,
		},
		error_message: Some(message.to_owned()),
		timestamp: now_ms(),
		extra: JsonObject::new(),
	}
}

/// Prices `usage` with the model's rates and stores the cost in it (`calculateCost`): the highest tier whose
/// threshold the request's input exceeds prices the whole request, and 1h cache writes cost twice the input rate.
pub fn calculate_cost(cost: &ModelCost, usage: &mut Usage) -> UsageCost {
	#[allow(
		clippy::cast_precision_loss,
		reason = "token counts are far below 2^52, as JS numbers"
	)]
	let tokens = |count: i64| count as f64;
	let input_tokens = tokens(usage.input + usage.cache_read + usage.cache_write);
	let (mut input, mut output, mut cache_read, mut cache_write) =
		(cost.input, cost.output, cost.cache_read, cost.cache_write);
	let mut matched = -1.0;
	for tier in cost.tiers.iter().flatten() {
		if input_tokens > tier.input_tokens_above && tier.input_tokens_above > matched {
			(input, output, cache_read, cache_write) = (tier.input, tier.output, tier.cache_read, tier.cache_write);
			matched = tier.input_tokens_above;
		}
	}
	let long_write = tokens(usage.cache_write_1h.unwrap_or(0));
	let short_write = tokens(usage.cache_write) - long_write;
	usage.cost.input = (input / 1_000_000.0) * tokens(usage.input);
	usage.cost.output = (output / 1_000_000.0) * tokens(usage.output);
	usage.cost.cache_read = (cache_read / 1_000_000.0) * tokens(usage.cache_read);
	usage.cost.cache_write = (cache_write * short_write + input * 2.0 * long_write) / 1_000_000.0;
	usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
	usage.cost.total_from_provider = Some(false);
	usage.cost.clone()
}

const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
	ModelThinkingLevel::Off,
	ModelThinkingLevel::Minimal,
	ModelThinkingLevel::Low,
	ModelThinkingLevel::Medium,
	ModelThinkingLevel::High,
	ModelThinkingLevel::Xhigh,
	ModelThinkingLevel::Max,
];

/// How a model's thinking level map treats a level.
enum Mapped {
	/// The map has no entry: the provider default.
	Absent,
	/// `null`: unsupported.
	Unsupported,
	/// A provider value.
	Value,
}

fn mapped_level(model: &Model, level: ModelThinkingLevel) -> Mapped {
	let Some(map) = model.thinking_level_map.as_ref() else {
		return Mapped::Absent;
	};
	let value = match level {
		ModelThinkingLevel::Off => map.off.as_ref(),
		ModelThinkingLevel::Minimal => map.minimal.as_ref(),
		ModelThinkingLevel::Low => map.low.as_ref(),
		ModelThinkingLevel::Medium => map.medium.as_ref(),
		ModelThinkingLevel::High => map.high.as_ref(),
		ModelThinkingLevel::Xhigh => map.xhigh.as_ref(),
		ModelThinkingLevel::Max => map.max.as_ref(),
	};
	match value {
		None => Mapped::Absent,
		Some(ThinkingLevelValue::Unsupported(())) => Mapped::Unsupported,
		Some(ThinkingLevelValue::Value(_)) => Mapped::Value,
	}
}

/// The thinking levels a model supports (`getSupportedThinkingLevels`): `off` alone without reasoning; levels mapped
/// to `null` are unsupported; `xhigh` and `max` only when mapped.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
	if !model.reasoning {
		return vec![ModelThinkingLevel::Off];
	}
	EXTENDED_THINKING_LEVELS
		.into_iter()
		.filter(|level| match mapped_level(model, *level) {
			Mapped::Unsupported => false,
			Mapped::Absent => !matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max),
			Mapped::Value => true,
		})
		.collect()
}

/// The nearest supported level: the requested one, else the next higher, else the next lower (`clampThinkingLevel`).
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
	let available = get_supported_thinking_levels(model);
	if available.contains(&level) {
		return level;
	}
	let requested = EXTENDED_THINKING_LEVELS
		.iter()
		.position(|candidate| *candidate == level)
		.unwrap_or(0);
	let higher = EXTENDED_THINKING_LEVELS[requested..].iter();
	let lower = EXTENDED_THINKING_LEVELS[..requested].iter().rev();
	higher
		.chain(lower)
		.find(|candidate| available.contains(candidate))
		.copied()
		.unwrap_or_else(|| available.first().copied().unwrap_or(ModelThinkingLevel::Off))
}
