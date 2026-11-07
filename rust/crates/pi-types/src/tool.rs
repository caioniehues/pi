//! Tool declarations the model sees (ports `Tool`, `ToolReference`, `ConstrainedSamplingConfig` and
//! `GrammarVariants` of `packages/ai/src/types.ts`). Executable tools (`AgentTool`) are `pi-agent`'s.

use crate::literal::False;
use pi_json::{JsonObject, JsonValue};
use serde::{Deserialize, Serialize};

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
	/// The name the model calls it by.
	pub name: String,
	/// What it does, for the model.
	pub description: String,
	/// The JSON Schema of its arguments.
	pub parameters: JsonValue,
	/// Provider-side constrained sampling; `false` turns it off for this tool.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub constrained_sampling: Option<ConstrainedSampling>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `false | ConstrainedSamplingConfig`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstrainedSampling {
	/// `false`: no constrained sampling for this tool.
	Disabled(False),
	/// A configuration.
	Config(ConstrainedSamplingConfig),
}

/// How a provider constrains sampling of a tool's arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstrainedSamplingConfig {
	/// `{ type: "json_schema" }`: the API's `strict` mode, JSON Schema constrained sampling.
	JsonSchema(JsonSchemaSampling),
	/// `{ type: "grammar" }`: provider-specific grammars for the same language.
	Grammar(GrammarSampling),
}

/// JSON Schema constrained sampling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonSchemaSampling {
	/// Whether a provider without support may ignore it (`prefer`) or must fail (`require`).
	pub strict: Strictness,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Grammar constrained sampling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrammarSampling {
	/// The grammar per format.
	pub variants: GrammarVariants,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `"prefer" | "require"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strictness {
	/// Use it where supported.
	Prefer,
	/// Fail where unsupported.
	Require,
}

/// OpenAI grammar variants (`Partial<Record<GrammarFormat, string>>`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrammarVariants {
	/// A Lark grammar.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub openai_lark: Option<String>,
	/// A regular expression.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub openai_regex: Option<String>,
	/// Formats this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// A tool named without its definition (`toolsRemoved`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolReference {
	/// The tool's name.
	pub name: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
