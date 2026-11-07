//! Provider-side constrained sampling of tool arguments (ports `packages/ai/src/api/constrained-sampling.ts`): the
//! strict JSON Schema subset, the strict-tool limit errors, and OpenAI grammar tools with their streamed JSON input.

use pi_json::{JsonObject, JsonValue};
use pi_types::{ConstrainedSampling, ConstrainedSamplingConfig, Strictness, Tool};

/// Why a schema has no strict form, or a grammar tool is unusable; the messages are TS's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstrainedSamplingError {
	/// The schema uses something strict mode rejects (`UnsupportedStrictJsonSchemaError`).
	#[error("{0}")]
	UnsupportedStrictSchema(String),
	/// Any other failure (a required strict tool, a malformed grammar tool or input).
	#[error("{0}")]
	Invalid(String),
}

/// Whether a provider's strict mode rejects a keyword with a value.
pub type UnsupportedStrictSchemaKeywordCheck<'a> = &'a dyn Fn(&str, &JsonValue) -> bool;

const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
	"$ref",
	"$defs",
	"definitions",
	"allOf",
	"oneOf",
	"patternProperties",
	"dependentSchemas",
	"dependencies",
	"unevaluatedProperties",
	"propertyNames",
	"contains",
	"prefixItems",
	"not",
	"if",
	"then",
	"else",
];

fn unsupported(message: impl Into<String>) -> ConstrainedSamplingError {
	ConstrainedSamplingError::UnsupportedStrictSchema(message.into())
}

fn types_of(schema: &JsonObject) -> Vec<&str> {
	match schema.get("type") {
		Some(JsonValue::String(kind)) => vec![kind.as_str()],
		Some(JsonValue::Array(kinds)) => kinds.iter().filter_map(JsonValue::as_str).collect(),
		_ => Vec::new(),
	}
}

fn is_structured_schema(schema: &JsonValue) -> bool {
	let Some(schema) = schema.as_object() else { return false };
	let types = types_of(schema);
	types.contains(&"object")
		|| types.contains(&"array")
		|| schema.contains_key("properties")
		|| schema.contains_key("items")
}

fn schema_allows_null(schema: &JsonValue) -> bool {
	let Some(schema) = schema.as_object() else { return false };
	if types_of(schema).contains(&"null") {
		return true;
	}
	if schema.get("const") == Some(&JsonValue::Null)
		|| schema
			.get("enum")
			.and_then(JsonValue::as_array)
			.is_some_and(|values| values.contains(&JsonValue::Null))
	{
		return true;
	}
	schema
		.get("anyOf")
		.and_then(JsonValue::as_array)
		.is_some_and(|variants| variants.iter().any(schema_allows_null))
}

fn make_node_strict(
	schema: &mut JsonValue,
	check: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<(), ConstrainedSamplingError> {
	let Some(object) = schema.as_object_mut() else {
		return Err(unsupported("boolean schemas are unsupported"));
	};
	if let Some(key) = UNSUPPORTED_STRICT_SCHEMA_KEYS
		.iter()
		.find(|key| object.contains_key(**key))
	{
		return Err(unsupported(format!("{key} schemas are unsupported")));
	}
	if let Some(check) = check
		&& let Some((key, value)) = object.iter().find(|(key, value)| check(key, value))
	{
		return Err(unsupported(format!(
			"{key}: {} is unsupported",
			pi_json::to_js_string(value)
		)));
	}
	if let Some(any_of) = object.get_mut("anyOf") {
		let Some(variants) = any_of.as_array_mut().filter(|variants| !variants.is_empty()) else {
			return Err(unsupported("anyOf must contain at least one schema"));
		};
		for variant in variants {
			if is_structured_schema(variant) {
				return Err(unsupported("object and array unions are unsupported"));
			}
			make_node_strict(variant, check)?;
		}
	}
	if let Some(items) = object.get_mut("items") {
		if items.is_array() {
			return Err(unsupported("tuple schemas are unsupported"));
		}
		make_node_strict(items, check)?;
	}
	let is_object = object.get("type").and_then(JsonValue::as_str) == Some("object");
	if object.contains_key("properties") && !is_object {
		return Err(unsupported("properties require type object"));
	}
	if !is_object {
		return Ok(());
	}
	make_object_strict(object, check)
}

fn make_object_strict(
	object: &mut JsonObject,
	check: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<(), ConstrainedSamplingError> {
	if object
		.get("additionalProperties")
		.is_some_and(|value| *value != JsonValue::Bool(false))
	{
		return Err(unsupported("schema-valued or true additionalProperties is unsupported"));
	}
	if object
		.get("properties")
		.is_some_and(|properties| !properties.is_object())
	{
		return Err(unsupported("object properties must be a schema map"));
	}
	let required: Vec<String> = match object.get("required") {
		None => Vec::new(),
		Some(JsonValue::Array(keys)) if keys.iter().all(JsonValue::is_string) => {
			keys.iter().filter_map(JsonValue::as_str).map(str::to_owned).collect()
		}
		Some(_) => return Err(unsupported("object required must be a string array")),
	};
	let names: Vec<String> = match object.get("properties") {
		Some(JsonValue::Object(properties)) => pi_json::js_key_order(properties)
			.map(|(name, _)| name.clone())
			.collect(),
		_ => Vec::new(),
	};
	if required.iter().any(|key| !names.contains(key)) {
		return Err(unsupported("required contains an unknown property"));
	}
	if let Some(JsonValue::Object(properties)) = object.get_mut("properties") {
		for name in &names {
			let Some(property) = properties.get_mut(name) else {
				continue;
			};
			make_node_strict(property, check)?;
			if !required.contains(name) && !schema_allows_null(property) {
				let original = std::mem::take(property);
				*property = serde_json::json!({ "anyOf": [original, { "type": "null" }] });
			}
		}
	}
	// Assignments keep an existing key's position, as in TS.
	object.insert(
		"required".into(),
		JsonValue::Array(names.into_iter().map(JsonValue::String).collect()),
	);
	object.insert("additionalProperties".into(), JsonValue::Bool(false));
	Ok(())
}

/// A tool schema in the strict subset of provider constrained sampling (`makeStrictJsonSchema`).
pub fn make_strict_json_schema(
	schema: &JsonValue,
	check: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<JsonObject, ConstrainedSamplingError> {
	let mut cloned = schema.clone();
	if !cloned.is_object() {
		return Err(unsupported("root schema must have type object"));
	}
	make_node_strict(&mut cloned, check)?;
	match cloned {
		JsonValue::Object(object) if object.get("type").and_then(JsonValue::as_str) == Some("object") => Ok(object),
		_ => Err(unsupported("root schema must have type object")),
	}
}

fn json_schema_strictness(tool: &Tool) -> Option<Strictness> {
	match &tool.constrained_sampling {
		Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema(config))) => Some(config.strict),
		_ => None,
	}
}

/// Whether a tool asks for strict JSON-schema sampling only when possible (`prefersJsonSchemaStrictSampling`).
pub fn prefers_json_schema_strict_sampling(tool: &Tool) -> bool {
	json_schema_strictness(tool) == Some(Strictness::Prefer)
}

/// The messages Claude rejects a request with whose strict tools exceed its limits.
const STRICT_TOOL_LIMIT_ERROR_MESSAGES: &[&str] = &[
	"The compiled grammar is too large",
	"Too many strict tools (",
	"Schemas contains too many parameters with union types (",
];

/// Whether a provider error says the strict tools exceed its limits (`isStrictToolLimitErrorMessage`).
pub fn is_strict_tool_limit_error_message(message: &str) -> bool {
	STRICT_TOOL_LIMIT_ERROR_MESSAGES
		.iter()
		.any(|limit| message.contains(limit))
}

/// The tool's parameters, in strict form when `strict` (`getJsonSchemaToolParameters`).
pub fn get_json_schema_tool_parameters(
	tool: &Tool,
	strict: Option<bool>,
) -> Result<JsonValue, ConstrainedSamplingError> {
	if strict == Some(true) {
		return make_strict_json_schema(&tool.parameters, None).map(JsonValue::Object);
	}
	Ok(tool.parameters.clone())
}

/// Whether a JSON-schema tool is sent strict (`resolveJsonSchemaStrictSampling`): `Some(true)` when it is, `None`
/// when not; an error when it requires strict mode and cannot have it.
pub fn resolve_json_schema_strict_sampling(
	tool: &Tool,
	supports_strict_mode: bool,
	check: Option<UnsupportedStrictSchemaKeywordCheck<'_>>,
) -> Result<Option<bool>, ConstrainedSamplingError> {
	let Some(strictness) = json_schema_strictness(tool) else {
		return Ok(None);
	};
	if supports_strict_mode {
		return match make_strict_json_schema(&tool.parameters, check) {
			Ok(_) => Ok(Some(true)),
			Err(ConstrainedSamplingError::UnsupportedStrictSchema(reason)) => match strictness {
				Strictness::Prefer => Ok(None),
				Strictness::Require => Err(ConstrainedSamplingError::Invalid(format!(
					"Tool \"{}\" requires JSON-schema constrained sampling, but {reason}.",
					tool.name
				))),
			},
			Err(other) => Err(other),
		};
	}
	match strictness {
		Strictness::Require => Err(ConstrainedSamplingError::Invalid(format!(
			"Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
			tool.name
		))),
		Strictness::Prefer => Ok(None),
	}
}

/// A grammar-constrained tool (`GrammarConstrainedSampling`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
	/// The grammar format.
	pub format: GrammarFormat,
	/// The grammar.
	pub definition: String,
	/// The one required string property the input goes to.
	pub input_property: String,
}

/// `"lark" | "regex"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrammarFormat {
	/// A Lark grammar.
	Lark,
	/// A regular expression.
	Regex,
}

/// The grammar of a tool when the provider supports grammar tools (`resolveGrammarConstrainedSampling`).
pub fn resolve_grammar_constrained_sampling(
	tool: &Tool,
	supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, ConstrainedSamplingError> {
	let Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar(config))) = &tool.constrained_sampling
	else {
		return Ok(None);
	};
	if !supports_openai_grammar_tools {
		return Ok(None);
	}
	let usable = |definition: &Option<String>| definition.clone().filter(|text| !text.trim().is_empty());
	let (format, definition) = match (
		usable(&config.variants.openai_lark),
		usable(&config.variants.openai_regex),
	) {
		(Some(lark), _) => (GrammarFormat::Lark, lark),
		(None, Some(regex)) => (GrammarFormat::Regex, regex),
		(None, None) => {
			return Err(ConstrainedSamplingError::Invalid(format!(
				"Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
				tool.name
			)));
		}
	};
	let input_property = infer_grammar_input_property(tool).map_err(|reason| {
		ConstrainedSamplingError::Invalid(format!(
			"Tool \"{}\" cannot use grammar constrained sampling: {reason}.",
			tool.name
		))
	})?;
	Ok(Some(GrammarConstrainedSampling {
		format,
		definition,
		input_property,
	}))
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String, String> {
	let schema = tool.parameters.as_object();
	if schema.and_then(|schema| schema.get("type")).and_then(JsonValue::as_str) != Some("object") {
		return Err("grammar constrained sampling requires an object parameter schema".into());
	}
	let required = schema
		.and_then(|schema| schema.get("required"))
		.and_then(JsonValue::as_array);
	let Some(property) = required
		.filter(|required| required.len() == 1)
		.and_then(|required| required[0].as_str())
	else {
		return Err("grammar constrained sampling requires exactly one required string property".into());
	};
	let definition = schema
		.and_then(|schema| schema.get("properties"))
		.and_then(|properties| properties.get(property));
	let Some(definition) =
		definition.filter(|definition| !matches!(definition, JsonValue::Null | JsonValue::Bool(false)))
	else {
		return Err(format!(
			"grammar constrained sampling requires a properties entry for {property}"
		));
	};
	if definition.get("type").and_then(JsonValue::as_str) != Some("string") {
		return Err(format!(
			"grammar constrained sampling property {property} must have type string"
		));
	}
	Ok(property.to_owned())
}

/// The input property of every grammar tool, by tool name (`createGrammarToolInputProperties`).
pub fn create_grammar_tool_input_properties(
	tools: &[Tool],
	supports_openai_grammar_tools: bool,
) -> Result<Vec<(String, String)>, ConstrainedSamplingError> {
	let mut properties = Vec::new();
	for tool in tools {
		if let Some(grammar) = resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)? {
			properties.retain(|(name, _): &(String, String)| *name != tool.name);
			properties.push((tool.name.clone(), grammar.input_property));
		}
	}
	Ok(properties)
}

/// The string input of a grammar tool call (`getGrammarToolInput`).
pub fn get_grammar_tool_input<'a>(
	tool_name: &str,
	arguments: &'a JsonObject,
	input_property: &str,
) -> Result<&'a str, ConstrainedSamplingError> {
	arguments
		.get(input_property)
		.and_then(JsonValue::as_str)
		.ok_or_else(|| {
			ConstrainedSamplingError::Invalid(format!(
				"Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
			))
		})
}

/// The streaming input of a grammar tool and how much of its JSON text was produced (`GrammarToolInputJsonBuffer`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GrammarToolInputJsonBuffer {
	/// The input so far.
	pub input: String,
	/// Whether the opening of the JSON text was produced.
	pub started: bool,
	/// Whether the JSON text was closed.
	pub closed: bool,
}

/// The JSON-string body of `text` (`JSON.stringify(text).slice(1, -1)`).
fn json_string_body(text: &str) -> String {
	let quoted = pi_json::to_js_string(&JsonValue::from(text));
	quoted[1..quoted.len() - 1].to_owned()
}

fn json_start(buffer: &mut GrammarToolInputJsonBuffer, input_property: &str) -> String {
	if buffer.started {
		return String::new();
	}
	buffer.started = true;
	format!("{{{}:\"", pi_json::to_js_string(&JsonValue::from(input_property)))
}

/// Appends `delta` to a streaming grammar input; the matching piece of its JSON arguments text, `None` for an empty
/// delta (`appendGrammarToolInputJsonDelta`).
pub fn append_grammar_tool_input_json_delta(
	buffer: &mut GrammarToolInputJsonBuffer,
	input_property: &str,
	delta: &str,
) -> Result<Option<String>, ConstrainedSamplingError> {
	if buffer.closed {
		return Err(ConstrainedSamplingError::Invalid(format!(
			"grammar tool input for property \"{input_property}\" changed after it was closed"
		)));
	}
	if delta.is_empty() {
		return Ok(None);
	}
	buffer.input.push_str(delta);
	Ok(Some(json_start(buffer, input_property) + &json_string_body(delta)))
}

/// Closes a streaming grammar input; the rest of its JSON arguments text, `None` when already closed with the same
/// input (`closeGrammarToolInputJson`).
pub fn close_grammar_tool_input_json(
	buffer: &mut GrammarToolInputJsonBuffer,
	input_property: &str,
	final_input: Option<&str>,
) -> Result<Option<String>, ConstrainedSamplingError> {
	if buffer.closed {
		if final_input.is_none_or(|input| input == buffer.input) {
			return Ok(None);
		}
		return Err(ConstrainedSamplingError::Invalid(format!(
			"grammar tool input for property \"{input_property}\" changed after it was closed"
		)));
	}
	let rest = match final_input {
		Some(input) => match input.strip_prefix(buffer.input.as_str()) {
			Some(rest) => rest.to_owned(),
			None => {
				return Err(ConstrainedSamplingError::Invalid(format!(
					"grammar tool input for property \"{input_property}\" changed non-monotonically"
				)));
			}
		},
		None => String::new(),
	};
	if let Some(input) = final_input {
		input.clone_into(&mut buffer.input);
	}
	buffer.closed = true;
	Ok(Some(format!(
		"{}{}\"}}",
		json_start(buffer, input_property),
		json_string_body(&rest)
	)))
}
