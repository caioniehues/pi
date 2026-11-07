//! TypeBox 1.3.27's `Value.Convert` tests as `test/coerce-typebox.test.ts` ports them to the conversion pass (pass 2),
//! case by case from `rust/fixtures/schema/coerce-typebox.json` (generated from that file by
//! `gen-coerce-typebox.mjs`): each case's conversions must give the asserted values and its checks the asserted
//! results. Cases whose values have no JSON form (bigint, undefined, symbol) are listed as skipped there.

use crate::{CompileOptions, compile, equal, formats};
use pi_json::JsonValue;

fn cases() -> Vec<JsonValue> {
	let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/schema/coerce-typebox.json");
	let text = std::fs::read_to_string(path).expect("the fixture exists");
	pi_json::parse(&text)
		.expect("the fixture is JSON")
		.as_array()
		.expect("the fixture is a list")
		.clone()
}

#[test]
fn value_convert_cases_of_typebox() {
	let cases = cases();
	assert_eq!(cases.len(), 221, "every it() of coerce-typebox.test.ts");
	let mut ported = 0;
	let mut failures = Vec::new();
	for (index, case) in cases.iter().enumerate() {
		let name = format!("#{index} {}", case["name"].as_str().unwrap_or("?"));
		let Some(steps) = case["steps"].as_array() else {
			continue;
		};
		ported += 1;
		for step in steps {
			let options = CompileOptions {
				formats: formats::standard(),
				coerce: true,
				..CompileOptions::default()
			};
			let validator = compile(&step["schema"], options).expect("the schema compiles");
			let expected = &step["expected"];
			if step["op"] == "convert" {
				let coercing = validator.coercing().expect("compiled with coerce");
				let actual = coercing.convert_only(&step["value"]);
				if !equal(&actual, expected) {
					failures.push(format!(
						"{name}: converted {} to {actual}, expected {expected}",
						step["value"]
					));
				}
			} else {
				let actual = validator.check(&step["value"]);
				if Some(actual) != expected.as_bool() {
					failures.push(format!(
						"{name}: check of {} is {actual}, expected {expected}",
						step["value"]
					));
				}
			}
		}
	}
	assert_eq!(ported, 186, "the cases with a JSON form");
	assert!(
		failures.is_empty(),
		"{} failures:\n{}",
		failures.len(),
		failures.join("\n")
	);
}
