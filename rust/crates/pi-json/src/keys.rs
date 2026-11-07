//! The order a JS object enumerates its keys in (`Object.keys`, `for...in`, `JSON.stringify`): array indices
//! ascending, then the other keys in insertion order.

use crate::JsonValue;
use serde_json::Map;

/// A JSON object's members in JS key order: canonical array indices (`"0"` to `"4294967294"`) ascending, then the
/// other keys in insertion order. Objects without index keys are walked in place, without allocating.
pub fn js_key_order(map: &Map<String, JsonValue>) -> impl Iterator<Item = (&String, &JsonValue)> {
	let indexed: Option<Vec<(u32, &String, &JsonValue)>> =
		map.keys().any(|key| array_index(key).is_some()).then(|| {
			let mut indexed: Vec<_> = map
				.iter()
				.filter_map(|(key, value)| array_index(key).map(|index| (index, key, value)))
				.collect();
			indexed.sort_unstable_by_key(|&(index, _, _)| index);
			indexed
		});
	let has_indexes = indexed.is_some();
	let named = map
		.iter()
		.filter(move |(key, _)| !has_indexes || array_index(key).is_none());
	indexed
		.into_iter()
		.flatten()
		.map(|(_, key, value)| (key, value))
		.chain(named)
}

/// The largest array index is 2^32 - 2 (ECMA-262 6.1.7); `"4294967295"` is an ordinary key.
const MAX_ARRAY_INDEX: u32 = u32::MAX - 1;

/// The array index a key names: canonical decimal (no sign, no leading zero) up to 2^32 - 2.
fn array_index(key: &str) -> Option<u32> {
	let canonical = key == "0" || (!key.starts_with('0') && !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()));
	if !canonical || key.len() > 10 {
		return None;
	}
	key.parse::<u32>().ok().filter(|&index| index <= MAX_ARRAY_INDEX)
}

#[cfg(test)]
mod tests {
	use super::js_key_order;
	use serde_json::json;

	#[test]
	fn index_keys_come_first_in_ascending_order() {
		let value = crate::parse(r#"{"b":1,"10":2,"a":3,"2":4,"4294967295":5,"01":6}"#).unwrap();
		let keys: Vec<&str> = js_key_order(value.as_object().unwrap())
			.map(|(k, _)| k.as_str())
			.collect();
		assert_eq!(keys, ["2", "10", "b", "a", "4294967295", "01"]);
		let plain = json!({"z": 1, "a": 2});
		let keys: Vec<&str> = js_key_order(plain.as_object().unwrap())
			.map(|(k, _)| k.as_str())
			.collect();
		assert_eq!(keys, ["z", "a"]);
	}
}
