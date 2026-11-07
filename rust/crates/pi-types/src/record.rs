//! `Record<string, V>`: a JSON object with typed values whose keys keep their insertion order, as a JS object
//! literal keeps them (index-like keys are reordered by `pi_json::to_js_string`, as `JSON.stringify` does).

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;

/// String keys to values of one type, in insertion order; a JSON object on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct Record<V> {
	entries: Vec<(String, V)>,
}

impl<V> Record<V> {
	/// An empty record.
	pub fn new() -> Self {
		Self { entries: Vec::new() }
	}

	/// Sets `key`: an existing key keeps its position, as assigning a property of a JS object does.
	pub fn insert(&mut self, key: impl Into<String>, value: V) {
		let key = key.into();
		match self.entries.iter_mut().find(|(existing, _)| *existing == key) {
			Some((_, slot)) => *slot = value,
			None => self.entries.push((key, value)),
		}
	}

	/// The value of `key`, if set.
	pub fn get(&self, key: &str) -> Option<&V> {
		self.entries
			.iter()
			.find_map(|(existing, value)| (existing == key).then_some(value))
	}

	/// The entries in insertion order.
	pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
		self.entries.iter().map(|(key, value)| (key.as_str(), value))
	}

	/// The number of entries.
	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// Whether the record has no entries.
	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

impl<V> Default for Record<V> {
	fn default() -> Self {
		Self::new()
	}
}

impl<K: Into<String>, V> FromIterator<(K, V)> for Record<V> {
	fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
		let mut record = Self::new();
		for (key, value) in iter {
			record.insert(key, value);
		}
		record
	}
}

impl<V: Serialize> Serialize for Record<V> {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let mut map = serializer.serialize_map(Some(self.entries.len()))?;
		for (key, value) in &self.entries {
			map.serialize_entry(key, value)?;
		}
		map.end()
	}
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Record<V> {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_map(RecordVisitor(PhantomData))
	}
}

struct RecordVisitor<V>(PhantomData<V>);

impl<'de, V: Deserialize<'de>> Visitor<'de> for RecordVisitor<V> {
	type Value = Record<V>;

	fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
		formatter.write_str("a JSON object")
	}

	fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
		// TS parity: `JSON.parse` keeps a repeated key at its first position with its last value; `insert` does too.
		let mut record = Record::new();
		while let Some((key, value)) = access.next_entry::<String, V>()? {
			record.insert(key, value);
		}
		Ok(record)
	}
}

#[cfg(test)]
mod tests {
	use super::Record;

	#[test]
	fn keeps_insertion_order_and_the_first_position_of_a_repeated_key() {
		let record: Record<u64> = serde_json::from_value(pi_json::parse(r#"{"b":1,"a":2,"b":3}"#).unwrap()).unwrap();
		assert_eq!(record.iter().collect::<Vec<_>>(), [("b", &3), ("a", &2)]);
		assert_eq!(
			pi_json::to_js_string(&serde_json::to_value(&record).unwrap()),
			r#"{"b":3,"a":2}"#
		);
		assert_eq!(record.get("a"), Some(&2));
		assert_eq!(record.len(), 2);
	}
}
