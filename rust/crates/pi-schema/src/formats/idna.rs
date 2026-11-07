//! The `hostname` and `idn-hostname` formats (TypeBox 1.3.27's `idna/`, MIT): labels of letters, digits and hyphens,
//! A-labels whose Punycode decodes to a valid U-label, and for `idn-hostname` U-labels under the rules of RFC 5891,
//! RFC 5892 (contextual code points) and RFC 5893 (Bidi). Lengths are counted in UTF-16 units, as TypeBox counts them.

use super::bidi::{has_bidi_chars, has_rtl, satisfies_bidi_rule};
use super::patterns::nfc;
use super::punycode;
use super::unicode::{is_combining_mark, is_disallowed, is_greek, is_hebrew, is_japanese, is_permitted, is_virama};

const MAX_HOSTNAME: usize = 253;
const MAX_LABEL: usize = 63;

/// A hostname of ASCII labels and A-labels (RFC 1123, RFC 5891).
pub(super) fn is_hostname(value: &str) -> bool {
	let length = utf16_length(value);
	if length == 0 || length > MAX_HOSTNAME || value.ends_with('.') {
		return false;
	}
	value
		.split('.')
		.all(|label| label_length_ok(label) && (is_puny_label(label) || is_ascii_label(label)))
}

/// An internationalized hostname (RFC 5890 to 5893).
pub(super) fn is_idn_hostname(value: &str) -> bool {
	if value.is_empty() || value.contains(' ') {
		return false;
	}
	let normalized = normalize_hostname(value);
	if utf16_length(&normalized) > MAX_HOSTNAME {
		return false;
	}
	let labels: Vec<&str> = normalized.split('.').collect();
	let bidi = labels.iter().any(|label| has_bidi_chars(label));
	labels.iter().all(|label| {
		label_length_ok(label)
			&& (is_puny_label(label) || is_unicode_label(label))
			&& (!bidi || satisfies_bidi_rule(label))
	})
}

/// Maps fullwidth ASCII (`ａ`, U+FF41) to ASCII, normalizes to NFC so equivalent sequences compare alike, removes the
/// code points IDNA mapping ignores, and maps the full stop variants to `.`, so the hostname splits into labels.
fn normalize_hostname(value: &str) -> String {
	let ascii: String = value
		.chars()
		.map(|c| match u32::from(c) {
			cp @ 0xff01..=0xff5e => char::from_u32(cp - 0xfee0).unwrap_or(c),
			_ => c,
		})
		.collect();
	nfc(&ascii)
		.chars()
		.filter(
			|c| !matches!(u32::from(*c), 0xad | 0x34f | 0x180b..=0x180d | 0x200b | 0xfe00..=0xfe0f | 0xe0100..=0xe01ef),
		)
		.map(|c| {
			if matches!(c, '\u{3002}' | '\u{ff0e}' | '\u{ff61}') {
				'.'
			} else {
				c
			}
		})
		.collect()
}

fn utf16_length(s: &str) -> usize {
	s.encode_utf16().count()
}

fn label_length_ok(label: &str) -> bool {
	(1..=MAX_LABEL).contains(&utf16_length(label))
}

/// An ASCII LDH label (RFC 5891 4.2.3.1): letters, digits and hyphens, no hyphen first or last, none in the third
/// and fourth positions (reserved for `xn--`).
fn is_ascii_label(label: &str) -> bool {
	let b = label.as_bytes();
	b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
		&& !label.starts_with('-')
		&& !label.ends_with('-')
		&& b.get(2..4) != Some(b"--")
}

/// An A-label: `xn--` then Punycode that decodes to a U-label with at least one non-ASCII code point.
fn is_puny_label(label: &str) -> bool {
	if !punycode::is_ace_prefixed(label) {
		return false;
	}
	let body = label[4..].to_lowercase();
	// A payload that is only a hyphen, or starts with the delimiter (like "-9uc"), has no basic code points before
	// it and is malformed.
	if body.rfind('-') == Some(0) {
		return false;
	}
	let Some(points) = punycode::decode(&body) else {
		return false;
	};
	if points.iter().all(|cp| *cp < 0x80) {
		return false;
	}
	// A surrogate is in no permitted category, so a label holding one is never a U-label.
	let decoded: Option<String> = points.into_iter().map(char::from_u32).collect();
	decoded.is_some_and(|decoded| is_unicode_label(&decoded))
}

/// A U-label under RFC 5891 4.2.3 and RFC 5892: A-label length, Bidi, hyphens, no leading combining mark, permitted
/// code points, and the contextual rules for the middle dot, keraia, geresh, the joiners and the katakana middle dot.
fn is_unicode_label(label: &str) -> bool {
	let non_ascii = !label.is_ascii();
	// RFC 5890 2.3.2.1: the A-label, "xn--" and the Punycode, has at most 63 octets.
	if non_ascii && punycode::encode(label).len() + 4 > MAX_LABEL {
		return false;
	}
	if has_rtl(label.chars().map(u32::from)) && !satisfies_bidi_rule(label) {
		return false;
	}
	let points: Vec<u32> = label.chars().map(u32::from).collect();
	let hyphens =
		points.first() == Some(&0x2d) || points.last() == Some(&0x2d) || points.get(2..4) == Some(&[0x2d, 0x2d][..]);
	if hyphens || points.first().is_some_and(|cp| is_combining_mark(*cp)) {
		return false;
	}
	let mut japanese = false;
	for (i, &cp) in points.iter().enumerate() {
		if is_disallowed(cp) || !is_permitted(cp) {
			return false;
		}
		japanese |= is_japanese(cp);
		let prev = i.checked_sub(1).map(|p| points[p]).filter(|p| *p != 0);
		let next = points.get(i + 1).copied().filter(|n| *n != 0);
		let context_ok = match cp {
			// MIDDLE DOT (Catalan): between two `l`.
			0xb7 => prev == Some(0x6c) && next == Some(0x6c),
			// GREEK KERAIA: before a Greek character.
			0x375 => next.is_some_and(is_greek),
			// HEBREW GERESH and GERSHAYIM: after a Hebrew character.
			0x5f3 | 0x5f4 => prev.is_some_and(is_hebrew),
			// ZWNJ (RFC 5892 A.1): full validation needs the joining types; ASCII before it can never satisfy it.
			0x200c => prev.is_some_and(|p| p >= 0x80 || is_virama(p)),
			// ZWJ (RFC 5892 A.2): after a virama.
			0x200d => prev.is_some_and(is_virama),
			_ => true,
		};
		if !context_ok {
			return false;
		}
	}
	// RFC 5892 A.7: a KATAKANA MIDDLE DOT needs a Hiragana, Katakana or Han character in the label.
	!points.contains(&0x30fb) || japanese
}
