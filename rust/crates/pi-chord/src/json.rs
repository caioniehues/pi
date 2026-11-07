//! Chord's JSON vocabulary (TS `json.ts`): strict JSON values with insertion-ordered objects.
//!
//! TS `copyJson` and `isJsonValue` guard against what a JS value can hold and JSON cannot (cycles, accessors, class
//! instances, non-finite numbers); a `JsonValue` is owned strict JSON by construction, so neither is ported.

/// A strict JSON value (from `pi-json`), objects in insertion order.
pub use pi_json::JsonValue;

/// The members of a JSON object, in insertion order.
pub type JsonObject = serde_json::Map<String, JsonValue>;
