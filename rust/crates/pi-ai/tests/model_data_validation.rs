//! Model data checks (`model_data.rs`, run by build.rs): ports the "generated model data validation" cases of
//! `model-data-validation.test.ts`. The "published model catalog hydration" cases test the TS hydration script,
//! which stays TS; "rejects missing provider shards imported by the aggregator" checks the TS aggregator file, which
//! the port reads its provider list from the manifest instead of.

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

use pi_ai::model_data::{
	MODEL_DATA_MANIFEST_FILE, MODEL_DATA_SCHEMA_VERSION, ModelDataStructure, assert_exact_model_ids,
	create_model_data_manifest, read_model_data_structure, validate_model_data_directory,
};
use pi_json::JsonValue;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const GENERATED_AT: &str = "2026-07-23T10:00:00.000Z";

struct Fixture {
	dir: PathBuf,
	structure: ModelDataStructure,
	values: JsonValue,
}

impl Drop for Fixture {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.dir);
	}
}

fn fixture(name: &str) -> Fixture {
	let dir = std::env::temp_dir().join(format!("pi-model-data-{name}-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).unwrap();
	let structure = structure(&[("chat:model-a", "openai-completions")]);
	let values = json!({ "chat:model-a": {
		"type": "chat", "id": "model-a", "name": "Model A", "api": "openai-completions", "provider": "test-provider",
		"baseUrl": "https://example.test/v1", "reasoning": false, "input": ["text"],
		"cost": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000, "maxTokens": 100
	} });
	write(
		&dir,
		&structure,
		&values,
		MODEL_DATA_SCHEMA_VERSION,
		"openai-completions",
	);
	Fixture { dir, structure, values }
}

fn structure(models: &[(&str, &str)]) -> ModelDataStructure {
	let models: BTreeMap<String, String> = models
		.iter()
		.map(|(key, api)| ((*key).to_owned(), (*api).to_owned()))
		.collect();
	[("test-provider".to_owned(), models)].into_iter().collect()
}

fn write(dir: &Path, structure: &ModelDataStructure, values: &JsonValue, schema_version: u64, api_group: &str) {
	let content = format!("{}\n", pi_json::to_js_string(&json!({ api_group: values })));
	write_content(dir, structure, &content, schema_version);
}

fn write_content(dir: &Path, structure: &ModelDataStructure, content: &str, schema_version: u64) {
	std::fs::write(dir.join("test-provider.json"), content).unwrap();
	let files: BTreeMap<String, String> = [("test-provider.json".to_owned(), content.to_owned())]
		.into_iter()
		.collect();
	let mut manifest = create_model_data_manifest(structure, &files, GENERATED_AT);
	manifest["schemaVersion"] = json!(schema_version);
	std::fs::write(
		dir.join(MODEL_DATA_MANIFEST_FILE),
		format!("{}\n", pi_json::to_js_string(&manifest)),
	)
	.unwrap();
}

fn error(fixture: &Fixture) -> String {
	validate_model_data_directory(&fixture.structure, &fixture.dir)
		.unwrap_err()
		.0
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_a_missing_upstream_model_from_an_exact_generated_allowlist() {
	assert_eq!(
		assert_exact_model_ids("qwen-token-plan-individual", ["model-a", "model-b"], ["model-a"])
			.unwrap_err()
			.0,
		"qwen-token-plan-individual model IDs do not match (missing: model-b)"
	);
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_an_unexpected_model_from_an_exact_generated_allowlist() {
	assert_eq!(
		assert_exact_model_ids("test-provider", ["model-a"], ["model-a", "model-b"])
			.unwrap_err()
			.0,
		"test-provider model IDs do not match (extra: model-b)"
	);
}

// TS: model-data-validation.test.ts
#[test]
fn reads_and_validates_api_grouped_model_data() {
	let fixture = fixture("reads");
	assert_eq!(read_model_data_structure(&fixture.dir).unwrap(), fixture.structure);
	assert!(validate_model_data_directory(&fixture.structure, &fixture.dir).is_ok());
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_a_missing_model_data_directory() {
	let fixture = fixture("missing");
	std::fs::remove_dir_all(&fixture.dir).unwrap();
	assert!(error(&fixture).contains("does not exist"));
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_a_wrong_model_field() {
	for (field, value, expected) in [
		("id", "wrong-id", "has id"),
		("provider", "wrong-provider", "has provider"),
		("api", "anthropic-messages", "has api"),
	] {
		let mut fixture = fixture(field);
		fixture.values["chat:model-a"][field] = json!(value);
		write(
			&fixture.dir,
			&fixture.structure,
			&fixture.values,
			MODEL_DATA_SCHEMA_VERSION,
			"openai-completions",
		);
		assert!(error(&fixture).contains(expected), "{field}");
	}
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_a_model_without_a_known_type() {
	let mut fixture = fixture("untyped");
	fixture.values["chat:model-a"].as_object_mut().unwrap().remove("type");
	write(
		&fixture.dir,
		&fixture.structure,
		&fixture.values,
		MODEL_DATA_SCHEMA_VERSION,
		"openai-completions",
	);
	assert!(error(&fixture).contains("expected \"chat\", \"image\", or \"classifier\""));
}

// TS: model-data-validation.test.ts
#[test]
fn validates_image_models_with_output_modalities_and_without_chat_limits() {
	let mut fixture = fixture("image");
	fixture.structure = structure(&[("image:image-a", "test-images")]);
	let mut image = json!({ "type": "image", "id": "image-a", "name": "Image A", "api": "test-images", "provider": "test-provider", "baseUrl": "https://example.test/v1", "input": ["text"], "output": ["image", "text"], "cost": { "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0 } });
	let validate = |image: &JsonValue, fixture: &Fixture| {
		write(
			&fixture.dir,
			&fixture.structure,
			&json!({ "image:image-a": image }),
			MODEL_DATA_SCHEMA_VERSION,
			"test-images",
		);
		validate_model_data_directory(&fixture.structure, &fixture.dir)
	};
	assert!(validate(&image, &fixture).is_ok());
	image.as_object_mut().unwrap().remove("output");
	assert!(
		validate(&image, &fixture)
			.unwrap_err()
			.0
			.contains("invalid output modalities")
	);
	image["output"] = json!(["text"]);
	assert!(
		validate(&image, &fixture)
			.unwrap_err()
			.0
			.contains("invalid output modalities")
	);
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_output_modalities_on_chat_models() {
	let mut fixture = fixture("chat-output");
	fixture.values["chat:model-a"]["output"] = json!(["text"]);
	write(
		&fixture.dir,
		&fixture.structure,
		&fixture.values,
		MODEL_DATA_SCHEMA_VERSION,
		"openai-completions",
	);
	assert!(error(&fixture).contains("unsupported output modalities"));
}

// TS: model-data-validation.test.ts
#[test]
fn validates_classifier_models_without_chat_output_limits() {
	let mut fixture = fixture("classifier");
	fixture.structure = structure(&[("classifier:classifier-a", "test-classifier")]);
	let classifier = json!({ "type": "classifier", "id": "classifier-a", "name": "Classifier A", "api": "test-classifier", "provider": "test-provider", "baseUrl": "https://example.test/v1", "input": ["text"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 }, "contextWindow": 1000 });
	write(
		&fixture.dir,
		&fixture.structure,
		&json!({ "classifier:classifier-a": classifier }),
		MODEL_DATA_SCHEMA_VERSION,
		"test-classifier",
	);
	assert!(validate_model_data_directory(&fixture.structure, &fixture.dir).is_ok());
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_a_model_in_the_wrong_api_group() {
	let fixture = fixture("group");
	write(
		&fixture.dir,
		&fixture.structure,
		&fixture.values,
		MODEL_DATA_SCHEMA_VERSION,
		"anthropic-messages",
	);
	assert!(error(&fixture).contains("grouped under API"));
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_duplicate_model_ids_across_api_groups() {
	let fixture = fixture("duplicate");
	let content = format!(
		"{}\n",
		pi_json::to_js_string(&json!({ "openai-completions": fixture.values, "anthropic-messages": fixture.values }))
	);
	write_content(&fixture.dir, &fixture.structure, &content, MODEL_DATA_SCHEMA_VERSION);
	assert!(error(&fixture).contains("more than one API group"));
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_missing_model_ids_and_stale_file_hashes() {
	let fixture = fixture("stale");
	std::fs::write(fixture.dir.join("test-provider.json"), "{}\n").unwrap();
	let message = error(&fixture);
	assert!(
		message.contains("manifest hash") || message.contains("model IDs"),
		"{message}"
	);
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_incompatible_schema_and_generation_stamps() {
	let fixture = fixture("stamps");
	write(
		&fixture.dir,
		&fixture.structure,
		&fixture.values,
		MODEL_DATA_SCHEMA_VERSION + 1,
		"openai-completions",
	);
	assert!(error(&fixture).contains("model data schema"));
	let path = fixture.dir.join(MODEL_DATA_MANIFEST_FILE);
	let mut manifest = pi_json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
	manifest["structureHash"] = json!("stale");
	std::fs::write(&path, pi_json::to_js_string(&manifest)).unwrap();
	assert!(error(&fixture).contains("generation stamp"));
}

// TS: model-data-validation.test.ts
#[test]
fn rejects_an_invalid_generation_timestamp() {
	let fixture = fixture("timestamp");
	let path = fixture.dir.join(MODEL_DATA_MANIFEST_FILE);
	let mut manifest = pi_json::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
	manifest["generatedAt"] = json!("invalid");
	std::fs::write(&path, pi_json::to_js_string(&manifest)).unwrap();
	assert!(error(&fixture).contains("generation timestamp"));
}

// Rust-only.
#[test]
#[cfg_attr(not(model_data), ignore = "run `npm run hydrate:model-data` in packages/ai")]
fn the_hydrated_data_directory_validates() {
	let dir = std::env::var_os("PI_MODEL_DATA_DIR").map_or_else(
		|| {
			PathBuf::from(concat!(
				env!("CARGO_MANIFEST_DIR"),
				"/../../../packages/ai/src/providers/data"
			))
		},
		PathBuf::from,
	);
	let structure = read_model_data_structure(&dir).unwrap();
	assert!(validate_model_data_directory(&structure, &dir).is_ok());
}
