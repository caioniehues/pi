//! The embedded catalog (`catalog.rs`): ports `supports-xhigh.test.ts`, `model-types.test.ts` (catalog getters),
//! `model-catalog-types.test.ts` (runtime parts) and the catalog cases of `max-thinking.test.ts`. They need hydrated
//! model data and are ignored without it.
//!
//! Not ported, by decision: `model-cost-tiers.test.ts` and `reasoning-options.test.ts` test the TS catalog generator
//! (`catalog/*.ts`), which stays TS; `message-types.test.ts` checks TS types only.

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

use pi_ai::catalog::{self, CLASSIFIER_MODELS, IMAGE_MODELS, MODELS};
use pi_ai::model_operations::get_supported_thinking_levels;
use pi_types::{Model, ModelCompat, ModelThinkingLevel as L, ThinkingLevelValue};

fn get(provider: &str, id: &str) -> &'static Model {
	MODELS
		.model(provider, id)
		.unwrap_or_else(|| panic!("{provider}/{id} in the catalog"))
}

fn levels(provider: &str, id: &str) -> Vec<L> {
	get_supported_thinking_levels(get(provider, id))
}

// Rust-only.
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn every_catalog_entry_reads_as_its_type() {
	assert!(catalog::is_hydrated());
	for provider in catalog::providers() {
		let raw: pi_json::JsonValue = pi_json::parse(catalog::provider_data(provider).unwrap()).unwrap();
		let count = |kind: &str| {
			raw.as_object()
				.unwrap()
				.values()
				.flat_map(|group| group.as_object().unwrap().values())
				.filter(|model| model["type"] == kind)
				.count()
		};
		assert_eq!(MODELS.get(provider).len(), count("chat"), "{provider} chat");
		assert_eq!(IMAGE_MODELS.get(provider).len(), count("image"), "{provider} image");
		assert_eq!(
			CLASSIFIER_MODELS.get(provider).len(),
			count("classifier"),
			"{provider} classifier"
		);
		for model in MODELS.get(provider) {
			assert_eq!(model.provider, provider);
		}
	}
}

// TS: supports-xhigh.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn supports_xhigh_anthropic() {
	assert!(levels("anthropic", "claude-opus-4-6").contains(&L::Max));
	assert!(!levels("anthropic", "claude-opus-4-6").contains(&L::Xhigh));
	for id in ["claude-opus-4-8", "claude-opus-5", "claude-sonnet-5"] {
		assert!(
			levels("anthropic", id).contains(&L::Xhigh) && levels("anthropic", id).contains(&L::Max),
			"{id}"
		);
	}
	assert!(levels("anthropic", "claude-sonnet-4-6").contains(&L::Max));
	assert!(!levels("anthropic", "claude-sonnet-4-6").contains(&L::Xhigh));
	let fable = levels("anthropic", "claude-fable-5");
	assert!(fable.contains(&L::Xhigh) && fable.contains(&L::Max) && !fable.contains(&L::Off));
	let sonnet45 = levels("anthropic", "claude-sonnet-4-5");
	assert!(!sonnet45.contains(&L::Xhigh) && !sonnet45.contains(&L::Max));
}

// TS: supports-xhigh.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn supports_xhigh_claude_5_5_metadata() {
	for (id, cost, temperature) in [
		("claude-opus-5-5", [4.0, 20.0, 0.2, 5.0], None),
		("claude-sonnet-5-5", [2.0, 10.0, 0.1, 2.5], Some(false)),
	] {
		let model = get("anthropic", id);
		assert_eq!(
			[
				model.cost.input,
				model.cost.output,
				model.cost.cache_read,
				model.cost.cache_write
			],
			cost
		);
		assert_eq!((model.context_window, model.max_tokens), (1_000_000, 128_000));
		let compat = model.compat.as_ref().and_then(ModelCompat::anthropic_messages).unwrap();
		assert_eq!(compat.force_adaptive_thinking, Some(true));
		assert_eq!(compat.supports_mid_convo_effort, Some(true));
		assert_eq!(compat.supports_mid_convo_system_messages, Some(true));
		assert_eq!(compat.supports_mid_convo_tool_changes, Some(true));
		if temperature.is_some() {
			assert_eq!(compat.supports_temperature, temperature);
		}
		assert_eq!(
			get_supported_thinking_levels(model),
			[L::Low, L::Medium, L::High, L::Xhigh, L::Max]
		);
	}
}

// TS: supports-xhigh.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn supports_xhigh_openai_and_codex() {
	for id in [
		"gpt-5.6-sol",
		"gpt-5.6-terra",
		"gpt-5.6-luna",
		"gpt-6-astra",
		"gpt-6-sol",
		"gpt-6-luna",
		"gpt-6.1-sol",
		"gpt-6.1-terra",
	] {
		assert!(levels("openai-codex", id).contains(&L::Xhigh), "{id}");
	}
	for id in [
		"gpt-5.6-sol",
		"gpt-5.6-terra",
		"gpt-5.6-luna",
		"gpt-6-sol",
		"gpt-6-luna",
		"gpt-6.1-terra",
	] {
		assert_eq!(
			levels("openai", id),
			[L::Off, L::Low, L::Medium, L::High, L::Xhigh, L::Max],
			"{id}"
		);
	}
	for (provider, expected) in [
		("openai", vec![L::Low, L::Medium, L::High, L::Xhigh, L::Max]),
		("azure", vec![L::Low, L::Medium, L::High, L::Xhigh, L::Max]),
		(
			"openai-codex",
			vec![L::Minimal, L::Low, L::Medium, L::High, L::Xhigh, L::Max],
		),
	] {
		assert_eq!(levels(provider, "gpt-6.1-sol"), expected, "{provider}");
		assert_eq!(
			get(provider, "gpt-6.1-sol").thinking_level_map.as_ref().unwrap().off,
			Some(ThinkingLevelValue::Unsupported(()))
		);
	}
	for provider in ["openai", "azure", "openai-codex"] {
		let map = get(provider, "gpt-6.1-terra").thinking_level_map.clone().unwrap();
		assert_eq!(map.off, Some(ThinkingLevelValue::Value("none".into())));
		let minimal = if provider == "openai-codex" {
			ThinkingLevelValue::Value("low".into())
		} else {
			ThinkingLevelValue::Unsupported(())
		};
		assert_eq!(map.minimal, Some(minimal), "{provider}");
	}
	for id in ["gpt-6-astra", "gpt-6-luna", "gpt-6-sol", "gpt-6.1-sol", "gpt-6.1-terra"] {
		assert_eq!(get("azure", id).context_window, 1_050_000, "{id}");
	}
	assert_eq!(levels("openai", "gpt-5.5-pro"), [L::Medium, L::High, L::Xhigh]);
	assert_eq!(
		levels("openrouter", "openai/gpt-5.5-pro"),
		[L::Medium, L::High, L::Xhigh]
	);
}

// TS: supports-xhigh.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn supports_xhigh_official_openai_metadata() {
	for (id, cost, codex_window) in [
		("gpt-6-sol", [2.0, 10.0, 0.2, 2.5], 272_000),
		("gpt-6-luna", [0.1, 0.5, 0.01, 0.125], 272_000),
		("gpt-6.1-sol", [2.0, 10.0, 0.1, 2.5], 500_000),
		("gpt-6.1-terra", [1.0, 5.0, 0.05, 1.25], 500_000),
	] {
		for provider in ["openai", "openai-codex"] {
			let model = get(provider, id);
			assert_eq!(
				[
					model.cost.input,
					model.cost.output,
					model.cost.cache_read,
					model.cost.cache_write
				],
				cost,
				"{provider}/{id}"
			);
			let tier = &model.cost.tiers.as_ref().unwrap()[0];
			assert_eq!(tier.input_tokens_above, 272_000.0);
			assert_eq!(
				[tier.input, tier.output, tier.cache_read, tier.cache_write],
				[cost[0] * 2.0, cost[1] * 1.5, cost[2] * 2.0, cost[3] * 2.0]
			);
			assert_eq!(
				model.context_window,
				if provider == "openai-codex" {
					codex_window
				} else {
					272_000
				}
			);
			assert_eq!(model.max_tokens, 128_000);
			let compat = model.compat.as_ref().and_then(ModelCompat::openai_responses).unwrap();
			assert_eq!(
				[
					compat.supports_additional_tools,
					compat.supports_mid_convo_system_messages,
					compat.supports_openai_grammar_tools,
					compat.supports_tool_search
				],
				[Some(true); 4]
			);
		}
	}
}

// TS: supports-xhigh.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn supports_xhigh_other_providers() {
	assert_eq!(levels("deepseek", "deepseek-flash"), [L::Off, L::Low, L::High, L::Max]);
	assert_eq!(
		levels("openrouter", "deepseek/deepseek-v4.1-flash"),
		[L::Off, L::Low, L::High, L::Max]
	);
	assert_eq!(levels("opencode-go", "deepseek-v4.1-flash"), [L::Low, L::High, L::Max]);
	for provider in ["moonshotai", "moonshotai-cn"] {
		assert_eq!(
			levels(provider, "kimi-k2.7-code"),
			[L::Minimal, L::Low, L::Medium, L::High]
		);
		assert_eq!(levels(provider, "kimi-k3"), [L::Low, L::High, L::Max]);
	}
	assert_eq!(levels("kimi-coding", "k3"), [L::Low, L::High, L::Max]);
	assert!(!levels("kimi-coding", "kimi-for-coding-highspeed").contains(&L::Off));
	assert_eq!(levels("opencode", "grok-build-0.1"), [L::High]);
	assert_eq!(
		levels("openrouter", "deepseek/deepseek-v4-flash"),
		[L::Off, L::High, L::Xhigh]
	);
	let opus = levels("openrouter", "anthropic/claude-opus-4.6");
	assert!(opus.contains(&L::Max) && !opus.contains(&L::Xhigh));
	for id in ["global.anthropic.claude-opus-5", "global.anthropic.claude-fable-5"] {
		let bedrock = levels("amazon-bedrock", id);
		assert!(bedrock.contains(&L::Xhigh) && bedrock.contains(&L::Max), "{id}");
	}
	assert!(!levels("amazon-bedrock", "global.anthropic.claude-fable-5").contains(&L::Off));
	assert_eq!(levels("xai", "grok-4.6"), [L::Low, L::Medium, L::High, L::Xhigh]);
}

// TS: max-thinking.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn max_thinking_exposes_xhigh_and_max_for_openai_codex() {
	for id in [
		"gpt-5.6-luna",
		"gpt-5.6-sol",
		"gpt-5.6-terra",
		"gpt-6-luna",
		"gpt-6-sol",
	] {
		let map = get("openai-codex", id).thinking_level_map.clone().unwrap();
		assert_eq!(map.xhigh, Some(ThinkingLevelValue::Value("xhigh".into())));
		assert_eq!(map.max, Some(ThinkingLevelValue::Value("max".into())));
		assert_eq!(
			levels("openai-codex", id),
			[L::Off, L::Minimal, L::Low, L::Medium, L::High, L::Xhigh, L::Max]
		);
	}
}

// TS: model-catalog-types.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn model_catalog_types_derive_api_id_and_provider() {
	for id in ["grok-4.5", "grok-4.6", "grok-4.7", "grok-4.3"] {
		let model = get("xai", id);
		assert_eq!(
			(model.api.as_str(), model.id.as_str(), model.provider.as_str()),
			("openai-responses", id, "xai")
		);
	}
	assert_eq!(get("github-copilot", "grok-4.5").api, "openai-responses");
	// Regression test for https://github.com/earendil-works/pi/issues/9209
	let gpt: Vec<&Model> = MODELS
		.get("github-copilot")
		.iter()
		.filter(|model| model.id.starts_with("gpt-"))
		.collect();
	assert!(!gpt.is_empty());
	assert!(gpt.iter().all(|model| model.api == "openai-responses"));
	for id in ["gpt-6-sol", "gpt-6-luna"] {
		let model = get("github-copilot", id);
		assert_eq!((model.context_window, model.max_tokens), (1_000_000, 128_000));
		let map = model.thinking_level_map.clone().unwrap();
		assert_eq!(
			(map.off, map.max),
			(
				Some(ThinkingLevelValue::Value("none".into())),
				Some(ThinkingLevelValue::Value("max".into()))
			)
		);
	}
}

// TS: model-types.test.ts
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn model_types_built_in_catalog_getters() {
	assert_eq!(get("openai", "gpt-4o-mini").id, "gpt-4o-mini");
	assert_eq!(get("openai", "gpt-4o").id, "gpt-4o");
}
