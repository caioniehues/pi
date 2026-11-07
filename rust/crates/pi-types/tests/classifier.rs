//! Classifier requests and results (`classifier.rs`): every question and answer type.

mod common;

use common::{USAGE, round_trip};
use pi_types::{ClassifierAnswer, ClassifierContext, ClassifierResult};

#[test]
fn contexts_round_trip_with_every_question_type() {
	let context: ClassifierContext = round_trip(
		r#"{"state":{"diff":"+x"},"images":[{"type":"image","data":"AA==","mimeType":"image/png"}],"questions":{"risk":{"type":"choice","instructions":"How risky?","criteria":{"low":"safe","high":"dangerous"}},"quality":{"type":"score","instructions":"Rate","criteria":["bad","ok","good"]},"tests":{"type":"bool","instructions":"Tests added?","criteria":{"true":"yes","false":"no"}}}}"#,
	);
	assert_eq!(context.questions.len(), 3);
}

#[test]
fn results_round_trip_with_every_answer_type() {
	let result: ClassifierResult = round_trip(&format!(
		r#"{{"api":"typesafe-system-one","provider":"typesafe","model":"s1","answers":{{"risk":{{"type":"choice","choice":"low","probabilities":{{"low":0.9,"high":0.1}},"confidence":0.8}},"quality":{{"type":"score","score":2.5,"confidence":0.6}},"tests":{{"type":"bool","probability":0.25}}}},"usage":{USAGE},"stopReason":"stop","timestamp":5}}"#
	));
	assert!(matches!(result.answers.get("tests"), Some(ClassifierAnswer::Bool(_))));
	round_trip::<ClassifierResult>(
		r#"{"api":"a","provider":"p","model":"m","answers":{},"stopReason":"error","errorMessage":"down","timestamp":5}"#,
	);
}
