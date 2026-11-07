//! Incremental, tolerant parsing of JSON text that arrives in pieces, such as streamed tool-call arguments (ports
//! `packages/ai/src/utils/streaming-json.ts`).
//!
//! `StreamingJsonParser::append` reads each piece once and `snapshot` returns the value of the text read so far. The
//! rules for incomplete and malformed text are the TS ones, listed there: an unterminated string shows the characters
//! read so far (an unterminated escape is held back), an unterminated container shows its members so far, a member
//! whose value has not started is omitted, an unterminated literal or number shows once it is a prefix of `true`,
//! `false`, `null` or forms a number; raw control characters and invalid escapes are kept, missing commas and colons are
//! tolerated, values in invalid positions and unknown words are dropped, text after the root value is ignored. The
//! parser never fails. `append` also returns the changes the piece made (`JsonChange`), as TS does.
//!
//! Decided differences from TS, both forced by Rust strings and owned JSON values:
//!
//! - A `\uD800`-`\uDBFF` escape is held back until the next escape shows whether a low surrogate completes it; a lone
//!   surrogate reads as U+FFFD (`pi_json::parse` does the same, a decided divergence). TS shows a lone high surrogate
//!   while its low half has not arrived.
//! - Cost. A JS snapshot shares strings and completed containers with the parser; a `JsonValue` snapshot copies them.
//!   So the open path's copy cost is counted in bytes here, not in members, and the copy credit per piece is larger.
//!   Documents below a few kilobytes are fresh at every snapshot, as in TS; a large document's preview lags behind its
//!   text, and the total copy work stays linear in the text, as in TS. Numbers that `f64` cannot hold (`1e400`) are
//!   dropped where JS shows `Infinity`.

use crate::json_parse::{ToolCallArguments, parse_tool_call_arguments};
use pi_json::{JsonObject, JsonValue};

/// One segment of a path from the root value: a member key or an array index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
	/// A member of an object.
	Key(String),
	/// An element of an array.
	Index(usize),
}

/// A path from the root value; empty for the root itself.
pub type JsonPath = Vec<PathSegment>;

/// One change a piece of text made to the value (`JsonChange`).
#[derive(Debug, Clone, PartialEq)]
pub enum JsonChange {
	/// A key, an array item or a finished value appeared at `path`: a new string with its first text, a container
	/// empty.
	Set {
		/// Where.
		path: JsonPath,
		/// The value.
		value: JsonValue,
	},
	/// The piece extended the open string at `path`.
	Append {
		/// Where.
		path: JsonPath,
		/// The added text.
		text: String,
	},
}

/// Where a value sits in its parent; `None` for a value in an invalid position, which is read but dropped.
#[derive(Debug, Clone, PartialEq)]
enum Slot {
	Root,
	Member(String),
	Element(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
	Key,
	Colon,
	Value,
	Comma,
}

/// An open object or array. Its container holds placeholders at the slots of its open child.
struct Frame {
	container: JsonValue,
	slot: Option<Slot>,
	/// Where the container sits, when its changes are reported.
	path: Option<JsonPath>,
	/// Whether changes inside it are reported; when not, its whole value is reported as it closes.
	reports_inside: bool,
	/// Approximate bytes of the completed values it holds: what a copy of it costs.
	bytes: usize,
	/// Objects: the key read last, where the member value goes.
	key: Option<String>,
	phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringTarget {
	Key,
	Value,
}

/// An unterminated string.
struct OpenString {
	target: StringTarget,
	slot: Option<Slot>,
	text: String,
	/// Escape read so far: empty for none, otherwise a backslash and up to five more characters.
	escape: String,
	/// A decoded high surrogate waiting for its low half.
	high_surrogate: Option<u16>,
	/// Where a value string sits, when its changes are reported.
	path: Option<JsonPath>,
	/// The index in `changes` of the change this string extends, while it is the newest change of the piece.
	change: Option<usize>,
}

// Changes are reported at most this deep, so a path costs O(1) to build.
const MAX_REPORTED_DEPTH: usize = 32;
// Copy credit per append and per appended byte, in bytes copied. The floor keeps documents of a few kilobytes fresh
// at every snapshot; see the module comment.
const CREDIT_PER_APPEND: usize = 4096;
const CREDIT_PER_BYTE: usize = 64;
// An unterminated number longer than this is not shown until it ends, so `snapshot()` stays cheap.
const MAX_PENDING_SCALAR_LENGTH: usize = 64;

/// Incremental, tolerant JSON parser; see the module comment for its rules and costs.
#[derive(Default)]
pub struct StreamingJsonParser {
	root: Option<JsonValue>,
	done: bool,
	stack: Vec<Frame>,
	string: Option<OpenString>,
	scalar: String,
	scalar_slot: Option<Slot>,
	/// What a fresh snapshot copies, in bytes.
	path_cost: usize,
	credit: usize,
	dirty: bool,
	snapshot: Option<JsonValue>,
	/// Incremented whenever `snapshot()` makes a new snapshot.
	version: u64,
	/// Changes of the piece being read.
	changes: Vec<JsonChange>,
}

impl StreamingJsonParser {
	/// A parser that has read nothing.
	pub fn new() -> Self {
		Self::default()
	}

	/// Reads the next piece of text; returns the changes it made, in order. Costs time linear in `text.len()`.
	pub fn append(&mut self, text: &str) -> Vec<JsonChange> {
		self.credit = self
			.credit
			.saturating_add(CREDIT_PER_APPEND + CREDIT_PER_BYTE.saturating_mul(text.len()));
		self.changes = Vec::new();
		if let Some(open) = &mut self.string {
			open.change = None;
		}
		let mut index = 0;
		while index < text.len() && !self.done {
			if self.string.is_some() {
				index = self.read_string(text, index);
			} else if !self.scalar.is_empty() {
				index = self.read_scalar(text, index);
			} else {
				// invariant: `index` is always at a character boundary of `text`.
				let character = text[index..].chars().next().unwrap_or(' ');
				index += character.len_utf8();
				self.structural(character);
			}
		}
		std::mem::take(&mut self.changes)
	}

	/// The value of the text read so far: `None` before a value starts. Returns the previous snapshot while the text
	/// read since did not change the value, or while a fresh copy of the open path is not yet paid for.
	pub fn snapshot(&mut self) -> Option<&JsonValue> {
		if self.dirty {
			let pending = self.pending_scalar();
			let cost = self.path_cost
				+ self.string.as_ref().map_or(0, |open| open.text.len())
				+ usize::from(pending.is_some());
			if cost <= self.credit {
				self.credit -= cost;
				self.dirty = false;
				self.snapshot = self.copy_open_path(pending);
				self.version += 1;
			}
		}
		self.snapshot.as_ref()
	}

	/// How many snapshots `snapshot()` has made: a new number means a new value.
	pub fn version(&self) -> u64 {
		self.version
	}

	/// The snapshot `snapshot()` returned last, without making a new one.
	pub fn last_snapshot(&self) -> Option<&JsonValue> {
		self.snapshot.as_ref()
	}

	/// Reads string characters from `start` until the string ends or the text does; returns the next index.
	fn read_string(&mut self, text: &str, start: usize) -> usize {
		let mut run_start = start;
		for (offset, character) in text[start..].char_indices() {
			let index = start + offset;
			if self.string.as_ref().is_some_and(|open| !open.escape.is_empty()) {
				if self.continue_escape(character) {
					run_start = index + character.len_utf8();
					continue;
				}
				// Not part of the escape: read it as a normal string character below.
				run_start = index;
			}
			if character == '"' {
				if index > run_start {
					self.append_string(&text[run_start..index]);
				}
				self.end_string();
				return index + 1;
			}
			if character == '\\' {
				if index > run_start {
					self.append_string(&text[run_start..index]);
				}
				if let Some(open) = &mut self.string {
					open.escape.push('\\');
				}
				run_start = index + 1;
			}
		}
		if text.len() > run_start {
			self.append_string(&text[run_start..]);
		}
		text.len()
	}

	/// Reads number or literal characters from `start` up to the next delimiter; returns the delimiter's index.
	fn read_scalar(&mut self, text: &str, start: usize) -> usize {
		let end = text[start..]
			.char_indices()
			.find(|&(_, character)| is_delimiter(character))
			.map_or(text.len(), |(offset, _)| start + offset);
		if end > start {
			self.scalar.push_str(&text[start..end]);
			if self.scalar_slot.is_some() {
				self.dirty = true;
			}
		}
		if end < text.len() {
			self.end_scalar();
		}
		end
	}

	/// Handles one character outside strings.
	fn structural(&mut self, character: char) {
		match character {
			' ' | '\t' | '\n' | '\r' => {}
			'"' => self.start_string(),
			'{' => self.open_container(JsonValue::Object(JsonObject::new())),
			'[' => self.open_container(JsonValue::Array(Vec::new())),
			'}' | ']' => self.close_container(),
			':' => {
				if let Some(frame) = self.stack.last_mut()
					&& frame.container.is_object()
					&& frame.phase == Phase::Colon
				{
					frame.phase = Phase::Value;
				}
			}
			',' => {
				if let Some(frame) = self.stack.last_mut() {
					frame.phase = if frame.container.is_array() {
						Phase::Value
					} else {
						Phase::Key
					};
				}
			}
			_ => {
				self.scalar = character.to_string();
				self.scalar_slot = self.value_slot();
				if self.scalar_slot.is_some() {
					self.dirty = true;
				}
			}
		}
	}

	/// Where a value starting now goes, or `None` when this position takes no value.
	fn value_slot(&self) -> Option<Slot> {
		let Some(frame) = self.stack.last() else {
			return self.root.is_none().then_some(Slot::Root);
		};
		if let JsonValue::Array(elements) = &frame.container {
			return Some(Slot::Element(elements.len()));
		}
		// A missing colon is tolerated: `{"a" 1}` reads as `{"a": 1}`.
		if matches!(frame.phase, Phase::Value | Phase::Colon) {
			return frame.key.clone().map(Slot::Member);
		}
		None
	}

	fn start_string(&mut self) {
		// A missing comma is tolerated: `{"a": 1 "b": 2}` reads the second member.
		if let Some(frame) = self.stack.last()
			&& frame.container.is_object()
			&& matches!(frame.phase, Phase::Key | Phase::Comma)
		{
			self.string = Some(OpenString::new(StringTarget::Key, None, None));
			return;
		}
		let slot = self.value_slot();
		let path = self.reported_path(slot.as_ref());
		self.place(slot.as_ref(), JsonValue::String(String::new()));
		let mut open = OpenString::new(StringTarget::Value, slot, path.clone());
		if let Some(path) = path {
			open.change = Some(self.report(JsonChange::Set {
				path,
				value: JsonValue::String(String::new()),
			}));
		}
		self.string = Some(open);
	}

	/// The path of a value at `slot` in the innermost open container, when its changes are reported.
	fn reported_path(&self, slot: Option<&Slot>) -> Option<JsonPath> {
		let slot = slot?;
		if *slot == Slot::Root {
			return Some(Vec::new());
		}
		let frame = self.stack.last()?;
		if !frame.reports_inside {
			return None;
		}
		let mut path = frame.path.clone()?;
		path.push(match slot {
			Slot::Member(key) => PathSegment::Key(key.clone()),
			Slot::Element(index) => PathSegment::Index(*index),
			Slot::Root => return None,
		});
		Some(path)
	}

	fn report(&mut self, change: JsonChange) -> usize {
		self.changes.push(change);
		self.changes.len() - 1
	}

	/// Continues an escape with `character`; false when it is not part of the escape and must be read normally.
	fn continue_escape(&mut self, character: char) -> bool {
		let Some(open) = &mut self.string else { return false };
		if open.escape == "\\" {
			if let Some(decoded) = simple_escape(character) {
				open.escape.clear();
				self.append_decoded(decoded);
				return true;
			}
			if character == 'u' {
				open.escape.push('u');
				return true;
			}
			// Invalid escape: keep the backslash as text and read the character normally, as repairJson() does.
			open.escape.clear();
			self.append_string("\\");
			return false;
		}
		if character.is_ascii_hexdigit() {
			open.escape.push(character);
			if open.escape.len() < 6 {
				return true;
			}
			// invariant: the escape is `\u` and four hex digits.
			let unit = u16::from_str_radix(&open.escape[2..], 16).unwrap_or(0xfffd);
			open.escape.clear();
			self.append_unit(unit);
			return true;
		}
		// `\u` without four hex digits: keep it as text and read the character normally, as repairJson() does.
		let literal = std::mem::take(&mut open.escape);
		self.append_string(&literal);
		false
	}

	/// Appends one decoded `\uXXXX` code unit, joining surrogate pairs; a lone surrogate becomes U+FFFD.
	fn append_unit(&mut self, unit: u16) {
		let held = self.string.as_mut().and_then(|open| open.high_surrogate.take());
		if let Some(high) = held {
			if (0xdc00..=0xdfff).contains(&unit) {
				let code = 0x10000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(unit) - 0xdc00);
				self.append_text(&char::from_u32(code).unwrap_or('\u{fffd}').to_string());
				return;
			}
			self.append_text("\u{fffd}");
		}
		if (0xd800..=0xdbff).contains(&unit) {
			if let Some(open) = &mut self.string {
				open.high_surrogate = Some(unit);
			}
			return;
		}
		self.append_text(&char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}').to_string());
	}

	/// Appends text decoded from a simple escape.
	fn append_decoded(&mut self, decoded: char) {
		self.append_string(decoded.encode_utf8(&mut [0; 4]));
	}

	/// Appends raw string text: a held high surrogate before it was lone.
	fn append_string(&mut self, text: &str) {
		if self
			.string
			.as_mut()
			.and_then(|open| open.high_surrogate.take())
			.is_some()
		{
			self.append_text("\u{fffd}");
		}
		self.append_text(text);
	}

	fn append_text(&mut self, text: &str) {
		let Some(open) = &mut self.string else { return };
		open.text.push_str(text);
		if open.target != StringTarget::Value {
			return;
		}
		if open.slot.is_some() {
			self.dirty = true;
		}
		let Some(path) = open.path.clone() else { return };
		// Extend this string's newest change while nothing came after it; otherwise report an append.
		if let Some(index) = open.change
			&& index + 1 == self.changes.len()
		{
			match &mut self.changes[index] {
				JsonChange::Set {
					value: JsonValue::String(value),
					..
				} => value.push_str(text),
				JsonChange::Append { text: appended, .. } => appended.push_str(text),
				JsonChange::Set { .. } => {}
			}
			return;
		}
		let index = self.report(JsonChange::Append {
			path,
			text: text.to_owned(),
		});
		if let Some(open) = &mut self.string {
			open.change = Some(index);
		}
	}

	fn end_string(&mut self) {
		if self
			.string
			.as_mut()
			.and_then(|open| open.high_surrogate.take())
			.is_some()
		{
			self.append_text("\u{fffd}");
		}
		let Some(open) = self.string.take() else { return };
		if open.target == StringTarget::Key {
			if let Some(frame) = self.stack.last_mut() {
				frame.key = Some(open.text);
				frame.phase = Phase::Colon;
			}
			return;
		}
		let bytes = open.text.len() + 2;
		self.place(open.slot.as_ref(), JsonValue::String(open.text));
		self.add_bytes(open.slot.as_ref(), bytes);
		self.value_ended();
	}

	fn end_scalar(&mut self) {
		let token = std::mem::take(&mut self.scalar);
		let slot = self.scalar_slot.take();
		if let Some(value) = scalar_value(&token) {
			self.place(slot.as_ref(), value.clone());
			self.add_bytes(slot.as_ref(), token.len());
			if let Some(path) = self.reported_path(slot.as_ref()) {
				self.report(JsonChange::Set { path, value });
			}
		}
		self.value_ended();
	}

	/// The value of the unterminated scalar at the end of the text, if it shows.
	fn pending_scalar(&self) -> Option<(Slot, JsonValue)> {
		let slot = self.scalar_slot.clone()?;
		if self.scalar.is_empty() || self.scalar.len() > MAX_PENDING_SCALAR_LENGTH {
			return None;
		}
		pending_scalar_value(&self.scalar).map(|value| (slot, value))
	}

	fn open_container(&mut self, container: JsonValue) {
		let slot = self.value_slot();
		let path = self.reported_path(slot.as_ref());
		let empty = container.clone();
		// The parent holds a placeholder until the container closes and moves in.
		self.place(slot.as_ref(), JsonValue::Null);
		let reports_inside = path.as_ref().is_some_and(|path| path.len() < MAX_REPORTED_DEPTH);
		let phase = if container.is_array() { Phase::Value } else { Phase::Key };
		self.stack.push(Frame {
			container,
			slot,
			path: path.clone(),
			reports_inside,
			bytes: 0,
			key: None,
			phase,
		});
		self.path_cost += 1;
		self.dirty = true;
		// A fresh container: the working one keeps changing, and a change never does.
		if let Some(path) = path {
			self.report(JsonChange::Set { path, value: empty });
		}
	}

	fn close_container(&mut self) {
		// A stray closer outside any container is ignored.
		let Some(frame) = self.stack.pop() else { return };
		self.path_cost -= frame.bytes + 1;
		self.dirty = true;
		// Too deep to report inside: the whole value, final now that it is closed.
		if let Some(path) = frame.path.clone()
			&& !frame.reports_inside
		{
			self.report(JsonChange::Set {
				path,
				value: frame.container.clone(),
			});
		}
		let bytes = frame.bytes + 2;
		self.place(frame.slot.as_ref(), frame.container);
		self.add_bytes(frame.slot.as_ref(), bytes);
		self.value_ended();
	}

	/// A value in the innermost frame, or the root value when there is no frame, has ended.
	fn value_ended(&mut self) {
		match self.stack.last_mut() {
			// Text after the root value is ignored, but only once a root value exists.
			None => self.done = self.root.is_some(),
			Some(frame) => frame.phase = Phase::Comma,
		}
	}

	/// Sets the value at `slot` in the innermost open container; nothing for a value in an invalid position.
	fn place(&mut self, slot: Option<&Slot>, value: JsonValue) {
		let Some(slot) = slot else { return };
		self.dirty = true;
		match slot {
			Slot::Root => self.root = Some(value),
			_ => {
				if let Some(frame) = self.stack.last_mut() {
					set_slot(&mut frame.container, slot, value);
				}
			}
		}
	}

	/// Counts a completed value's bytes into the copy cost of the frame that holds it.
	fn add_bytes(&mut self, slot: Option<&Slot>, bytes: usize) {
		let Some(slot) = slot else { return };
		if *slot == Slot::Root {
			return;
		}
		let key_bytes = match slot {
			Slot::Member(key) => key.len() + 3,
			_ => 1,
		};
		if let Some(frame) = self.stack.last_mut() {
			frame.bytes += bytes + key_bytes;
			self.path_cost += bytes + key_bytes;
		}
	}

	/// A fresh snapshot: copies of the open containers, innermost first, each holding the copy of its open child.
	fn copy_open_path(&self, pending: Option<(Slot, JsonValue)>) -> Option<JsonValue> {
		let leaf = self
			.string
			.as_ref()
			.filter(|open| open.target == StringTarget::Value)
			.and_then(|open| {
				open.slot
					.clone()
					.map(|slot| (slot, JsonValue::String(open.text.clone())))
			})
			.or(pending);
		if self.stack.is_empty() {
			return match leaf {
				Some((Slot::Root, value)) => Some(value),
				_ => self.root.clone(),
			};
		}
		let mut child = leaf;
		for frame in self.stack.iter().rev() {
			let mut copy = frame.container.clone();
			if let Some((slot, value)) = child.take() {
				set_slot(&mut copy, &slot, value);
			}
			child = frame.slot.clone().map(|slot| (slot, copy));
		}
		// The outermost open container is the root unless it sits in an invalid position.
		match child {
			Some((Slot::Root, value)) => Some(value),
			_ => self.root.clone(),
		}
	}
}

impl OpenString {
	fn new(target: StringTarget, slot: Option<Slot>, path: Option<JsonPath>) -> Self {
		Self {
			target,
			slot,
			text: String::new(),
			escape: String::new(),
			high_surrogate: None,
			path,
			change: None,
		}
	}
}

/// Sets `slot` of a container: an element at the end is pushed, a member keeps its first position.
fn set_slot(container: &mut JsonValue, slot: &Slot, value: JsonValue) {
	match (container, slot) {
		(JsonValue::Array(elements), Slot::Element(index)) => {
			if *index >= elements.len() {
				elements.push(value);
			} else {
				elements[*index] = value;
			}
		}
		(JsonValue::Object(members), Slot::Member(key)) => {
			members.insert(key.clone(), value);
		}
		_ => {}
	}
}

/// The arguments of one streaming tool call: the raw text, for the one full parse when the call ends, and a
/// `StreamingJsonParser` for the partial arguments in between (`ToolCallArgumentsStream`). Providers keep one per
/// call in their own stream state, never on the content block.
#[derive(Default)]
pub struct ToolCallArgumentsStream {
	text: String,
	parser: StreamingJsonParser,
	/// The parser snapshot version the caller last saw.
	version: u64,
	/// The arguments shown while the snapshot is not an object.
	empty: JsonObject,
}

impl ToolCallArgumentsStream {
	/// A stream that has received nothing.
	pub fn new() -> Self {
		Self::default()
	}

	/// The raw argument text received so far.
	pub fn text(&self) -> &str {
		&self.text
	}

	/// The partial arguments as of the last snapshot; tool arguments are an object, so any other partial value shows
	/// as no arguments yet.
	pub fn arguments(&self) -> &JsonObject {
		match self.parser.last_snapshot() {
			Some(JsonValue::Object(object)) => object,
			_ => &self.empty,
		}
	}

	/// Appends a piece of argument text; returns the partial arguments when they changed (TS returns a new object
	/// then and the same object otherwise).
	pub fn append(&mut self, delta: &str) -> Option<&JsonObject> {
		self.text.push_str(delta);
		self.parser.append(delta);
		self.partial()
	}

	/// Takes the provider's complete argument text. When it continues the text so far, returns the missing piece;
	/// otherwise starts over with `text` and returns `None` as the piece. The partial arguments follow when they changed,
	/// and always after a restart.
	pub fn replace(&mut self, text: &str) -> (Option<String>, Option<&JsonObject>) {
		let Some(delta) = text.strip_prefix(self.text.as_str()).map(str::to_owned) else {
			self.text.clear();
			self.parser = StreamingJsonParser::new();
			self.version = 0;
			self.append(text);
			// TS parity: a restart always returns new arguments, even when they read the same.
			return (None, Some(self.arguments()));
		};
		if delta.is_empty() {
			return (Some(delta), self.partial());
		}
		let arguments = self.append(&delta).is_some();
		(Some(delta), arguments.then(|| self.arguments()))
	}

	/// The final arguments: one full parse of the complete text, which reports malformed arguments.
	pub fn finish(&self) -> ToolCallArguments {
		parse_tool_call_arguments(&self.text)
	}

	fn partial(&mut self) -> Option<&JsonObject> {
		self.parser.snapshot();
		if self.parser.version() == self.version {
			return None;
		}
		self.version = self.parser.version();
		Some(self.arguments())
	}
}

fn simple_escape(character: char) -> Option<char> {
	Some(match character {
		'"' => '"',
		'\\' => '\\',
		'/' => '/',
		'b' => '\u{8}',
		'f' => '\u{c}',
		'n' => '\n',
		'r' => '\r',
		't' => '\t',
		_ => return None,
	})
}

fn is_delimiter(character: char) -> bool {
	matches!(
		character,
		' ' | '\t' | '\n' | '\r' | ',' | ':' | '{' | '}' | '[' | ']' | '"'
	)
}

/// The literals and their values.
const LITERALS: [(&str, bool, Option<bool>); 3] = [
	("true", false, Some(true)),
	("false", false, Some(false)),
	("null", true, None),
];

fn literal_value(is_null: bool, flag: Option<bool>) -> JsonValue {
	if is_null {
		JsonValue::Null
	} else {
		JsonValue::Bool(flag.unwrap_or(false))
	}
}

/// The value of a complete number or literal token; `None` for anything else, which is dropped.
fn scalar_value(token: &str) -> Option<JsonValue> {
	for (literal, is_null, flag) in LITERALS {
		if token == literal {
			return Some(literal_value(is_null, flag));
		}
	}
	number_value(token)
}

/// The value an unterminated number or literal token shows.
fn pending_scalar_value(token: &str) -> Option<JsonValue> {
	for (literal, is_null, flag) in LITERALS {
		if literal.starts_with(token) {
			return Some(literal_value(is_null, flag));
		}
	}
	if let Some(value) = number_value(token) {
		return Some(value);
	}
	let mantissa = token
		.strip_suffix(['+', '-'])
		.unwrap_or(token)
		.strip_suffix(['e', 'E'])?;
	number_value(mantissa).filter(|_| !mantissa.contains(['e', 'E']))
}

/// A JSON number token (`-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`) as the value `pi_json::parse` reads.
fn number_value(token: &str) -> Option<JsonValue> {
	if !is_json_number(token) {
		return None;
	}
	pi_json::parse(token).ok()
}

fn is_json_number(token: &str) -> bool {
	let bytes = token.as_bytes();
	let mut at = usize::from(bytes.first() == Some(&b'-'));
	let digits = |at: &mut usize| {
		let start = *at;
		while *at < bytes.len() && bytes[*at].is_ascii_digit() {
			*at += 1;
		}
		*at - start
	};
	match bytes.get(at) {
		Some(b'0') => at += 1,
		Some(b'1'..=b'9') => {
			digits(&mut at);
		}
		_ => return false,
	}
	if bytes.get(at) == Some(&b'.') {
		at += 1;
		if digits(&mut at) == 0 {
			return false;
		}
	}
	if matches!(bytes.get(at), Some(b'e' | b'E')) {
		at += 1;
		if matches!(bytes.get(at), Some(b'+' | b'-')) {
			at += 1;
		}
		if digits(&mut at) == 0 {
			return false;
		}
	}
	at == bytes.len()
}
