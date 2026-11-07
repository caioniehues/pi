//! Structured classification requests and results (ports the classifier types of `packages/ai/src/types.ts`).

use crate::content::ImageContent;
use crate::record::Record;
use crate::usage::Usage;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};

/// What a classifier judges: a state, optional images, and named questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierContext {
	/// The state judged.
	pub state: JsonObject,
	/// Images judged with the state; only models with image input accept them.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub images: Option<Vec<ImageContent>>,
	/// The questions by name.
	pub questions: Record<ClassifierQuestion>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// One question, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierQuestion {
	/// Pick one of named options.
	Choice(ClassifierChoiceQuestion),
	/// Score along ordered criteria.
	Score(ClassifierScoreQuestion),
	/// Answer yes or no.
	Bool(ClassifierBoolQuestion),
}

/// `type: "choice"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierChoiceQuestion {
	/// The question.
	pub instructions: String,
	/// The options by name, with what each means.
	pub criteria: Record<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `type: "score"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierScoreQuestion {
	/// The question.
	pub instructions: String,
	/// The scale's levels, lowest first.
	pub criteria: Vec<String>,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `type: "bool"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierBoolQuestion {
	/// The question.
	pub instructions: String,
	/// What true and false mean.
	pub criteria: ClassifierBoolCriteria,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `{ true: string; false: string }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierBoolCriteria {
	/// What true means.
	#[serde(rename = "true")]
	pub when_true: String,
	/// What false means.
	#[serde(rename = "false")]
	pub when_false: String,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// One answer, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClassifierAnswer {
	/// The picked option.
	Choice(ClassifierChoiceAnswer),
	/// The score.
	Score(ClassifierScoreAnswer),
	/// The probability of true.
	Bool(ClassifierBoolAnswer),
}

/// `type: "choice"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierChoiceAnswer {
	/// The picked option.
	pub choice: String,
	/// The probability of each option.
	pub probabilities: Record<f64>,
	/// The confidence in the pick.
	pub confidence: f64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `type: "score"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierScoreAnswer {
	/// The score.
	pub score: f64,
	/// The confidence in it.
	pub confidence: f64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// `type: "bool"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassifierBoolAnswer {
	/// The probability of true.
	pub probability: f64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// Why a classification stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClassifierStopReason {
	/// It finished.
	Stop,
	/// It failed.
	Error,
	/// It was aborted.
	Aborted,
}

/// The result of a classification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifierResult {
	/// The classifier API.
	pub api: String,
	/// The provider id.
	pub provider: String,
	/// The model id.
	pub model: String,
	/// The answers by question name.
	pub answers: Record<ClassifierAnswer>,
	/// Tokens and their catalog cost, when the service reports token counts.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub usage: Option<Usage>,
	/// Why it stopped.
	pub stop_reason: ClassifierStopReason,
	/// The error text of a failed classification.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error_message: Option<String>,
	/// Epoch milliseconds.
	pub timestamp: u64,
	/// Fields this port does not know, kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}
