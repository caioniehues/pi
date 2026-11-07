//! The normalizer: rewrites what legitimately differs between two runs (ids, times, temp paths, the server port,
//! versions, timings) to stable placeholders, with one id mapping shared by all artefacts of a run so
//! cross-references survive. Everything else, key order and number formatting included, is kept: lines are
//! compared as text after normalization.

use pi_json::{JsonObject, JsonValue};
use std::collections::HashMap;
use std::path::Path;

/// One run's normalizer; feed it the artefacts in `ALL_ARTEFACTS` order so ids number the same way every time.
pub struct Normalizer {
	/// Literal replacements, longest first: temp paths and the server address.
	literals: Vec<(String, &'static str)>,
	uuids: Mapping,
	entry_ids: Mapping,
}

/// Values keyed by these names are replaced whatever they hold: they are timings, versions or ids that a
/// faithful implementation still cannot reproduce.
const KEYED: &[(&str, &str)] = &[
	("durationMs", "$DURATION"),
	("wall_time_seconds", "$DURATION"),
	("responseId", "$RESPONSE_ID"),
];

/// Request headers that the TS SDKs and undici add, not pi; they are no parity requirement (decided divergence,
/// rust/docs/slices.md), so `requests_jsonl` drops them. Headers pi sets stay compared.
const SDK_HEADERS: &[&str] = &[
	"accept-encoding",
	"accept-language",
	"connection",
	"content-length",
	"host",
	"sec-fetch-mode",
	"user-agent",
];

/// The prefix of the Stainless SDK headers, dropped like `SDK_HEADERS`.
const SDK_HEADER_PREFIX: &str = "x-stainless-";

impl Normalizer {
	/// A normalizer for a run whose temp root is `root` and whose fake provider listens on `server`.
	pub fn new(root: &Path, server: &str) -> Self {
		let root = root.display().to_string();
		// The session directory name encodes the cwd (`--<path without the leading slash, / as ->--`).
		let encoded_root = root.trim_start_matches('/').replace(['/', '\\', ':'], "-");
		let mut literals = vec![(root, "$ROOT"), (encoded_root, "$ROOT"), (server.to_owned(), "$SERVER")];
		literals.sort_by_key(|(text, _)| std::cmp::Reverse(text.len()));
		Self {
			literals,
			uuids: Mapping::new("uuid"),
			entry_ids: Mapping::new("id"),
		}
	}

	/// Normalizes JSONL text line by line. A line that is not the `JSON.stringify` form of its own value (or not
	/// JSON) becomes `{"$noncanonical": "<line>"}` with only text rules applied, so formatting differences show.
	pub fn jsonl(&mut self, text: &str) -> String {
		self.lines(text, |_| {})
	}

	/// `jsonl` for the fake provider's `requests.jsonl`: drops the SDK headers of each record first.
	pub fn requests_jsonl(&mut self, text: &str) -> String {
		self.lines(text, |record| {
			if let Some(JsonValue::Object(headers)) = record.get_mut("headers") {
				headers.retain(|name, _| !SDK_HEADERS.contains(&name.as_str()) && !name.starts_with(SDK_HEADER_PREFIX));
			}
		})
	}

	fn lines(&mut self, text: &str, prepare: impl Fn(&mut JsonValue)) -> String {
		let mut out = String::new();
		for line in text.lines() {
			let normalized = match pi_json::parse(line) {
				Ok(mut value) if pi_json::to_js_string(&value) == line => {
					prepare(&mut value);
					self.value(&mut value);
					pi_json::to_js_string(&value)
				}
				_ => {
					let mut marker = JsonObject::new();
					marker.insert("$noncanonical".into(), JsonValue::String(self.text(line)));
					pi_json::to_js_string(&JsonValue::Object(marker))
				}
			};
			out.push_str(&normalized);
			out.push('\n');
		}
		out
	}

	/// Normalizes a JSON value in place.
	pub fn value(&mut self, value: &mut JsonValue) {
		match value {
			JsonValue::String(text) => *text = self.string(text),
			JsonValue::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
			JsonValue::Object(object) => self.object(object),
			JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => {}
		}
	}

	fn object(&mut self, object: &mut JsonObject) {
		let entries = std::mem::take(object);
		for (key, mut value) in entries {
			if let Some(placeholder) = keyed_placeholder(&key, &value) {
				value = JsonValue::String(placeholder.to_owned());
			} else {
				self.value(&mut value);
			}
			object.insert(self.string(&key), value);
		}
	}

	/// A whole string: text rules, then an 8-hex entry id (the whole string) becomes `id-N`.
	fn string(&mut self, text: &str) -> String {
		let replaced = self.text(text);
		if replaced.len() == 8
			&& replaced
				.bytes()
				.all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
		{
			return self.entry_ids.name(&replaced);
		}
		replaced
	}

	/// Text rules inside any text: literals, file-name timestamps, ISO timestamps, UUIDs.
	fn text(&mut self, text: &str) -> String {
		let mut out = text.to_owned();
		for (literal, placeholder) in &self.literals {
			if !literal.is_empty() && out.contains(literal.as_str()) {
				out = out.replace(literal.as_str(), placeholder);
			}
		}
		out = replace_template(&out, FILE_TIME, |_| "$FILE_TIME".to_owned());
		out = replace_template(&out, ISO_TIME, |_| "$TIME".to_owned());
		replace_template(&out, UUID, |found| self.uuids.name(found))
	}
}

/// `timestamp` numbers are epoch milliseconds (message times); other `KEYED` names are replaced in any form.
fn keyed_placeholder(key: &str, value: &JsonValue) -> Option<&'static str> {
	if key == "timestamp" {
		return value.is_number().then_some("$EPOCH_MS");
	}
	if key == "version" {
		return value.is_string().then_some("$VERSION");
	}
	KEYED
		.iter()
		.find(|(name, _)| *name == key)
		.map(|(_, placeholder)| *placeholder)
}

/// Stable names for values in order of first appearance: `uuid-1`, `uuid-2`, ...
struct Mapping {
	prefix: &'static str,
	names: HashMap<String, String>,
}

impl Mapping {
	fn new(prefix: &'static str) -> Self {
		Self {
			prefix,
			names: HashMap::new(),
		}
	}

	fn name(&mut self, value: &str) -> String {
		let next = self.names.len() + 1;
		self.names
			.entry(value.to_owned())
			.or_insert_with(|| format!("{}-{next}", self.prefix))
			.clone()
	}
}

// Templates: `h` a hex digit, `d` a decimal digit, anything else itself.
const UUID: &str = "hhhhhhhh-hhhh-hhhh-hhhh-hhhhhhhhhhhh";
const ISO_TIME: &str = "dddd-dd-ddTdd:dd:dd.dddZ";
/// The session file name's timestamp: `toISOString()` with `:` and `.` replaced by `-`.
const FILE_TIME: &str = "dddd-dd-ddTdd-dd-dd-dddZ";

/// Replaces every match of `template` not inside a longer alphanumeric run.
fn replace_template(text: &str, template: &str, mut name: impl FnMut(&str) -> String) -> String {
	let bytes = text.as_bytes();
	let width = template.len();
	let mut out = String::with_capacity(text.len());
	let mut start = 0;
	let mut index = 0;
	while index + width <= bytes.len() {
		let bounded = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
		let after = bytes.get(index + width);
		if bounded
			&& after.is_none_or(|byte| !byte.is_ascii_alphanumeric())
			&& matches_at(&bytes[index..index + width], template)
		{
			out.push_str(&text[start..index]);
			out.push_str(&name(&text[index..index + width]));
			index += width;
			start = index;
		} else {
			index += 1;
		}
	}
	out.push_str(&text[start..]);
	out
}

fn matches_at(bytes: &[u8], template: &str) -> bool {
	bytes.iter().zip(template.bytes()).all(|(&byte, class)| match class {
		b'h' => byte.is_ascii_hexdigit(),
		b'd' => byte.is_ascii_digit(),
		literal => byte == literal,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn normalizer() -> Normalizer {
		Normalizer::new(Path::new("/tmp/pi-difftest-1-0"), "127.0.0.1:4000")
	}

	#[test]
	fn ids_map_in_order_of_first_appearance_across_calls() {
		let mut normalizer = normalizer();
		let first = normalizer
			.jsonl("{\"id\":\"a1b2c3d4\",\"parentId\":null,\"session\":\"0193f0e2-1111-7222-8333-444455556666\"}\n");
		let second = normalizer.jsonl("{\"leafId\":\"ffff0000\",\"targetId\":\"a1b2c3d4\"}\n");
		assert_eq!(first, "{\"id\":\"id-1\",\"parentId\":null,\"session\":\"uuid-1\"}\n");
		assert_eq!(second, "{\"leafId\":\"id-2\",\"targetId\":\"id-1\"}\n");
	}

	#[test]
	fn paths_server_and_times_become_placeholders() {
		let mut value = json!({
			"cwd": "/tmp/pi-difftest-1-0/cwd",
			"file": "/tmp/pi-difftest-1-0/agent/sessions/--tmp-pi-difftest-1-0-cwd--/2026-11-06T10-00-00-000Z_0193f0e2-1111-7222-8333-444455556666.jsonl",
			"baseUrl": "http://127.0.0.1:4000/v1",
			"timestamp": "2026-11-06T10:00:00.000Z",
			"message": { "timestamp": 1_794_020_029_134_u64, "durationMs": 3 },
			"version": "1.5.0",
			"sessionVersion": 3,
		});
		normalizer().value(&mut value);
		assert_eq!(
			value,
			json!({
				"cwd": "$ROOT/cwd",
				"file": "$ROOT/agent/sessions/--$ROOT-cwd--/$FILE_TIME_uuid-1.jsonl",
				"baseUrl": "http://$SERVER/v1",
				"timestamp": "$TIME",
				"message": { "timestamp": "$EPOCH_MS", "durationMs": "$DURATION" },
				"version": "$VERSION",
				"sessionVersion": 3,
			})
		);
	}

	#[test]
	fn key_order_and_number_forms_are_not_masked() {
		let mut normalizer = normalizer();
		let reordered = "{\"output\":1,\"input\":2}";
		assert_eq!(normalizer.jsonl(reordered), "{\"output\":1,\"input\":2}\n");
		assert_eq!(
			normalizer.jsonl("{\"input\":1.0}"),
			"{\"$noncanonical\":\"{\\\"input\\\":1.0}\"}\n"
		);
		assert_eq!(normalizer.jsonl("not json"), "{\"$noncanonical\":\"not json\"}\n");
	}

	#[test]
	fn requests_lose_sdk_headers_only() {
		let line = r#"{"url":"/v1/messages?beta=true","headers":{"accept":"application/json","host":"h","user-agent":"u","x-api-key":"k","x-stainless-os":"Linux"},"body":{}}"#;
		assert_eq!(
			normalizer().requests_jsonl(line),
			"{\"url\":\"/v1/messages?beta=true\",\"headers\":{\"accept\":\"application/json\",\"x-api-key\":\"k\"},\"body\":{}}\n"
		);
	}

	#[test]
	fn only_whole_strings_of_eight_hex_digits_are_entry_ids() {
		let mut value = json!(["deadbeef", "deadbeefx", "the deadbeef", "DEADBEEF", "12345678"]);
		normalizer().value(&mut value);
		assert_eq!(value, json!(["id-1", "deadbeefx", "the deadbeef", "DEADBEEF", "id-2"]));
	}

	#[test]
	fn templates_need_alphanumeric_boundaries() {
		let mut normalizer = normalizer();
		assert_eq!(
			normalizer.text("x0193f0e2-1111-7222-8333-444455556666 and 0193f0e2-1111-7222-8333-444455556666."),
			"x0193f0e2-1111-7222-8333-444455556666 and uuid-1."
		);
		assert_eq!(normalizer.text("at 2026-11-06T10:00:00.123Z!"), "at $TIME!");
	}
}
