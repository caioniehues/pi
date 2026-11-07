//! Tool declarations (`tool.rs`): constrained sampling in each form.

mod common;

use common::round_trip;
use pi_types::{ConstrainedSampling, ConstrainedSamplingConfig, Tool};

#[test]
fn constrained_sampling_is_false_or_a_config() {
	let base = r#""name":"t","description":"d","parameters":{"type":"object"}"#;
	let tool: Tool = round_trip(&format!(r#"{{{base},"constrainedSampling":false}}"#));
	assert!(matches!(
		tool.constrained_sampling,
		Some(ConstrainedSampling::Disabled(_))
	));
	let tool: Tool = round_trip(&format!(
		r#"{{{base},"constrainedSampling":{{"type":"json_schema","strict":"require"}}}}"#
	));
	assert!(matches!(
		tool.constrained_sampling,
		Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema(_)))
	));
	round_trip::<Tool>(&format!(
		r#"{{{base},"constrainedSampling":{{"type":"grammar","variants":{{"openai_lark":"start: x","openai_regex":"x+"}}}}}}"#
	));
	round_trip::<Tool>(&format!("{{{base}}}"));
	assert!(common::read::<Tool>(&format!(r#"{{{base},"constrainedSampling":true}}"#)).is_err());
}
