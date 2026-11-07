//! JSON literals as types: `true` and `false` for TS fields typed with a boolean literal (`success: true`,
//! `constrainedSampling: false`), and one-value string enums for tag fields (`role: "assistant"`,
//! `type: "toolCall"`). A struct that carries its own tag writes it wherever it appears, alone or in a union, as the
//! TS object does; the unions of such structs are untagged and tell their variants apart by these literals.

use serde::de::{Error, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Declares a tag type: a one-variant enum that serializes as `$value` and deserializes only from it.
macro_rules! tag_type {
	($(#[$doc:meta])* $name:ident :: $variant:ident = $value:literal) => {
		$(#[$doc])*
		#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
		pub enum $name {
			#[doc = concat!("`\"", $value, "\"`.")]
			#[default]
			#[serde(rename = $value)]
			$variant,
		}
	};
}
pub(crate) use tag_type;

/// The literal `true`: serializes as `true`, deserializes only from `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct True;

/// The literal `false`: serializes as `false`, deserializes only from `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct False;

impl Serialize for True {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_bool(true)
	}
}

impl<'de> Deserialize<'de> for True {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		expect_bool(deserializer, true).map(|()| True)
	}
}

impl Serialize for False {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_bool(false)
	}
}

impl<'de> Deserialize<'de> for False {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		expect_bool(deserializer, false).map(|()| False)
	}
}

fn expect_bool<'de, D: Deserializer<'de>>(deserializer: D, expected: bool) -> Result<(), D::Error> {
	let found = bool::deserialize(deserializer)?;
	if found == expected {
		Ok(())
	} else {
		Err(D::Error::invalid_value(
			Unexpected::Bool(found),
			&if expected { "true" } else { "false" },
		))
	}
}

#[cfg(test)]
mod tests {
	use super::{False, True};

	tag_type!(
		/// A tag for the test.
		Example::Value = "example"
	);

	fn read<T: serde::de::DeserializeOwned>(text: &str) -> serde_json::Result<T> {
		serde_json::from_value(pi_json::parse(text)?)
	}

	fn write<T: serde::Serialize>(value: &T) -> String {
		pi_json::to_js_string(&serde_json::to_value(value).unwrap())
	}

	#[test]
	fn literals_accept_only_their_value() {
		assert_eq!(write(&True), "true");
		assert!(read::<True>("false").is_err());
		assert_eq!(read::<False>("false").unwrap(), False);
		assert!(read::<False>("true").is_err());
		assert_eq!(write(&Example::Value), r#""example""#);
		assert!(read::<Example>(r#""other""#).is_err());
	}
}
