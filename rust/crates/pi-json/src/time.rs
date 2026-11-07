//! Timestamps as the TS implementation writes and reads them: `Date.prototype.toISOString` and `Date.parse` of the
//! ECMA-262 Date Time String Format (21.4.1.32), as V8 in Node 26 reads it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Formats a time as `Date.prototype.toISOString` does (`YYYY-MM-DDTHH:mm:ss.sssZ`, `±YYYYYY` outside 0..=9999).
/// Past the JS range (±8.64e15 ms), where `toISOString` throws, it prints the proleptic Gregorian date anyway.
pub fn iso_ms(time: SystemTime) -> String {
	let ms = epoch_ms(time);
	let (days, ms_of_day) = (ms.div_euclid(MS_PER_DAY), ms.rem_euclid(MS_PER_DAY));
	let (year, month, day) = civil_from_days(days);
	let year = if (0..=9999).contains(&year) {
		format!("{year:04}")
	} else {
		format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.unsigned_abs())
	};
	format!(
		"{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
		ms_of_day / MS_PER_HOUR,
		ms_of_day / MS_PER_MINUTE % 60,
		ms_of_day / 1000 % 60,
		ms_of_day % 1000
	)
}

const MS_PER_DAY: i128 = 86_400_000;
const MS_PER_HOUR: i128 = 3_600_000;
const MS_PER_MINUTE: i128 = 60_000;

/// Milliseconds since the epoch, rounded down like a JS `Date` holding this time.
fn epoch_ms(time: SystemTime) -> i128 {
	// invariant: a Duration's milliseconds (below 2^64 * 1000) fit an i128.
	match time.duration_since(UNIX_EPOCH) {
		Ok(after) => i128::try_from(after.as_millis()).unwrap_or(i128::MAX),
		Err(before) => {
			let before = before.duration();
			let partial = i128::from(before.subsec_nanos() % 1_000_000 != 0);
			-(i128::try_from(before.as_millis()).unwrap_or(i128::MAX) + partial)
		}
	}
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i128) -> (i128, i128, i128) {
	// Computed here rather than with a date library: JS dates reach year 275760.
	let shifted = days + 719_468;
	let era = shifted.div_euclid(146_097);
	let day_of_era = shifted.rem_euclid(146_097);
	let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_index = (5 * day_of_year + 2) / 153;
	let day = day_of_year - (153 * month_index + 2) / 5 + 1;
	let month = if month_index < 10 {
		month_index + 3
	} else {
		month_index - 9
	};
	let year = year_of_era + era * 400 + i128::from(month <= 2);
	(year, month, day)
}

/// Parses what `Date.parse` in Node 26 reads as an ISO date, to the same millisecond; `None` where it returns `NaN`
/// or a local time. Accepted: `YYYY`, `YYYY-MM` or `YYYY-MM-DD` (years also `±YYYYYY`, not `-000000`; days
/// 01..=31 roll over into the next month as in JS), alone (UTC) or followed by `T`, `HH:mm`, `HH:mm:ss` or
/// `HH:mm:ss.f+` (any number of fraction digits, cut to milliseconds; `24:00` is the next day) and an offset `Z`,
/// `±HH:mm` or `±HHmm`; the result within ±8.64e15 ms. As V8 does, `t`, `z` and a space may stand for `T` and `Z`,
/// and a date alone may end in `Z`. Date-times without an offset are local time in JS and return `None`.
pub fn parse_iso_ms(text: &str) -> Option<SystemTime> {
	// Trust boundary: the text comes from files and the wire.
	let ms = parse_date_time(&mut Cursor { rest: text.as_bytes() })?;
	from_epoch_ms(ms)
}

/// The largest distance from the epoch a JS `Date` holds (ECMA-262 21.4.1.31 `TimeClip`).
const MAX_DATE_MS: i128 = 8_640_000_000_000_000;

fn parse_date_time(cursor: &mut Cursor) -> Option<i128> {
	let date_ms = parse_date(cursor)? * MS_PER_DAY;
	let ms = if cursor.done() {
		date_ms
	} else if cursor.eat(b"Zz") {
		cursor.done().then_some(date_ms)?
	} else if cursor.eat(b"Tt ") {
		let time_ms = parse_time(cursor)?;
		let offset_ms = parse_offset(cursor)?;
		cursor.done().then_some(date_ms + time_ms - offset_ms)?
	} else {
		return None;
	};
	(ms.abs() <= MAX_DATE_MS).then_some(ms)
}

/// `YYYY[-MM[-DD]]` as days since the epoch; JS's `MakeDay` lets `2024-02-30` be March 1.
fn parse_date(cursor: &mut Cursor) -> Option<i128> {
	let year = parse_year(cursor)?;
	let (mut month, mut day) = (1, 1);
	if cursor.eat(b"-") {
		month = cursor.number(2)?;
		if cursor.eat(b"-") {
			day = cursor.number(2)?;
		}
	}
	if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
		return None;
	}
	Some(days_from_civil(year, month) + day - 1)
}

fn parse_year(cursor: &mut Cursor) -> Option<i128> {
	if cursor.eat(b"+") {
		return cursor.number(6);
	}
	if cursor.eat(b"-") {
		// ECMA-262: `-000000` is not a year; V8 accepts `+000000`.
		return cursor.number(6).filter(|&year| year != 0).map(|year| -year);
	}
	cursor.number(4)
}

/// Days from 1970-01-01 to the first of `month` in `year` (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i128, month: i128) -> i128 {
	let year = if month <= 2 { year - 1 } else { year };
	let era = year.div_euclid(400);
	let year_of_era = year.rem_euclid(400);
	let day_of_year = (153 * ((month + 9) % 12) + 2) / 5;
	let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
	era * 146_097 + day_of_era - 719_468
}

/// `HH:mm[:ss[.f+]]` as milliseconds into the day; `24:00` only with zero seconds and fraction.
fn parse_time(cursor: &mut Cursor) -> Option<i128> {
	let hour = cursor.number(2)?;
	if !cursor.eat(b":") {
		return None;
	}
	let minute = cursor.number(2)?;
	let (mut second, mut ms, mut fraction_is_zero) = (0, 0, true);
	if cursor.eat(b":") {
		second = cursor.number(2)?;
		if cursor.eat(b".") {
			(ms, fraction_is_zero) = cursor.fraction()?;
		}
	}
	let next_midnight = hour == 24 && minute == 0 && second == 0 && fraction_is_zero;
	if (hour > 23 && !next_midnight) || minute > 59 || second > 59 {
		return None;
	}
	Some(hour * MS_PER_HOUR + minute * MS_PER_MINUTE + second * 1000 + ms)
}

/// `Z`, `±HH:mm` or `±HHmm` as milliseconds east of UTC; `None` when missing (a local time) or malformed.
fn parse_offset(cursor: &mut Cursor) -> Option<i128> {
	if cursor.eat(b"Zz") {
		return Some(0);
	}
	let sign = if cursor.eat(b"+") {
		1
	} else if cursor.eat(b"-") {
		-1
	} else {
		return None;
	};
	let hours = cursor.number(2)?;
	// TS parity: V8 also reads the basic form `+0100`.
	cursor.eat(b":");
	let minutes = cursor.number(2)?;
	if hours > 23 || minutes > 59 {
		return None;
	}
	Some(sign * (hours * MS_PER_HOUR + minutes * MS_PER_MINUTE))
}

/// The unread rest of the text being parsed.
struct Cursor<'a> {
	rest: &'a [u8],
}

impl Cursor<'_> {
	fn done(&self) -> bool {
		self.rest.is_empty()
	}

	/// Consumes the next byte when it is one of `accepted`.
	fn eat(&mut self, accepted: &[u8]) -> bool {
		match self.rest.split_first() {
			Some((byte, rest)) if accepted.contains(byte) => {
				self.rest = rest;
				true
			}
			_ => false,
		}
	}

	/// Consumes exactly `width` ASCII digits.
	fn number(&mut self, width: usize) -> Option<i128> {
		let digits = self
			.rest
			.get(..width)
			.filter(|digits| digits.iter().all(u8::is_ascii_digit))?;
		self.rest = &self.rest[width..];
		Some(
			digits
				.iter()
				.fold(0, |value, digit| value * 10 + i128::from(digit - b'0')),
		)
	}

	/// Consumes one or more fraction digits: the milliseconds they start with, and whether all of them are zero.
	fn fraction(&mut self) -> Option<(i128, bool)> {
		let count = self.rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
		if count == 0 {
			return None;
		}
		let (digits, rest) = self.rest.split_at(count);
		self.rest = rest;
		// TS parity: V8 reads every digit but keeps the first three, which rounds down.
		let ms = (0..3).fold(0, |value, index| {
			value * 10 + digits.get(index).map_or(0, |digit| i128::from(digit - b'0'))
		});
		Some((ms, digits.iter().all(|&digit| digit == b'0')))
	}
}

fn from_epoch_ms(ms: i128) -> Option<SystemTime> {
	let offset = Duration::from_millis(u64::try_from(ms.unsigned_abs()).ok()?);
	if ms < 0 {
		UNIX_EPOCH.checked_sub(offset)
	} else {
		UNIX_EPOCH.checked_add(offset)
	}
}

#[cfg(test)]
mod tests {
	use super::{iso_ms, parse_iso_ms};
	use std::time::{Duration, UNIX_EPOCH};

	#[test]
	fn sub_millisecond_times_round_down() {
		assert_eq!(
			iso_ms(UNIX_EPOCH + Duration::from_nanos(1_999_999)),
			"1970-01-01T00:00:00.001Z"
		);
		assert_eq!(iso_ms(UNIX_EPOCH - Duration::from_nanos(1)), "1969-12-31T23:59:59.999Z");
	}

	#[test]
	fn offsetless_date_times_are_local_and_rejected() {
		// The corpus cannot hold these: Node's answer depends on the generating machine's time zone.
		assert_eq!(parse_iso_ms("1970-01-01T00:00:00"), None);
		assert_eq!(parse_iso_ms("1970-01-01 00:00"), None);
		assert_eq!(parse_iso_ms("1970-01-01"), Some(UNIX_EPOCH));
	}

	#[test]
	fn a_z_after_a_date_ends_the_text() {
		assert_eq!(parse_iso_ms("2024ZT12:00Z"), None);
		assert_eq!(parse_iso_ms("1970z"), Some(UNIX_EPOCH));
	}
}
