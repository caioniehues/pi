//! `equal()` (TS `test/equal.test.ts`): JSON equality, a drop-in for TypeBox's `Value.Equal`. TS compares against
//! TypeBox's results; these are them for the JSON cases (`null` vs `undefined` and `{ a: undefined }` have no JSON
//! form).

use pi_schema::equal;
use serde_json::json;

#[test]
fn compares_json_values_structurally_as_typeboxs_value_equal_does() {
	let cases = [
		(json!(1), json!(1), true),
		(json!("a"), json!("b"), false),
		(json!(null), json!(null), true),
		(
			json!({ "a": 1, "b": [1, { "c": 2 }] }),
			json!({ "b": [1, { "c": 2 }], "a": 1 }),
			true,
		),
		(json!({ "a": 1 }), json!({ "a": 1, "b": 2 }), false),
		(json!([1, 2]), json!([2, 1]), false),
		(json!([1]), json!({ "0": 1 }), false),
		(json!({}), json!([]), false),
		(json!(1), json!(1.0), true),
	];
	for (a, b, expected) in cases {
		assert_eq!(equal(&a, &b), expected, "{a} {b}");
	}
}
