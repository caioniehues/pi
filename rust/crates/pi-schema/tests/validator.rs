//! `compile()` and Validator (TS `test/validator.test.ts`): checks, errors, values, options and schemas from other
//! sources, case by case with every expected message. TS cases about values JSON cannot hold (`undefined`, bytes,
//! bigints, NaN) become the decided JSON behaviour: those types match no value.

#![allow(
	clippy::expect_used,
	reason = "test helpers fail the test on a schema that does not compile"
)]

use pi_json::JsonValue;
use pi_schema::{CompileError, CompileOptions, Compiled, Draft, UnresolvedRefs, ValidationError, compile, formats};
use serde_json::json;

fn plain(schema: &JsonValue) -> Compiled {
	compile(schema, CompileOptions::default()).expect("the schema compiles")
}

fn with(schema: &JsonValue, options: CompileOptions) -> Compiled {
	compile(schema, options).expect("the schema compiles")
}

fn err(path: &str, keyword: &str, message: &str) -> ValidationError {
	ValidationError::new(path, keyword, message)
}

fn read() -> JsonValue {
	json!({
		"type": "object",
		"properties": { "path": { "type": "string" }, "offset": { "type": "integer", "minimum": 1 } },
		"required": ["path"],
		"additionalProperties": false,
	})
}

fn block() -> JsonValue {
	json!({
		"anyOf": [
			{
				"type": "object",
				"properties": { "kind": { "const": "text" }, "text": { "type": "string" } },
				"required": ["kind", "text"],
				"additionalProperties": false,
			},
			{
				"type": "object",
				"properties": { "kind": { "const": "image" }, "width": { "type": "integer", "minimum": 0 } },
				"required": ["kind", "width"],
				"additionalProperties": false,
			},
		],
	})
}

mod errors {
	use super::*;

	#[test]
	fn reports_a_missing_field_and_an_unknown_field_at_the_field() {
		assert_eq!(
			plain(&read()).errors(&json!({ "offset": 0, "extra": true })),
			[
				err("/path", "required", "missing"),
				err("/offset", "minimum", "expected >= 1"),
				err("/extra", "additionalProperties", "unknown field"),
			]
		);
	}

	#[test]
	fn is_empty_for_a_valid_value() {
		assert_eq!(plain(&read()).errors(&json!({ "path": "a.ts" })), []);
	}

	#[test]
	fn reports_the_branch_a_tagged_unions_tag_selects() {
		let validator = plain(&json!({ "type": "array", "items": block() }));
		assert_eq!(
			validator.errors(&json!([{ "kind": "text", "text": "a" }, { "kind": "image", "width": -1 }])),
			[err("/1/width", "minimum", "expected >= 0")]
		);
		assert_eq!(
			validator.errors(&json!([{ "kind": "video" }])),
			[err("/0/kind", "anyOf", r#"expected one of ["text","image"]"#)]
		);
	}

	#[test]
	fn escapes_tilde_and_slash_in_paths_as_json_pointer_does() {
		let validator = plain(&json!({ "type": "object", "properties": { "a/b~c": { "type": "string" } } }));
		assert_eq!(
			validator.errors(&json!({ "a/b~c": 1 })),
			[err("/a~1b~0c", "type", "expected string, got integer")]
		);
	}

	// Rust-only: JS enumerates array-index keys first, ascending, then the others in insertion order (review of C2).
	#[test]
	fn reports_members_in_js_key_order() {
		let validator = plain(&json!({
			"properties": { "b": { "type": "string" }, "1": { "type": "string" } },
			"required": ["b", "1", "0"],
			"additionalProperties": false,
		}));
		let value = pi_json::parse(r#"{"z":1,"2":1,"a":1,"10":1,"b":5,"1":5}"#).expect("JSON");
		let paths: Vec<String> = validator.errors(&value).into_iter().map(|e| e.path).collect();
		assert_eq!(paths, ["/0", "/1", "/b", "/2", "/10", "/z", "/a"]);
	}

	#[test]
	fn follows_ref_into_the_referenced_schema() {
		let validator = plain(&json!({
			"$defs": { "point": { "type": "object", "properties": { "x": { "type": "number" } }, "required": ["x"] } },
			"type": "array",
			"items": { "$ref": "#/$defs/point" },
		}));
		assert_eq!(
			validator.errors(&json!([{ "x": 1 }, {}])),
			[err("/1/x", "required", "missing")]
		);
	}
}

mod values {
	use super::*;

	// TS: "treats a property set to undefined as absent". JSON has no undefined: a null is a value.
	#[test]
	fn treats_an_optional_property_set_to_null_as_present() {
		let validator = plain(&read());
		assert!(validator.check(&json!({ "path": "a.ts" })));
		assert!(!validator.check(&json!({ "path": "a.ts", "offset": null })));
		assert!(!validator.check(&json!({})));
	}

	// TS: "checks bytes as Uint8Array with length bounds". No JSON value is bytes.
	#[test]
	fn rejects_every_value_for_bytes() {
		let validator =
			plain(&json!({ "type": "object", "properties": { "data": { "type": "bytes", "maxLength": 4 } } }));
		assert!(validator.check(&json!({})));
		for data in [json!([1, 2]), json!("abcd"), json!({}), json!(null)] {
			assert!(!validator.check(&json!({ "data": data })));
		}
	}

	#[test]
	fn checks_properties_named_like_object_prototype_members_as_own_properties() {
		let validator = plain(&json!({
			"type": "object",
			"properties": { "constructor": { "type": "string" }, "toString": { "type": "number" } },
			"required": ["constructor"],
			"additionalProperties": false,
		}));
		assert!(!validator.check(&json!({})));
		assert!(validator.check(&json!({ "constructor": "a" })));
		assert!(!validator.check(&json!({ "constructor": "a", "toString": "b" })));
	}

	#[test]
	fn does_not_accept_every_value_for_a_type_named_like_an_object_prototype_member() {
		assert!(!plain(&json!({ "type": "constructor" })).check(&json!(1)));
	}

	#[test]
	fn checks_a_detached_check_function() {
		let validator = plain(&read());
		let check = |v: &&JsonValue| validator.check(v);
		let values = [json!({ "path": "a" }), json!({ "path": 1 })];
		let valid: Vec<&JsonValue> = values.iter().filter(check).collect();
		assert_eq!(valid, [&json!({ "path": "a" })]);
	}
}

mod options {
	use super::*;

	#[test]
	fn treats_format_as_an_annotation_unless_formats_are_given() {
		let schema = json!({ "type": "string", "format": "email" });
		let with_formats = || CompileOptions {
			formats: formats::standard(),
			..CompileOptions::default()
		};
		assert!(plain(&schema).check(&json!("not an email")));
		assert!(!with(&schema, with_formats()).check(&json!("not an email")));
		assert!(with(&schema, with_formats()).check(&json!("a@example.com")));
	}

	#[test]
	fn resolves_ref_to_registered_documents() {
		let point = json!({ "type": "object", "properties": { "x": { "type": "number" } }, "required": ["x"] });
		let options = CompileOptions {
			schemas: vec![("https://example.com/point.json".to_owned(), point)],
			..CompileOptions::default()
		};
		let validator = with(&json!({ "$ref": "https://example.com/point.json" }), options);
		assert!(validator.check(&json!({ "x": 1 })));
		assert!(!validator.check(&json!({})));
	}

	#[test]
	fn throws_for_a_ref_that_names_nothing() {
		for reference in ["https://example.com/missing.json", "#/$defs/missing"] {
			let result = compile(&json!({ "$ref": reference }), CompileOptions::default());
			let message = result.err().map(|e| e.to_string()).unwrap_or_default();
			assert!(message.starts_with("Unresolved $ref"), "{message}");
		}
	}

	#[test]
	fn applies_draft_7_semantics_ref_overrides_its_siblings() {
		let schema = json!({ "definitions": { "s": { "type": "string" } }, "$ref": "#/definitions/s", "maxLength": 1 });
		let draft7 = CompileOptions {
			draft: Draft::V7,
			..CompileOptions::default()
		};
		assert!(with(&schema, draft7).check(&json!("ab")));
		assert!(!plain(&schema).check(&json!("ab")));
	}
}

mod schemas {
	use super::*;

	// TS builds this schema with TypeBox's `Type.Object({ name: Type.String(), tags: Type.Array(Type.String()) })`.
	#[test]
	fn compiles_typebox_schemas_as_they_are() {
		let validator = plain(&json!({
			"type": "object",
			"properties": { "name": { "type": "string" }, "tags": { "type": "array", "items": { "type": "string" } } },
			"required": ["name", "tags"],
		}));
		assert!(validator.check(&json!({ "name": "a", "tags": [] })));
		assert_eq!(
			validator.errors(&json!({ "name": "a", "tags": [1] })),
			[err("/tags/0", "type", "expected string, got integer")]
		);
	}

	#[test]
	fn compiles_deep_frozen_schemas_and_never_modifies_a_schema() {
		let mut schema = block();
		schema["$defs"] = json!({ "s": { "type": "string", "pattern": "^a" } });
		let before = pi_json::to_js_string(&schema);
		let validator = plain(&schema);
		assert!(validator.check(&json!({ "kind": "text", "text": "x" })));
		assert_eq!(validator.errors(&json!({ "kind": "image", "width": -1 })).len(), 1);
		assert_eq!(pi_json::to_js_string(&schema), before);
	}

	#[test]
	fn compiles_a_recursive_schema() {
		let validator = plain(&json!({
			"type": "object",
			"properties": { "value": { "type": "number" }, "children": { "type": "array", "items": { "$ref": "#" } } },
			"required": ["value"],
		}));
		assert!(validator.check(&json!({ "value": 1, "children": [{ "value": 2, "children": [{ "value": 3 }] }] })));
		assert_eq!(
			validator.errors(&json!({ "value": 1, "children": [{ "value": 2, "children": [{}] }] })),
			[err("/children/0/children/0/value", "required", "missing")]
		);
	}

	// #11697
	#[test]
	fn applies_a_relative_root_id_once_when_resolving_local_references() {
		let schema = json!({
			"$id": "schemas/tool.json",
			"type": "object",
			"properties": { "count": { "$ref": "#/$defs/count" } },
			"required": ["count"],
			"$defs": { "count": { "type": "integer" } },
		});
		assert!(plain(&schema).check(&json!({ "count": 2 })));
		let reject = CompileOptions {
			unresolved_refs: UnresolvedRefs::Reject,
			..CompileOptions::default()
		};
		let validator = with(&schema, reject);
		assert!(validator.check(&json!({ "count": 2 })));
		assert_eq!(
			validator.errors(&json!({ "count": "a" })),
			[err("/count", "type", "expected integer, got string")]
		);
		let coercing = CompileOptions {
			coerce: true,
			..CompileOptions::default()
		};
		let parsed = with(&schema, coercing)
			.coercing()
			.map(|c| c.parse(&json!({ "count": "2" })));
		assert_eq!(parsed, Some(Ok(json!({ "count": 2 }))));
	}

	#[test]
	fn applies_a_relative_root_id_once_in_schemas_with_unevaluated_keywords() {
		let reject = || CompileOptions {
			unresolved_refs: UnresolvedRefs::Reject,
			..CompileOptions::default()
		};
		let props = json!({
			"$id": "schemas/tool.json",
			"type": "object",
			"properties": { "count": { "$ref": "#/$defs/count" } },
			"$defs": { "count": { "type": "integer" } },
			"unevaluatedProperties": false,
		});
		let validator = with(&props, reject());
		assert!(validator.check(&json!({ "count": 2 })));
		assert_eq!(
			validator.errors(&json!({ "count": 2, "extra": 1 })),
			[err("/extra", "unevaluatedProperties", "unknown field")]
		);
		let items = json!({
			"$id": "schemas/list.json",
			"type": "array",
			"prefixItems": [{ "$ref": "#/$defs/count" }],
			"$defs": { "count": { "type": "integer" } },
			"unevaluatedItems": false,
		});
		assert!(with(&items, reject()).check(&json!([2])));
		assert!(!plain(&items).check(&json!([2, 3])));
	}

	#[test]
	fn explains_errors_under_nested_resources_with_a_relative_id() {
		let cloned = json!({
			"$id": "https://example.com/root.json",
			"allOf": [{ "$ref": "#/$defs/child" }],
			"$defs": {
				"child": {
					"$id": "schemas/tool.json",
					"$ref": "#/$defs/props",
					"unevaluatedProperties": false,
					"$defs": { "props": { "properties": { "count": { "type": "integer" } } } },
				},
			},
		});
		assert_eq!(
			plain(&cloned).errors(&json!({ "count": 2, "extra": 1 })),
			[err("/extra", "unevaluatedProperties", "unknown field")]
		);
		let branch = json!({
			"$id": "https://example.com/root.json",
			"anyOf": [
				{ "$id": "a/b.json", "$ref": "#/$defs/n", "$defs": { "n": { "type": "integer" } } },
				{ "type": "boolean" },
			],
		});
		assert_eq!(
			plain(&branch).errors(&json!({})),
			[err("", "type", "expected integer, got object")]
		);
	}

	#[test]
	fn throws_for_a_malformed_schema() {
		let result = compile(&json!({ "properties": { "a": 1 } }), CompileOptions::default());
		assert_eq!(result.err(), Some(CompileError::Malformed("1".to_owned())));
		let message = CompileError::Malformed("1".to_owned()).to_string();
		assert!(message.contains("must be an object or a boolean"), "{message}");
	}
}

mod unresolved_references {
	use super::*;

	fn dangling() -> JsonValue {
		json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } })
	}

	fn external() -> JsonValue {
		json!({ "type": "object", "properties": { "t": { "$ref": "https://example.com/schemas/t.json" } } })
	}

	fn reject() -> CompileOptions {
		CompileOptions {
			unresolved_refs: UnresolvedRefs::Reject,
			..CompileOptions::default()
		}
	}

	#[test]
	fn throw_when_the_schema_compiles_by_default() {
		let message = |schema: &JsonValue| compile(schema, CompileOptions::default()).err().map(|e| e.to_string());
		assert!(message(&dangling()).is_some_and(|m| m.contains("Unresolved $ref #/definitions/Missing")));
		assert!(message(&external()).is_some_and(|m| m.contains("Unresolved $ref https://example.com/schemas/t.json")));
	}

	#[test]
	fn reject_only_values_at_their_place_with_unresolved_refs_reject() {
		for schema in [dangling(), external()] {
			let validator = with(&schema, reject());
			assert!(validator.check(&json!({})));
			assert!(!validator.check(&json!({ "t": 1 })));
		}
		assert_eq!(
			with(&dangling(), reject()).errors(&json!({ "t": 1 })),
			[err("/t", "$ref", "unresolved reference #/definitions/Missing")]
		);
	}

	#[test]
	fn leave_values_at_their_place_uncoerced() {
		let options = CompileOptions {
			coerce: true,
			..reject()
		};
		let parsed = with(&dangling(), options)
			.coercing()
			.map(|c| c.parse(&json!({ "t": "1" })));
		assert_eq!(
			parsed,
			Some(Err(vec![err(
				"/t",
				"$ref",
				"unresolved reference #/definitions/Missing"
			)]))
		);
	}

	// TS builds this schema with TypeBox's `Type.Object({ a: Type.Ref("A") })`.
	#[test]
	fn reject_a_typebox_ref_without_its_definition() {
		let schema = json!({ "type": "object", "properties": { "a": { "$ref": "A" } }, "required": ["a"] });
		assert_eq!(
			with(&schema, reject()).errors(&json!({ "a": { "x": "1" } })),
			[err("/a", "$ref", "unresolved reference A")]
		);
	}
}

mod leaf_checks_inlined_by_object_and_array_loops {
	use super::*;

	#[test]
	fn agree_with_the_standalone_check_as_a_property_a_pattern_property_and_an_item() {
		let leaves = [
			(json!({ "const": "a" }), json!(["a", "b", 1, null])),
			(json!({ "const": null }), json!([null, 0, false, "null"])),
			(json!({ "enum": ["a", 1, null] }), json!(["a", 1, null, "b", 2, true])),
			(json!({ "type": ["string", "null"] }), json!(["a", null, 1, {}, []])),
			(json!({ "type": ["integer", "boolean"] }), json!([1, 1.5, true, "1"])),
			(json!({ "type": ["number", "array"] }), json!([1.5, 2, [], {}])),
			(json!({ "type": ["object", "bytes"] }), json!([{}, [], null])),
			(
				json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }),
				json!(["a", null, 1]),
			),
			(
				json!({ "anyOf": [{ "type": "integer" }, { "type": "string", "minLength": 2 }] }),
				json!([1, 1.5, "ab", "a"]),
			),
			(
				json!({ "type": "integer", "minimum": 1, "exclusiveMaximum": 5 }),
				json!([1, 4, 5, 0, 2.5, "2"]),
			),
			(
				json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 1 }),
				json!([0.5, 1, 0, 1.5, "0.5"]),
			),
			(json!({ "type": "integer", "multipleOf": 2 }), json!([2, 3, 2.5])),
			(
				json!({ "type": "string", "minLength": 2, "maxLength": 3 }),
				json!(["ab", "abc", "a", "abcd", "😀😀", 12]),
			),
			(json!({ "type": "string", "pattern": "^a+$" }), json!(["aa", "ab", 1])),
			(json!({ "minimum": 1 }), json!([1, 0, "x", null])),
		];
		for (schema, values) in leaves {
			let alone = plain(&schema);
			let closed =
				plain(&json!({ "type": "object", "properties": { "p": schema }, "additionalProperties": false }));
			let open = plain(&json!({ "type": "object", "properties": { "p": schema }, "required": ["q"] }));
			let items = plain(&json!({ "type": "array", "items": schema }));
			for v in values.as_array().into_iter().flatten() {
				let expected = alone.check(v);
				let actual = [
					closed.check(&json!({ "p": v })),
					open.check(&json!({ "p": v, "q": 1 })),
					items.check(&json!([v])),
				];
				assert_eq!(actual, [expected; 3], "{schema} {v}");
			}
		}
	}

	#[test]
	fn find_properties_in_any_order() {
		let validator = plain(&json!({
			"type": "object",
			"properties": { "a": { "const": 1 }, "b": { "type": "string" }, "c": { "type": "integer", "minimum": 0 } },
			"required": ["a", "c"],
			"additionalProperties": false,
		}));
		assert!(validator.check(&json!({ "a": 1, "b": "x", "c": 0 })));
		assert!(validator.check(&json!({ "c": 0, "b": "x", "a": 1 })));
		assert!(validator.check(&json!({ "b": "x", "c": 0, "a": 1 })));
		assert!(!validator.check(&json!({ "a": 1, "c": 0, "a2": 1 })));
		assert!(!validator.check(&json!({ "c": -1, "a": 1 })));
		assert!(!validator.check(&json!({ "c": 0, "b": 1, "a": 1 })));
		// TS: `{ c: 0, b: undefined, a: 1 }` passes; JSON has no undefined, and a null `b` is not a string.
		assert!(!validator.check(&json!({ "c": 0, "b": null, "a": 1 })));
		assert!(!validator.check(&json!({ "c": 0, "b": "x" })));
	}
}

mod string_lengths {
	use super::*;

	// TS: "count an unpaired high surrogate as one code point and a pair as one". A Rust string holds no unpaired
	// surrogate; a pair is one character.
	#[test]
	fn count_a_surrogate_pair_as_one_code_point() {
		assert!(plain(&json!({ "type": "string", "maxLength": 1 })).check(&json!("😀")));
		assert!(!plain(&json!({ "type": "string", "minLength": 2 })).check(&json!("😀")));
		assert!(plain(&json!({ "type": "string", "minLength": 2 })).check(&json!("😀A")));
		assert!(!plain(&json!({ "type": "string", "maxLength": 1 })).check(&json!("😀A")));
	}
}

mod error_messages {
	use super::*;

	#[test]
	fn list_at_most_20_enum_values() {
		let values: Vec<JsonValue> = (0..5000).map(|i| json!(format!("v{i}"))).collect();
		let validator =
			plain(&json!({ "type": "object", "properties": { "e": { "type": "string", "enum": values } } }));
		assert!(validator.check(&json!({ "e": "v4999" })));
		let errors = validator.errors(&json!({ "e": "v5000" }));
		let listed = pi_json::to_js_string(&JsonValue::Array(values[..20].to_vec()));
		assert_eq!(
			errors[0].message,
			format!("expected one of {}, ... and 4980 more]", &listed[..listed.len() - 1])
		);
		assert_eq!(
			plain(&json!({ "enum": ["a", "b"] })).errors(&json!("c")),
			[err("", "enum", r#"expected one of ["a","b"]"#)]
		);
	}

	#[test]
	fn accept_a_10000_value_enum() {
		let values: Vec<JsonValue> = (0..10_000).map(|i| json!(format!("member{i}"))).collect();
		assert!(plain(&json!({ "enum": values })).check(&json!("member9999")));
	}

	#[test]
	fn describe_contains_bounds_in_words() {
		let at = |schema: JsonValue| {
			let mut schema = schema;
			schema["type"] = json!("array");
			plain(&schema).errors(&json!(["a"])).first().map(|e| e.message.clone())
		};
		assert_eq!(
			at(json!({ "contains": { "type": "number" } })).as_deref(),
			Some("expected at least 1 matching item")
		);
		assert_eq!(
			at(json!({ "contains": { "type": "number" }, "minContains": 2 })).as_deref(),
			Some("expected at least 2 matching items")
		);
		assert_eq!(
			at(json!({ "contains": { "type": "string" }, "maxContains": 0, "minContains": 0 })).as_deref(),
			Some("expected at most 0 matching items")
		);
		assert_eq!(
			at(json!({ "contains": { "type": "number" }, "minContains": 1, "maxContains": 3 })).as_deref(),
			Some("expected between 1 and 3 matching items")
		);
	}

	#[test]
	fn blame_a_one_of_failure_on_the_branch_that_declares_most_of_the_objects_properties() {
		let filter = json!({
			"type": "object",
			"properties": { "where": { "$ref": "#/$defs/F" } },
			"$defs": {
				"F": {
					"oneOf": [
						{
							"type": "object",
							"properties": { "field": { "type": "string" }, "eq": { "type": "string" } },
							"required": ["field", "eq"],
							"additionalProperties": false,
						},
						{
							"type": "object",
							"properties": { "and": { "type": "array", "items": { "$ref": "#/$defs/F" } } },
							"required": ["and"],
							"additionalProperties": false,
						},
					],
				},
			},
		});
		assert_eq!(
			plain(&filter).errors(&json!({ "where": { "and": [{ "field": "a", "eq": 1, "x": 2 }] } })),
			[
				err("/where/and/0/eq", "type", "expected string, got integer"),
				err("/where/and/0/x", "additionalProperties", "unknown field"),
			]
		);
	}

	// TS passes one object at both paths; here they are two equal values, explained at each path all the same.
	#[test]
	fn explain_one_object_reached_through_the_same_reference_at_two_paths_at_each_path() {
		let schema = json!({
			"type": "object",
			"properties": { "a": { "$ref": "#/$defs/P" }, "b": { "$ref": "#/$defs/P" } },
			"$defs": { "P": { "type": "object", "properties": { "n": { "type": "integer" } } } },
		});
		assert_eq!(
			plain(&schema).errors(&json!({ "a": { "n": "x" }, "b": { "n": "x" } })),
			[
				err("/a/n", "type", "expected integer, got string"),
				err("/b/n", "type", "expected integer, got string"),
			]
		);
	}
}

mod typeboxs_extra_formats {
	use super::*;

	#[test]
	fn check_url_and_json_pointer_uri_fragment() {
		let options = || CompileOptions {
			formats: formats::standard(),
			..CompileOptions::default()
		};
		let url = with(&json!({ "type": "string", "format": "url" }), options());
		assert!(url.check(&json!("https://example.com/a?b=1")));
		assert!(!url.check(&json!("not a url")));
		let fragment = with(
			&json!({ "type": "string", "format": "json-pointer-uri-fragment" }),
			options(),
		);
		assert!(fragment.check(&json!("#/a/b~1c")));
		assert!(!fragment.check(&json!("#/a b")));
		assert!(!fragment.check(&json!("/a")));
	}
}

mod typeboxs_javascript_types {
	use super::*;

	// TS: "check Type.BigInt with its bounds". No JSON value is a bigint: `{ type: "bigint" }` matches nothing,
	// with or without bounds, and a union with null takes only null.
	#[test]
	fn check_type_bigint_as_matching_no_json_value() {
		let schema = json!({ "anyOf": [{ "type": "bigint", "minimum": 0 }, { "type": "null" }] });
		let validator = plain(&schema);
		assert!(validator.check(&json!(null)));
		for value in [json!(5), json!(0), json!("5")] {
			assert!(!validator.check(&value));
		}
		assert!(!plain(&json!({ "type": "bigint", "maximum": 10, "multipleOf": 5 })).check(&json!(10)));
		assert_eq!(
			plain(&json!({ "type": "bigint" })).errors(&json!(2)),
			[err("", "type", "expected bigint, got integer")]
		);
	}

	// TS: "check Type.Undefined: only undefined, and a required property with it must be present". JSON has no
	// undefined: the type matches nothing, so a required property of it can never be valid.
	#[test]
	fn check_type_undefined_as_matching_no_json_value() {
		assert!(!plain(&json!({ "type": "undefined" })).check(&json!(null)));
		let schema = json!({
			"type": "object",
			"properties": { "selection": { "const": "average" }, "principalId": { "type": "undefined" } },
			"required": ["selection", "principalId"],
		});
		let validator = plain(&schema);
		assert!(!validator.check(&json!({ "selection": "average" })));
		assert!(!validator.check(&json!({ "selection": "average", "principalId": null })));
		assert_eq!(
			validator.errors(&json!({ "selection": "average" })),
			[err("/principalId", "required", "missing")]
		);
		assert_eq!(
			validator.errors(&json!({ "selection": "average", "principalId": "x" })),
			[err("/principalId", "type", "expected undefined, got string")]
		);
	}

	// TS: "check a required property set to undefined by its schema". Without undefined, a required property must
	// be present, whatever its schema.
	#[test]
	fn require_a_required_property_whatever_its_schema() {
		for property in [
			json!({ "anyOf": [{ "type": "string" }, { "type": "undefined" }] }),
			json!({}),
		] {
			for closed in [false, true] {
				let mut schema = json!({ "type": "object", "properties": { "a": property }, "required": ["a"] });
				if closed {
					schema["additionalProperties"] = json!(false);
				}
				let validator = plain(&schema);
				assert!(!validator.check(&json!({})));
			}
		}
		let name =
			plain(&json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }));
		assert_eq!(name.errors(&json!({})), [err("/name", "required", "missing")]);
		let optional = plain(&json!({ "type": "object", "properties": { "name": { "type": "string" } } }));
		assert!(optional.check(&json!({})));
	}

	// TS: "check multipleOf of bigints exactly, however large". A JSON number is a double: a multiple of 0 or of a
	// fraction it is not a multiple of fails.
	#[test]
	fn check_multiple_of_of_numbers_exactly() {
		assert!(plain(&json!({ "multipleOf": 5 })).check(&json!(1e300)));
		assert!(!plain(&json!({ "multipleOf": 0 })).check(&json!(4)));
		assert!(plain(&json!({ "multipleOf": 0.5 })).check(&json!(3)));
		assert!(!plain(&json!({ "multipleOf": 0.5 })).check(&json!(3.25)));
	}
}

// Not in TS: validators are shared across threads (tool definitions are long-lived and reused by concurrent calls).
#[test]
fn validators_are_send_and_sync() {
	fn shareable<T: Send + Sync>() {}
	shareable::<Compiled>();
	shareable::<pi_schema::Validator>();
	shareable::<pi_schema::CoercingValidator>();
}
