//! The incremental JSON parser (`streaming_json.rs`): ports `streaming-json.test.ts` case by case. The random
//! documents come from the same kind of seeded generator (its own numbers); the reference reader is ported.
//!
//! Decided differences checked here: a lone surrogate escape shows nothing until its pair arrives and reads as
//! U+FFFD if none does; numbers `f64` cannot hold are dropped.

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

use pi_ai::json_parse::{parse_streaming_json, parse_tool_call_arguments};
use pi_ai::streaming_json::{JsonChange, PathSegment, StreamingJsonParser, ToolCallArgumentsStream};
use pi_json::{JsonObject, JsonValue};
use serde_json::json;

// #9265: streamed tool-call arguments were re-parsed in full on every delta.

/// Seeded random numbers, so every run checks the same documents.
struct Random(u64);

impl Random {
	fn next(&mut self) -> f64 {
		self.0 = self
			.0
			.wrapping_mul(6_364_136_223_846_793_005)
			.wrapping_add(1_442_695_040_888_963_407);
		((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
	}

	fn below(&mut self, count: usize) -> usize {
		((self.next() * count as f64) as usize).min(count - 1)
	}
}

const STRING_PIECES: &[&str] = &[
	"a", "b", "x y", " ", "\n", "\t", "\\", "\"", "/", "é", "😀", "\u{1}", "\u{1f}", "\u{2028}", "{", "]", ",",
];

fn random_string(rand: &mut Random) -> String {
	let length = rand.below(6);
	(0..length)
		.map(|_| STRING_PIECES[rand.below(STRING_PIECES.len())])
		.collect()
}

fn random_scalar(rand: &mut Random) -> JsonValue {
	match rand.below(10) {
		0 => JsonValue::from(random_string(rand)),
		1 => json!(rand.below(2000) as i64 - 1000),
		2 => json!(rand.next() * 100.0),
		3 => json!(1e21),
		4 => json!(-5e-8),
		5 => json!(-0.0),
		6 => json!(0),
		7 => json!(true),
		8 => json!(false),
		_ => JsonValue::Null,
	}
}

fn random_value(rand: &mut Random, depth: u32) -> JsonValue {
	let roll = rand.next();
	if depth > 3 || roll < 0.3 {
		return random_scalar(rand);
	}
	if roll < 0.65 {
		let mut members = JsonObject::new();
		for _ in 0..rand.below(4) {
			let key = random_string(rand);
			members.insert(key, random_value(rand, depth + 1));
		}
		return JsonValue::Object(members);
	}
	JsonValue::Array((0..rand.below(4)).map(|_| random_value(rand, depth + 1)).collect())
}

/// JSON text of `value`, with random insignificant whitespace when `spaced`.
fn render(value: &JsonValue, rand: &mut Random, spaced: bool) -> String {
	let space = |rand: &mut Random| {
		if spaced {
			["", "", " ", "\n  ", "\t", "\r\n"][rand.below(6)].to_owned()
		} else {
			String::new()
		}
	};
	match value {
		JsonValue::Array(items) => {
			let parts: Vec<String> = items.iter().map(|item| render(item, rand, spaced)).collect();
			let separator = format!("{},{}", space(rand), space(rand));
			format!("[{}{}{}]", space(rand), parts.join(&separator), space(rand))
		}
		JsonValue::Object(members) => {
			let parts: Vec<String> = members
				.iter()
				.map(|(key, item)| {
					format!(
						"{}{}:{}{}",
						pi_json::to_js_string(&JsonValue::from(key.as_str())),
						space(rand),
						space(rand),
						render(item, rand, spaced)
					)
				})
				.collect();
			let separator = format!("{},{}", space(rand), space(rand));
			format!("{{{}{}{}}}", space(rand), parts.join(&separator), space(rand))
		}
		JsonValue::Number(number)
			if number.as_f64() == Some(0.0) && number.as_f64().is_some_and(f64::is_sign_negative) =>
		{
			"-0".into()
		}
		other => pi_json::to_js_string(other),
	}
}

fn random_documents(seed: u64, count: usize, spaced: bool) -> Vec<String> {
	let mut rand = Random(seed);
	(0..count)
		.map(|_| {
			let value = if rand.next() < 0.8 {
				let path = random_string(&mut rand);
				json!({ "path": path, "args": random_value(&mut rand, 0) })
			} else {
				random_value(&mut rand, 0)
			};
			render(&value, &mut rand, spaced)
		})
		.collect()
}

/// Reference for `StreamingJsonParser` on a prefix of valid JSON: a plain recursive-descent reading of the whole
/// prefix that follows the documented rules, written independently of the incremental parser.
struct Reference {
	pending_scalars: bool,
}

struct Read {
	value: JsonValue,
	complete: bool,
	end: usize,
}

impl Reference {
	fn snapshot(&self, text: &str) -> Option<JsonValue> {
		self.value(text, skip_space(text, 0)).map(|read| read.value)
	}

	fn value(&self, text: &str, start: usize) -> Option<Read> {
		match text.as_bytes().get(start)? {
			b'"' => Some(read_string(text, start)),
			b'{' => Some(self.object(text, start)),
			b'[' => Some(self.array(text, start)),
			_ => self.scalar(text, start),
		}
	}

	fn object(&self, text: &str, start: usize) -> Read {
		let mut members = JsonObject::new();
		let partial = |members: JsonObject| Read {
			value: JsonValue::Object(members),
			complete: false,
			end: text.len(),
		};
		let mut index = skip_space(text, start + 1);
		if index >= text.len() {
			return partial(members);
		}
		if text.as_bytes()[index] == b'}' {
			return Read {
				value: JsonValue::Object(members),
				complete: true,
				end: index + 1,
			};
		}
		loop {
			assert_eq!(
				text.as_bytes()[index],
				b'"',
				"reference reads valid JSON only: {text:?}"
			);
			let key = read_string(text, index);
			if !key.complete {
				return partial(members);
			}
			index = skip_space(text, key.end);
			if index >= text.len() {
				return partial(members);
			}
			index = skip_space(text, index + 1);
			let Some(member) = self.value(text, index) else {
				return partial(members);
			};
			let key = key.value.as_str().unwrap().to_owned();
			members.insert(key, member.value);
			if !member.complete {
				return partial(members);
			}
			index = skip_space(text, member.end);
			if index >= text.len() {
				return partial(members);
			}
			if text.as_bytes()[index] == b'}' {
				return Read {
					value: JsonValue::Object(members),
					complete: true,
					end: index + 1,
				};
			}
			index = skip_space(text, index + 1);
			if index >= text.len() {
				return partial(members);
			}
		}
	}

	fn array(&self, text: &str, start: usize) -> Read {
		let mut elements = Vec::new();
		let partial = |elements: Vec<JsonValue>| Read {
			value: JsonValue::Array(elements),
			complete: false,
			end: text.len(),
		};
		let mut index = skip_space(text, start + 1);
		if index >= text.len() {
			return partial(elements);
		}
		if text.as_bytes()[index] == b']' {
			return Read {
				value: JsonValue::Array(elements),
				complete: true,
				end: index + 1,
			};
		}
		loop {
			let Some(element) = self.value(text, index) else {
				return partial(elements);
			};
			elements.push(element.value);
			if !element.complete {
				return partial(elements);
			}
			index = skip_space(text, element.end);
			if index >= text.len() {
				return partial(elements);
			}
			if text.as_bytes()[index] == b']' {
				return Read {
					value: JsonValue::Array(elements),
					complete: true,
					end: index + 1,
				};
			}
			index = skip_space(text, index + 1);
			if index >= text.len() {
				return partial(elements);
			}
		}
	}

	fn scalar(&self, text: &str, start: usize) -> Option<Read> {
		let end = text[start..]
			.find([' ', '\t', '\n', '\r', ',', ':', '{', '}', '[', ']', '"'])
			.map_or(text.len(), |offset| start + offset);
		let token = &text[start..end];
		if end < text.len() {
			return Some(Read {
				value: pi_json::parse(token).unwrap(),
				complete: true,
				end,
			});
		}
		if !self.pending_scalars {
			return None;
		}
		for (literal, value) in [
			("true", json!(true)),
			("false", json!(false)),
			("null", JsonValue::Null),
		] {
			if literal.starts_with(token) {
				return Some(Read {
					value,
					complete: false,
					end,
				});
			}
		}
		if let Ok(value) = pi_json::parse(token) {
			return Some(Read {
				value,
				complete: false,
				end,
			});
		}
		let mantissa = token
			.strip_suffix(['+', '-'])
			.unwrap_or(token)
			.strip_suffix(['e', 'E'])?;
		pi_json::parse(mantissa).ok().map(|value| Read {
			value,
			complete: false,
			end,
		})
	}
}

fn skip_space(text: &str, start: usize) -> usize {
	let mut index = start;
	while index < text.len() && b" \t\n\r".contains(&text.as_bytes()[index]) {
		index += 1;
	}
	index
}

/// A string at `start`, with the documented escape rules (lone surrogates as U+FFFD, a held high half hidden).
fn read_string(text: &str, start: usize) -> Read {
	let mut units: Vec<u16> = Vec::new();
	let bytes = text.as_bytes();
	let mut index = start + 1;
	let finish = |units: &[u16], complete: bool, end: usize| {
		let mut units = units.to_vec();
		if !complete && units.last().is_some_and(|unit| (0xd800..0xdc00).contains(unit)) {
			units.pop();
		}
		Read {
			value: JsonValue::from(String::from_utf16_lossy(&units)),
			complete,
			end,
		}
	};
	while index < text.len() {
		let character = text[index..].chars().next().unwrap();
		if character == '"' {
			return finish(&units, true, index + 1);
		}
		if character != '\\' {
			units.extend(character.encode_utf16(&mut [0; 2]).iter());
			index += character.len_utf8();
			continue;
		}
		let Some(&next) = bytes.get(index + 1) else {
			return finish(&units, false, text.len());
		};
		let simple = match next {
			b'"' => Some('"'),
			b'\\' => Some('\\'),
			b'/' => Some('/'),
			b'b' => Some('\u{8}'),
			b'f' => Some('\u{c}'),
			b'n' => Some('\n'),
			b'r' => Some('\r'),
			b't' => Some('\t'),
			_ => None,
		};
		if let Some(simple) = simple {
			units.push(simple as u16);
			index += 2;
			continue;
		}
		if next == b'u' {
			let mut end = index + 2;
			while end < index + 6 && end < text.len() && bytes[end].is_ascii_hexdigit() {
				end += 1;
			}
			if end == index + 6 {
				units.push(u16::from_str_radix(&text[index + 2..end], 16).unwrap());
				index = end;
				continue;
			}
			if end == text.len() {
				return finish(&units, false, end);
			}
			units.extend(text[index..end].encode_utf16());
			index = end;
			continue;
		}
		units.push(u16::from(b'\\'));
		index += 1;
	}
	finish(&units, false, text.len())
}

fn reference(text: &str) -> Option<JsonValue> {
	Reference { pending_scalars: true }.snapshot(text)
}

/// Feeds `text` in pieces of `size` characters (or a random size); the snapshot after each piece and the text read.
fn stream(text: &str, mut size: impl FnMut() -> usize) -> Vec<(String, Option<JsonValue>)> {
	let mut parser = StreamingJsonParser::new();
	let mut steps = Vec::new();
	let mut rest = text;
	let mut read = String::new();
	while !rest.is_empty() {
		let take = rest.char_indices().nth(size()).map_or(rest.len(), |(at, _)| at);
		let (piece, tail) = rest.split_at(take);
		parser.append(piece);
		read.push_str(piece);
		rest = tail;
		steps.push((read.clone(), parser.snapshot().cloned()));
	}
	steps
}

fn final_snapshot(text: &str, size: usize) -> Option<JsonValue> {
	stream(text, || size.max(1)).pop().and_then(|(_, snapshot)| snapshot)
}

fn seed_of(text: &str) -> u64 {
	text.bytes()
		.fold(7u64, |hash, byte| hash.wrapping_mul(31).wrapping_add(u64::from(byte)))
		| 1
}

fn sizes(text: &str) -> Vec<Box<dyn FnMut() -> usize>> {
	let mut rand = Random(seed_of(text));
	vec![
		Box::new(|| 1),
		Box::new(|| 2),
		Box::new(|| 3),
		Box::new(|| 7),
		Box::new(move || 1 + rand.below(12)),
	]
}

// TS: streaming-json.test.ts
#[test]
fn matches_the_reference_after_every_piece_at_every_chunk_size_with_insignificant_whitespace() {
	for text in random_documents(1, 250, true) {
		for size in sizes(&text) {
			for (read, snapshot) in stream(&text, size) {
				assert_eq!(snapshot, reference(&read), "{read:?}");
			}
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn matches_todays_parse_streaming_json_at_every_prefix_of_compact_documents() {
	for text in random_documents(2, 400, false) {
		for (read, snapshot) in stream(&text, || 1) {
			// Documented differences: parseStreamingJson trims trailing whitespace, shows an unfinished root `null` as
			// {}, and (in Rust) partial-json shows a held high surrogate that the parser hides.
			if read.ends_with(char::is_whitespace)
				|| (snapshot == Some(JsonValue::Null) && "null".starts_with(read.as_str()))
			{
				continue;
			}
			let expected = parse_streaming_json(&read);
			assert_eq!(snapshot.unwrap_or_else(|| json!({})), expected, "{read:?}");
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn reads_complete_documents_exactly_like_json_parse() {
	for text in random_documents(3, 300, true)
		.into_iter()
		.chain(random_documents(4, 300, false))
	{
		let expected = pi_json::parse(&text).unwrap();
		for size in [1, 2, 3, 7, 5] {
			assert_eq!(final_snapshot(&text, size), Some(expected.clone()), "{text}");
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn never_changes_a_snapshot_once_it_is_returned() {
	// Snapshots are owned values handed out by reference and replaced, never edited: a clone taken at each step
	// equals the value kept for that step.
	for text in random_documents(5, 150, true) {
		let mut parser = StreamingJsonParser::new();
		let mut kept = Vec::new();
		for piece in text.split_inclusive(',') {
			parser.append(piece);
			kept.push(parser.snapshot().cloned());
		}
		let mut replay = StreamingJsonParser::new();
		for (piece, snapshot) in text.split_inclusive(',').zip(&kept) {
			replay.append(piece);
			assert_eq!(replay.snapshot(), snapshot.as_ref());
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn repairs_raw_control_characters_and_invalid_escapes_like_parse_streaming_json_and_shows_them_while_streaming() {
	let mut rand = Random(6);
	let malformed = [
		"\n", "\t", "\u{0}", "\\d", "\\x41", "\\u12", "\\uZZZZ", "\\u00e9", "\\\\", "\\q", "a", " ", "é",
	];
	for round in 0..300 {
		let content: String = (0..6).map(|_| malformed[rand.below(malformed.len())]).collect();
		let text = format!(r#"{{"path":"p","content":"{content}","n":{round}}}"#);
		assert_eq!(final_snapshot(&text, 1), Some(parse_streaming_json(&text)), "{text:?}");
		for (read, snapshot) in stream(&text, || 1) {
			assert_eq!(snapshot, reference(&read), "{read:?}");
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn holds_an_escape_back_until_it_is_complete_at_every_split_position() {
	for (text, value) in [
		(r#"{"a":"x\u00e9y"}"#, "xéy"),
		(r#"{"a":"x\ny"}"#, "x\ny"),
		(r#"{"a":"x\\y"}"#, "x\\y"),
		(r#"{"a":"x\"y"}"#, "x\"y"),
		(r#"{"a":"x\/y"}"#, "x/y"),
	] {
		for split in 0..=text.len() {
			let mut parser = StreamingJsonParser::new();
			parser.append(&text[..split]);
			assert_eq!(
				parser.snapshot().cloned(),
				reference(&text[..split]),
				"{}",
				&text[..split]
			);
			parser.append(&text[split..]);
			assert_eq!(parser.snapshot(), Some(&json!({ "a": value })));
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn joins_surrogate_pairs_split_across_pieces_escaped_or_raw() {
	let escaped = r#"{"a":"\ud83d\ude00!"}"#;
	let raw = r#"{"a":"😀!"}"#;
	for text in [escaped, raw] {
		for split in (0..=text.len()).filter(|split| text.is_char_boundary(*split)) {
			let mut parser = StreamingJsonParser::new();
			parser.append(&text[..split]);
			parser.snapshot();
			parser.append(&text[split..]);
			assert_eq!(parser.snapshot(), Some(&json!({ "a": "😀!" })));
		}
	}
	// Decided difference: a high surrogate whose low half has not arrived is held back (TS shows it alone), and reads
	// as U+FFFD once something else follows.
	let mut parser = StreamingJsonParser::new();
	parser.append(r#"{"a":"\ud83d"#);
	assert_eq!(parser.snapshot(), Some(&json!({ "a": "" })));
	parser.append("x\"}");
	assert_eq!(parser.snapshot(), Some(&json!({ "a": "\u{fffd}x" })));
}

// TS: streaming-json.test.ts
#[test]
fn keeps_trailing_whitespace_of_an_unterminated_string() {
	let mut parser = StreamingJsonParser::new();
	parser.append("{\"code\":\"x\t\t  ");
	assert_eq!(parser.snapshot(), Some(&json!({ "code": "x\t\t  " })));
}

// TS: streaming-json.test.ts
#[test]
fn keeps_raw_control_characters_and_invalid_escapes_visible_while_streaming() {
	let mut parser = StreamingJsonParser::new();
	parser.append("{\"a\":\"x\ny \\d \\u12\"");
	assert_eq!(parser.snapshot(), Some(&json!({ "a": "x\ny \\d \\u12" })));
}

// TS: streaming-json.test.ts
#[test]
fn keeps_invalid_u_escapes_literal_without_dropping_fields_10868() {
	assert_eq!(
		final_snapshot(r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#, 1),
		Some(json!({ "path": "a.tex", "content": "\\usepackage{x}", "mode": "w" }))
	);
	assert_eq!(
		final_snapshot(r#"{"text":"caf\u00e9 \url"}"#, 3),
		Some(json!({ "text": "café \\url" }))
	);
	let mut parser = StreamingJsonParser::new();
	parser.append(r#"{"text":"ab\u00"#);
	assert_eq!(parser.snapshot(), Some(&json!({ "text": "ab" })));
}

// TS: streaming-json.test.ts
#[test]
fn makes_proto_an_own_member_as_json_parse_does() {
	// A JSON object map has no prototype; `__proto__` is an ordinary member at every split.
	let text = r#"{"__proto__":{"polluted":true},"b":{"__proto__":"x"}}"#;
	for split in 0..=text.len() {
		let mut parser = StreamingJsonParser::new();
		parser.append(&text[..split]);
		parser.append(&text[split..]);
		assert_eq!(parser.snapshot(), Some(&pi_json::parse(text).unwrap()));
	}
}

// TS: streaming-json.test.ts
#[test]
fn keeps_the_last_value_of_a_duplicate_key_in_the_first_keys_position_as_json_parse_does() {
	let text = r#"{"a":1,"b":2,"a":"three"}"#;
	let snapshot = final_snapshot(text, 1).unwrap();
	let keys: Vec<&String> = snapshot.as_object().unwrap().keys().collect();
	assert_eq!(keys, ["a", "b"]);
	assert_eq!(snapshot, pi_json::parse(text).unwrap());
}

/// partial-json (MIT) vectors, with every partial type allowed, then Vercel AI SDK fixJson (Apache-2.0) vectors.
fn vectors() -> Vec<(&'static str, Option<JsonValue>)> {
	vec![
		("\"", Some(json!(""))),
		("[\"", Some(json!([""]))),
		("{\"\": \"", Some(json!({ "": "" }))),
		("n", Some(JsonValue::Null)),
		("t", Some(json!(true))),
		("f", Some(json!(false))),
		("0", Some(json!(0))),
		("-1.25e+4", Some(json!(-1.25e4))),
		("-1.25e+", Some(json!(-1.25))),
		("-1.25e", Some(json!(-1.25))),
		// Differences, documented in streaming-json.ts: an invalid escape stays visible, Infinity/NaN words drop.
		("\" \\x12", Some(json!(" \\x12"))),
		("I", None),
		("-I", None),
		("N", None),
		("", None),
		("nul", Some(JsonValue::Null)),
		("fals", Some(json!(false))),
		("12.2", Some(json!(12.2))),
		("-12", Some(json!(-12))),
		("-", None),
		("2.5e", Some(json!(2.5))),
		("2.5e-", Some(json!(2.5))),
		("2.5e3", Some(json!(2.5e3))),
		("-2.5e3", Some(json!(-2.5e3))),
		("2.5E", Some(json!(2.5))),
		("2.5E-", Some(json!(2.5))),
		("2.5E3", Some(json!(2.5e3))),
		("-2.5E3", Some(json!(-2.5e3))),
		("12.34e", Some(json!(12.34))),
		("5e", Some(json!(5))),
		("\"abc", Some(json!("abc"))),
		(
			"\"value with \\\"quoted\\\" text and \\\\ escape",
			Some(json!("value with \"quoted\" text and \\ escape")),
		),
		("\"value with \\", Some(json!("value with "))),
		("\"\\u", Some(json!(""))),
		("\"\\u12", Some(json!(""))),
		("\"text \\u00", Some(json!("text "))),
		("{\"a\":\"\\u12", Some(json!({ "a": "" }))),
		("\"value with unicode \u{3c}\"", Some(json!("value with unicode <"))),
		("[", Some(json!([]))),
		("[[1], [2", Some(json!([[1], [2]]))),
		("[[\"1\"], [\"2", Some(json!([["1"], ["2"]]))),
		("[[false], [nu", Some(json!([[false], [null]]))),
		("[[[]], [[]", Some(json!([[[]], [[]]]))),
		("[[{}], [{", Some(json!([[{}], [{}]]))),
		("[1, ", Some(json!([1]))),
		("[[], 123", Some(json!([[], 123]))),
		("{\"key\":", Some(json!({}))),
		(
			"{\"a\": {\"b\": 1}, \"c\": {\"d\": 2",
			Some(json!({ "a": { "b": 1 }, "c": { "d": 2 } })),
		),
		(
			"{\"a\": {\"b\": \"1\"}, \"c\": {\"d\": 2",
			Some(json!({ "a": { "b": "1" }, "c": { "d": 2 } })),
		),
		(
			"{\"a\": {\"b\": false}, \"c\": {\"d\": 2",
			Some(json!({ "a": { "b": false }, "c": { "d": 2 } })),
		),
		(
			"{\"a\": {\"b\": []}, \"c\": {\"d\": 2",
			Some(json!({ "a": { "b": [] }, "c": { "d": 2 } })),
		),
		(
			"{\"a\": {\"b\": {}}, \"c\": {\"d\": 2",
			Some(json!({ "a": { "b": {} }, "c": { "d": 2 } })),
		),
		("{\"ke", Some(json!({}))),
		("{\"k1\": 1, \"k2", Some(json!({ "k1": 1 }))),
		("{\"k1\": 1, \"k2\":", Some(json!({ "k1": 1 }))),
		("{\"key\": \"value\"  ", Some(json!({ "key": "value" }))),
		("{\"a\": {\"b\": {}", Some(json!({ "a": { "b": {} } }))),
		("[1, [2, 3, [", Some(json!([1, [2, 3, []]]))),
		("[false, [true, [", Some(json!([false, [true, []]]))),
		("{\"key\": {\"subKey\":", Some(json!({ "key": {} }))),
		(
			"{\"key\": 123, \"key2\": {\"subKey\":",
			Some(json!({ "key": 123, "key2": {} })),
		),
		(
			"{\"key\": null, \"key2\": {\"subKey\":",
			Some(json!({ "key": null, "key2": {} })),
		),
		("{\"key\": [1, 2, {", Some(json!({ "key": [1, 2, {}] }))),
		("[1, 2, {\"key\": \"value\",", Some(json!([1, 2, { "key": "value" }]))),
		(
			"{\"a\": {\"b\": [\"c\", {\"d\": \"e\",",
			Some(json!({ "a": { "b": ["c", { "d": "e" }] } })),
		),
		(
			"{\"a\": {\"b\": {\"c\": {\"d\":",
			Some(json!({ "a": { "b": { "c": {} } } })),
		),
		("{\"a\": 1, \"b\": [", Some(json!({ "a": 1, "b": [] }))),
		("{\"a\": 1, \"b\": {", Some(json!({ "a": 1, "b": {} }))),
		("{\"a\": 1, \"b\": \"", Some(json!({ "a": 1, "b": "" }))),
		(
			"{\n  \"a\": [\n    {\n      \"a1\": \"v1\",\n      \"a2\": \"v2\",\n      \"a3\": \"v3\"\n    }\n  ],\n  \"b\": [\n    {\n      \"b1\": \"n",
			Some(json!({ "a": [{ "a1": "v1", "a2": "v2", "a3": "v3" }], "b": [{ "b1": "n" }] })),
		),
		(
			"{\"type\":\"div\",\"children\":[{\"type\":\"Card\",\"props\":{}",
			Some(json!({ "type": "div", "children": [{ "type": "Card", "props": {} }] })),
		),
		// Difference: fixJson completes `12.` to 12; like parseStreamingJson, a number cut after its dot is omitted.
		("12.", None),
		("12.e", None),
	]
}

// TS: streaming-json.test.ts
#[test]
fn reads_ported_test_vectors() {
	for (input, expected) in vectors() {
		for size in [input.chars().count().max(1), 1, 2] {
			assert_eq!(final_snapshot(input, size), expected, "{input:?} in pieces of {size}");
		}
	}
}

fn base64(text: &str) -> Vec<u8> {
	const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
	let mut bits = 0u32;
	let mut count = 0;
	let mut out = Vec::new();
	for byte in text.bytes().filter(|byte| *byte != b'=') {
		bits = (bits << 6) | ALPHABET.iter().position(|letter| *letter == byte).unwrap() as u32;
		count += 6;
		if count >= 8 {
			count -= 8;
			out.push((bits >> count) as u8);
		}
	}
	out
}

// TS: streaming-json.test.ts
#[test]
fn reads_every_json_test_suite_document_json_parse_accepts_exactly_like_it_and_never_throws_on_the_rest() {
	let path = concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/../../../packages/ai/test/data/json-test-suite.json"
	);
	let suite = pi_json::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
	for (name, encoded) in suite["files"].as_object().unwrap() {
		let text = String::from_utf8_lossy(&base64(encoded.as_str().unwrap())).into_owned();
		let expected = pi_json::parse(&text).ok();
		for size in [text.chars().count().max(1), 1, 3] {
			let snapshot = final_snapshot(&text, size);
			// TS parity: documents that only hold a lone surrogate differ (decided divergence); others match.
			if let Some(expected) = &expected
				&& !text.contains("\\uD")
				&& !text.contains("\\ud")
			{
				assert_eq!(snapshot.as_ref(), Some(expected), "{name}");
			}
		}
	}
}

const MALFORMED: &[&str] = &[
	"{\"a\":1 \"b\":2}",
	"{\"a\" 1, \"b\": 2}",
	"{a:1, 'b': 2}",
	"{\"a\":1,,\"b\":2,}",
	"[1 2, , 3,]",
	"{\"a\":[1,2}, \"b\":3]",
	"]]]}}}{",
	"{\"a\":NaN,\"b\":Infinity,\"c\":-Infinity,\"d\":undefined,\"e\":1}",
	"{\"a\":01,\"b\":1.,\"c\":.5,\"d\":+1,\"e\":0x10,\"f\":1e}",
	"{\"a\":\"x\"} trailing garbage {\"b\":2}",
	"{\"a\": tru, \"b\": nulll, \"c\": falsey}",
	"{\"a\":{\"b\":{\"c\":",
	"{\"a\\\": 1}",
	"\"unterminated \\",
	"// comment\n{\"a\":1}",
	"\u{0}{\u{1}\"a\"\u{2}:\u{3}1}",
	"{\"a\":\"x\"}}}]]]",
];

// TS: streaming-json.test.ts
#[test]
fn never_throws_keeps_snapshots_plain_json_and_unchanged_at_every_chunk_size() {
	for text in MALFORMED {
		let whole = final_snapshot(text, text.len());
		for size in sizes(text) {
			let steps = stream(text, size);
			// Every chunk size reads malformed text the same way.
			assert_eq!(
				steps.last().and_then(|(_, snapshot)| snapshot.clone()),
				whole,
				"{text:?}"
			);
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn tolerates_missing_commas_and_colons_and_drops_values_in_invalid_positions() {
	let whole = |text: &str| final_snapshot(text, text.len());
	assert_eq!(whole(r#"{"a":1 "b":2}"#), Some(json!({ "a": 1, "b": 2 })));
	assert_eq!(whole(r#"{"a" 1, "b": 2}"#), Some(json!({ "a": 1, "b": 2 })));
	assert_eq!(whole("[1 2, , 3,]"), Some(json!([1, 2, 3])));
	assert_eq!(whole(r#"{"a":NaN,"b":Infinity,"e":1}"#), Some(json!({ "e": 1 })));
	assert_eq!(
		whole(r#"{"a":"x"} trailing garbage {"b":2}"#),
		Some(json!({ "a": "x" }))
	);
}

/// A copy of a value kept current by applying the parser's changes, as a writer such as Durable keeps one.
#[derive(Default)]
struct Mirror {
	value: Option<JsonValue>,
}

impl Mirror {
	fn apply(&mut self, changes: Vec<JsonChange>) {
		for change in changes {
			let path = match &change {
				JsonChange::Set { path, .. } | JsonChange::Append { path, .. } => path.clone(),
			};
			let Some((last, parents)) = path.split_last() else {
				match change {
					JsonChange::Set { value, .. } => self.value = Some(value),
					JsonChange::Append { text, .. } => {
						let current = self
							.value
							.take()
							.and_then(|value| value.as_str().map(str::to_owned))
							.unwrap_or_default();
						self.value = Some(JsonValue::from(current + &text));
					}
				}
				continue;
			};
			let mut parent = self.value.as_mut().unwrap();
			for segment in parents {
				parent = match segment {
					PathSegment::Key(key) => parent.get_mut(key.as_str()).unwrap(),
					PathSegment::Index(index) => parent.get_mut(*index).unwrap(),
				};
			}
			let slot = match (last, parent) {
				(PathSegment::Key(key), JsonValue::Object(members)) => {
					members.entry(key.clone()).or_insert(JsonValue::Null)
				}
				(PathSegment::Index(index), JsonValue::Array(items)) => {
					if *index >= items.len() {
						items.push(JsonValue::Null);
					}
					&mut items[*index]
				}
				_ => panic!("change path does not match the value"),
			};
			match change {
				JsonChange::Set { value, .. } => *slot = value,
				JsonChange::Append { text, .. } => {
					*slot = JsonValue::from(slot.as_str().unwrap_or_default().to_owned() + &text);
				}
			}
		}
	}
}

// TS: streaming-json.test.ts
#[test]
fn survives_random_corruption_of_valid_documents() {
	let mut rand = Random(7);
	let noise = ["\"", "\\", "{", "}", "[", "]", ",", ":", "x", "1", "-", "\n", "t"];
	for text in random_documents(8, 300, true) {
		let mut characters: Vec<String> = text.chars().map(String::from).collect();
		for _ in 0..3 {
			let at = rand.below(characters.len() + 1);
			if rand.next() < 0.5 {
				if at < characters.len() {
					characters.remove(at);
				}
			} else {
				characters.insert(at, noise[rand.below(noise.len())].to_owned());
			}
		}
		let corrupted: String = characters.concat();
		let mut parser = StreamingJsonParser::new();
		let mut mirror = Mirror::default();
		let chars: Vec<char> = corrupted.chars().collect();
		for piece in chars.chunks(4) {
			mirror.apply(parser.append(&piece.iter().collect::<String>()));
		}
		// A space ends a number or literal at the end, which snapshots show and changes do not report yet.
		mirror.apply(parser.append(" "));
		assert_eq!(mirror.value.as_ref(), parser.snapshot(), "{corrupted:?}");
	}
}

// TS: streaming-json.test.ts
#[test]
fn changes_keep_a_copy_equal_to_the_value_read_so_far_at_every_chunk_size_and_to_json_parse_at_the_end() {
	let changes_reference = Reference { pending_scalars: false };
	for text in random_documents(9, 200, true) {
		for mut size in sizes(&text) {
			let mut parser = StreamingJsonParser::new();
			let mut mirror = Mirror::default();
			let mut rest = text.as_str();
			let mut read = String::new();
			while !rest.is_empty() {
				let take = rest.char_indices().nth(size()).map_or(rest.len(), |(at, _)| at);
				let (piece, tail) = rest.split_at(take);
				mirror.apply(parser.append(piece));
				read.push_str(piece);
				rest = tail;
				assert_eq!(mirror.value, changes_reference.snapshot(&read), "{read:?}");
			}
			// A bare root number or literal is reported once a delimiter ends it.
			mirror.apply(parser.append(" "));
			assert_eq!(mirror.value, Some(pi_json::parse(&text).unwrap()), "{text}");
		}
	}
}

fn key(name: &str) -> PathSegment {
	PathSegment::Key(name.into())
}

fn set(path: Vec<PathSegment>, value: JsonValue) -> JsonChange {
	JsonChange::Set { path, value }
}

fn append(path: Vec<PathSegment>, text: &str) -> JsonChange {
	JsonChange::Append {
		path,
		text: text.into(),
	}
}

// TS: streaming-json.test.ts
#[test]
fn report_a_number_that_ends_and_a_string_that_starts_in_one_piece_in_order() {
	let mut parser = StreamingJsonParser::new();
	assert_eq!(parser.append(r#"{"n":1"#), [set(vec![], json!({}))]);
	assert_eq!(
		parser.append(r#"2,"s":"ab"#),
		[set(vec![key("n")], json!(12)), set(vec![key("s")], json!("ab"))]
	);
	assert_eq!(parser.append(r#"c","t":tru"#), [append(vec![key("s")], "c")]);
	assert_eq!(parser.append("e}"), [set(vec![key("t")], json!(true))]);
	assert_eq!(parser.append(r#" {"ignored":1}"#), []);
}

// TS: streaming-json.test.ts
#[test]
fn report_nested_objects_and_arrays_as_empty_containers_then_their_members_and_items() {
	let mut parser = StreamingJsonParser::new();
	let index = PathSegment::Index;
	assert_eq!(
		parser.append(r#"{"a":[1,{"b":["x""#),
		[
			set(vec![], json!({})),
			set(vec![key("a")], json!([])),
			set(vec![key("a"), index(0)], json!(1)),
			set(vec![key("a"), index(1)], json!({})),
			set(vec![key("a"), index(1), key("b")], json!([])),
			set(vec![key("a"), index(1), key("b"), index(0)], json!("x")),
		]
	);
	assert_eq!(
		parser.append(r#","y"#),
		[set(vec![key("a"), index(1), key("b"), index(1)], json!("y"))]
	);
	assert_eq!(
		parser.append(r#"z"]}],"a":"again"}"#),
		[
			append(vec![key("a"), index(1), key("b"), index(1)], "z"),
			set(vec![key("a")], json!("again"))
		]
	);
}

// TS: streaming-json.test.ts
#[test]
fn hold_split_escapes_back_and_report_their_decoded_text_once_complete() {
	let text = r#"{"a":"x\u00e9\n\\\"\ud83d\ude00\q"}"#;
	let expected = "xé\n\\\"😀\\q";
	let chars: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
	for first in 1..chars.len() - 1 {
		for second in first..chars.len() - 1 {
			let mut parser = StreamingJsonParser::new();
			let mut mirror = Mirror::default();
			for piece in [
				&text[..chars[first]],
				&text[chars[first]..chars[second]],
				&text[chars[second]..],
			] {
				mirror.apply(parser.append(piece));
			}
			assert_eq!(mirror.value, Some(json!({ "a": expected })));
		}
	}
	let mut parser = StreamingJsonParser::new();
	parser.append(r#"{"a":"x"#);
	assert_eq!(parser.append("\\u00"), []);
	assert_eq!(parser.append("e9"), [append(vec![key("a")], "é")]);
}

// TS: streaming-json.test.ts
#[test]
fn report_a_container_nested_deeper_than_32_levels_once_whole_when_it_closes() {
	let depth = 40;
	let text = format!("{}\"deep\"{}", "{\"a\":".repeat(depth), "}".repeat(depth));
	let mut parser = StreamingJsonParser::new();
	let mut mirror = Mirror::default();
	for piece in text.as_bytes().chunks(3) {
		mirror.apply(parser.append(std::str::from_utf8(piece).unwrap()));
	}
	assert_eq!(mirror.value, Some(pi_json::parse(&text).unwrap()));
	let changes = StreamingJsonParser::new().append(&text);
	// The containers down to path length 32, then the one at length 32 again, whole, as it closes.
	assert_eq!(changes.len(), 34);
	let JsonChange::Set { path, value } = changes.last().unwrap() else {
		panic!("a set")
	};
	assert_eq!(path.len(), 32);
	assert_eq!(value, &pi_json::parse(&text[32 * 5..text.len() - 32]).unwrap());
}

// TS: streaming-json.test.ts
#[test]
fn report_nothing_for_values_in_invalid_positions_and_text_after_the_root() {
	let mut parser = StreamingJsonParser::new();
	assert_eq!(
		parser.append(r#"{"a":1 2, "b" "x"}"#),
		[
			set(vec![], json!({})),
			set(vec![key("a")], json!(1)),
			set(vec![key("b")], json!("x"))
		]
	);
	assert_eq!(parser.append(r#"{"c":3}"#), []);
}

fn write_arguments(size: usize) -> String {
	let content: String = (0..20_000)
		.map(|line| format!("const line{line} = \"value {line}\"; // \\ done\n"))
		.collect();
	pi_json::to_js_string(&json!({ "path": "/src/file.ts", "content": &content[..size] }))
}

fn stream_allocation(text: &str, delta: usize) -> u64 {
	allocation_counter::measure(|| {
		let mut arguments = ToolCallArgumentsStream::new();
		for piece in text.as_bytes().chunks(delta) {
			arguments.append(std::str::from_utf8(piece).unwrap());
		}
		std::hint::black_box(arguments.finish());
	})
	.bytes_total
}

// TS: streaming-json.test.ts
#[test]
fn allocates_linearly_in_the_streamed_text_never_per_accumulated_text() {
	let (small, large) = (write_arguments(50_000), write_arguments(500_000));
	let (small_bytes, large_bytes) = (stream_allocation(&small, 32), stream_allocation(&large, 32));
	// Ten times the text: linear growth is about 10x, quadratic about 100x.
	assert!(
		large_bytes / (large.len() as u64) < 400,
		"{} bytes per character",
		large_bytes / large.len() as u64
	);
	assert!(large_bytes / small_bytes < 20, "{large_bytes} / {small_bytes}");
}

// TS: streaming-json.test.ts
#[test]
fn keeps_very_large_open_arrays_linear_by_letting_their_preview_lag() {
	let elements = |count: usize| pi_json::to_js_string(&json!({ "items": (0..count).collect::<Vec<_>>() }));
	let (small, large) = (elements(10_000), elements(100_000));
	let (small_bytes, large_bytes) = (stream_allocation(&small, 16), stream_allocation(&large, 16));
	assert!(large_bytes / small_bytes < 20, "{large_bytes} / {small_bytes}");
	let mut parser = StreamingJsonParser::new();
	let mut shown = 0;
	for piece in large.as_bytes().chunks(16) {
		parser.append(std::str::from_utf8(piece).unwrap());
		let items = parser
			.snapshot()
			.and_then(|snapshot| snapshot.get("items"))
			.and_then(JsonValue::as_array)
			.map_or(0, Vec::len);
		// The preview only moves forward.
		assert!(items >= shown);
		shown = items;
	}
	assert_eq!(parser.snapshot(), Some(&pi_json::parse(&large).unwrap()));
}

// TS: streaming-json.test.ts
#[test]
fn reads_deep_nesting_without_recursion_and_in_linear_time() {
	// Decided difference: TS reads 200,000 levels; the parser here does not recurse either, but a `JsonValue` that
	// deep cannot be cloned or dropped (serde_json recurses), so the case runs at a depth snapshots can hold.
	let depth = 500;
	let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
	let started = std::time::Instant::now();
	let mut parser = StreamingJsonParser::new();
	for piece in text.as_bytes().chunks(7) {
		parser.append(std::str::from_utf8(piece).unwrap());
		parser.snapshot();
	}
	let mut value = parser.snapshot().unwrap();
	for _ in 0..depth {
		value = &value[0];
	}
	assert_eq!(value, &json!(1));
	assert!(started.elapsed().as_secs() < 5);
}

// TS: streaming-json.test.ts
#[test]
fn reads_one_huge_piece_such_as_a_providers_complete_arguments_in_linear_time() {
	let text = write_arguments(500_000);
	let bytes = allocation_counter::measure(|| {
		let mut parser = StreamingJsonParser::new();
		parser.append(&text);
		std::hint::black_box(parser.snapshot());
	})
	.bytes_total;
	assert!(
		bytes / (text.len() as u64) < 100,
		"{} bytes per character",
		bytes / text.len() as u64
	);
}

// TS: streaming-json.test.ts
#[test]
fn shows_partial_arguments_as_an_object_and_finishes_with_one_full_parse() {
	let mut arguments = ToolCallArgumentsStream::new();
	arguments.append("  ");
	assert!(arguments.arguments().is_empty());
	assert_eq!(
		arguments.append(r#"{"path":"a.ts","content":"x"#).cloned(),
		Some(object(json!({ "path": "a.ts", "content": "x" })))
	);
	let before = arguments.append("y").cloned();
	assert_eq!(arguments.append(""), None);
	assert_eq!(Some(arguments.arguments().clone()), before);
	assert_eq!(arguments.text(), r#"  {"path":"a.ts","content":"xy"#);
	assert_eq!(
		arguments.finish(),
		parse_tool_call_arguments(r#"  {"path":"a.ts","content":"xy"#)
	);
	arguments.append("\"}");
	assert_eq!(
		arguments.finish().arguments,
		object(json!({ "path": "a.ts", "content": "xy" }))
	);
}

// TS: streaming-json.test.ts
#[test]
fn shows_non_object_roots_as_no_arguments_yet() {
	let mut arguments = ToolCallArgumentsStream::new();
	arguments.append("[1,2");
	assert!(arguments.arguments().is_empty());
	arguments.append("]");
	assert!(
		arguments
			.finish()
			.arguments_error
			.unwrap()
			.starts_with("Tool arguments must be a JSON object")
	);
}

// TS: streaming-json.test.ts
#[test]
fn replaces_the_text_with_the_providers_complete_text_returning_the_missing_piece() {
	let mut arguments = ToolCallArgumentsStream::new();
	arguments.append(r#"{"a":"x"#);
	let (delta, shown) = arguments.replace(r#"{"a":"xyz"}"#);
	assert_eq!(
		(delta.as_deref(), shown.cloned()),
		(Some("yz\"}"), Some(object(json!({ "a": "xyz" }))))
	);
	let (delta, shown) = arguments.replace(r#"{"b":2}"#);
	assert_eq!((delta, shown.cloned()), (None, Some(object(json!({ "b": 2 })))));
	assert_eq!(arguments.text(), r#"{"b":2}"#);
	// A restart reports its arguments even when they show nothing yet.
	let mut restarted = ToolCallArgumentsStream::new();
	restarted.append(r#"{"a":1}"#);
	let (delta, shown) = restarted.replace(" ");
	assert_eq!((delta, shown.cloned()), (None, Some(JsonObject::new())));
}

// TS: streaming-json.test.ts
#[test]
fn is_collected_with_its_snapshots_once_the_stream_is_dropped() {
	// Snapshots are owned by the stream; dropping it frees them, which the allocation counter shows.
	let info = allocation_counter::measure(|| {
		let mut arguments = ToolCallArgumentsStream::new();
		for index in 0..100 {
			arguments.append(&format!(r#"{{"a{index}":"{index}"#));
		}
	});
	assert_eq!(info.bytes_current, 0);
}

fn object(value: JsonValue) -> JsonObject {
	value.as_object().cloned().unwrap()
}
