//! Bounded output streams and adaptive progress commits (TS `output.ts`): `OutputBuffer` keeps a head or tail window
//! of a stream at a cost proportional to each chunk, and `Progress` paces commits of a changing value by their size.
//!
//! A Rust `str` cannot hold a lone surrogate, so the TS cases that count one as U+FFFD are the decoder's: invalid
//! UTF-8 in byte chunks decodes to U+FFFD (three bytes), as `TextDecoder` does.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::{Context as TaskContext, Poll, Waker};

/// Which end of a stream the limits keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retain {
	/// The first lines.
	Head,
	/// The last lines.
	Tail,
}

/// Retention limits of one output stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLimits {
	/// Bytes kept at most.
	pub max_bytes: usize,
	/// Lines kept at most.
	pub max_lines: usize,
	/// Which end is kept.
	pub retain: Retain,
}

/// Retained output and what the limits dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedOutput {
	/// The kept text, sanitized.
	pub text: String,
	/// Bytes of the stream not kept.
	pub dropped_bytes: usize,
	/// Lines of the stream not kept.
	pub dropped_lines: usize,
}

/// An exact slice of the input within the limits, and what it left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSlice {
	/// The kept slice.
	pub text: String,
	/// Its UTF-8 length.
	pub bytes: usize,
	/// Bytes of the input not kept.
	pub dropped_bytes: usize,
	/// Lines of the input not kept.
	pub dropped_lines: usize,
}

/// Output a producer omitted before a chunk, measured on the decoded text (every U+FFFD counts as three bytes; no
/// sanitizing is applied).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputSkip {
	/// UTF-8 byte length of the omitted text.
	pub bytes: usize,
	/// Newlines (U+000A) in the omitted text.
	pub newlines: usize,
	/// Whether the omitted text ends with a newline.
	pub ends_with_newline: bool,
}

/// One chunk of a stream: decoded text, or bytes decoded as UTF-8 across chunks.
#[derive(Clone, Copy, Debug)]
pub enum OutputChunk<'a> {
	/// Text.
	Text(&'a str),
	/// UTF-8 bytes; a character may continue in the next byte chunk.
	Bytes(&'a [u8]),
}

/// Why `OutputBuffer::push` refused a chunk; the message is the TS one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
	/// A skip on a head window.
	#[error("Skipped output requires tail retention")]
	SkipNeedsTail,
}

const NEWLINE: u8 = b'\n';
const ESCAPE: u8 = 0x1b;

/// Removes escape sequences and control characters that break display and transcripts; tabs and newlines stay.
pub fn sanitize_output(text: &str) -> String {
	without_escape_sequences(text)
		.chars()
		.filter(|&ch| !matches!(ch, '\x00'..='\x08' | '\x0b'..='\x1f' | '\u{fff9}'..='\u{fffb}'))
		.collect()
}

/// `text` without whole 7-bit escape sequences (TS `ESCAPE_SEQUENCE`): strings (OSC, DCS, APC, PM, SOS) up to BEL or
/// ST, CSI, and short `ESC x` forms; an ESC that starts none of them stays (the control filter removes it).
fn without_escape_sequences(text: &str) -> String {
	let bytes = text.as_bytes();
	let mut out = String::with_capacity(text.len());
	let mut kept_from = 0;
	let mut at = 0;
	while at < bytes.len() {
		if bytes[at] != ESCAPE {
			at += 1;
			continue;
		}
		match escape_sequence_end(bytes, at) {
			Some(end) => {
				// Sequences start and end at ASCII bytes, so both cuts are character boundaries.
				out.push_str(&text[kept_from..at]);
				kept_from = end;
				at = end;
			}
			None => at += 1,
		}
	}
	out.push_str(&text[kept_from..]);
	out
}

/// The end of the escape sequence starting at `start` (an ESC), trying the TS alternatives in order.
fn escape_sequence_end(bytes: &[u8], start: usize) -> Option<usize> {
	let next = *bytes.get(start + 1)?;
	if matches!(next, b']' | b'P' | b'X' | b'^' | b'_') {
		let mut at = start + 2;
		while let Some(&byte) = bytes.get(at) {
			match byte {
				0x07 => return Some(at + 1),
				ESCAPE if bytes.get(at + 1) == Some(&b'\\') => return Some(at + 2),
				ESCAPE => break,
				_ => at += 1,
			}
		}
	}
	if next == b'[' {
		let mut at = start + 2;
		while bytes.get(at).is_some_and(|byte| (0x30..=0x3f).contains(byte)) {
			at += 1;
		}
		while bytes.get(at).is_some_and(|byte| (0x20..=0x2f).contains(byte)) {
			at += 1;
		}
		if bytes.get(at).is_some_and(|byte| (0x40..=0x7e).contains(byte)) {
			return Some(at + 1);
		}
	}
	let mut at = start + 1;
	while bytes.get(at).is_some_and(|byte| (0x20..=0x2f).contains(byte)) {
		at += 1;
	}
	bytes
		.get(at)
		.is_some_and(|byte| (0x30..=0x7e).contains(byte))
		.then_some(at + 1)
}

/// Bounds `text` to whole lines within the limits: the first lines for `Head`, the last for `Tail`. The result is an
/// exact slice, trailing newline included; a single line longer than `max_bytes` is cut on a character boundary.
pub fn bound_output(text: &str, limits: &OutputLimits) -> OutputSlice {
	let bytes = text.as_bytes();
	let (from, to) = match limits.retain {
		Retain::Head => head_range(bytes, limits),
		Retain::Tail => tail_range(bytes, limits),
	};
	let kept = &bytes[from..to];
	OutputSlice {
		// The ranges end on character boundaries.
		text: text[from..to].to_owned(),
		bytes: kept.len(),
		dropped_bytes: bytes.len() - kept.len(),
		dropped_lines: line_count(bytes) - line_count(kept),
	}
}

fn head_range(bytes: &[u8], limits: &OutputLimits) -> (usize, usize) {
	if limits.max_lines == 0 || limits.max_bytes == 0 {
		return (0, 0);
	}
	let mut end = bytes.len();
	let mut newlines = bytes.iter().enumerate().filter(|&(_, &byte)| byte == NEWLINE);
	if let Some((index, _)) = newlines.nth(limits.max_lines - 1) {
		end = index + 1;
	}
	if end > limits.max_bytes {
		end = match last_index_of(bytes, limits.max_bytes - 1) {
			Some(newline) => newline + 1,
			None => character_end(bytes, limits.max_bytes),
		};
	}
	(0, end)
}

fn tail_range(bytes: &[u8], limits: &OutputLimits) -> (usize, usize) {
	if limits.max_lines == 0 || limits.max_bytes == 0 {
		return (bytes.len(), bytes.len());
	}
	// A trailing newline ends the last line rather than starting another.
	let last = if bytes.last() == Some(&NEWLINE) {
		bytes.len().checked_sub(2)
	} else {
		bytes.len().checked_sub(1)
	};
	let mut start = 0;
	let mut lines = 1;
	let mut index = last.and_then(|last| last_index_of(bytes, last));
	while let Some(at) = index {
		if lines == limits.max_lines {
			start = at + 1;
			break;
		}
		lines += 1;
		index = at.checked_sub(1).and_then(|before| last_index_of(bytes, before));
	}
	if bytes.len() - start > limits.max_bytes {
		let from = bytes.len() - limits.max_bytes;
		let newline = index_of(bytes, from - 1);
		// The first line starting inside the byte window, or a cut of the last line when it alone is too long.
		start = match newline {
			Some(newline) if newline + 1 < bytes.len() => newline + 1,
			_ => character_start(bytes, from),
		};
	}
	(start, bytes.len())
}

/// The first newline at or after `from`.
fn index_of(bytes: &[u8], from: usize) -> Option<usize> {
	bytes
		.get(from..)?
		.iter()
		.position(|&byte| byte == NEWLINE)
		.map(|at| from + at)
}

/// The last newline at or before `from`.
fn last_index_of(bytes: &[u8], from: usize) -> Option<usize> {
	let end = (from + 1).min(bytes.len());
	bytes[..end].iter().rposition(|&byte| byte == NEWLINE)
}

/// The last character boundary at or before `index`.
pub fn character_end(bytes: &[u8], index: usize) -> usize {
	let mut end = index;
	while end > 0 && bytes.get(end).copied().unwrap_or(0) & 0xc0 == 0x80 {
		end -= 1;
	}
	end
}

/// The first character boundary at or after `index`.
fn character_start(bytes: &[u8], index: usize) -> usize {
	let mut start = index;
	while start < bytes.len() && bytes[start] & 0xc0 == 0x80 {
		start += 1;
	}
	start
}

fn line_count(bytes: &[u8]) -> usize {
	if bytes.is_empty() {
		return 0;
	}
	count_newlines(bytes) + usize::from(bytes.last() != Some(&NEWLINE))
}

fn count_newlines(bytes: &[u8]) -> usize {
	// A newline is one byte, so counting the pieces between them counts it without a dependency.
	bytes.split(|&byte| byte == NEWLINE).count() - 1
}

/// UTF-8 byte length of `content` (TS `utf8ByteLength`; a Rust string holds no lone surrogate to count as U+FFFD).
pub fn utf8_byte_length(content: &str) -> usize {
	content.len()
}

/// Bounded running output of one stream, such as a tool call's or a process's. Accepting a chunk costs time
/// proportional to the chunk: head retention stops storing once the window is full, and tail retention drops stored
/// text the window no longer needs when it snapshots. Counts of the whole stream keep the dropped totals exact.
#[derive(Debug)]
pub struct OutputBuffer {
	limits: OutputLimits,
	/// Bytes of an incomplete character from the last byte chunk.
	decoder: Utf8Decoder,
	started: bool,
	/// Stored chunks: for head the start of the stream, for tail a suffix that still contains the next window.
	chunks: Vec<StoredChunk>,
	stored_bytes: usize,
	stored_newlines: usize,
	full: bool,
	total_bytes: usize,
	total_newlines: usize,
	ends_with_newline: bool,
}

#[derive(Debug)]
struct StoredChunk {
	text: String,
	bytes: usize,
	newlines: usize,
}

impl OutputBuffer {
	/// An empty buffer with `limits`.
	pub fn new(limits: OutputLimits) -> Self {
		Self {
			limits,
			decoder: Utf8Decoder::default(),
			started: false,
			chunks: Vec::new(),
			stored_bytes: 0,
			stored_newlines: 0,
			full: false,
			total_bytes: 0,
			total_newlines: 0,
			ends_with_newline: true,
		}
	}

	/// Bytes currently held; bounded by the limits plus one chunk.
	pub fn stored_bytes(&self) -> usize {
		self.stored_bytes
	}

	/// Accepts a chunk; true when anything was accepted. `skipped` is output omitted right before the chunk, which
	/// must exceed the tail window by at least one byte or line; only tail retention takes it.
	pub fn push(&mut self, chunk: OutputChunk<'_>, skipped: Option<OutputSkip>) -> Result<bool, OutputError> {
		if skipped.is_some() && self.limits.retain != Retain::Tail {
			return Err(OutputError::SkipNeedsTail);
		}
		// A string chunk or a skip ends an incomplete character of earlier bytes (it becomes U+FFFD).
		let pending = match (chunk, skipped) {
			(OutputChunk::Text(_), _) | (_, Some(_)) => self.decoder.finish(),
			(OutputChunk::Bytes(_), None) => String::new(),
		};
		let mut text = match chunk {
			OutputChunk::Text(text) => text.to_owned(),
			OutputChunk::Bytes(bytes) => self.decoder.decode(bytes),
		};
		let first = !self.started && pending.is_empty() && skipped.is_none();
		if !pending.is_empty() || !text.is_empty() || skipped.is_some() {
			self.started = true;
		}
		// As in `StreamDecoder`: only a byte-order mark at the very start of byte output is dropped.
		if first && matches!(chunk, OutputChunk::Bytes(_)) && text.starts_with('\u{feff}') {
			text.drain(..'\u{feff}'.len_utf8());
		}
		let Some(skipped) = skipped else {
			return Ok(self.accept(pending + &text));
		};
		self.accept(pending);
		self.skip(skipped);
		self.accept(text);
		Ok(true)
	}

	/// Counts omitted output; nothing stored before it can be in the window once the text after it arrives.
	fn skip(&mut self, skipped: OutputSkip) {
		if skipped.bytes == 0 {
			return;
		}
		self.total_bytes += skipped.bytes;
		self.total_newlines += skipped.newlines;
		self.ends_with_newline = skipped.ends_with_newline;
		self.chunks.clear();
		self.stored_bytes = 0;
		self.stored_newlines = 0;
	}

	/// Flushes an incomplete trailing character as U+FFFD; call when the stream ends.
	pub fn end(&mut self) {
		let rest = self.decoder.finish();
		self.accept(rest);
	}

	fn accept(&mut self, text: String) -> bool {
		if text.is_empty() {
			return false;
		}
		let bytes = text.len();
		let newlines = count_newlines(text.as_bytes());
		self.total_bytes += bytes;
		self.total_newlines += newlines;
		self.ends_with_newline = text.ends_with('\n');
		if self.full {
			return true;
		}
		self.chunks.push(StoredChunk { text, bytes, newlines });
		self.stored_bytes += bytes;
		self.stored_newlines += newlines;
		if self.limits.retain == Retain::Head {
			// Nothing past a full window is ever needed.
			self.full = self.stored_bytes > self.limits.max_bytes || self.stored_newlines >= self.limits.max_lines;
			return true;
		}
		// Drop leading chunks while the rest still holds more than a window, plus one byte or line, so the window's
		// line start can still be found. Each chunk is dropped once.
		let mut drop_count = 0;
		for chunk in &self.chunks[..self.chunks.len() - 1] {
			let bytes_after = self.stored_bytes - chunk.bytes;
			let newlines_after = self.stored_newlines - chunk.newlines;
			if bytes_after <= self.limits.max_bytes + 1 && newlines_after <= self.limits.max_lines + 1 {
				break;
			}
			self.stored_bytes = bytes_after;
			self.stored_newlines = newlines_after;
			drop_count += 1;
		}
		self.chunks.drain(..drop_count);
		true
	}

	/// The retained, sanitized output and what the limits dropped from the whole stream.
	pub fn snapshot(&mut self) -> BoundedOutput {
		let stored: String = self.chunks.iter().map(|chunk| chunk.text.as_str()).collect();
		let kept = bound_output(&stored, &self.limits);
		let stored_lines = lines(self.stored_newlines, stored.is_empty() || stored.ends_with('\n'));
		let kept_lines = stored_lines - kept.dropped_lines;
		// Tail windows never reach back before this one, but finding a later window's first line needs what precedes it:
		// keep the shortest suffix longer than the window by a byte or a line, as `accept` does.
		if self.limits.retain == Retain::Tail || self.chunks.len() > 1 {
			let text = match self.limits.retain {
				Retain::Tail => tail_margin(&stored, &self.limits).to_owned(),
				Retain::Head => stored,
			};
			let newlines = count_newlines(text.as_bytes());
			self.stored_bytes = text.len();
			self.stored_newlines = newlines;
			self.chunks = if text.is_empty() {
				Vec::new()
			} else {
				vec![StoredChunk {
					bytes: text.len(),
					text,
					newlines,
				}]
			};
		}
		BoundedOutput {
			text: sanitize_output(&kept.text),
			dropped_bytes: self.total_bytes - kept.bytes,
			dropped_lines: lines(self.total_newlines, self.ends_with_newline) - kept_lines,
		}
	}
}

/// The shortest suffix of `text` with more than `max_bytes` bytes or more than `max_lines` newlines, or all of it.
/// The tail window of any text that ends with this suffix, followed by anything, is the same as of `text` followed by
/// it.
fn tail_margin<'a>(text: &'a str, limits: &OutputLimits) -> &'a str {
	let bytes = text.as_bytes();
	let byte_start = if bytes.len() > limits.max_bytes {
		character_end(bytes, bytes.len() - limits.max_bytes - 1)
	} else {
		0
	};
	let line_start = bytes
		.iter()
		.enumerate()
		.rev()
		.filter(|&(_, &byte)| byte == NEWLINE)
		.nth(limits.max_lines)
		.map_or(0, |(index, _)| index);
	&text[byte_start.max(line_start)..]
}

/// Lines of text with `newlines` newlines; a final unterminated line counts.
fn lines(newlines: usize, terminated: bool) -> usize {
	newlines + usize::from(!terminated)
}

/// A streaming UTF-8 decoder as `TextDecoder("utf-8", { ignoreBOM: true })`: each maximal invalid subpart becomes
/// U+FFFD, and an incomplete character at a chunk's end waits for the next chunk.
#[derive(Debug, Default)]
struct Utf8Decoder {
	pending: Vec<u8>,
}

impl Utf8Decoder {
	fn decode(&mut self, chunk: &[u8]) -> String {
		let mut bytes = std::mem::take(&mut self.pending);
		bytes.extend_from_slice(chunk);
		let mut out = String::with_capacity(bytes.len());
		let mut rest = &bytes[..];
		loop {
			match std::str::from_utf8(rest) {
				Ok(text) => {
					out.push_str(text);
					break;
				}
				Err(error) => {
					let (valid, after) = rest.split_at(error.valid_up_to());
					// invariant: `valid_up_to` is the length of the valid prefix.
					out.push_str(std::str::from_utf8(valid).unwrap_or_default());
					let Some(length) = error.error_len() else {
						self.pending = after.to_vec();
						break;
					};
					out.push('\u{fffd}');
					rest = &after[length..];
				}
			}
		}
		out
	}

	/// Ends the stream: an incomplete character becomes U+FFFD.
	fn finish(&mut self) -> String {
		if self.pending.is_empty() {
			String::new()
		} else {
			self.pending.clear();
			"\u{fffd}".to_owned()
		}
	}
}

/// Each progress commit also buys a pause proportional to what it wrote.
pub const PROGRESS_BYTES_PER_SECOND: f64 = 100.0 * 1024.0;

/// An error a progress commit failed with.
pub type ProgressError = Arc<dyn std::error::Error + Send + Sync>;

/// A progress commit: resolves to the bytes it wrote.
pub type ProgressWrite = Pin<Box<dyn Future<Output = Result<u64, ProgressError>> + Send>>;

/// The clock, timer and task runner progress commits pace on. TS uses `Date.now`, `setTimeout` and the event loop;
/// with no runtime here, the owner supplies all three (a commit is a future that must run on its own).
pub trait ProgressClock: Send + Sync {
	/// Milliseconds since an arbitrary origin.
	fn now(&self) -> f64;

	/// Calls `callback` once after about `delay_ms`; returns a function that disarms it.
	fn set_timer(&self, callback: Box<dyn FnOnce() + Send>, delay_ms: f64) -> Box<dyn FnOnce() + Send>;

	/// Runs `task` to completion in the background.
	fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>);
}

/// Adaptive progress commits: the first change after an idle period commits at once; each commit then delays the next
/// by at least `min_interval_ms` and by its written size at 100 KiB/s. At most one commit is in flight; changes made
/// meanwhile coalesce into the next one.
#[derive(Clone)]
pub struct Progress(Arc<ProgressShared>);

/// One waiter of `mark_and_wait`, settled by the commit that includes its change, or by whoever `stop` hands it to.
#[derive(Clone)]
pub struct ProgressWaiter(Arc<Mutex<Settle>>);

/// The future of `mark_and_wait` and `stop`'s wait.
pub struct ProgressWait(Arc<Mutex<Settle>>);

struct ProgressShared {
	write: Box<dyn Fn() -> ProgressWrite + Send + Sync>,
	on_error: Box<dyn Fn(&ProgressError) + Send + Sync>,
	min_interval_ms: f64,
	clock: Arc<dyn ProgressClock>,
	state: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
	waiters: Vec<ProgressWaiter>,
	timer: Option<ArmedTimer>,
	/// Distinguishes timers, so a callback of a timer already replaced or fired changes nothing.
	next_timer: u64,
	/// Settled when the commit in flight finished; `Some` while one is.
	in_flight: Option<ProgressWaiter>,
	next_at: f64,
	dirty: bool,
	stopped: bool,
}

/// The timer pacing the next commit; `disarm` is `None` until `set_timer` returned.
struct ArmedTimer {
	id: u64,
	disarm: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Default)]
struct Settle {
	result: Option<Result<(), ProgressError>>,
	wakers: Vec<Waker>,
}

impl Progress {
	/// Paces `write` (a commit, resolving to the bytes it wrote) on `clock`; `on_error` gets a failed commit's error.
	pub fn new(
		write: impl Fn() -> ProgressWrite + Send + Sync + 'static,
		on_error: impl Fn(&ProgressError) + Send + Sync + 'static,
		min_interval_ms: f64,
		clock: Arc<dyn ProgressClock>,
	) -> Self {
		Progress(Arc::new(ProgressShared {
			write: Box::new(write),
			on_error: Box::new(on_error),
			min_interval_ms,
			clock,
			state: Mutex::new(ProgressState::default()),
		}))
	}

	/// Schedules a commit.
	pub fn mark(&self) {
		self.0.lock().dirty = true;
		schedule(&self.0);
	}

	/// Schedules a commit; resolves with the commit that includes this change.
	pub fn mark_and_wait(&self) -> ProgressWait {
		let waiter = ProgressWaiter::default();
		let wait = waiter.wait();
		self.0.lock().waiters.push(waiter);
		self.mark();
		wait
	}

	/// Stops committing and waits for the commit in flight; returns the waiters the final commit must settle.
	pub fn stop(&self) -> impl Future<Output = Vec<ProgressWaiter>> + Send + 'static {
		let (disarm, in_flight) = {
			let mut state = self.0.lock();
			state.stopped = true;
			let disarm = state.timer.take().and_then(|timer| timer.disarm);
			(disarm, state.in_flight.as_ref().map(ProgressWaiter::wait))
		};
		if let Some(disarm) = disarm {
			disarm();
		}
		let shared = self.0.clone();
		async move {
			if let Some(in_flight) = in_flight {
				// A failed commit was reported already; stopping only waits for it.
				let _settled = in_flight.await;
			}
			std::mem::take(&mut shared.lock().waiters)
		}
	}
}

impl ProgressShared {
	fn lock(&self) -> MutexGuard<'_, ProgressState> {
		self.state.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

/// Commits now, or arms a timer for when the pause after the last commit is over.
fn schedule(shared: &Arc<ProgressShared>) {
	let (wait, id) = {
		let mut state = shared.lock();
		if state.stopped || state.timer.is_some() || state.in_flight.is_some() {
			return;
		}
		let wait = state.next_at - shared.clock.now();
		if wait > 0.0 {
			let id = state.next_timer;
			state.next_timer += 1;
			// Armed before `set_timer`, so a clock that fires the callback at once finds this timer, not none.
			state.timer = Some(ArmedTimer { id, disarm: None });
			(wait, id)
		} else {
			(wait, 0)
		}
	};
	if wait <= 0.0 {
		flush(shared);
		return;
	}
	let weak: Weak<ProgressShared> = Arc::downgrade(shared);
	let disarm = shared.clock.set_timer(
		Box::new(move || {
			let Some(shared) = weak.upgrade() else {
				return;
			};
			let fired = {
				let mut state = shared.lock();
				let current = state.timer.as_ref().is_some_and(|timer| timer.id == id);
				if current {
					state.timer = None;
				}
				current
			};
			// Checks the clock again: a timer that fires early waits for the rest.
			if fired {
				schedule(&shared);
			}
		}),
		wait,
	);
	let mut state = shared.lock();
	match state.timer.as_mut() {
		Some(timer) if timer.id == id => timer.disarm = Some(disarm),
		// The callback already ran (or `stop` took the timer): nothing is left to disarm.
		_ => {}
	}
}

fn flush(shared: &Arc<ProgressShared>) {
	let (waiters, done) = {
		let mut state = shared.lock();
		if state.stopped || !state.dirty {
			return;
		}
		state.dirty = false;
		let done = ProgressWaiter::default();
		state.in_flight = Some(done.clone());
		(std::mem::take(&mut state.waiters), done)
	};
	let started = shared.clock.now();
	let commit = (shared.write)();
	let shared_for_task = shared.clone();
	shared.clock.spawn(Box::pin(async move {
		let result = commit.await;
		let shared = shared_for_task;
		let next_at = match &result {
			#[expect(clippy::cast_precision_loss, reason = "a commit's size is far below 2^52 bytes")]
			Ok(bytes) => {
				started
					+ shared
						.min_interval_ms
						.max(*bytes as f64 * 1000.0 / PROGRESS_BYTES_PER_SECOND)
			}
			Err(_) => started + shared.min_interval_ms,
		};
		let dirty = {
			let mut state = shared.lock();
			state.next_at = next_at;
			state.in_flight = None;
			state.dirty
		};
		let outcome = result.map(drop);
		for waiter in waiters {
			waiter.settle(outcome.clone());
		}
		if let Err(error) = &outcome {
			(shared.on_error)(error);
		}
		done.resolve();
		if dirty {
			schedule(&shared);
		}
	}));
}

impl Default for ProgressWaiter {
	fn default() -> Self {
		ProgressWaiter(Arc::new(Mutex::new(Settle::default())))
	}
}

impl ProgressWaiter {
	/// Settles the waiter successfully (the change it waits for was committed).
	pub fn resolve(&self) {
		self.settle(Ok(()));
	}

	/// Settles the waiter with `error`.
	pub fn reject(&self, error: ProgressError) {
		self.settle(Err(error));
	}

	fn settle(&self, result: Result<(), ProgressError>) {
		let wakers = {
			let mut settle = self.0.lock().unwrap_or_else(PoisonError::into_inner);
			if settle.result.is_some() {
				return;
			}
			settle.result = Some(result);
			std::mem::take(&mut settle.wakers)
		};
		wakers.into_iter().for_each(Waker::wake);
	}

	fn wait(&self) -> ProgressWait {
		ProgressWait(self.0.clone())
	}
}

impl Future for ProgressWait {
	type Output = Result<(), ProgressError>;

	fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
		let mut settle = self.0.lock().unwrap_or_else(PoisonError::into_inner);
		if let Some(result) = &settle.result {
			return Poll::Ready(result.clone());
		}
		settle.wakers.push(cx.waker().clone());
		Poll::Pending
	}
}

#[cfg(test)]
mod tests {
	use super::{
		BoundedOutput, OutputBuffer, OutputChunk, OutputLimits, OutputSkip, Progress, ProgressClock, ProgressError,
		Retain, bound_output, sanitize_output,
	};
	use crate::test_executor::{Executor, now};
	use std::future::Future;
	use std::pin::Pin;
	use std::sync::atomic::{AtomicBool, Ordering};
	use std::sync::{Arc, Mutex};
	use tokio_util::sync::CancellationToken;

	fn head(max_lines: usize, max_bytes: usize) -> OutputLimits {
		OutputLimits {
			max_bytes,
			max_lines,
			retain: Retain::Head,
		}
	}

	fn tail(max_lines: usize, max_bytes: usize) -> OutputLimits {
		OutputLimits {
			max_bytes,
			max_lines,
			retain: Retain::Tail,
		}
	}

	fn bound(text: &str, limits: OutputLimits) -> (String, usize, usize) {
		let slice = bound_output(text, &limits);
		(slice.text, slice.dropped_bytes, slice.dropped_lines)
	}

	fn kept(text: &str, dropped_bytes: usize, dropped_lines: usize) -> (String, usize, usize) {
		(text.to_owned(), dropped_bytes, dropped_lines)
	}

	fn push(buffer: &mut OutputBuffer, text: &str) {
		buffer.push(OutputChunk::Text(text), None).unwrap();
	}

	fn bytes(buffer: &mut OutputBuffer, bytes: &[u8]) {
		buffer.push(OutputChunk::Bytes(bytes), None).unwrap();
	}

	fn snapshot(text: &str, dropped_bytes: usize, dropped_lines: usize) -> BoundedOutput {
		BoundedOutput {
			text: text.to_owned(),
			dropped_bytes,
			dropped_lines,
		}
	}

	/// A tail slice of `content` of at most `max_bytes`, cut forward to a character boundary (TS `bufferTail`).
	fn buffer_tail(content: &str, max_bytes: usize) -> &str {
		if content.len() <= max_bytes {
			return content;
		}
		let mut start = content.len() - max_bytes;
		while !content.is_char_boundary(start) {
			start += 1;
		}
		&content[start..]
	}

	fn assert_matches_buffer_tail(input: &str, limits: &[usize]) {
		for &max_bytes in limits {
			let kept = bound_output(input, &tail(10, max_bytes)).text;
			assert_eq!(kept, buffer_tail(input, max_bytes), "{input:?} {max_bytes}");
			assert!(kept.len() <= max_bytes);
		}
	}

	fn sampled_byte_limits(input: &str) -> Vec<usize> {
		let total = input.len();
		let mut limits: Vec<usize> = [
			0,
			1,
			2,
			3,
			4,
			5,
			8,
			total / 2,
			total.saturating_sub(4),
			total.saturating_sub(1),
			total,
			total + 1,
		]
		.to_vec();
		limits.sort_unstable();
		limits.dedup();
		limits
	}

	// TS: output.test.ts > tool output bounds > removes control characters but keeps tabs, newlines, and other text
	#[test]
	fn removes_control_characters_but_keeps_tabs_newlines_and_other_text() {
		assert_eq!(
			sanitize_output("a\0b\tc\nd\re\u{7}f\u{fff9}g\u{fffb}h😀"),
			"ab\tc\ndefgh😀"
		);
	}

	// TS: output.test.ts > tool output bounds > removes whole ANSI escape sequences, not only their ESC bytes (#11604)
	#[test]
	fn issue_11604_removes_whole_ansi_escape_sequences() {
		assert_eq!(sanitize_output("\x1b[31mERROR\x1b[0m\n"), "ERROR\n");
		assert_eq!(sanitize_output("\x1b[1;38;5;208mx\x1b[?25l\x1b[2K\x1b(By\x1b7z"), "xyz");
		assert_eq!(
			sanitize_output("\x1b]8;;https://pi.dev\x07link\x1b]8;;\x1b\\ \x1b]0;title\x07ok"),
			"link ok"
		);
		let mut output = OutputBuffer::new(tail(100, 1024));
		for chunk in ["\x1b[3", "1mERROR\x1b[0", "m\n"] {
			push(&mut output, chunk);
		}
		assert_eq!(output.snapshot().text, "ERROR\n");
	}

	// TS: output.test.ts > tool output bounds > keeps output within the limits unchanged, keeps nothing with a zero
	// limit, keeps exact slices of whole lines, cuts at the byte limit on whole lines when possible
	#[test]
	fn bounds_whole_lines() {
		assert_eq!(bound("a\nb\n", head(2, 1000)), kept("a\nb\n", 0, 0));
		assert_eq!(bound("a\nb", tail(2, 1000)), kept("a\nb", 0, 0));
		assert_eq!(bound("", tail(2, 1000)), kept("", 0, 0));
		assert_eq!(bound("ab\ncd\n", head(10, 0)), kept("", 6, 2));
		assert_eq!(bound("ab\ncd\n", tail(0, 1000)), kept("", 6, 2));
		assert_eq!(bound("a\nb\nc\n", head(2, 1000)), kept("a\nb\n", 2, 1));
		assert_eq!(bound("a\nb\nc\n", tail(2, 1000)), kept("b\nc\n", 2, 1));
		assert_eq!(bound("a\nb\nc", tail(2, 1000)), kept("b\nc", 2, 1));
		assert_eq!(bound("a\nb\nc\n\n", tail(3, 1000)), kept("b\nc\n\n", 2, 1));
		assert_eq!(bound("aa\nbb\ncc\n", head(10, 7)), kept("aa\nbb\n", 3, 1));
		assert_eq!(bound("aa\nbb\ncc\n", tail(10, 7)), kept("bb\ncc\n", 3, 1));
	}

	// TS: output.test.ts > tool output bounds > cuts a single line longer than the byte limit on a character boundary,
	// keeps a U+FEFF at the start of a kept slice
	#[test]
	fn cuts_a_long_line_on_a_character_boundary() {
		assert_eq!(bound("ééé\n", head(10, 5)), kept("éé", 3, 0));
		assert_eq!(bound("x\néééé", tail(10, 5)), kept("éé", 6, 1));
		assert_eq!(bound("x\u{feff}a", tail(10, 4)), kept("\u{feff}a", 1, 0));
	}

	/// Checks `prefix` and every extension of it by up to `depth` characters of `alphabet`.
	fn exhaustive(prefix: &str, depth: usize, alphabet: &[&str]) {
		assert_matches_buffer_tail(prefix, &sampled_byte_limits(prefix));
		if depth == 0 {
			return;
		}
		for character in alphabet {
			exhaustive(&format!("{prefix}{character}"), depth - 1, alphabet);
		}
	}

	// TS: output.test.ts > tool output bounds > cuts tails exactly like Buffer across deterministic fuzz cases (the
	// alphabet without lone surrogates; "surrogate edge cases" needs strings Rust cannot hold)
	#[test]
	fn cuts_tails_exactly_like_buffer_across_fuzz_cases() {
		let alphabet = [
			"a",
			"\u{7f}",
			"\u{80}",
			"é",
			"\u{7ff}",
			"\u{800}",
			"中",
			"\u{d7ff}",
			"🙂",
			"\u{e000}",
			"\u{ffff}",
			"👩‍💻",
		];
		exhaustive("", 3, &alphabet);
		let mut seed: u32 = 0x1234_5678;
		let mut random = move || {
			seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			f64::from(seed) / 4_294_967_296.0
		};
		#[expect(
			clippy::cast_possible_truncation,
			clippy::cast_sign_loss,
			reason = "small and non-negative"
		)]
		for _ in 0..1_000 {
			let length = (random() * 80.0) as usize;
			let input: String = (0..length)
				.map(|_| alphabet[index(&mut random, alphabet.len())])
				.collect();
			assert_matches_buffer_tail(&input, &sampled_byte_limits(&input));
		}
	}

	// TS: output.test.ts > OutputBuffer > keeps exact totals across chunks and decodes UTF-8 split across byte chunks
	#[test]
	fn keeps_exact_totals_and_decodes_utf8_split_across_byte_chunks() {
		let mut buffer = OutputBuffer::new(tail(2, 1000));
		let emoji = "😀\n".as_bytes();
		push(&mut buffer, "a\nb\n");
		bytes(&mut buffer, &emoji[..2]);
		bytes(&mut buffer, &emoji[2..]);
		assert_eq!(buffer.snapshot(), snapshot("b\n😀\n", 2, 1));
	}

	// TS: output.test.ts > OutputBuffer > sanitizes the retained text but counts the raw stream
	#[test]
	fn sanitizes_the_retained_text_but_counts_the_raw_stream() {
		let mut buffer = OutputBuffer::new(tail(1, 1000));
		push(&mut buffer, "a\u{7}\n");
		assert_eq!(buffer.snapshot(), snapshot("a\n", 0, 0));
		push(&mut buffer, "b\u{1b}\n");
		assert_eq!(buffer.snapshot(), snapshot("b\n", 3, 1));
	}

	// TS: output.test.ts > OutputBuffer > drops a byte-order mark only at the start of the output
	#[test]
	fn drops_a_byte_order_mark_only_at_the_start_of_the_output() {
		let mut buffer = OutputBuffer::new(tail(10, 1000));
		bytes(&mut buffer, &[0xef]);
		bytes(&mut buffer, &[0xbb, 0xbf, 0x61]);
		push(&mut buffer, "b");
		bytes(&mut buffer, &[0xef, 0xbb, 0xbf, 0x63]);
		buffer.end();
		assert_eq!(buffer.snapshot().text, "ab\u{feff}c");
	}

	// TS: output.test.ts > OutputBuffer > flushes an incomplete character before a string chunk and at the end
	#[test]
	fn flushes_an_incomplete_character_before_a_string_chunk_and_at_the_end() {
		let mut buffer = OutputBuffer::new(tail(10, 1000));
		let euro = "€".as_bytes();
		bytes(&mut buffer, &euro[..1]);
		push(&mut buffer, "x");
		bytes(&mut buffer, &euro[..2]);
		buffer.end();
		assert_eq!(buffer.snapshot().text, "\u{fffd}x\u{fffd}");
	}

	// TS: output.test.ts > OutputBuffer > matches bounding the whole stream when several chunks arrive between
	// snapshots
	#[test]
	fn matches_bounding_the_whole_stream() {
		for limits in [head(3, 40), tail(3, 40), head(50, 25), tail(50, 25)] {
			let mut buffer = OutputBuffer::new(limits);
			let mut stream = String::new();
			for index in 0..300 {
				let chunk = if index % 7 == 0 {
					format!("{}\n", "é".repeat(index % 30))
				} else {
					format!("line {index}\n")
				};
				stream.push_str(&chunk);
				push(&mut buffer, &chunk);
				if index % 5 != 4 {
					continue;
				}
				let expected = bound_output(&stream, &limits);
				assert_eq!(
					buffer.snapshot(),
					snapshot(&expected.text, expected.dropped_bytes, expected.dropped_lines)
				);
			}
		}
	}

	// TS: output.test.ts > OutputBuffer > stops storing head output once the window is full, and stores only the tail
	// window after each snapshot
	#[test]
	fn stores_only_the_window() {
		let mut buffer = OutputBuffer::new(head(2, 1000));
		for index in 0..1000 {
			push(&mut buffer, &format!("line {index}\n"));
		}
		assert!(buffer.stored_bytes() < 20);
		assert_eq!(buffer.snapshot(), snapshot("line 0\nline 1\n", 8876, 998));

		let mut buffer = OutputBuffer::new(tail(3, 100));
		let mut stream = String::new();
		for index in 0..2000 {
			let chunk = format!("line {index}\n\n");
			stream.push_str(&chunk);
			push(&mut buffer, &chunk);
			let snapshot = buffer.snapshot();
			assert!(buffer.stored_bytes() <= 100);
			assert_eq!(snapshot.text, bound_output(&stream, &tail(3, 100)).text);
		}
	}

	/// TS mulberry32.
	fn mulberry32(seed: u32) -> impl FnMut() -> f64 {
		let mut state = seed;
		move || {
			state = state.wrapping_add(0x6d2b_79f5);
			let mut t = state;
			t = (t ^ (t >> 15)).wrapping_mul(t | 1);
			t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 0x3d));
			f64::from(t ^ (t >> 14)) / 4_294_967_296.0
		}
	}

	const ALPHABET: [&str; 15] = [
		"a", "b", "z", " ", "\n", "\n", "\n", "\r\n", "\t", "é", "€", "😀", "\u{fffd}", "\x01", "\x1b",
	];

	#[expect(
		clippy::cast_possible_truncation,
		clippy::cast_sign_loss,
		reason = "small and non-negative"
	)]
	fn index(next: &mut impl FnMut() -> f64, count: usize) -> usize {
		(next() * f64::from(u32::try_from(count).unwrap())) as usize
	}

	fn random_chunk(next: &mut impl FnMut() -> f64) -> String {
		let length = index(next, 12);
		(0..length).map(|_| ALPHABET[index(next, ALPHABET.len())]).collect()
	}

	fn measure(text: &str) -> OutputSkip {
		OutputSkip {
			bytes: text.len(),
			newlines: text.matches('\n').count(),
			ends_with_newline: text.ends_with('\n'),
		}
	}

	fn random_tail_limits(next: &mut impl FnMut() -> f64) -> OutputLimits {
		let max_bytes = 1 + index(next, 40);
		let max_lines = 1 + index(next, 5);
		tail(max_lines, max_bytes)
	}

	// TS: output-skip.test.ts > OutputBuffer skipped output > keeps the same tail whenever progress snapshots happen
	#[test]
	fn keeps_the_same_tail_whenever_snapshots_happen() {
		for seed in 1..=3000 {
			let mut next = mulberry32(seed);
			let limits = random_tail_limits(&mut next);
			let (mut plain, mut sampled) = (OutputBuffer::new(limits), OutputBuffer::new(limits));
			for _ in 0..index(&mut next, 30) {
				let chunk = random_chunk(&mut next);
				push(&mut plain, &chunk);
				push(&mut sampled, &chunk);
				if next() < 0.4 {
					sampled.snapshot();
				}
			}
			assert_eq!(sampled.snapshot(), plain.snapshot(), "seed {seed}");
		}
	}

	// TS: output-skip.test.ts > OutputBuffer skipped output > matches the full stream for every legal skip pattern
	#[test]
	fn matches_the_full_stream_for_every_legal_skip_pattern() {
		for seed in 1..=3000 {
			let mut next = mulberry32(seed);
			let limits = random_tail_limits(&mut next);
			let (mut full, mut skipping) = (OutputBuffer::new(limits), OutputBuffer::new(limits));
			let mut pending = String::new();
			let flush = |pending: &mut String,
			             skipping: &mut OutputBuffer,
			             full: &mut OutputBuffer,
			             next: &mut dyn FnMut() -> f64| {
				if pending.is_empty() {
					return;
				}
				let cuts: Vec<usize> = pending
					.char_indices()
					.map(|(at, _)| at)
					.skip(1)
					.chain([pending.len()])
					.filter(|&cut| {
						let rest = measure(&pending[cut..]);
						rest.bytes > limits.max_bytes || rest.newlines > limits.max_lines
					})
					.collect();
				if !cuts.is_empty() && next() < 0.7 {
					let cut = cuts[index(&mut || next(), cuts.len())];
					skipping
						.push(OutputChunk::Text(&pending[cut..]), Some(measure(&pending[..cut])))
						.unwrap();
				} else {
					push(skipping, pending);
				}
				pending.clear();
				if next() < 0.5 {
					skipping.snapshot();
				}
				assert_eq!(skipping.snapshot(), full.snapshot(), "seed {seed}");
			};
			for _ in 0..index(&mut next, 40) {
				let chunk = random_chunk(&mut next);
				push(&mut full, &chunk);
				if next() < 0.3 {
					full.snapshot();
				}
				pending.push_str(&chunk);
				if next() < 0.3 {
					flush(&mut pending, &mut skipping, &mut full, &mut next);
				}
			}
			flush(&mut pending, &mut skipping, &mut full, &mut next);
			full.end();
			skipping.end();
			assert_eq!(skipping.snapshot(), full.snapshot(), "seed {seed}");
		}
	}

	// TS: output-skip.test.ts > OutputBuffer skipped output > counts skipped bytes, lines and the final newline exactly,
	// and refuses skips for head retention
	#[test]
	fn counts_skipped_output_exactly_and_refuses_skips_for_head_retention() {
		let limits = tail(2, 1000);
		let (mut full, mut skipping) = (OutputBuffer::new(limits), OutputBuffer::new(limits));
		let (omitted, kept) = ("one\ntwo\nthr", "ee\nfour\nfive\nsix");
		push(&mut full, &format!("{omitted}{kept}"));
		skipping.push(OutputChunk::Text(kept), Some(measure(omitted))).unwrap();
		assert_eq!(skipping.snapshot(), full.snapshot());
		assert_eq!(skipping.snapshot(), snapshot("five\nsix", 19, 4));
		let mut head_buffer = OutputBuffer::new(head(2, 10));
		let refused = head_buffer.push(
			OutputChunk::Text("x\ny\nz\n"),
			Some(OutputSkip {
				bytes: 3,
				newlines: 1,
				ends_with_newline: true,
			}),
		);
		assert_eq!(
			refused.unwrap_err().to_string(),
			"Skipped output requires tail retention"
		);
	}

	/// A fake clock: virtual time, timers fired by `advance`, commits run on a test executor.
	struct FakeClock {
		now: Mutex<f64>,
		timers: Mutex<Vec<Timer>>,
		executor: Executor,
	}

	struct Timer {
		due: f64,
		delay: f64,
		callback: Option<Box<dyn FnOnce() + Send>>,
		disarmed: Arc<AtomicBool>,
	}

	impl FakeClock {
		fn new(start: f64) -> Arc<Self> {
			Arc::new(FakeClock {
				now: Mutex::new(start),
				timers: Mutex::default(),
				executor: Executor::default(),
			})
		}

		fn set(&self, time: f64) {
			*self.now.lock().unwrap() = time;
		}

		/// Runs tasks, then fires due timers in order up to `by` ms from now.
		fn advance(&self, by: f64) {
			self.executor.run();
			let target = self.now() + by;
			loop {
				let next = {
					let mut timers = self.timers.lock().unwrap();
					let due = timers
						.iter_mut()
						.filter(|timer| {
							timer.callback.is_some() && !timer.disarmed.load(Ordering::SeqCst) && timer.due <= target
						})
						.min_by(|a, b| a.due.total_cmp(&b.due));
					due.map(|timer| (timer.due, timer.callback.take()))
				};
				let Some((due, Some(callback))) = next else { break };
				self.set(due);
				callback();
				self.executor.run();
			}
			self.set(target);
			self.executor.run();
		}

		fn delays(&self) -> Vec<f64> {
			self.timers.lock().unwrap().iter().map(|timer| timer.delay).collect()
		}

		/// Fires timer `index` now, whatever its due time (a timer that fires early).
		fn fire(&self, index: usize) {
			let callback = self.timers.lock().unwrap()[index].callback.take();
			if let Some(callback) = callback {
				callback();
			}
			self.executor.run();
		}
	}

	impl ProgressClock for FakeClock {
		fn now(&self) -> f64 {
			*self.now.lock().unwrap()
		}

		fn set_timer(&self, callback: Box<dyn FnOnce() + Send>, delay_ms: f64) -> Box<dyn FnOnce() + Send> {
			let disarmed = Arc::new(AtomicBool::new(false));
			let due = self.now() + delay_ms;
			self.timers.lock().unwrap().push(Timer {
				due,
				delay: delay_ms,
				callback: Some(callback),
				disarmed: disarmed.clone(),
			});
			Box::new(move || disarmed.store(true, Ordering::SeqCst))
		}

		fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) {
			self.executor.spawn(task);
		}
	}

	/// A progress whose commits record the time and write `size` bytes.
	fn recording(
		clock: &Arc<FakeClock>,
		size: Arc<Mutex<u64>>,
		min_interval_ms: f64,
	) -> (Progress, Arc<Mutex<Vec<f64>>>) {
		let commits: Arc<Mutex<Vec<f64>>> = Arc::default();
		let (on_commit, on_clock) = (commits.clone(), clock.clone());
		let progress = Progress::new(
			move || {
				on_commit.lock().unwrap().push(on_clock.now());
				let size = *size.lock().unwrap();
				Box::pin(async move { Ok(size) })
			},
			|_| {},
			min_interval_ms,
			clock.clone(),
		);
		(progress, commits)
	}

	// TS: output.test.ts > Progress > commits the first change at once, then waits at least 100 ms and the written size
	// at 100 KiB/s
	#[test]
	fn commits_at_once_then_waits_the_minimum_and_the_written_size() {
		let clock = FakeClock::new(0.0);
		let size = Arc::new(Mutex::new(50 * 1024));
		let (progress, commits) = recording(&clock, size.clone(), 100.0);
		progress.mark();
		clock.advance(0.0);
		assert_eq!(*commits.lock().unwrap(), [0.0]);
		*size.lock().unwrap() = 10;
		progress.mark();
		progress.mark();
		clock.advance(499.0);
		assert_eq!(*commits.lock().unwrap(), [0.0]);
		clock.advance(1.0);
		assert_eq!(*commits.lock().unwrap(), [0.0, 500.0]);
		progress.mark();
		clock.advance(99.0);
		assert_eq!(*commits.lock().unwrap(), [0.0, 500.0]);
		clock.advance(1.0);
		assert_eq!(*commits.lock().unwrap(), [0.0, 500.0, 600.0]);
	}

	// TS: output.test.ts > Progress > waits the configured minimum interval between small commits
	#[test]
	fn waits_the_configured_minimum_interval_between_small_commits() {
		let clock = FakeClock::new(0.0);
		let (progress, commits) = recording(&clock, Arc::new(Mutex::new(10)), 500.0);
		progress.mark();
		clock.advance(0.0);
		progress.mark();
		clock.advance(499.0);
		assert_eq!(*commits.lock().unwrap(), [0.0]);
		clock.advance(1.0);
		assert_eq!(*commits.lock().unwrap(), [0.0, 500.0]);
	}

	// TS: output.test.ts > Progress > paces on the clock it is given
	#[test]
	fn paces_on_the_clock_it_is_given() {
		let clock = FakeClock::new(1_000.0);
		let (progress, commits) = recording(&clock, Arc::new(Mutex::new(10)), 100.0);
		let first = progress.mark_and_wait();
		clock.advance(0.0);
		assert!(now(first).is_some_and(|result| result.is_ok()));
		assert_eq!(*commits.lock().unwrap(), [1_000.0]);
		clock.set(1_040.0);
		let second = progress.mark_and_wait();
		assert_eq!(clock.delays(), [60.0]);
		clock.set(1_080.0);
		clock.fire(0);
		assert_eq!(*commits.lock().unwrap(), [1_000.0]);
		assert_eq!(clock.delays(), [60.0, 20.0]);
		clock.set(1_100.0);
		clock.fire(1);
		assert!(now(second).is_some_and(|result| result.is_ok()));
		assert_eq!(*commits.lock().unwrap(), [1_000.0, 1_100.0]);
		progress.mark();
		assert_eq!(clock.delays(), [60.0, 20.0, 100.0]);
		assert_eq!(now(progress.stop()).map(|waiters| waiters.len()), Some(0));
		assert!(clock.timers.lock().unwrap()[2].disarmed.load(Ordering::SeqCst));
	}

	// TS: output.test.ts > Progress > rejects the waiters of a failed commit and reports its error
	#[test]
	fn rejects_the_waiters_of_a_failed_commit_and_reports_its_error() {
		let clock = FakeClock::new(0.0);
		let errors: Arc<Mutex<Vec<String>>> = Arc::default();
		let on_error = errors.clone();
		let progress = Progress::new(
			|| Box::pin(async { Err(Arc::new(std::io::Error::other("commit failed")) as ProgressError) }),
			move |error| on_error.lock().unwrap().push(error.to_string()),
			100.0,
			clock.clone(),
		);
		let wait = progress.mark_and_wait();
		clock.advance(0.0);
		assert_eq!(
			now(wait)
				.and_then(Result::err)
				.map(|error| error.to_string())
				.as_deref(),
			Some("commit failed")
		);
		assert_eq!(*errors.lock().unwrap(), ["commit failed"]);
	}

	// TS: output.test.ts > Progress > stops: waits for the commit in flight and hands back waiters no commit covered yet
	#[test]
	fn stops_waiting_for_the_commit_in_flight_and_hands_back_uncovered_waiters() {
		let clock = FakeClock::new(0.0);
		let release = CancellationToken::new();
		let gate = release.clone();
		let progress = Progress::new(
			move || {
				let gate = gate.clone();
				Box::pin(async move {
					gate.cancelled_owned().await;
					Ok(0)
				})
			},
			|_| {},
			100.0,
			clock.clone(),
		);
		let first = progress.mark_and_wait();
		let second = progress.mark_and_wait();
		clock.advance(0.0);
		let stopped: Arc<Mutex<Option<Vec<super::ProgressWaiter>>>> = Arc::default();
		let (on_stop, stop) = (stopped.clone(), progress.stop());
		clock.executor.spawn(async move {
			*on_stop.lock().unwrap() = Some(stop.await);
		});
		clock.advance(0.0);
		assert!(stopped.lock().unwrap().is_none());
		release.cancel();
		clock.advance(0.0);
		assert!(now(first).is_some_and(|result| result.is_ok()));
		let pending = stopped.lock().unwrap().take().unwrap();
		assert_eq!(pending.len(), 1);
		pending[0].resolve();
		assert!(now(second).is_some_and(|result| result.is_ok()));
	}

	/// A clock whose timers fire inside `set_timer`, after moving time to their due time.
	struct EagerClock {
		now: Mutex<f64>,
		executor: Executor,
	}

	impl ProgressClock for EagerClock {
		fn now(&self) -> f64 {
			*self.now.lock().unwrap()
		}

		fn set_timer(&self, callback: Box<dyn FnOnce() + Send>, delay_ms: f64) -> Box<dyn FnOnce() + Send> {
			*self.now.lock().unwrap() += delay_ms;
			callback();
			Box::new(|| {})
		}

		fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) {
			self.executor.spawn(task);
		}
	}

	// Rust-only (D0b review): a clock that fires a timer synchronously inside `set_timer` does not leave a stale timer
	// behind that would block every later commit.
	#[test]
	fn keeps_committing_on_a_clock_that_fires_synchronously() {
		let clock = Arc::new(EagerClock {
			now: Mutex::new(0.0),
			executor: Executor::default(),
		});
		let commits = Arc::new(Mutex::new(Vec::new()));
		let (on_commit, on_clock) = (commits.clone(), clock.clone());
		let progress = Progress::new(
			move || {
				on_commit.lock().unwrap().push(on_clock.now());
				Box::pin(async { Ok(10) })
			},
			|_| {},
			100.0,
			clock.clone(),
		);
		for _ in 0..3 {
			progress.mark();
			clock.executor.run();
		}
		assert_eq!(*commits.lock().unwrap(), [0.0, 100.0, 200.0]);
	}
}
