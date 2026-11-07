//! Usage (`usage.rs`): optional splits and the legacy form without `totalTokens`.

mod common;

use common::{USAGE, round_trip, write};
use pi_types::Usage;

#[test]
fn usage_round_trips_with_and_without_optional_fields() {
	round_trip::<Usage>(USAGE);
	let usage: Usage = round_trip(
		r#"{"input":1,"output":9,"cacheRead":0,"cacheWrite":4,"cacheWrite1h":4,"reasoning":3,"totalTokens":14,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0.5,"totalFromProvider":true}}"#,
	);
	assert_eq!(usage.cache_write_1h, Some(4));
	// TS parity: sessions from before `totalTokens` existed lack it.
	let legacy: Usage = round_trip(
		r#"{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}"#,
	);
	assert_eq!(legacy.total_tokens, None);
}

#[test]
fn zero_usage_writes_what_providers_start_with() {
	assert_eq!(
		write(&Usage::zero()),
		r#"{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}"#
	);
}
