//! Token usage and its cost (ports `Usage` of `packages/ai/src/types.ts`).

use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

/// Tokens a request used and what they cost. Token counts are `i64`: TS can write negatives
/// (`google-generative-ai.ts` and `google-vertex.ts` subtract cached from prompt tokens without clamping).
/// Field order is the interface's, which `openai-completions` writes;
/// `anthropic-messages` adds `cacheWrite1h` and `reasoning` after `cost` (a second writer order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
	/// Input tokens not read from or written to the cache.
	pub input: i64,
	/// Output tokens, reasoning included.
	pub output: i64,
	/// Input tokens read from the cache.
	pub cache_read: i64,
	/// Input tokens written to the cache.
	pub cache_write: i64,
	/// The part of `cache_write` written with 1h retention (Anthropic only).
	#[serde(rename = "cacheWrite1h", default, skip_serializing_if = "Option::is_none")]
	pub cache_write_1h: Option<i64>,
	/// Reasoning tokens, a subset of `output`, when the provider reports them.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub reasoning: Option<i64>,
	/// All tokens of the request.
	// TS parity: declared required, but sessions written before the field existed lack it (the fixture
	// `large-session.jsonl`); TS readers fall back to the sum of the parts (`calculateContextTokens`).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub total_tokens: Option<i64>,
	/// The cost in USD.
	pub cost: UsageCost,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// The cost of a request in USD, per kind of token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
	/// Catalog-derived input cost.
	pub input: f64,
	/// Catalog-derived output cost.
	pub output: f64,
	/// Catalog-derived cache-read cost.
	pub cache_read: f64,
	/// Catalog-derived cache-write cost.
	pub cache_write: f64,
	/// The provider's billed cost when it reports one, else the catalog estimate.
	pub total: f64,
	/// Whether `total` is the provider's billed cost; absent means false.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub total_from_provider: Option<bool>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

impl Usage {
	/// No tokens and no cost, as providers start a response (`totalTokens: 0` written).
	pub fn zero() -> Self {
		Self {
			input: 0,
			output: 0,
			cache_read: 0,
			cache_write: 0,
			cache_write_1h: None,
			reasoning: None,
			total_tokens: Some(0),
			cost: UsageCost {
				input: 0.0,
				output: 0.0,
				cache_read: 0.0,
				cache_write: 0.0,
				total: 0.0,
				total_from_provider: None,
				extra: JsonObject::new(),
			},
			extra: JsonObject::new(),
		}
	}
}
