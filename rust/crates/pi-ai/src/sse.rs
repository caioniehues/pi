//! A Server-Sent Events decoder with the exact rules of `iterateSseMessages` in
//! `packages/ai/src/api/anthropic-messages.ts`, used by every SSE wire API.
//!
//! Lines end at CR, LF or CRLF; a CR is a line end as soon as it arrives, so a CRLF split across two chunks yields an
//! extra empty line (TS parity). `data:` lines join with newlines, `event:` names the event, lines starting with `:`
//! are comments, an empty line dispatches, and a final unterminated line and event are flushed at the end of the
//! body. Bytes decode as `TextDecoder` does: UTF-8 across chunk boundaries, invalid bytes as U+FFFD, a leading BOM
//! dropped.

use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use std::collections::VecDeque;

/// One event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
	/// The `event:` name, if any.
	pub event: Option<String>,
	/// The `data:` lines joined with newlines.
	pub data: String,
	/// Every non-empty line of the event, comments included.
	pub raw: Vec<String>,
}

/// Decodes events from a byte stream.
pub struct SseDecoder<S> {
	body: S,
	bytes: Vec<u8>,
	started: bool,
	buffer: String,
	/// The length of the buffer's prefix already scanned without finding a line end.
	scanned: usize,
	state: State,
	ready: VecDeque<ServerSentEvent>,
	finished: bool,
}

#[derive(Default)]
struct State {
	event: Option<String>,
	data: Vec<String>,
	raw: Vec<String>,
}

impl<S, E> SseDecoder<S>
where
	S: Stream<Item = Result<Bytes, E>> + Unpin,
{
	/// A decoder of `body`.
	pub fn new(body: S) -> Self {
		Self {
			body,
			bytes: Vec::new(),
			started: false,
			buffer: String::new(),
			scanned: 0,
			state: State::default(),
			ready: VecDeque::new(),
			finished: false,
		}
	}

	/// The next event; `Ok(None)` at the end of the body, `Err` when the body fails.
	pub async fn next_event(&mut self) -> Result<Option<ServerSentEvent>, E> {
		loop {
			if let Some(event) = self.ready.pop_front() {
				return Ok(Some(event));
			}
			if self.finished {
				return Ok(None);
			}
			match self.body.next().await {
				Some(chunk) => {
					let chunk = chunk?;
					self.bytes.extend_from_slice(&chunk);
					self.decode_available();
					self.consume_lines();
				}
				None => self.finish(),
			}
		}
	}

	/// Decodes the pending bytes into the line buffer, keeping an incomplete sequence at the end for the next chunk.
	fn decode_available(&mut self) {
		let valid_until = self.bytes.len() - incomplete_tail(&self.bytes);
		let decoded = String::from_utf8_lossy(&self.bytes[..valid_until]).into_owned();
		self.bytes.drain(..valid_until);
		self.push_text(&decoded);
	}

	fn push_text(&mut self, text: &str) {
		let text = if self.started {
			text
		} else {
			text.strip_prefix('\u{feff}').unwrap_or(text)
		};
		if !text.is_empty() {
			self.started = true;
		}
		self.buffer.push_str(text);
	}

	/// Decodes every complete line of the buffer. Linear in the text: lines are taken by offset and the buffer is
	/// drained once, and the scan of an unterminated line resumes where it stopped.
	fn consume_lines(&mut self) {
		let mut start = 0;
		loop {
			let from = start.max(self.scanned);
			let Some(offset) = self.buffer[from..].find(['\r', '\n']) else {
				break;
			};
			let end = from + offset;
			let mut next = end + 1;
			if self.buffer.as_bytes()[end] == b'\r' && self.buffer.as_bytes().get(next) == Some(&b'\n') {
				next += 1;
			}
			let line = self.buffer[start..end].to_owned();
			start = next;
			if let Some(event) = self.decode_line(line) {
				self.ready.push_back(event);
			}
		}
		self.buffer.drain(..start);
		self.scanned = self.buffer.len();
	}

	fn finish(&mut self) {
		self.finished = true;
		if !self.bytes.is_empty() {
			let rest = String::from_utf8_lossy(&std::mem::take(&mut self.bytes)).into_owned();
			self.push_text(&rest);
		}
		self.consume_lines();
		if !self.buffer.is_empty() {
			let line = std::mem::take(&mut self.buffer);
			self.scanned = 0;
			if let Some(event) = self.decode_line(line) {
				self.ready.push_back(event);
			}
		}
		if let Some(event) = self.flush() {
			self.ready.push_back(event);
		}
	}

	fn decode_line(&mut self, line: String) -> Option<ServerSentEvent> {
		if line.is_empty() {
			return self.flush();
		}
		if !line.starts_with(':') {
			let (field, value) = match line.split_once(':') {
				Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
				None => (line.as_str(), ""),
			};
			match field {
				"event" => self.state.event = Some(value.to_owned()),
				"data" => self.state.data.push(value.to_owned()),
				_ => {}
			}
		}
		self.state.raw.push(line);
		None
	}

	fn flush(&mut self) -> Option<ServerSentEvent> {
		// TS parity: `!state.event` treats an empty event name as none.
		if self.state.event.as_deref().is_none_or(str::is_empty) && self.state.data.is_empty() {
			return None;
		}
		let state = std::mem::take(&mut self.state);
		Some(ServerSentEvent {
			event: state.event,
			data: state.data.join("\n"),
			raw: state.raw,
		})
	}
}

/// The length of a UTF-8 sequence cut off at the end of `bytes`, which more bytes may complete.
fn incomplete_tail(bytes: &[u8]) -> usize {
	for back in 1..=bytes.len().min(3) {
		let byte = bytes[bytes.len() - back];
		if byte & 0b1100_0000 == 0b1000_0000 {
			continue;
		}
		let needed = match byte {
			0xc0..=0xdf => 2,
			0xe0..=0xef => 3,
			0xf0..=0xf7 => 4,
			_ => return 0,
		};
		return if needed > back { back } else { 0 };
	}
	0
}
