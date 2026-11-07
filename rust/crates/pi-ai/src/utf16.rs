//! JS string lengths and slices: TS measures text in UTF-16 code units (`text.length`, `slice`), so every count that
//! reaches a token estimate, a truncation or a telemetry attribute is taken the same way here.

/// The length of `text` in UTF-16 code units (`text.length`).
pub fn utf16_len(text: &str) -> usize {
	text.chars().map(char::len_utf16).sum()
}

/// `text.slice(0, units)`, widened to the end of a character a slice would cut in half.
///
/// A JS slice can end between the two halves of a surrogate pair and keep a lone high surrogate; a Rust string
/// cannot hold one, so the cut character is kept whole (it counts two units, as in JS).
pub fn utf16_prefix(text: &str, units: usize) -> &str {
	let mut counted = 0;
	for (index, character) in text.char_indices() {
		if counted >= units {
			return &text[..index];
		}
		counted += character.len_utf16();
	}
	text
}

#[cfg(test)]
mod tests {
	use super::{utf16_len, utf16_prefix};

	#[test]
	fn counts_and_cuts_in_utf16_units() {
		assert_eq!(utf16_len("a😀é"), 4);
		assert_eq!(utf16_prefix("a😀é", 1), "a");
		assert_eq!(utf16_prefix("a😀é", 2), "a😀");
		assert_eq!(utf16_prefix("a😀é", 3), "a😀");
		assert_eq!(utf16_prefix("ab", 9), "ab");
	}
}
