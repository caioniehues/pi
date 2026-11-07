//! `compile(schema, coerce)`: `coerce()` and `parse()` (TS `test/coerce.test.ts`), the three coercion passes
//! (optional nulls, TypeBox's conversion rules, primitive coercion by type), their order where they conflict, and the
//! guarantees around them. TS's identity cases (`toBe(value)`) become equality: a Rust `coerce` returns an owned value.
//! Cases about `bigint` and `undefined` become the decided JSON behaviour: those types convert nothing.

#![allow(
	clippy::expect_used,
	reason = "test helpers fail the test on a schema that does not compile"
)]

use pi_json::JsonValue;
use pi_schema::builders::{arr, int, map, nullable, num, obj, opt, str, tuple};
use pi_schema::{CoercingValidator, CompileOptions, ParseResult, ValidationError, compile, equal, formats};
use serde_json::json;

fn none() -> JsonValue {
	json!({})
}

fn coercing(schema: &JsonValue) -> CoercingValidator {
	let options = CompileOptions {
		formats: formats::standard(),
		coerce: true,
		..CompileOptions::default()
	};
	compile(schema, options)
		.expect("the schema compiles")
		.into_coercing()
		.expect("compiled with coerce")
}

fn coerce(schema: &JsonValue, value: &JsonValue) -> JsonValue {
	coercing(schema).coerce(value)
}

/// The coerced `value` property of `{ value }` under an object schema with one required property `value`.
fn coerce_value(schema: JsonValue, value: JsonValue) -> JsonValue {
	let mut root = json!({ "type": "object", "properties": {}, "required": ["value"] });
	root["properties"]["value"] = schema;
	let mut input = json!({});
	input["value"] = value;
	coerce(&root, &input)["value"].clone()
}

#[track_caller]
fn assert_json(actual: &JsonValue, expected: &JsonValue) {
	assert!(equal(actual, expected), "got {actual}, expected {expected}");
}

#[track_caller]
fn assert_parsed(result: &ParseResult, expected: &JsonValue) {
	match result {
		Ok(value) => assert_json(value, expected),
		Err(errors) => panic!("expected {expected}, got errors {errors:?}"),
	}
}

fn err(path: &str, keyword: &str, message: &str) -> ValidationError {
	ValidationError::new(path, keyword, message)
}

mod compile_options {
	use super::*;

	#[test]
	fn adds_coerce_and_parse_only_with_coerce_true() {
		let schema = obj(vec![("n", num(none()).into())], none());
		let plain = compile(&schema, CompileOptions::default()).expect("compiles");
		assert!(plain.coercing().is_none());
		let with_coerce = coercing(&schema);
		assert!(with_coerce.check(&json!({ "n": 1 })));
	}
}

mod coerce {
	use super::*;

	#[test]
	fn never_modifies_the_value() {
		let value = json!({ "count": "42", "tags": "a" });
		let schema = obj(
			vec![("count", num(none()).into()), ("tags", arr(str(none()), none()).into())],
			none(),
		);
		assert_json(&coerce(&schema, &value), &json!({ "count": 42, "tags": ["a"] }));
		assert_eq!(value, json!({ "count": "42", "tags": "a" }));
	}

	#[test]
	fn returns_the_value_itself_when_nothing_changes() {
		let value = json!({ "a": { "b": [1, 2] }, "c": "x" });
		let inner = obj(vec![("b", arr(num(none()), none()).into())], none());
		let schema = obj(vec![("a", inner.into()), ("c", str(none()).into())], none());
		assert_eq!(coerce(&schema, &value), value);
	}

	#[test]
	fn copies_only_the_changed_paths() {
		let value = json!({ "a": { "b": [1, 2] }, "c": { "d": "1" } });
		let schema = obj(
			vec![
				("a", obj(vec![("b", arr(num(none()), none()).into())], none()).into()),
				("c", obj(vec![("d", num(none()).into())], none()).into()),
			],
			none(),
		);
		assert_json(
			&coerce(&schema, &value),
			&json!({ "a": { "b": [1, 2] }, "c": { "d": 1 } }),
		);
		assert_eq!(value["c"], json!({ "d": "1" }));
	}

	#[test]
	fn works_on_frozen_schemas() {
		let schema = json!({ "type": "object", "properties": { "n": { "type": "integer" } } });
		assert_json(&coerce(&schema, &json!({ "n": "7" })), &json!({ "n": 7 }));
	}

	// TS: "still coerces when the Function constructor is unavailable". Rust generates no code; the case is the
	// plain coercion.
	#[test]
	fn coerces_without_generated_code() {
		let schema = obj(vec![("count", num(none()).into())], none());
		assert_json(&coerce(&schema, &json!({ "count": "42" })), &json!({ "count": 42 }));
	}

	#[test]
	fn leaves_values_it_cannot_coerce_for_the_check_to_report() {
		let validator = coercing(&obj(vec![("count", num(none()).into())], none()));
		let coerced = validator.coerce(&json!({ "count": "many" }));
		assert_eq!(coerced, json!({ "count": "many" }));
		assert!(!validator.check(&coerced));
	}
}

mod parse {
	use super::*;

	#[test]
	fn returns_the_coerced_value_when_it_is_valid() {
		let schema = obj(
			vec![("count", num(none()).into()), ("tags", arr(str(none()), none()).into())],
			none(),
		);
		let result = coercing(&schema).parse(&json!({ "count": "42", "tags": "a" }));
		assert_parsed(&result, &json!({ "count": 42, "tags": ["a"] }));
	}

	#[test]
	fn returns_the_errors_of_the_coerced_value_when_it_is_invalid() {
		let schema = obj(
			vec![("count", num(none()).into()), ("name", str(none()).into())],
			none(),
		);
		assert_eq!(
			coercing(&schema).parse(&json!({ "count": "many", "name": 7 })),
			Err(vec![err("/count", "type", "expected number, got string")])
		);
	}
}

mod validate {
	use super::*;

	#[test]
	fn checks_without_coercing_with_or_without_the_coerce_option() {
		let schema = obj(
			vec![("count", num(none()).into()), ("tags", arr(str(none()), none()).into())],
			none(),
		);
		let value = json!({ "count": 42, "tags": ["a"] });
		let plain = compile(&schema, CompileOptions::default()).expect("compiles");
		for validator in [&*plain, &*coercing(&schema)] {
			assert_eq!(validator.validate(&value), Ok(()));
			// parse() would turn "42" into 42 and "a" into ["a"]; validate() reports both.
			assert_eq!(
				validator.validate(&json!({ "count": "42", "tags": "a" })),
				Err(vec![
					err("/count", "type", "expected number, got string"),
					err("/tags", "type", "expected array, got string"),
				])
			);
		}
	}
}

mod bigint_and_undefined_as_typeboxs_value_convert {
	use super::*;

	// TS: "converts to a bigint like TypeBox's TryBigInt". No JSON value is a bigint, so nothing converts to one.
	#[test]
	fn convert_nothing_to_a_bigint() {
		for value in [json!("42"), json!("12n"), json!(3.7), json!(true), json!("x")] {
			assert_json(&coerce_value(json!({ "type": "bigint" }), value.clone()), &value);
		}
	}

	// TS: "converts to undefined like TypeBox's TryUndefined". No JSON value is undefined.
	#[test]
	fn convert_nothing_to_undefined() {
		for value in [
			json!(null),
			json!(0),
			json!(false),
			json!(""),
			json!("0"),
			json!("null"),
			json!("x"),
		] {
			assert_json(&coerce_value(json!({ "type": "undefined" }), value.clone()), &value);
		}
	}
}

mod pass_1_optional_nulls {
	use super::*;

	#[test]
	fn drops_null_in_optional_non_nullable_properties_also_nested() {
		let schema = obj(
			vec![
				("path", str(none()).into()),
				("offset", opt(num(none()))),
				("nullable", opt(nullable(str(none()), none()))),
				(
					"metadata",
					obj(vec![("enabled", opt(json!({ "type": "boolean" })))], none()).into(),
				),
			],
			json!({ "open": true }),
		);
		let value = json!({ "path": "file.txt", "offset": null, "nullable": null, "metadata": { "enabled": null } });
		assert_json(
			&coerce(&schema, &value),
			&json!({ "path": "file.txt", "nullable": null, "metadata": {} }),
		);
	}

	#[test]
	fn keeps_a_null_whose_property_is_a_ref_the_reference_may_allow_it() {
		let schema = json!({
			"type": "object",
			"properties": { "value": { "$ref": "#/$defs/value" } },
			"$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
		});
		assert_json(&coerce(&schema, &json!({ "value": null })), &json!({ "value": null }));
	}

	#[test]
	fn walks_array_items() {
		let item = obj(vec![("a", str(none()).into()), ("b", opt(int(none())))], none());
		let schema = json!({ "type": "array", "items": item });
		assert_json(
			&coerce(&schema, &json!([{ "a": "x", "b": null }])),
			&json!([{ "a": "x" }]),
		);
	}
}

mod pass_2_typeboxs_conversion_rules_on_json_schema {
	use super::*;

	#[test]
	fn converts_primitives_by_type() {
		let cases = [
			(json!({ "type": "number" }), json!("42"), json!(42)),
			(json!({ "type": "number" }), json!(""), json!(0)),
			(json!({ "type": "number" }), json!("true"), json!(1)),
			(json!({ "type": "integer" }), json!("3.5"), json!(3)),
			(json!({ "type": "integer" }), json!(1.5), json!(1)),
			(json!({ "type": "boolean" }), json!("1"), json!(true)),
			(json!({ "type": "boolean" }), json!("FALSE"), json!(false)),
			(json!({ "type": "null" }), json!("null"), json!(null)),
			(json!({ "type": "string" }), json!(42), json!("42")),
		];
		for (schema, input, expected) in cases {
			assert_json(&coerce_value(schema, input), &expected);
		}
	}

	#[test]
	fn wraps_a_single_value_into_an_array_and_converts_the_items() {
		let numbers = json!({ "type": "array", "items": { "type": "number" } });
		assert_json(&coerce_value(numbers, json!("1")), &json!([1]));
		let strings = json!({ "type": "array", "items": { "type": "string" } });
		assert_json(&coerce_value(strings, json!(null)), &json!(["null"]));
	}

	#[test]
	fn converts_tuple_items_in_place_and_never_wraps_for_a_tuple() {
		let tuple =
			json!({ "type": "array", "prefixItems": [{ "type": "number" }, { "type": "boolean" }], "items": false });
		assert_json(&coerce_value(tuple.clone(), json!(["1", "true"])), &json!([1, true]));
		assert_json(&coerce_value(tuple, json!("1")), &json!("1"));
	}

	#[test]
	fn converts_const_and_enum_values_only_when_the_result_is_a_member() {
		assert_json(&coerce_value(json!({ "const": 1 }), json!("1")), &json!(1));
		assert_json(&coerce_value(json!({ "const": 1 }), json!("2")), &json!("2"));
		assert_json(&coerce_value(json!({ "enum": [1, 2, "hello"] }), json!("2")), &json!(2));
		assert_json(
			&coerce_value(json!({ "enum": [1, 2, "hello"] }), json!("world")),
			&json!("world"),
		);
	}

	#[test]
	fn keeps_a_value_one_union_branch_accepts_else_takes_the_first_branch_whose_conversion_the_union_accepts() {
		let union = json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] });
		assert_json(&coerce_value(union.clone(), json!(null)), &json!(null));
		assert_json(&coerce_value(union.clone(), json!("42")), &json!(42));
		assert_json(&coerce_value(union, json!("null")), &json!(null));
		let one_of = json!({ "oneOf": [{ "type": "boolean" }, { "type": "number" }] });
		assert_json(&coerce_value(one_of, json!("1")), &json!(true));
	}

	#[test]
	fn converts_all_of_with_each_member_in_turn() {
		let open = json!({ "open": true });
		let schema = json!({
			"allOf": [
				obj(vec![("a", num(none()).into())], open.clone()),
				obj(vec![("b", json!({ "type": "boolean" }).into())], open),
			],
		});
		assert_json(
			&coerce(&schema, &json!({ "a": "1", "b": "true" })),
			&json!({ "a": 1, "b": true }),
		);
	}

	#[test]
	fn follows_ref() {
		let schema = json!({
			"type": "object",
			"properties": { "item": { "$ref": "#/$defs/item" } },
			"$defs": { "item": { "type": "object", "properties": { "id": { "type": "integer" } } } },
		});
		assert_json(
			&coerce(&schema, &json!({ "item": { "id": "7" } })),
			&json!({ "item": { "id": 7 } }),
		);
	}

	#[test]
	fn follows_a_relative_ref_from_the_root_id_on_the_first_call_of_a_schema_with_dynamic_ref() {
		let schema = json!({
			"$id": "https://example.com/root.json",
			"$dynamicAnchor": "node",
			"type": "object",
			"properties": {
				"count": { "$ref": "defs/n.json" },
				"children": { "type": "array", "items": { "$dynamicRef": "#node" } },
			},
		});
		let options = || CompileOptions {
			coerce: true,
			schemas: vec![(
				"https://example.com/defs/n.json".to_owned(),
				json!({ "type": "integer" }),
			)],
			..CompileOptions::default()
		};
		let validator = |options| {
			compile(&schema, options)
				.ok()
				.and_then(pi_schema::Compiled::into_coercing)
		};
		let first = validator(options()).map(|v| v.coerce(&json!({ "count": "2" })));
		assert_eq!(first, Some(json!({ "count": 2 })));
		let validator = validator(options()).expect("compiles");
		assert_parsed(&validator.parse(&json!({ "count": "2" })), &json!({ "count": 2 }));
		assert_parsed(&validator.parse(&json!({ "count": "2" })), &json!({ "count": 2 }));
	}

	#[test]
	fn converts_pattern_properties_and_additional_properties() {
		let schema = json!({
			"type": "object",
			"properties": { "a": { "type": "number" } },
			"patternProperties": { "^n_": { "type": "number" } },
			"additionalProperties": { "type": "boolean" },
		});
		assert_json(
			&coerce(&schema, &json!({ "a": "1", "n_x": "2", "other": "true" })),
			&json!({ "a": 1, "n_x": 2, "other": true }),
		);
	}
}

mod pass_3_primitive_coercion_by_type {
	use super::*;

	#[test]
	fn coerces_like_ajvs_coerce_types() {
		let cases = [
			(json!({ "type": "number" }), json!(true), json!(1)),
			(json!({ "type": "number" }), json!(null), json!(0)),
			(json!({ "type": "integer" }), json!("42"), json!(42)),
			(json!({ "type": "boolean" }), json!("true"), json!(true)),
			(json!({ "type": "boolean" }), json!(0), json!(false)),
			(json!({ "type": "string" }), json!(true), json!("true")),
			(json!({ "type": "null" }), json!(""), json!(null)),
			(json!({ "type": "null" }), json!(0), json!(null)),
			(json!({ "type": "null" }), json!(false), json!(null)),
		];
		for (schema, input, expected) in cases {
			assert_json(&coerce_value(schema, input), &expected);
		}
	}

	#[test]
	fn leaves_a_value_that_matches_one_of_a_type_arrays_types_else_takes_the_first_coercible_type() {
		assert_json(
			&coerce_value(json!({ "type": ["number", "string"] }), json!("1")),
			&json!("1"),
		);
		assert_json(
			&coerce_value(json!({ "type": ["boolean", "number"] }), json!("1")),
			&json!(1),
		);
		assert_json(
			&coerce_value(json!({ "type": ["integer", "null"] }), json!("5")),
			&json!(5),
		);
	}

	#[test]
	fn accepts_null_for_nullable_array_schemas_with_items() {
		let schema = json!({ "type": ["array", "null"], "items": { "type": "string" } });
		assert_json(&coerce_value(schema, json!(null)), &json!(null));
	}

	#[test]
	fn coerces_nullable_unions_when_the_value_matches_no_branch() {
		let any_of = json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] });
		assert_json(&coerce_value(any_of, json!("42")), &json!(42));
		let one_of = json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] });
		assert_json(&coerce_value(one_of, json!(null)), &json!(null));
	}

	#[test]
	fn leaves_values_no_rule_coerces() {
		let validator = coercing(
			&json!({ "type": "object", "properties": { "value": { "type": "integer" } }, "required": ["value"] }),
		);
		for input in [json!("abc"), json!("4x"), json!({}), json!(["1", "2"])] {
			let coerced = validator.coerce(&json!({ "value": input }));
			assert!(!validator.check(&coerced), "{coerced}");
		}
	}
}

mod where_the_passes_conflict_the_conversion_rules_win {
	use super::*;

	#[test]
	fn turns_null_into_null_text_for_a_required_string() {
		assert_json(&coerce_value(json!({ "type": "string" }), json!(null)), &json!("null"));
	}

	#[test]
	fn turns_an_empty_string_into_0_for_a_number() {
		assert_json(&coerce_value(json!({ "type": "number" }), json!("")), &json!(0));
	}

	#[test]
	fn turns_1_and_0_text_into_booleans() {
		assert_json(&coerce_value(json!({ "type": "boolean" }), json!("0")), &json!(false));
	}

	#[test]
	fn truncates_42_1_for_an_integer() {
		assert_json(&coerce_value(json!({ "type": "integer" }), json!("42.1")), &json!(42));
	}

	#[test]
	fn drops_an_optional_null_before_converting_it() {
		let schema = obj(vec![("name", opt(str(none())))], none());
		assert_json(&coerce(&schema, &json!({ "name": null })), &json!({}));
	}
}

mod recursive_schemas {
	use super::*;

	fn tree() -> JsonValue {
		json!({
			"type": "object",
			"properties": { "t": { "$ref": "#/$defs/Tree" } },
			"$defs": {
				"Tree": { "anyOf": [{ "type": "string" }, { "type": "array", "items": { "$ref": "#/$defs/Tree" } }] },
			},
		})
	}

	#[test]
	fn convert_through_a_recursive_union_by_its_first_accepting_branch() {
		let validator = coercing(&tree());
		assert_parsed(
			&validator.parse(&json!({ "t": ["a", ["b"]] })),
			&json!({ "t": ["a", ["b"]] }),
		);
		assert_parsed(&validator.parse(&json!({ "t": 5 })), &json!({ "t": "5" }));
		assert_parsed(&validator.parse(&json!({ "t": [5] })), &json!({ "t": ["5"] }));
		let json_schema = json!({
			"type": "object",
			"properties": { "data": { "$ref": "#/$defs/Json" } },
			"required": ["data"],
			"$defs": {
				"Json": {
					"anyOf": [
						{ "type": "string" },
						{ "type": "number" },
						{ "type": "boolean" },
						{ "type": "array", "items": { "$ref": "#/$defs/Json" } },
						{ "type": "object", "additionalProperties": { "$ref": "#/$defs/Json" } },
					],
				},
			},
		});
		assert_parsed(
			&coercing(&json_schema).parse(&json!({ "data": { "a": [1, null] } })),
			&json!({ "data": { "a": [1, "null"] } }),
		);
	}

	#[test]
	fn leave_a_value_a_recursive_schema_cannot_convert_as_it_is_for_the_check_to_report() {
		let validator = coercing(&tree());
		assert!(validator.parse(&json!({ "t": {} })).is_err());
		assert_eq!(
			validator.coerce(&json!({ "t": { "a": 1 } })),
			json!({ "t": { "a": 1 } })
		);
	}

	#[test]
	fn convert_a_scalar_or_array_schema_that_refers_to_itself() {
		let schema = json!({
			"type": "object",
			"properties": {
				"value": {
					"anyOf": [{ "type": "number" }, { "type": "array", "items": { "$ref": "#/properties/value" } }],
				},
			},
			"required": ["value"],
		});
		let validator = coercing(&schema);
		assert_parsed(&validator.parse(&json!({ "value": "42" })), &json!({ "value": 42 }));
		assert_parsed(
			&validator.parse(&json!({ "value": ["1", ["2"]] })),
			&json!({ "value": [1, [2]] }),
		);
		assert!(validator.parse(&json!({ "value": {} })).is_err());
	}

	// TS: "stop at a schema that cycles through shared JS objects instead of $ref". A JSON value cannot hold a cycle;
	// the same schema written with a `$ref` to the root.
	#[test]
	fn stop_at_a_schema_that_cycles_through_the_root() {
		let validator =
			coercing(&json!({ "anyOf": [{ "type": "number" }, { "type": "array", "items": { "$ref": "#" } }] }));
		assert_parsed(&validator.parse(&json!("42")), &json!(42));
		assert_parsed(&validator.parse(&json!(["1", ["2"]])), &json!([1, [2]]));
		assert!(validator.parse(&json!({})).is_err());
	}

	#[test]
	fn reset_the_reentry_state_between_calls_and_leave_the_input_unchanged() {
		let validator =
			coercing(&json!({ "anyOf": [{ "type": "number" }, { "type": "array", "items": { "$ref": "#" } }] }));
		for _ in 0..20 {
			assert!(validator.parse(&json!({})).is_err());
			assert_parsed(&validator.parse(&json!("42")), &json!(42));
			let input = json!([1, "2", ["3"]]);
			assert_parsed(&validator.parse(&input), &json!([1, 2, [3]]));
			assert_eq!(input, json!([1, "2", ["3"]]));
		}
	}

	#[test]
	fn convert_and_explain_a_deep_value_that_every_branch_of_a_recursive_union_rejects_in_linear_time() {
		// Each object branch converts and explains every child: without memoizing per call this is 2^depth work.
		let s = json!({
			"anyOf": [
				{ "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["a"] },
				{ "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["b"] },
			],
		});
		let validator =
			coercing(&json!({ "type": "object", "properties": { "s": { "$ref": "#/$defs/S" } }, "$defs": { "S": s } }));
		let mut value = json!({ "x": 1 });
		for _ in 0..40 {
			value = json!({ "c": value });
		}
		let mut expected: Vec<ValidationError> = (0..=40)
			.map(|i| err(&format!("/s{}/a", "/c".repeat(i)), "required", "missing"))
			.collect();
		expected.push(err(
			&format!("/s{}/x", "/c".repeat(40)),
			"type",
			"expected object, got integer",
		));
		assert_eq!(validator.parse(&json!({ "s": value })), Err(expected));
	}

	// TS builds this schema with TypeBox's `Type.Cyclic({ Tree: Type.Union([...]) }, "Tree")`.
	#[test]
	fn convert_a_typebox_cyclic_union() {
		let tree = json!({
			"$defs": {
				"Tree": {
					"anyOf": [{ "type": "string" }, { "type": "array", "items": { "$ref": "Tree" } }],
					"$id": "Tree",
				},
			},
			"$ref": "Tree",
		});
		let validator = coercing(&json!({ "type": "object", "properties": { "t": tree }, "required": ["t"] }));
		assert_parsed(&validator.parse(&json!({ "t": [5] })), &json!({ "t": ["5"] }));
		assert!(validator.parse(&json!({ "t": { "a": 1 } })).is_err());
	}
}

mod member_keywords_across_the_passes {
	use super::*;

	#[test]
	fn keep_a_pattern_matched_property_from_additional_properties_in_every_pass() {
		// TS builds this record with TypeBox's
		// `Type.Record(Type.String(), Type.Number(), { additionalProperties: Type.String() })`.
		let record = json!({
			"type": "object",
			"patternProperties": { "^.*$": { "type": "number" } },
			"additionalProperties": { "type": "string" },
		});
		assert_parsed(&coercing(&record).parse(&json!({ "n1": "1" })), &json!({ "n1": 1 }));
		let plain = json!({
			"type": "object",
			"patternProperties": { "^n_": { "type": ["integer", "null"] } },
			"additionalProperties": { "type": "string" },
		});
		assert_json(
			&coerce(&plain, &json!({ "n_a": "5", "other": 7 })),
			&json!({ "n_a": 5, "other": "7" }),
		);
	}

	#[test]
	fn drop_optional_nulls_inside_declared_pattern_and_additional_property_values_alike() {
		let item = obj(vec![("n", opt(int(json!({ "minimum": 1 }))))], none());
		let schemas = [
			obj(vec![("entry", item.clone().into())], none()),
			map(item.clone(), none()),
			json!({ "type": "object", "patternProperties": { "^entry$": item }, "additionalProperties": false }),
		];
		for schema in schemas {
			let input = json!({ "entry": { "n": null } });
			assert_parsed(&coercing(&schema).parse(&input), &json!({ "entry": {} }));
			assert_eq!(input, json!({ "entry": { "n": null } }));
		}
	}

	#[test]
	fn keep_a_null_under_pattern_and_additional_properties_their_keys_are_not_optional_properties() {
		let schema = json!({ "type": "object", "additionalProperties": { "type": ["integer", "null"] } });
		assert_parsed(&coercing(&schema).parse(&json!({ "n": null })), &json!({ "n": null }));
	}

	#[test]
	fn drop_optional_nulls_in_tuple_items_as_in_array_items() {
		let item = obj(vec![("n", opt(int(none())))], none());
		assert_json(
			&coercing(&arr(item.clone(), none())).coerce(&json!([{ "n": null }])),
			&json!([{}]),
		);
		assert_json(
			&coercing(&tuple(vec![item], none())).coerce(&json!([{ "n": null }])),
			&json!([{}]),
		);
	}

	#[test]
	fn coerce_tuple_items_by_type_in_pass_3() {
		let schema = json!({
			"type": "array",
			"prefixItems": [{ "type": ["integer", "null"] }, { "type": ["boolean", "null"] }],
		});
		assert_json(&coerce(&schema, &json!(["5", "true"])), &json!([5, true]));
	}

	#[test]
	fn convert_the_properties_a_union_node_declares_besides_its_branches() {
		let schema = json!({
			"type": "object",
			"properties": { "a": { "type": "integer" }, "b": { "type": "integer" } },
			"anyOf": [{ "required": ["a"] }, { "required": ["b"] }],
		});
		assert_parsed(&coercing(&schema).parse(&json!({ "a": "3.5" })), &json!({ "a": 3 }));
	}

	#[test]
	fn check_any_of_and_one_of_candidates_after_the_nodes_own_properties_convert() {
		for keyword in ["anyOf", "oneOf"] {
			let mut schema = json!({
				"type": "object",
				"properties": { "c": { "type": "integer" } },
				"required": ["c"],
			});
			schema[keyword] = json!([
				{
					"type": "object",
					"properties": { "a": { "type": "array", "items": { "type": "integer" } } },
					"required": ["a"],
				},
				{ "type": "object", "properties": { "b": { "type": "boolean" } }, "required": ["b"] },
			]);
			let input = json!({ "a": "3", "c": "2" });
			assert_parsed(&coercing(&schema).parse(&input), &json!({ "a": [3], "c": 2 }));
			assert_eq!(input, json!({ "a": "3", "c": "2" }));
		}
	}
}

// Rust-only: the memo of `$ref` results compares values key by key in order (review of C2), so a second value
// with the same members in another order keeps its own order, as TS's identity memo does.
#[test]
fn keep_the_key_order_of_each_value_converted_through_one_reference() {
	let schema = json!({
		"properties": { "x": { "$ref": "#/$defs/P" }, "y": { "$ref": "#/$defs/P" } },
		"$defs": { "P": { "type": "object", "properties": { "a": { "type": "number" }, "b": { "type": "number" } } } },
	});
	let coerced = coerce(
		&schema,
		&json!({ "x": { "a": "1", "b": "2" }, "y": { "b": "2", "a": "1" } }),
	);
	assert_eq!(
		pi_json::to_js_string(&coerced),
		r#"{"x":{"a":1,"b":2},"y":{"b":2,"a":1}}"#
	);
}
