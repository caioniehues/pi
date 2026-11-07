//! Reading JSON text the TS implementation wrote: `serde_json` after a pre-pass for lone surrogate escapes.
//!
//! A JS string is UTF-16 and may hold an unpaired surrogate; `JSON.stringify` writes it as an escape (`"\ud83d"`),
//! for example when streamed partial-JSON tool arguments split an emoji escape across two `a` ops. `serde_json`
//! rejects such text outright, which would make TS-written frames and stored documents unreadable. Decided rule
//! (2026-11-06, D0a review): every lone surrogate escape, `\uD800`-`\uDBFF` not followed by `\uDC00`-`\uDFFF` and any
//! `\uDC00`-`\uDFFF` without a high surrogate before it, reads as U+FFFD; parsing never fails on one. A pair split
//! across two ops and joined by TS therefore reads as two U+FFFD where TS has the character: a recorded, accepted
//! divergence (rust/docs/slices.md, "Decided divergences"); well-formed text round-trips exactly. The pi-chord delta
//! corpus (`rust/fixtures/chord-delta/gen.mjs`) leaves out cases whose results hold lone surrogates for this reason.

use crate::JsonValue;
use std::borrow::Cow;

/// Parses JSON text as `serde_json::from_str` does, but reads each lone surrogate escape as U+FFFD.
pub fn parse(text: &str) -> serde_json::Result<JsonValue> {
	parse_bytes(text.as_bytes())
}

/// `parse` for UTF-8 bytes.
pub fn parse_bytes(bytes: &[u8]) -> serde_json::Result<JsonValue> {
	serde_json::from_slice(&replace_lone_surrogates(bytes))
}

/// The escape every lone surrogate escape becomes; as long as the escape it replaces, so offsets do not move.
const REPLACEMENT: &[u8; 6] = b"\\uFFFD";

/// `bytes` with every lone surrogate escape rewritten to `�`; borrowed when there is none.
fn replace_lone_surrogates(bytes: &[u8]) -> Cow<'_, [u8]> {
	// The fast path: text without `\u` has no escape to look at.
	if !bytes.windows(2).any(|pair| pair == b"\\u") {
		return Cow::Borrowed(bytes);
	}
	let mut out = Cow::Borrowed(bytes);
	let mut at = 0;
	while at < bytes.len() {
		if bytes[at] != b'\\' {
			at += 1;
			continue;
		}
		// Every backslash starts an escape (outside strings it is invalid JSON, which serde_json reports), so skipping
		// the escaped byte keeps `\\u` from being read as an escape.
		let Some(unit) = escape_unit(bytes, at) else {
			at += 2;
			continue;
		};
		if is_high(unit) && escape_unit(bytes, at + 6).is_some_and(is_low) {
			at += 12;
			continue;
		}
		if is_high(unit) || is_low(unit) {
			out.to_mut()[at..at + 6].copy_from_slice(REPLACEMENT);
		}
		at += 6;
	}
	out
}

/// The code unit of the `\uXXXX` escape at `at`, if one is there.
fn escape_unit(bytes: &[u8], at: usize) -> Option<u16> {
	let escape = bytes.get(at..at + 6)?;
	if escape[0] != b'\\' || escape[1] != b'u' {
		return None;
	}
	let hex = std::str::from_utf8(&escape[2..]).ok()?;
	u16::from_str_radix(hex, 16).ok()
}

fn is_high(unit: u16) -> bool {
	(0xd800..=0xdbff).contains(&unit)
}

fn is_low(unit: u16) -> bool {
	(0xdc00..=0xdfff).contains(&unit)
}

#[cfg(test)]
mod tests {
	use super::{parse, parse_bytes};
	use serde_json::json;

	#[test]
	fn lone_surrogates_read_as_replacement_characters() {
		assert_eq!(parse(r#""\ud83d""#).unwrap(), json!("\u{FFFD}"));
		assert_eq!(parse(r#""\ude00x""#).unwrap(), json!("\u{FFFD}x"));
		assert_eq!(parse(r#""\ud83d😀""#).unwrap(), json!("\u{FFFD}😀"));
		assert_eq!(parse(r#"["\ude00\ud83d"]"#).unwrap(), json!(["\u{FFFD}\u{FFFD}"]));
		assert_eq!(parse(r#"{"\udbff":1}"#).unwrap(), json!({"\u{FFFD}": 1}));
	}

	#[test]
	fn well_formed_text_reads_exactly() {
		assert_eq!(parse(r#""😀é\\ud83d""#).unwrap(), json!("😀é\\ud83d"));
		assert_eq!(parse_bytes(br#"{"a":[1,"b"]}"#).unwrap(), json!({"a": [1, "b"]}));
		assert!(parse(r#""\u12""#).is_err());
		assert!(parse("[1,").is_err());
	}
}
