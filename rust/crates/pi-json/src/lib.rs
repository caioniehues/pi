//! pi-json: the one place that turns values into the bytes the TS implementation would have written (JSON text,
//! numbers, ISO timestamps, UUIDs).

mod number;
mod stringify;
mod time;
mod uuid;

pub use number::js_number_to_string;
pub use stringify::{to_js_string, to_js_string_pretty};
pub use time::{iso_ms, parse_iso_ms};
pub use uuid::{UuidError, uuid_v4, uuid_v7};

/// A JSON value with object keys in insertion order (`serde_json`'s `preserve_order`).
pub type JsonValue = serde_json::Value;

/// Lowercase hex digits, for `\u00XX` escapes and UUIDs.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
