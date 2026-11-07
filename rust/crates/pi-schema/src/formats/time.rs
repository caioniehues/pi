//! The RFC 3339 date and time formats (TypeBox 1.3.27's `IsDate`, `IsTime`, `IsDateTime`, MIT): their shapes, the
//! days of each month, and a leap second only at 23:59:60 UTC.

/// A full date, `2020-12-12`, with the days its month has.
pub(super) fn is_date(value: &str) -> bool {
	let b = value.as_bytes();
	let shaped = b.len() == 10 && b[4] == b'-' && b[7] == b'-';
	if !shaped
		|| ![0..4, 5..7, 8..10]
			.into_iter()
			.all(|r| b[r].iter().all(u8::is_ascii_digit))
	{
		return false;
	}
	let (year, month, day) = (digits(&b[0..4]), digits(&b[5..7]), digits(&b[8..10]));
	let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
	let days = match month {
		2 if leap => 29,
		2 => 28,
		4 | 6 | 9 | 11 => 30,
		_ => 31,
	};
	(1..=12).contains(&month) && day >= 1 && day <= days
}

/// A time with a time zone, `20:20:40Z` or `20:20:40.5+01:00`; seconds may be 60 for a leap second.
pub(super) fn is_time(value: &str) -> bool {
	let b = value.as_bytes();
	let two = |at: usize| {
		b.get(at..at + 2)
			.filter(|d| d.iter().all(u8::is_ascii_digit))
			.map(digits)
	};
	let (Some(hour), Some(minute), Some(second)) = (two(0), two(3), two(6)) else {
		return false;
	};
	if b[2] != b':' || b[5] != b':' {
		return false;
	}
	let mut rest = &b[8..];
	if rest.first() == Some(&b'.') {
		let fraction = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
		if fraction == 0 {
			return false;
		}
		rest = &rest[1 + fraction..];
	}
	let offset = match rest {
		[b'Z' | b'z'] => 0,
		[sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] if [h1, h2, m1, m2].iter().all(|d| d.is_ascii_digit()) => {
			let (offset_hour, offset_minute) = (digits(&[*h1, *h2]), digits(&[*m1, *m2]));
			if offset_hour > 23 || offset_minute > 59 {
				return false;
			}
			let minutes = offset_hour * 60 + offset_minute;
			if *sign == b'-' { -minutes } else { minutes }
		}
		// TypeBox requires a time zone (strictTimeZone).
		_ => return false,
	};
	if hour > 23 || minute > 59 || second > 60 {
		return false;
	}
	if second < 60 {
		return true;
	}
	// A leap second must fall on 23:59:60 UTC.
	(hour * 60 + minute - offset).rem_euclid(1440) == 1439
}

/// A date and a time joined by `T` (either case), `2020-12-12T20:20:40+00:00`.
pub(super) fn is_date_time(value: &str) -> bool {
	let mut parts = value.split(['T', 't']);
	match (parts.next(), parts.next(), parts.next()) {
		(Some(date), Some(time), None) => is_date(date) && is_time(time),
		_ => false,
	}
}

fn digits(bytes: &[u8]) -> i32 {
	bytes.iter().fold(0, |n, d| n * 10 + i32::from(d - b'0'))
}
