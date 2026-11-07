//! `JSON.stringify` of Node 26, byte for byte: compact and indented output, key order, string escapes and numbers.

use crate::HEX_DIGITS;
use crate::number::js_number_to_string;
use serde_json::{Map, Number, Value};

/// Serializes a value exactly as `JSON.stringify(value)` does in Node 26.
pub fn to_js_string(value: &Value) -> String {
	let mut out = String::new();
	write_value(value, "", 0, &mut out);
	out
}

/// `JSON.stringify` clamps the indent to ten spaces.
const MAX_INDENT: usize = 10;

/// Serializes a value exactly as `JSON.stringify(value, null, indent)` does in Node 26.
pub fn to_js_string_pretty(value: &Value, indent: usize) -> String {
	// TS parity: an indent above ten prints ten spaces, an indent of zero prints the compact form.
	let gap = " ".repeat(indent.min(MAX_INDENT));
	let mut out = String::new();
	write_value(value, &gap, 0, &mut out);
	out
}

/// Writes one value at nesting `depth`; an empty `gap` is the compact form, as in the spec's `SerializeJSONProperty`.
fn write_value(value: &Value, gap: &str, depth: usize, out: &mut String) {
	match value {
		Value::Null => out.push_str("null"),
		Value::Bool(true) => out.push_str("true"),
		Value::Bool(false) => out.push_str("false"),
		Value::Number(number) => write_number(number, out),
		Value::String(text) => write_string(text, out),
		Value::Array(items) => write_array(items, gap, depth, out),
		Value::Object(map) => write_object(map, gap, depth, out),
	}
}

fn write_number(number: &Number, out: &mut String) {
	// TS parity: every JS number is a double, so integers past 2^53 print rounded, as Node would after parsing them.
	// invariant: `as_f64` fails only for an `arbitrary_precision` Number outside the f64 range. The workspace never
	// enables that serde_json feature, but any crate in a build can (features unify) and no compile-time guard can
	// see a feature another crate turns on; the 0 below would then print for such a number instead of failing.
	let value = number.as_f64().unwrap_or(0.0);
	out.push_str(&js_number_to_string(value));
}

/// Escapes `"`, `\` and control characters below U+0020, as `JSON.stringify` does; everything else is written raw.
fn write_string(text: &str, out: &mut String) {
	// Node also escapes lone surrogates as `\udXXX`; a Rust `str` is valid UTF-8 and cannot hold one.
	out.reserve(text.len() + 2);
	out.push('"');
	let mut start = 0;
	for (index, byte) in text.bytes().enumerate() {
		let short = match byte {
			b'"' => Some("\\\""),
			b'\\' => Some("\\\\"),
			b'\x08' => Some("\\b"),
			b'\x0c' => Some("\\f"),
			b'\n' => Some("\\n"),
			b'\r' => Some("\\r"),
			b'\t' => Some("\\t"),
			0x00..=0x1f => None,
			_ => continue,
		};
		out.push_str(&text[start..index]);
		match short {
			Some(escape) => out.push_str(escape),
			None => push_unicode_escape(byte, out),
		}
		start = index + 1;
	}
	out.push_str(&text[start..]);
	out.push('"');
}

fn push_unicode_escape(byte: u8, out: &mut String) {
	out.push_str("\\u00");
	out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
	out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
}

fn write_array(items: &[Value], gap: &str, depth: usize, out: &mut String) {
	if items.is_empty() {
		out.push_str("[]");
		return;
	}
	out.push('[');
	for (index, item) in items.iter().enumerate() {
		write_separator(index, gap, depth + 1, out);
		write_value(item, gap, depth + 1, out);
	}
	write_line_break(gap, depth, out);
	out.push(']');
}

fn write_object(map: &Map<String, Value>, gap: &str, depth: usize, out: &mut String) {
	if map.is_empty() {
		out.push_str("{}");
		return;
	}
	// Most objects have no index keys; they keep insertion order without the sort's allocation.
	if map.keys().any(|key| array_index(key).is_some()) {
		write_members(js_key_order(map), gap, depth, out);
	} else {
		write_members(map.iter(), gap, depth, out);
	}
}

fn write_members<'a>(
	members: impl Iterator<Item = (&'a String, &'a Value)>,
	gap: &str,
	depth: usize,
	out: &mut String,
) {
	out.push('{');
	for (index, (key, value)) in members.enumerate() {
		write_separator(index, gap, depth + 1, out);
		write_string(key, out);
		out.push(':');
		if !gap.is_empty() {
			out.push(' ');
		}
		write_value(value, gap, depth + 1, out);
	}
	write_line_break(gap, depth, out);
	out.push('}');
}

/// The order a JS object enumerates its keys in: array indices ascending, then the other keys in insertion order.
fn js_key_order(map: &Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
	let mut indexed: Vec<(u32, &String, &Value)> = map
		.iter()
		.filter_map(|(key, value)| array_index(key).map(|index| (index, key, value)))
		.collect();
	indexed.sort_unstable_by_key(|&(index, _, _)| index);
	let named = map.iter().filter(|(key, _)| array_index(key).is_none());
	indexed.into_iter().map(|(_, key, value)| (key, value)).chain(named)
}

/// The largest array index is 2^32 - 2 (ECMA-262 6.1.7); `"4294967295"` is an ordinary key.
const MAX_ARRAY_INDEX: u32 = u32::MAX - 1;

/// The array index a key names: canonical decimal (no sign, no leading zero) up to 2^32 - 2.
fn array_index(key: &str) -> Option<u32> {
	let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
	if !canonical || key.len() > 10 {
		return None;
	}
	key.parse::<u32>().ok().filter(|&index| index <= MAX_ARRAY_INDEX)
}

/// Writes `,` before every item but the first, then the line break and indent of `depth` when indenting.
fn write_separator(index: usize, gap: &str, depth: usize, out: &mut String) {
	if index > 0 {
		out.push(',');
	}
	write_line_break(gap, depth, out);
}

fn write_line_break(gap: &str, depth: usize, out: &mut String) {
	if gap.is_empty() {
		return;
	}
	out.push('\n');
	for _ in 0..depth {
		out.push_str(gap);
	}
}

#[cfg(test)]
mod tests {
	use super::{to_js_string, to_js_string_pretty};
	use serde_json::json;

	#[test]
	fn index_keys_come_first_in_ascending_order() {
		let value: serde_json::Value =
			serde_json::from_str(r#"{"b":1,"10":2,"a":3,"2":4,"4294967295":5,"01":6}"#).unwrap();
		assert_eq!(
			to_js_string(&value),
			r#"{"2":4,"10":2,"b":1,"a":3,"4294967295":5,"01":6}"#
		);
	}

	#[test]
	fn indent_is_clamped_to_ten_and_zero_is_compact() {
		let value = json!({"a": [1]});
		assert_eq!(to_js_string_pretty(&value, 0), r#"{"a":[1]}"#);
		assert_eq!(to_js_string_pretty(&value, 4), "{\n    \"a\": [\n        1\n    ]\n}");
		assert_eq!(to_js_string_pretty(&value, 12), to_js_string_pretty(&value, 10));
		assert_eq!(
			to_js_string_pretty(&json!({"e": {}, "f": []}), 2),
			"{\n  \"e\": {},\n  \"f\": []\n}"
		);
	}

	#[test]
	fn escapes_only_quotes_backslashes_and_control_characters() {
		let value = json!("\"\\/\u{1}\u{1f}\u{7f}\u{2028}\u{2029}\u{1F600}\x08\x0c\n\r\t");
		assert_eq!(
			to_js_string(&value),
			"\"\\\"\\\\/\\u0001\\u001f\u{7f}\u{2028}\u{2029}\u{1F600}\\b\\f\\n\\r\\t\""
		);
	}
}
