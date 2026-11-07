//! A fast deterministic hash to shorten long strings (ports `packages/ai/src/utils/hash.ts`).

/// `shortHash`: two 32-bit multiplicative hashes over the UTF-16 code units, printed in base 36.
pub fn short_hash(text: &str) -> String {
	let mut h1: u32 = 0xdead_beef;
	let mut h2: u32 = 0x41c6_ce57;
	for unit in text.encode_utf16() {
		let unit = u32::from(unit);
		h1 = (h1 ^ unit).wrapping_mul(2_654_435_761);
		h2 = (h2 ^ unit).wrapping_mul(1_597_334_677);
	}
	h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507) ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
	h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507) ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
	format!("{}{}", base36(h2), base36(h1))
}

/// `Number.prototype.toString(36)` of an unsigned integer.
fn base36(mut value: u32) -> String {
	const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
	let mut digits = Vec::new();
	loop {
		digits.push(DIGITS[(value % 36) as usize]);
		value /= 36;
		if value == 0 {
			break;
		}
	}
	digits.reverse();
	// invariant: every digit is ASCII.
	digits.into_iter().map(char::from).collect()
}

#[cfg(test)]
mod tests {
	use super::short_hash;

	#[test]
	fn matches_the_ts_hash() {
		// Values printed by Node 26 for `shortHash` of the same inputs.
		assert_eq!(short_hash(""), "k4n83c7h0j2b");
		assert_eq!(short_hash("hello"), "1h6qa0qrowduu");
		assert_eq!(short_hash("😀 call_1|fc_2"), "1lsmea7zs97r");
	}
}
