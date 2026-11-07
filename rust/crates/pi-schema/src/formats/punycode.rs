//! Punycode (RFC 3492), as TypeBox 1.3.27's `idna/format/puny.ts` (MIT) decodes and encodes it for the hostname
//! checks: decoding takes lowercase digits only and fails where TypeBox throws.

const BASE: u64 = 36;
const TMIN: u64 = 1;
const TMAX: u64 = 26;
const SKEW: u64 = 38;
const DAMP: u64 = 700;
const INITIAL_BIAS: u64 = 72;
const INITIAL_N: u64 = 128;
const MAX_CODE_POINT: u64 = 0x10_ffff;

/// Whether a label has the ACE prefix `xn--` (any case).
pub(super) fn is_ace_prefixed(value: &str) -> bool {
	value.get(..4).is_some_and(|prefix| prefix.eq_ignore_ascii_case("xn--"))
}

fn adapt(delta: u64, points: u64, first: bool) -> u64 {
	let mut delta = if first { delta / DAMP } else { delta >> 1 };
	delta += delta / points;
	let mut k = 0;
	while delta > ((BASE - TMIN) * TMAX) >> 1 {
		delta /= BASE - TMIN;
		k += BASE;
	}
	k + ((BASE - TMIN + 1) * delta) / (delta + SKEW)
}

fn threshold(k: u64, bias: u64) -> u64 {
	if k <= bias {
		TMIN
	} else if k >= bias + TMAX {
		TMAX
	} else {
		k - bias
	}
}

/// The code points of a Punycode payload (after `xn--`); `None` where TypeBox throws: a non-basic code point before
/// the last delimiter, a digit outside `a-z0-9`, a truncated number, or a code point past U+10FFFF.
#[expect(clippy::many_single_char_names, reason = "the names of RFC 3492's algorithm")]
pub(super) fn decode(value: &str) -> Option<Vec<u32>> {
	// Every non-ASCII character fails one of the checks below, so the payload is handled as bytes.
	if !value.is_ascii() {
		return None;
	}
	let input = value.as_bytes();
	let delimiter = value.rfind('-');
	let mut output: Vec<u32> = match delimiter {
		Some(at) if at > 0 => input[..at].iter().map(|b| u32::from(*b)).collect(),
		_ => Vec::new(),
	};
	let (mut n, mut i, mut bias) = (INITIAL_N, 0u64, INITIAL_BIAS);
	let mut at = delimiter.map_or(0, |d| d + 1);
	while at < input.len() {
		let old_i = i;
		let mut w = 1u64;
		let mut k = BASE;
		loop {
			let c = *input.get(at)?;
			at += 1;
			let digit = match c {
				b'a'..=b'z' => u64::from(c - b'a'),
				b'0'..=b'9' => u64::from(c - b'0') + 26,
				_ => return None,
			};
			i = i.checked_add(digit.checked_mul(w)?)?;
			let t = threshold(k, bias);
			if digit < t {
				break;
			}
			w = w.checked_mul(BASE - t)?;
			k += BASE;
		}
		let length = output.len() as u64 + 1;
		bias = adapt(i - old_i, length, old_i == 0);
		n = n.checked_add(i / length)?;
		i %= length;
		if n > MAX_CODE_POINT {
			return None;
		}
		// invariant: n <= 0x10FFFF and i < length <= the output's length + 1, both checked above.
		output.insert(usize::try_from(i).ok()?, u32::try_from(n).ok()?);
		i += 1;
	}
	Some(output)
}

/// The Punycode payload of a Unicode label, to measure the length of its A-label (RFC 5891 4.2.4).
#[expect(clippy::many_single_char_names, reason = "the names of RFC 3492's algorithm")]
pub(super) fn encode(input: &str) -> String {
	let points: Vec<u64> = input.chars().map(|c| u64::from(u32::from(c))).collect();
	let basic: String = input.chars().filter(char::is_ascii).collect();
	let basic_length = basic.len() as u64;
	let mut result = basic;
	if basic_length > 0 {
		result.push('-');
	}
	let (mut n, mut delta, mut bias) = (INITIAL_N, 0u64, INITIAL_BIAS);
	let mut handled = basic_length;
	while handled < points.len() as u64 {
		let m = points.iter().copied().filter(|&cp| cp >= n).min().unwrap_or(n);
		delta += (m - n) * (handled + 1);
		n = m;
		for &cp in &points {
			if cp < n {
				delta += 1;
			}
			if cp == n {
				let mut q = delta;
				let mut k = BASE;
				loop {
					let t = threshold(k, bias);
					if q < t {
						break;
					}
					result.push(digit_char(t + (q - t) % (BASE - t)));
					q = (q - t) / (BASE - t);
					k += BASE;
				}
				result.push(digit_char(q));
				bias = adapt(delta, handled + 1, handled == basic_length);
				delta = 0;
				handled += 1;
			}
		}
		delta += 1;
		n += 1;
	}
	result
}

fn digit_char(digit: u64) -> char {
	// invariant: Punycode digits are below 36.
	let digit = u8::try_from(digit).unwrap_or(0);
	char::from(if digit < 26 { b'a' + digit } else { b'0' + digit - 26 })
}

#[cfg(test)]
mod tests {
	use super::{decode, encode};

	#[test]
	fn encodes_and_decodes_rfc_3492_samples() {
		assert_eq!(encode("bücher"), "bcher-kva");
		assert_eq!(encode("münchen"), "mnchen-3ya");
		let decoded: Option<String> =
			decode("bcher-kva").map(|points| points.into_iter().filter_map(char::from_u32).collect());
		assert_eq!(decoded.as_deref(), Some("bücher"));
		assert_eq!(decode("ab$"), None);
		assert_eq!(decode("é-a"), None);
	}
}
