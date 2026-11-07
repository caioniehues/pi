//! Checks of the generated model data directory (ports `packages/ai/scripts/model-data.ts`): the manifest's schema
//! version, generation stamp and file hashes, and every model's identity and required metadata. `build.rs` runs them
//! before embedding the catalog (it includes this file), and they are public for tests and tools.

use pi_json::{JsonObject, JsonValue};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// The manifest schema version this crate reads.
pub const MODEL_DATA_SCHEMA_VERSION: u64 = 6;

/// The manifest's file name inside the data directory.
pub const MODEL_DATA_MANIFEST_FILE: &str = ".manifest.json";

/// Provider id to `type:id` model keys to the API group each sits in.
pub type ModelDataStructure = BTreeMap<String, BTreeMap<String, String>>;

/// Why model data is not usable; the message is the TS error's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ModelDataError(pub String);

/// Fails unless `expected` and `actual` hold the same ids, naming the missing and extra ones.
pub fn assert_exact_model_ids<'a>(
	label: &str,
	expected: impl IntoIterator<Item = &'a str>,
	actual: impl IntoIterator<Item = &'a str>,
) -> Result<(), ModelDataError> {
	let expected: Vec<&str> = expected.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
	let actual: Vec<&str> = actual.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
	if expected == actual {
		return Ok(());
	}
	Err(ModelDataError(format!(
		"{label} model IDs do not match ({})",
		describe_set_difference(&expected, &actual)
	)))
}

/// `missing: a, b; extra: c`, the parts that are not empty.
fn describe_set_difference(expected: &[&str], actual: &[&str]) -> String {
	let missing: Vec<&str> = expected
		.iter()
		.copied()
		.filter(|value| !actual.contains(value))
		.collect();
	let extra: Vec<&str> = actual
		.iter()
		.copied()
		.filter(|value| !expected.contains(value))
		.collect();
	let mut parts = Vec::new();
	if !missing.is_empty() {
		parts.push(format!("missing: {}", missing.join(", ")));
	}
	if !extra.is_empty() {
		parts.push(format!("extra: {}", extra.join(", ")));
	}
	parts.join("; ")
}

/// The SHA-256 of `structure` as sorted JSON: the manifest's `structureHash`.
pub fn model_data_structure_hash(structure: &ModelDataStructure) -> String {
	// TS parity: `sortedRecord` then `JSON.stringify`; a BTreeMap is already sorted.
	let normalized: JsonObject = structure
		.iter()
		.map(|(provider, models)| {
			let models: JsonObject = models
				.iter()
				.map(|(key, api)| (key.clone(), JsonValue::from(api.as_str())))
				.collect();
			(provider.clone(), JsonValue::Object(models))
		})
		.collect();
	sha256(&pi_json::to_js_string(&JsonValue::Object(normalized)))
}

/// The manifest TS writes for `structure` and the files' contents.
pub fn create_model_data_manifest(
	structure: &ModelDataStructure,
	file_contents: &BTreeMap<String, String>,
	generated_at: &str,
) -> JsonValue {
	let files: JsonObject = file_contents
		.iter()
		.map(|(file, content)| (file.clone(), JsonValue::from(sha256(content))))
		.collect();
	let mut manifest = JsonObject::new();
	manifest.insert("schemaVersion".into(), JsonValue::from(MODEL_DATA_SCHEMA_VERSION));
	manifest.insert("generatedAt".into(), JsonValue::from(generated_at));
	manifest.insert(
		"structureHash".into(),
		JsonValue::from(model_data_structure_hash(structure)),
	);
	manifest.insert("files".into(), JsonValue::Object(files));
	JsonValue::Object(manifest)
}

/// Lowercase hex SHA-256 of UTF-8 text.
fn sha256(text: &str) -> String {
	let digest = Sha256::digest(text.as_bytes());
	let mut hex = String::with_capacity(64);
	for byte in digest {
		// invariant: writing to a String cannot fail.
		let _ = write!(hex, "{byte:02x}");
	}
	hex
}

/// The structure of the data directory: the providers its manifest lists, each with the models of its file.
pub fn read_model_data_structure(data_dir: &Path) -> Result<ModelDataStructure, ModelDataError> {
	let mut errors = Vec::new();
	let manifest = read_json_object(
		&data_dir.join(MODEL_DATA_MANIFEST_FILE),
		"model data manifest",
		&mut errors,
	);
	let files = manifest
		.as_ref()
		.and_then(|manifest| manifest.get("files"))
		.and_then(JsonValue::as_object);
	let Some(files) = files else {
		errors.push("model data manifest has no file hashes".into());
		return Err(ModelDataError(errors.join("\n")));
	};
	let mut structure = ModelDataStructure::new();
	for file in files.keys() {
		let Some(provider) = file.strip_suffix(".json") else {
			continue;
		};
		structure.insert(
			provider.to_owned(),
			read_provider_structure(&data_dir.join(file), provider)?,
		);
	}
	Ok(structure)
}

fn read_provider_structure(path: &Path, provider: &str) -> Result<BTreeMap<String, String>, ModelDataError> {
	let mut errors = Vec::new();
	let Some(groups) = read_json_object(path, &format!("{provider}.json"), &mut errors) else {
		return Err(ModelDataError(errors.join("\n")));
	};
	let label = path.display();
	let mut models = BTreeMap::new();
	for (api, value) in &groups {
		let Some(group) = value.as_object() else {
			return Err(ModelDataError(format!(
				"{label} API group {} must be an object",
				quoted(api)
			)));
		};
		for key in group.keys() {
			if models.insert(key.clone(), api.clone()).is_some() {
				return Err(ModelDataError(format!(
					"{label} contains {key} in more than one API group"
				)));
			}
		}
	}
	if models.is_empty() {
		return Err(ModelDataError(format!("{label} contains no generated model data")));
	}
	Ok(models)
}

/// Checks the data directory against `structure`, as `validateModelDataDirectory` does; all errors at once.
pub fn validate_model_data_directory(structure: &ModelDataStructure, data_dir: &Path) -> Result<(), ModelDataError> {
	if !data_dir.is_dir() {
		return Err(ModelDataError(format!(
			"Generated model data directory does not exist: {}",
			data_dir.display()
		)));
	}
	let mut errors = Vec::new();
	let mut expected_files: Vec<String> = structure.keys().map(|provider| format!("{provider}.json")).collect();
	// File names sort differently from provider ids (`google-vertex.json` before `google.json`).
	expected_files.sort();
	let actual_files = data_files(data_dir);
	if expected_files != actual_files {
		errors.push(format!(
			"provider data files do not match the generated catalog ({})",
			describe_set_difference(&as_strs(&expected_files), &as_strs(&actual_files))
		));
	}
	let manifest = read_json_object(
		&data_dir.join(MODEL_DATA_MANIFEST_FILE),
		"model data manifest",
		&mut errors,
	);
	let manifest_files = check_manifest(manifest.as_ref(), structure, &expected_files, &mut errors);
	for (provider, expected_models) in structure {
		check_provider_file(data_dir, provider, expected_models, manifest_files, &mut errors);
	}
	if errors.is_empty() {
		Ok(())
	} else {
		Err(validation_errors(&errors))
	}
}

/// The `.json` files of the directory, sorted, without the manifest.
fn data_files(data_dir: &Path) -> Vec<String> {
	let mut files: Vec<String> = fs::read_dir(data_dir)
		.into_iter()
		.flatten()
		.flatten()
		.filter_map(|entry| entry.file_name().into_string().ok())
		.filter(|name| {
			Path::new(name).extension().is_some_and(|extension| extension == "json") && name != MODEL_DATA_MANIFEST_FILE
		})
		.collect();
	files.sort();
	files
}

fn as_strs(values: &[String]) -> Vec<&str> {
	values.iter().map(String::as_str).collect()
}

/// Checks the manifest's stamps; returns its file hashes when it has them.
fn check_manifest<'a>(
	manifest: Option<&'a JsonObject>,
	structure: &ModelDataStructure,
	expected_files: &[String],
	errors: &mut Vec<String>,
) -> Option<&'a JsonObject> {
	let schema_version = manifest.and_then(|manifest| manifest.get("schemaVersion"));
	if schema_version.and_then(JsonValue::as_u64) != Some(MODEL_DATA_SCHEMA_VERSION) {
		let found = schema_version.map_or_else(|| "undefined".to_owned(), pi_json::to_js_string);
		errors.push(format!(
			"model data schema is {found}, expected {MODEL_DATA_SCHEMA_VERSION}"
		));
	}
	let generated_at = manifest
		.and_then(|manifest| manifest.get("generatedAt"))
		.and_then(JsonValue::as_str);
	if generated_at.and_then(pi_json::parse_iso_ms).is_none() {
		errors.push("model data manifest has an invalid generation timestamp".into());
	}
	let structure_hash = manifest
		.and_then(|manifest| manifest.get("structureHash"))
		.and_then(JsonValue::as_str);
	if structure_hash != Some(model_data_structure_hash(structure).as_str()) {
		errors.push("model data generation stamp does not match the generated catalog".into());
	}
	let files = manifest
		.and_then(|manifest| manifest.get("files"))
		.and_then(JsonValue::as_object);
	match files {
		None => errors.push("model data manifest has no file hashes".into()),
		Some(files) => {
			let mut names: Vec<String> = files.keys().cloned().collect();
			names.sort();
			if names != expected_files {
				errors.push(format!(
					"manifest file hashes do not match provider data files ({})",
					describe_set_difference(&as_strs(expected_files), &as_strs(&names))
				));
			}
		}
	}
	files
}

fn check_provider_file(
	data_dir: &Path,
	provider: &str,
	expected_models: &BTreeMap<String, String>,
	manifest_files: Option<&JsonObject>,
	errors: &mut Vec<String>,
) {
	let filename = format!("{provider}.json");
	let path = data_dir.join(&filename);
	let Ok(content) = fs::read_to_string(&path) else { return };
	if manifest_files.is_some_and(|files| files.get(&filename).and_then(JsonValue::as_str) != Some(&sha256(&content))) {
		errors.push(format!("{filename} does not match its manifest hash"));
	}
	let Some(groups) = parse_json_object(&content, &filename, errors) else {
		return;
	};
	let mut actual_models = BTreeMap::new();
	for (api, value) in &groups {
		let Some(group) = value.as_object() else {
			errors.push(format!("{filename} API group {} must be an object", quoted(api)));
			continue;
		};
		for (key, model) in group {
			if actual_models.contains_key(key) {
				errors.push(format!("{provider}/{key} appears in more than one API group"));
				continue;
			}
			actual_models.insert(key.clone(), api.clone());
			let model_id = key.split_once(':').map_or(key.as_str(), |(_, id)| id);
			validate_model_value(model, provider, model_id, api, errors);
			if let Some(object) = model.as_object() {
				let identity = format!("{}:{}", js_string(object.get("type")), js_string(object.get("id")));
				if *key != identity {
					errors.push(format!("{provider}/{key} has mismatched type/id identity"));
				}
			}
		}
	}
	let expected_ids: Vec<&str> = expected_models.keys().map(String::as_str).collect();
	let actual_ids: Vec<&str> = actual_models.keys().map(String::as_str).collect();
	if expected_ids != actual_ids {
		errors.push(format!(
			"{filename} model IDs do not match the generated catalog ({})",
			describe_set_difference(&expected_ids, &actual_ids)
		));
	}
	for (model_id, expected_api) in expected_models {
		if let Some(actual_api) = actual_models.get(model_id).filter(|api| *api != expected_api) {
			errors.push(format!(
				"{provider}/{model_id} is grouped under API {}, expected {}",
				quoted(actual_api),
				quoted(expected_api)
			));
		}
	}
}

/// `String(value)` of a JSON value, `undefined` when absent.
fn js_string(value: Option<&JsonValue>) -> String {
	match value {
		None => "undefined".into(),
		Some(JsonValue::String(text)) => text.clone(),
		Some(other) => pi_json::to_js_string(other),
	}
}

/// `JSON.stringify` of a string.
fn quoted(text: &str) -> String {
	pi_json::to_js_string(&JsonValue::from(text))
}

/// `JSON.stringify(value)`, `undefined` when absent.
fn json_or_undefined(value: Option<&JsonValue>) -> String {
	value.map_or_else(|| "undefined".to_owned(), pi_json::to_js_string)
}

fn validate_model_value(
	value: &JsonValue,
	provider: &str,
	model_id: &str,
	expected_api: &str,
	errors: &mut Vec<String>,
) {
	let label = format!("{provider}/{model_id}");
	let Some(model) = value.as_object() else {
		errors.push(format!("{label} must be an object"));
		return;
	};
	for (field, expected) in [("id", model_id), ("provider", provider), ("api", expected_api)] {
		if model.get(field).and_then(JsonValue::as_str) != Some(expected) {
			errors.push(format!(
				"{label} has {field} {}, expected {}",
				json_or_undefined(model.get(field)),
				quoted(expected)
			));
		}
	}
	if model.get("name").and_then(JsonValue::as_str).is_none_or(str::is_empty) {
		errors.push(format!("{label} has no model name"));
	}
	if !model.get("baseUrl").is_some_and(JsonValue::is_string) {
		errors.push(format!("{label} has no baseUrl string"));
	}
	if !is_modality_list(model.get("input")) {
		errors.push(format!("{label} has invalid input modalities"));
	}
	check_type_specific(model, &label, errors);
	check_cost(model.get("cost"), &label, errors);
}

fn check_type_specific(model: &JsonObject, label: &str, errors: &mut Vec<String>) {
	let model_type = model.get("type").and_then(JsonValue::as_str);
	if model_type == Some("image") {
		let output = model.get("output");
		let has_image = output
			.and_then(JsonValue::as_array)
			.is_some_and(|items| items.iter().any(|item| item == "image"));
		if !is_modality_list(output) || !has_image {
			errors.push(format!("{label} has invalid output modalities"));
		}
	} else if model.contains_key("output") {
		errors.push(format!("{label} has unsupported output modalities"));
	}
	match model_type {
		Some("chat") => {
			if !model.get("reasoning").is_some_and(JsonValue::is_boolean) {
				errors.push(format!("{label} has no reasoning boolean"));
			}
			check_positive(model, "contextWindow", label, errors);
			check_positive(model, "maxTokens", label, errors);
		}
		Some("classifier") => check_positive(model, "contextWindow", label, errors),
		Some("image") => {}
		_ => errors.push(format!(
			"{label} has type {}, expected \"chat\", \"image\", or \"classifier\"",
			json_or_undefined(model.get("type"))
		)),
	}
}

fn check_positive(model: &JsonObject, field: &str, label: &str, errors: &mut Vec<String>) {
	if !model
		.get(field)
		.and_then(JsonValue::as_f64)
		.is_some_and(|value| value > 0.0)
	{
		errors.push(format!("{label} has invalid {field}"));
	}
}

fn check_cost(cost: Option<&JsonValue>, label: &str, errors: &mut Vec<String>) {
	let Some(cost) = cost.and_then(JsonValue::as_object) else {
		errors.push(format!("{label} has invalid cost metadata"));
		return;
	};
	for field in ["input", "output", "cacheRead", "cacheWrite"] {
		if !cost.get(field).is_some_and(JsonValue::is_number) {
			errors.push(format!("{label} has invalid cost.{field}"));
		}
	}
}

fn is_modality_list(value: Option<&JsonValue>) -> bool {
	value
		.and_then(JsonValue::as_array)
		.is_some_and(|items| !items.is_empty() && items.iter().all(|item| item == "text" || item == "image"))
}

/// The most errors one failure lists, as TS shows them.
const VISIBLE_ERRORS: usize = 30;

fn validation_errors(errors: &[String]) -> ModelDataError {
	let visible = &errors[..errors.len().min(VISIBLE_ERRORS)];
	let lines: Vec<String> = visible.iter().map(|error| format!("  - {error}")).collect();
	let suffix = if errors.len() > visible.len() {
		format!("\n  ... and {} more", errors.len() - visible.len())
	} else {
		String::new()
	};
	ModelDataError(format!("Invalid generated model data:\n{}{suffix}", lines.join("\n")))
}

/// Reads and parses a JSON object file, recording why it is not one.
fn read_json_object(path: &Path, description: &str, errors: &mut Vec<String>) -> Option<JsonObject> {
	match fs::read_to_string(path) {
		Ok(content) => parse_json_object(&content, description, errors),
		Err(error) => {
			errors.push(format!("{description} is not valid JSON: {error}"));
			None
		}
	}
}

fn parse_json_object(content: &str, description: &str, errors: &mut Vec<String>) -> Option<JsonObject> {
	// Trust boundary: files on disk.
	match pi_json::parse(content) {
		Ok(JsonValue::Object(object)) => Some(object),
		Ok(_) => {
			errors.push(format!("{description} must contain a JSON object"));
			None
		}
		Err(error) => {
			errors.push(format!("{description} is not valid JSON: {error}"));
			None
		}
	}
}
