//! Deterministic chunk boundaries: how a block's text is cut into deltas and how rendered events are cut into
//! network writes, so the TS and the Rust pi read the same bytes in the same pieces.

use crate::script::Chunking;
use bytes::Bytes;

/// The deltas of one block's text: the whole text, or pieces of `tokens` characters with chunking `Tokens`.
/// Empty text has no delta.
pub fn deltas(text: &str, chunking: Option<Chunking>) -> Vec<&str> {
	if text.is_empty() {
		return Vec::new();
	}
	let Some(Chunking::Tokens(size)) = chunking else {
		return vec![text];
	};
	let mut pieces = Vec::new();
	let mut start = 0;
	for (count, (index, _)) in text.char_indices().enumerate() {
		if count > 0 && count % size == 0 {
			pieces.push(&text[start..index]);
			start = index;
		}
	}
	pieces.push(&text[start..]);
	pieces
}

/// The writes of a rendered stream: one per event, or pieces of `bytes` bytes of the whole stream with chunking
/// `Bytes`.
pub fn writes(events: Vec<String>, chunking: Option<Chunking>) -> Vec<Bytes> {
	let Some(Chunking::Bytes(size)) = chunking else {
		return events.into_iter().map(Bytes::from).collect();
	};
	let stream = Bytes::from(events.concat());
	(0..stream.len())
		.step_by(size)
		.map(|start| stream.slice(start..(start + size).min(stream.len())))
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn deltas_without_chunking_is_the_whole_text() {
		assert_eq!(deltas("hello world", None), vec!["hello world"]);
		assert_eq!(deltas("hello", Some(Chunking::Bytes(2))), vec!["hello"]);
		assert_eq!(deltas("", None), Vec::<&str>::new());
	}

	#[test]
	fn deltas_cut_by_characters_not_bytes() {
		assert_eq!(deltas("abcdefg", Some(Chunking::Tokens(3))), vec!["abc", "def", "g"]);
		assert_eq!(deltas("aéb€c", Some(Chunking::Tokens(2))), vec!["aé", "b€", "c"]);
		assert_eq!(deltas("ab", Some(Chunking::Tokens(5))), vec!["ab"]);
	}

	#[test]
	fn writes_are_one_per_event_or_fixed_byte_pieces() {
		let events = vec!["ab".to_owned(), "cde".to_owned()];
		assert_eq!(
			writes(events.clone(), None),
			vec![Bytes::from("ab"), Bytes::from("cde")]
		);
		assert_eq!(writes(events.clone(), Some(Chunking::Tokens(1))).len(), 2);
		assert_eq!(
			writes(events, Some(Chunking::Bytes(2))),
			vec![Bytes::from("ab"), Bytes::from("cd"), Bytes::from("e")]
		);
	}

	#[test]
	fn byte_writes_cut_through_utf8_sequences() {
		let pieces = writes(vec!["é".to_owned()], Some(Chunking::Bytes(1)));
		assert_eq!(pieces, vec![Bytes::from_static(&[0xc3]), Bytes::from_static(&[0xa9])]);
	}
}
