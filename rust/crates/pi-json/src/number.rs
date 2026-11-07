//! `Number.prototype.toString` for doubles: ECMA-262 `Number::toString` (6.1.6.1.20) on `ryu`'s shortest digits.

/// Integers below this magnitude are exact in a double and print as plain integers.
const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;

/// Formats a finite double exactly as JS `String(n)` does; `-0` prints `0`. Non-finite values print as JS does too.
pub fn js_number_to_string(value: f64) -> String {
	if value.is_nan() {
		return "NaN".to_owned();
	}
	if value == 0.0 {
		return "0".to_owned();
	}
	if value.is_infinite() {
		return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
	}
	if value.fract() == 0.0 && value.abs() < EXACT_INTEGER_LIMIT {
		// The fast path for the common case: the cast is exact below 2^53, and so is the shortest form.
		#[expect(clippy::cast_possible_truncation, reason = "integral and below 2^53, checked above")]
		return (value as i64).to_string();
	}
	let mut buffer = ryu::Buffer::new();
	let (digits, exponent) = shortest_digits(buffer.format_finite(value.abs()));
	let mut out = String::with_capacity(digits.len() + 8);
	if value < 0.0 {
		out.push('-');
	}
	write_digits(&digits, exponent, &mut out);
	out
}

/// Splits `ryu` output (`1.5e-7`, `123.0`, `0.001`) into the digits `s` without leading or trailing zeros and the
/// exponent `n` of ECMA-262: the value is `0.s * 10^n`.
fn shortest_digits(formatted: &str) -> (String, i32) {
	let (mantissa, exponent) = match formatted.split_once('e') {
		// invariant: ryu writes a decimal exponent that fits an i32 (three digits and a sign at most).
		Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap_or(0)),
		None => (formatted, 0),
	};
	let integer_len = mantissa.find('.').unwrap_or(mantissa.len());
	let mut digits = String::with_capacity(mantissa.len());
	let mut leading_zeros = 0;
	for digit in mantissa.chars().filter(|&c| c != '.') {
		if digit == '0' && digits.is_empty() {
			leading_zeros += 1;
		} else {
			digits.push(digit);
		}
	}
	digits.truncate(digits.trim_end_matches('0').len());
	(digits, digit_count(integer_len) + exponent - leading_zeros)
}

fn digit_count(count: usize) -> i32 {
	// invariant: ryu output is at most a few dozen characters.
	i32::try_from(count).unwrap_or(i32::MAX)
}

/// Writes the digits `s` (k of them) at exponent `n` in the form `Number::toString` picks for that `n`.
fn write_digits(digits: &str, n: i32, out: &mut String) {
	let k = digit_count(digits.len());
	if k <= n && n <= 21 {
		out.push_str(digits);
		push_zeros(n - k, out);
	} else if 0 < n && n <= 21 {
		let (integer, fraction) = digits.split_at(n.unsigned_abs() as usize);
		out.push_str(integer);
		out.push('.');
		out.push_str(fraction);
	} else if -6 < n && n <= 0 {
		out.push_str("0.");
		push_zeros(-n, out);
		out.push_str(digits);
	} else {
		let (first, rest) = digits.split_at(1);
		out.push_str(first);
		if !rest.is_empty() {
			out.push('.');
			out.push_str(rest);
		}
		out.push('e');
		out.push(if n > 0 { '+' } else { '-' });
		out.push_str(&(n - 1).unsigned_abs().to_string());
	}
}

fn push_zeros(count: i32, out: &mut String) {
	for _ in 0..count {
		out.push('0');
	}
}

#[cfg(test)]
mod tests {
	use super::js_number_to_string;

	#[test]
	fn non_finite_values_print_as_js() {
		assert_eq!(js_number_to_string(f64::NAN), "NaN");
		assert_eq!(js_number_to_string(f64::INFINITY), "Infinity");
		assert_eq!(js_number_to_string(f64::NEG_INFINITY), "-Infinity");
	}

	#[test]
	fn every_branch_of_number_to_string() {
		let cases = [
			(-0.0, "0"),
			(42.0, "42"),
			(9_007_199_254_740_992.0, "9007199254740992"),
			(1_152_921_504_606_846_976.0, "1152921504606847000"),
			(1e20, "100000000000000000000"),
			(123.456, "123.456"),
			(0.5, "0.5"),
			(0.000_001, "0.000001"),
			(1e-7, "1e-7"),
			(1.5e-7, "1.5e-7"),
			(1e21, "1e+21"),
			(1.5e300, "1.5e+300"),
			(5e-324, "5e-324"),
		];
		for (value, expected) in cases {
			assert_eq!(js_number_to_string(value), expected, "{value:e}");
		}
	}
}
