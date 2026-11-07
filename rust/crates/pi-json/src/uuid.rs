//! Random and time-ordered UUIDs, as `crypto.randomUUID` and `uuidv7` (`packages/ai/src/utils/uuid.ts`) make them.

use crate::HEX_DIGITS;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

/// Why `uuid_v7` could not make an id: the `RangeError`s of the TS implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UuidError {
	/// The timestamp (supplied, or the clock's) is past 48 bits or before the epoch.
	#[error("UUIDv7 timestamp must be an integer between 0 and {MAX_UUID_V7_TIMESTAMP}")]
	TimestampOutOfRange,
	/// The process has made 2^41 ids since its first one.
	#[error("UUIDv7 generator sequence exhausted")]
	SequenceExhausted,
}

/// Makes a random (version 4) UUID, lowercase and hyphenated.
pub fn uuid_v4() -> String {
	let mut bytes = random_bytes();
	bytes[6] = 0x40 | (bytes[6] & 0x0f);
	bytes[8] = 0x80 | (bytes[8] & 0x3f);
	hyphenated(&bytes)
}

const MAX_UUID_V7_TIMESTAMP: u64 = 0xffff_ffff_ffff;
const MAX_SEQUENCE: u64 = (1 << 41) - 1;

/// The generator's process-wide state, shared by every thread as the TS module state is shared by every caller.
struct Generator {
	/// The highest clock reading used so far; `None` before the first id made from the clock.
	last_ordinary_timestamp: Option<u64>,
	/// A random 40-bit start on first use, then incremented once per id, whatever its timestamp.
	sequence: Option<u64>,
}

static GENERATOR: Mutex<Generator> = Mutex::new(Generator {
	last_ordinary_timestamp: None,
	sequence: None,
});

/// Makes a time-ordered (version 7) UUID; a supplied timestamp is kept as is, for follower ids.
pub fn uuid_v7(timestamp_ms: Option<u64>) -> Result<String, UuidError> {
	let source = match timestamp_ms {
		Some(timestamp) => Source::Supplied(timestamp),
		None => Source::Clock(clock_ms()?),
	};
	let (Source::Supplied(requested) | Source::Clock(requested)) = source;
	if requested > MAX_UUID_V7_TIMESTAMP {
		return Err(UuidError::TimestampOutOfRange);
	}
	// Nothing under the lock can panic, so a poisoned lock still holds consistent state.
	let mut generator = GENERATOR.lock().unwrap_or_else(PoisonError::into_inner);
	let bytes = generator.next(source, random_bytes())?;
	drop(generator);
	Ok(hyphenated(&bytes))
}

/// Where an id's timestamp comes from: a caller (kept as is) or the clock (kept from stepping back).
#[derive(Clone, Copy)]
enum Source {
	Supplied(u64),
	Clock(u64),
}

impl Generator {
	/// The id's bytes for a range-checked timestamp, advancing the state; `bytes` are fresh random bytes.
	fn next(&mut self, source: Source, mut bytes: [u8; 16]) -> Result<[u8; 16], UuidError> {
		let timestamp = match source {
			Source::Supplied(timestamp) => timestamp,
			Source::Clock(reading) => {
				// A clock that steps back reuses the highest reading, so ids stay ordered.
				let timestamp = self.last_ordinary_timestamp.map_or(reading, |last| reading.max(last));
				self.last_ordinary_timestamp = Some(timestamp);
				timestamp
			}
		};
		let sequence = match self.sequence {
			None => u64::from_be_bytes([0, 0, 0, bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]]),
			Some(MAX_SEQUENCE) => return Err(UuidError::SequenceExhausted),
			Some(sequence) => sequence + 1,
		};
		self.sequence = Some(sequence);
		bytes[..6].copy_from_slice(&timestamp.to_be_bytes()[2..]);
		bytes[6] = 0x70 | (low_byte(sequence >> 37) & 0x0f);
		bytes[7] = low_byte(sequence >> 29);
		bytes[8] = 0x80 | (low_byte(sequence >> 23) & 0x3f);
		bytes[9] = low_byte(sequence >> 15);
		bytes[10] = low_byte(sequence >> 7);
		bytes[11] = low_byte(sequence << 1) | (bytes[11] & 0x01);
		Ok(bytes)
	}
}

/// `Date.now()`; a clock before the epoch fails as the TS range check would.
fn clock_ms() -> Result<u64, UuidError> {
	let since_epoch = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map_err(|_| UuidError::TimestampOutOfRange)?;
	u64::try_from(since_epoch.as_millis()).map_err(|_| UuidError::TimestampOutOfRange)
}

fn low_byte(value: u64) -> u8 {
	value.to_be_bytes()[7]
}

fn random_bytes() -> [u8; 16] {
	let mut bytes = [0; 16];
	// invariant: the OS random source fails only on platforms the port does not support; Node's
	// `crypto.getRandomValues` throws there too.
	#[expect(clippy::expect_used, reason = "no supported platform lacks an OS random source")]
	getrandom::fill(&mut bytes).expect("OS random source");
	bytes
}

fn hyphenated(bytes: &[u8; 16]) -> String {
	let mut out = String::with_capacity(36);
	for (index, byte) in bytes.iter().enumerate() {
		if matches!(index, 4 | 6 | 8 | 10) {
			out.push('-');
		}
		out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
		out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
	}
	out
}

#[cfg(test)]
mod tests {
	use super::{Generator, MAX_SEQUENCE, MAX_UUID_V7_TIMESTAMP, Source, UuidError, uuid_v4, uuid_v7};

	fn is_uuid(text: &str, version: char) -> bool {
		let bytes = text.as_bytes();
		text.len() == 36
			&& [8, 13, 18, 23].iter().all(|&index| bytes[index] == b'-')
			&& text
				.chars()
				.filter(|&c| c != '-')
				.all(|c| matches!(c, '0'..='9' | 'a'..='f'))
			&& text[14..15].starts_with(version)
			&& matches!(&text[19..20], "8" | "9" | "a" | "b")
	}

	#[test]
	fn v4_is_random_and_well_formed() {
		let (first, second) = (uuid_v4(), uuid_v4());
		assert!(is_uuid(&first, '4'), "{first}");
		assert_ne!(first, second);
	}

	#[test]
	fn v7_keeps_a_supplied_timestamp_and_orders_by_sequence() {
		let first = uuid_v7(Some(0x0123_4567_89ab)).unwrap();
		let second = uuid_v7(Some(0x0123_4567_89ab)).unwrap();
		assert!(is_uuid(&first, '7'), "{first}");
		assert!(first.starts_with("01234567-89ab-7"), "{first}");
		assert!(first < second, "{first} {second}");
	}

	#[test]
	fn v7_from_the_clock_is_ordered() {
		let ids: Vec<String> = (0..100).map(|_| uuid_v7(None).unwrap()).collect();
		assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
	}

	#[test]
	fn v7_rejects_timestamps_past_48_bits() {
		assert_eq!(
			uuid_v7(Some(MAX_UUID_V7_TIMESTAMP + 1)),
			Err(UuidError::TimestampOutOfRange)
		);
		assert!(uuid_v7(Some(MAX_UUID_V7_TIMESTAMP)).is_ok());
	}

	#[test]
	fn v7_fails_once_the_sequence_is_exhausted() {
		// A local generator: exhausting the process-wide one would break every later id in the test binary.
		let mut generator = Generator {
			last_ordinary_timestamp: None,
			sequence: Some(MAX_SEQUENCE - 1),
		};
		assert!(generator.next(Source::Supplied(0), [0; 16]).is_ok());
		assert_eq!(
			generator.next(Source::Supplied(0), [0; 16]),
			Err(UuidError::SequenceExhausted)
		);
	}
}
