//! Catalog models (`model.rs`): chat, image and classifier models with their metadata.

mod common;

use common::round_trip;
use pi_types::{ClassifierModel, ImageModel, Model, ModelThinkingLevel, ThinkingLevel, ThinkingLevelValue};

#[test]
fn chat_models_round_trip_with_every_optional_field() {
	let model: Model = round_trip(
		r#"{"id":"m","name":"M","api":"openai-completions","provider":"local","baseUrl":"http://localhost:8080/v1","type":"chat","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"tiers":[{"inputTokensAbove":200000,"input":6,"output":22.5,"cacheRead":0.6,"cacheWrite":7.5}]},"contextWindow":131072,"maxTokens":8192,"headers":{"X-Key":"v"},"compat":{"supportsStore":false},"thinkingLevelMap":{"off":null,"high":"high","max":null},"promptCache":{"short":300,"long":3600},"inputLimits":{"maxRequestBytes":32000000,"images":{"resize":{"maxWidth":2000,"maxHeight":2000,"maxBytes":5000000,"jpegQuality":85},"maxPerMessage":20,"maxPerRequest":100}},"samplingParams":{"top_p":0.95},"samplingParamsByThinkingLevel":{"off":{"temperature":0.7}}}"#,
	);
	let map = model.thinking_level_map.unwrap();
	assert_eq!(map.off, Some(ThinkingLevelValue::Unsupported(())));
	assert_eq!(map.high, Some(ThinkingLevelValue::Value("high".into())));
	assert_eq!(map.low, None);
}

#[test]
fn image_and_classifier_models_round_trip() {
	round_trip::<ImageModel>(
		r#"{"id":"i","name":"I","api":"openrouter-images","provider":"openrouter","baseUrl":"https://openrouter.ai/api/v1","type":"image","input":["text","image"],"output":["image","text"],"cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}"#,
	);
	round_trip::<ClassifierModel>(
		r#"{"id":"c","name":"C","api":"typesafe-system-one","provider":"typesafe","baseUrl":"https://x","type":"classifier","input":["text"],"cost":{"input":0.1,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":8192,"compat":{"k":1}}"#,
	);
	assert!(common::read::<ImageModel>(r#"{"id":"i","name":"I","api":"a","provider":"p","baseUrl":"b","type":"chat","input":[],"output":[],"cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0}}"#).is_err());
}

#[test]
fn thinking_levels_use_the_ts_names() {
	assert_eq!(round_trip::<ThinkingLevel>(r#""xhigh""#), ThinkingLevel::Xhigh);
	assert_eq!(round_trip::<ModelThinkingLevel>(r#""off""#), ModelThinkingLevel::Off);
	assert!(common::read::<ThinkingLevel>(r#""off""#).is_err());
}
