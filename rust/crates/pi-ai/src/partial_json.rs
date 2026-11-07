//! The `partial-json` parser (npm `partial-json` 0.1.7, `Allow.ALL`) that `parseStreamingJson` falls back to: it
//! parses a prefix of JSON text, completing unterminated strings, numbers, literals, arrays and objects.
//!
//! Ported line by line, quirks included: a key is read from wherever the parser stands, a fallback slices up to the
//! last `e` or backslash of the whole text, and `substring` clamps and swaps its bounds. Indices are byte offsets;
//! every bound the parser computes sits next to an ASCII character, so slices stay on character boundaries. `NaN` and
//! `Infinity` read as `null`, their `JSON.stringify` form, since a JSON value cannot hold them.

use pi_json::{JsonObject, JsonValue};

/// Why a text could not be parsed even partially.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartialJsonError {
	/// The text is empty.
	Empty,
	/// The text ends where a value must follow (`PartialJSON`).
	Partial(String),
	/// The text is not JSON (`MalformedJSON`, or `JSON.parse`'s own error from a fallback).
	Malformed(String),
}

/// Parses a prefix of JSON text (`parse(text, Allow.ALL)`).
pub fn parse(text: &str) -> Result<JsonValue, PartialJsonError> {
	let trimmed = text.trim_matches([' ', '\t', '\n', '\r', '\u{b}', '\u{c}', '\u{feff}', '\u{a0}']);
	if trimmed.is_empty() {
		return Err(PartialJsonError::Empty);
	}
	Parser {
		text: trimmed,
		index: 0,
	}
	.parse_any()
}

struct Parser<'a> {
	text: &'a str,
	index: usize,
}

impl Parser<'_> {
	fn byte(&self, at: usize) -> Option<u8> {
		self.text.as_bytes().get(at).copied()
	}

	fn len(&self) -> usize {
		self.text.len()
	}

	/// `String.prototype.substring`: bounds clamped to the text, swapped when reversed.
	fn substring(&self, start: usize, end: usize) -> &str {
		let (start, end) = (start.min(self.len()), end.min(self.len()));
		let (start, end) = if start > end { (end, start) } else { (start, end) };
		self.text.get(start..end).unwrap_or("")
	}

	fn rest(&self) -> &str {
		self.substring(self.index, self.len())
	}

	fn partial(&self, message: &str) -> PartialJsonError {
		PartialJsonError::Partial(format!("{message} at position {}", self.index))
	}

	fn malformed(&self, message: &str) -> PartialJsonError {
		PartialJsonError::Malformed(format!("{message} at position {}", self.index))
	}

	fn parse_any(&mut self) -> Result<JsonValue, PartialJsonError> {
		self.skip_blank();
		if self.index >= self.len() {
			return Err(self.partial("Unexpected end of input"));
		}
		match self.byte(self.index) {
			Some(b'"') => return self.parse_str().map(JsonValue::String),
			Some(b'{') => return Ok(self.parse_obj()),
			Some(b'[') => return Ok(self.parse_arr()),
			_ => {}
		}
		for (word, value, min_remaining) in [
			("null", JsonValue::Null, 0),
			("true", JsonValue::Bool(true), 0),
			("false", JsonValue::Bool(false), 0),
			("Infinity", JsonValue::Null, 0),
			("-Infinity", JsonValue::Null, 2),
			("NaN", JsonValue::Null, 0),
		] {
			let remaining = self.len() - self.index;
			let exact = self.substring(self.index, self.index + word.len()) == word;
			let partial = remaining >= min_remaining && remaining < word.len() && word.starts_with(self.rest());
			if exact || partial {
				self.index += word.len();
				return Ok(value);
			}
		}
		self.parse_num()
	}

	fn parse_str(&mut self) -> Result<String, PartialJsonError> {
		let start = self.index;
		let mut escape = false;
		self.index += 1;
		while self.index < self.len()
			&& (self.byte(self.index) != Some(b'"') || (escape && self.byte(self.index - 1) == Some(b'\\')))
		{
			escape = if self.byte(self.index) == Some(b'\\') {
				!escape
			} else {
				false
			};
			self.index += 1;
		}
		let escape = usize::from(escape);
		if self.byte(self.index) == Some(b'"') {
			self.index += 1;
			let literal = self.substring(start, self.index - escape).to_owned();
			return json_string(&literal).ok_or_else(|| self.malformed("SyntaxError: invalid string"));
		}
		let literal = format!("{}\"", self.substring(start, self.index - escape));
		if let Some(text) = json_string(&literal) {
			return Ok(text);
		}
		// SyntaxError: invalid escape sequence; TS parity: up to the last backslash of the whole text.
		let cut = self.text.rfind('\\').unwrap_or(0);
		let literal = format!("{}\"", self.substring(start, cut));
		json_string(&literal).ok_or_else(|| PartialJsonError::Malformed("SyntaxError: invalid string".into()))
	}

	/// Objects never fail: a member that does not parse ends the object (`Allow.OBJ`).
	fn parse_obj(&mut self) -> JsonValue {
		self.index += 1;
		self.skip_blank();
		let mut object = JsonObject::new();
		while self.byte(self.index) != Some(b'}') {
			self.skip_blank();
			if self.index >= self.len() {
				return JsonValue::Object(object);
			}
			let Ok(key) = self.parse_str() else {
				return JsonValue::Object(object);
			};
			self.skip_blank();
			// Skips the colon, whatever stands there.
			self.index += 1;
			match self.parse_any() {
				Ok(value) => {
					object.insert(key, value);
				}
				Err(_) => return JsonValue::Object(object),
			}
			self.skip_blank();
			if self.byte(self.index) == Some(b',') {
				self.index += 1;
			}
		}
		self.index += 1;
		JsonValue::Object(object)
	}

	/// Arrays never fail: an element that does not parse ends the array (`Allow.ARR`).
	fn parse_arr(&mut self) -> JsonValue {
		self.index += 1;
		let mut array = Vec::new();
		while self.byte(self.index) != Some(b']') {
			match self.parse_any() {
				Ok(value) => array.push(value),
				Err(_) => return JsonValue::Array(array),
			}
			self.skip_blank();
			if self.byte(self.index) == Some(b',') {
				self.index += 1;
			}
		}
		self.index += 1;
		JsonValue::Array(array)
	}

	fn parse_num(&mut self) -> Result<JsonValue, PartialJsonError> {
		if self.index == 0 {
			if self.text == "-" {
				return Err(self.malformed("Not sure what '-' is"));
			}
			if let Ok(value) = pi_json::parse(self.text) {
				return Ok(value);
			}
			let cut = self.text.rfind('e').unwrap_or(0);
			return pi_json::parse(self.substring(0, cut))
				.map_err(|error| self.malformed(&format!("SyntaxError: {error}")));
		}
		let start = self.index;
		if self.byte(self.index) == Some(b'-') {
			self.index += 1;
		}
		while let Some(byte) = self.byte(self.index) {
			if matches!(byte, b',' | b']' | b'}') {
				break;
			}
			self.index += 1;
		}
		let token = self.substring(start, self.index);
		if let Ok(value) = pi_json::parse(token) {
			return Ok(value);
		}
		if token == "-" {
			return Err(self.partial("Not sure what '-' is"));
		}
		// TS parity: `lastIndexOf("e")` of the whole text; -1 makes `substring(start, -1)` the text before `start`.
		let cut = self.text.rfind('e').unwrap_or(0);
		pi_json::parse(self.substring(start, cut)).map_err(|error| self.malformed(&format!("SyntaxError: {error}")))
	}

	fn skip_blank(&mut self) {
		while matches!(self.byte(self.index), Some(b' ' | b'\n' | b'\r' | b'\t')) {
			self.index += 1;
		}
	}
}

/// `JSON.parse` of a string literal; `None` when it is not one.
fn json_string(literal: &str) -> Option<String> {
	match pi_json::parse(literal) {
		Ok(JsonValue::String(text)) => Some(text),
		_ => None,
	}
}
