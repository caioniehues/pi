//! The character classes of the IDNA checks (TypeBox 1.3.27's `idna/pattern/pattern.ts`, MIT): Unicode general
//! categories and scripts, by the same ECMAScript `\p{...}` classes TypeBox tests, and RFC 5892's code point lists.
//! Every class takes a code point as a `u32`, since a decoded Punycode label can hold a surrogate, which is in none
//! of the property classes.

use super::patterns::pattern;
use regress::Regex;
use std::sync::LazyLock;

fn has_property(re: &Regex, cp: u32) -> bool {
	let Some(c) = char::from_u32(cp) else { return false };
	re.find(c.encode_utf8(&mut [0; 4])).is_some()
}

static LETTER: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{L}", "u"));

/// `\p{L}`.
pub(super) fn is_letter(cp: u32) -> bool {
	has_property(&LETTER, cp)
}

static MARK_NONSPACING: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{Mn}", "u"));

/// `\p{Mn}`.
pub(super) fn is_mark_nonspacing(cp: u32) -> bool {
	has_property(&MARK_NONSPACING, cp)
}

static MARK_SPACING_COMBINING: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{Mc}", "u"));
static MARK_ENCLOSING: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{Me}", "u"));

/// `[\p{Mn}\p{Mc}\p{Me}]`.
pub(super) fn is_combining_mark(cp: u32) -> bool {
	is_mark_nonspacing(cp) || has_property(&MARK_SPACING_COMBINING, cp) || has_property(&MARK_ENCLOSING, cp)
}

static NUMBER_DECIMAL: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{Nd}", "u"));

/// RFC 5892's PVALID general categories, as TypeBox approximates them: letters, `-+`, `.,:/`, decimal digits,
/// non-spacing and spacing marks, and the CONTEXTO and PVALID exceptions.
pub(super) fn is_permitted(cp: u32) -> bool {
	is_letter(cp)
		|| matches!(cp, 0x2d | 0x2b | 0x2e | 0x2c | 0x3a | 0x2f)
		|| has_property(&NUMBER_DECIMAL, cp)
		|| is_mark_nonspacing(cp)
		|| has_property(&MARK_SPACING_COMBINING, cp)
		|| matches!(cp, 0xb7 | 0x375 | 0x5f3 | 0x5f4 | 0x200c | 0x200d | 0x30fb)
		|| matches!(cp, 0xdf | 0x3c2 | 0x6fd | 0x6fe | 0xf0b | 0x3007)
}

/// RFC 5892's DISALLOWED exceptions.
pub(super) fn is_disallowed(cp: u32) -> bool {
	matches!(
		cp,
		0x640 | 0x7fa | 0x302e | 0x302f | 0x3031 | 0x3032 | 0x3033 | 0x3034 | 0x3035 | 0x303b
	)
}

/// The viramas RFC 5892's ZWJ and ZWNJ rules accept before a joiner.
pub(super) fn is_virama(cp: u32) -> bool {
	matches!(
		cp,
		0x094d
			| 0x09cd | 0x0a4d
			| 0x0acd | 0x0b4d
			| 0x0bcd | 0x0c4d
			| 0x0ccd | 0x0d3b
			| 0x0d3c | 0x0d4d
			| 0x0dca | 0x1b44
			| 0x1baa | 0x1bab
			| 0xa9c0 | 0x11046
			| 0x1107f | 0x110b9
			| 0x11133 | 0x11134
			| 0x111c0 | 0x11235
			| 0x1134d | 0x11442
			| 0x114c2 | 0x115bf
			| 0x1163f | 0x116b6
			| 0x11c3f | 0x11d44
			| 0x11d45
	)
}

static SCRIPT_GREEK: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{Script=Greek}", "u"));

/// `\p{Script=Greek}`.
pub(super) fn is_greek(cp: u32) -> bool {
	has_property(&SCRIPT_GREEK, cp)
}

static SCRIPT_HEBREW: LazyLock<Regex> = LazyLock::new(|| pattern(r"\p{Script=Hebrew}", "u"));

/// `\p{Script=Hebrew}`.
pub(super) fn is_hebrew(cp: u32) -> bool {
	has_property(&SCRIPT_HEBREW, cp)
}

static SCRIPT_JAPANESE: LazyLock<Regex> =
	LazyLock::new(|| pattern(r"[\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Han}]", "u"));

/// Hiragana, Katakana or Han.
pub(super) fn is_japanese(cp: u32) -> bool {
	has_property(&SCRIPT_JAPANESE, cp)
}

static SCRIPT_ARABIC_LETTER: LazyLock<Regex> = LazyLock::new(|| {
	pattern(
		r"[\p{Script=Arabic}\p{Script=Syriac}\p{Script=Thaana}\p{Script=Mandaic}]",
		"u",
	)
});

/// The scripts whose letters have the Bidi class AL.
pub(super) fn is_arabic_letter(cp: u32) -> bool {
	has_property(&SCRIPT_ARABIC_LETTER, cp)
}
