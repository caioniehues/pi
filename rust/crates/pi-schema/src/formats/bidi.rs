//! The Bidi rule of RFC 5893 as TypeBox 1.3.27's `idna/format/bidi.ts` (MIT) applies it: a simplified Bidi class per
//! code point, whether a label is right-to-left, and the parts of the rule the official tests cover.

use super::punycode;
use super::unicode::{is_arabic_letter, is_hebrew, is_letter, is_mark_nonspacing};

/// The Bidi classes TypeBox tells apart; every other code point is `On`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BidiClass {
	L,
	R,
	Al,
	En,
	An,
	Nsm,
	On,
}

fn bidi_class(cp: u32) -> BidiClass {
	if (0x30..=0x39).contains(&cp) || (0x6f0..=0x6f9).contains(&cp) {
		BidiClass::En
	} else if (0x660..=0x669).contains(&cp) {
		BidiClass::An
	} else if is_mark_nonspacing(cp) {
		BidiClass::Nsm
	} else if is_hebrew(cp) {
		BidiClass::R
	} else if is_arabic_letter(cp) {
		BidiClass::Al
	} else if is_letter(cp) {
		BidiClass::L
	} else {
		BidiClass::On
	}
}

/// Whether a label, decoded from Punycode if it is an A-label, is right-to-left (RFC 5893 1.4); a label that does
/// not decode is not.
pub(super) fn has_bidi_chars(label: &str) -> bool {
	if punycode::is_ace_prefixed(label) {
		return punycode::decode(&label[4..].to_lowercase()).is_some_and(|points| has_rtl(points.into_iter()));
	}
	has_rtl(label.chars().map(u32::from))
}

/// RFC 5893 1.4: "An RTL label is a label that contains at least one character of type R, AL, or AN."
pub(super) fn has_rtl(mut points: impl Iterator<Item = u32>) -> bool {
	points.any(|cp| matches!(bidi_class(cp), BidiClass::R | BidiClass::Al | BidiClass::An))
}

/// The Bidi rule: the first character L, R or AL; only the classes its direction allows; no EN and AN together in
/// a right-to-left label.
pub(super) fn satisfies_bidi_rule(label: &str) -> bool {
	let mut rtl = false;
	let (mut european, mut arabic) = (false, false);
	for (i, cp) in label.chars().map(u32::from).enumerate() {
		let class = bidi_class(cp);
		if i == 0 {
			if !matches!(class, BidiClass::L | BidiClass::R | BidiClass::Al) {
				return false;
			}
			rtl = matches!(class, BidiClass::R | BidiClass::Al);
		}
		let allowed = match class {
			BidiClass::R | BidiClass::Al | BidiClass::An => rtl,
			BidiClass::L => !rtl,
			BidiClass::En | BidiClass::Nsm | BidiClass::On => true,
		};
		if !allowed {
			return false;
		}
		european |= class == BidiClass::En;
		arabic |= class == BidiClass::An;
	}
	!(rtl && european && arabic)
}
