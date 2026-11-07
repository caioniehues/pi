//! Time-ordered UUIDs: ports `uuid.test.ts` against `pi_json::uuid_v7`, which `packages/ai/src/utils/uuid.ts` ports.
//! The fake clock and fake random source of the TS test have no injection point here; ordering is checked on the
//! real clock and the tail randomness by uniqueness.

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

fn timestamp_of(uuid: &str) -> u64 {
	u64::from_str_radix(&uuid.replace('-', "")[..12], 16).unwrap()
}

fn is_v7(uuid: &str) -> bool {
	let bytes = uuid.as_bytes();
	uuid.len() == 36
		&& bytes[14] == b'7'
		&& matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
		&& uuid
			.chars()
			.all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

// TS: uuid.test.ts
#[test]
fn generates_ordered_uuidv7s_while_preserving_follower_timestamps() {
	let ordinary: Vec<String> = (0..4).map(|_| pi_json::uuid_v7(None).unwrap()).collect();
	let follower = 0x0123_4567_89ab - 1000;
	let followers = [
		pi_json::uuid_v7(Some(follower)).unwrap(),
		pi_json::uuid_v7(Some(follower)).unwrap(),
	];
	for id in ordinary.iter().chain(&followers) {
		assert!(is_v7(id), "{id}");
	}
	let mut sorted = ordinary.clone();
	sorted.sort();
	assert_eq!(sorted, ordinary);
	assert_eq!(
		followers.iter().map(|id| timestamp_of(id)).collect::<Vec<_>>(),
		[follower, follower]
	);
	assert_ne!(followers[0], followers[1]);
}

// TS: uuid.test.ts
#[test]
fn uses_fresh_randomness_for_every_uuid_tail() {
	let tails: std::collections::HashSet<String> = (0..100)
		.map(|_| pi_json::uuid_v7(Some(0x0123_4567_89ab)).unwrap()[28..].to_owned())
		.collect();
	assert_eq!(tails.len(), 100);
}

// TS: uuid.test.ts
#[test]
fn accepts_timestamp_boundaries() {
	for timestamp in [0, (1u64 << 48) - 1] {
		assert_eq!(timestamp_of(&pi_json::uuid_v7(Some(timestamp)).unwrap()), timestamp);
	}
}

// TS: uuid.test.ts
#[test]
fn rejects_invalid_timestamps() {
	// -1, 1.5, NaN and Infinity are unrepresentable as a `u64`; 2^48 is the one that type admits.
	assert_eq!(
		pi_json::uuid_v7(Some(1 << 48)),
		Err(pi_json::UuidError::TimestampOutOfRange)
	);
}
