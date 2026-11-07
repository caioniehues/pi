//! Image generation (`images.rs`): contexts and results.

mod common;

use common::{USAGE, round_trip};
use pi_types::{AssistantImages, ImagesContext, ImagesStopReason};

#[test]
fn contexts_and_results_round_trip() {
	round_trip::<ImagesContext>(
		r#"{"input":[{"type":"text","text":"a cat"},{"type":"image","data":"AA==","mimeType":"image/png"}]}"#,
	);
	let result: AssistantImages = round_trip(&format!(
		r#"{{"api":"openrouter-images","provider":"openrouter","model":"m","output":[{{"type":"image","data":"AA==","mimeType":"image/png"}}],"responseId":"r","usage":{USAGE},"stopReason":"aborted","errorMessage":"cancelled","timestamp":9}}"#
	));
	assert_eq!(result.stop_reason, ImagesStopReason::Aborted);
}
