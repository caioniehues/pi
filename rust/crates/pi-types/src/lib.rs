//! pi-types: every data type that crosses a file or a wire, with serde representations that read and write what the
//! TypeScript implementation reads and writes (ports `packages/ai/src/types.ts`, `packages/agent/src/types.ts` and
//! the coding agent's message, session, event and RPC types).
//!
//! Representation rules, applied uniformly:
//!
//! - Tagged unions are enums tagged by `type` (events, entries, RPC records). Messages and content blocks also appear
//!   alone (`toolResults`, `toolCall`, `systemMessage`, `partial`), where TS still writes their `role` or `type`, so
//!   their structs carry the tag as a literal field (`literal.rs`) and their unions are untagged.
//! - `x?: T` is `Option<T>`, absent in and absent out; `x: T | null` is a required `Option<T>` written as `null`
//!   (`deserialize_with = "Option::deserialize"`: a missing key fails the struct, so the object is kept as `Other`
//!   rather than rewritten with `null`); `x: T | undefined` is omitted when absent. Each such field says which in its comment.
//! - Every struct that is persisted or sent carries `extra`, a flattened object holding the fields this port does not
//!   know, so they round-trip: their values are kept, but they are written after the known fields (`flatten`), so an
//!   unknown field written by TS between known ones moves to the end. Field order is the order the TS writer creates
//!   the object in, so the JSON this crate writes (through `pi_json::to_js_string`) has the TS key order.
//! - The `Other` pattern: [`SessionEntry`], [`AgentMessage`], [`AgentSessionEvent`] and [`AssistantMessageEvent`]
//!   are untagged wrappers of a
//!   `Known…` enum and `Other(JsonObject)`. An object whose tag this port does not know (an extension's message role,
//!   an entry type from a newer pi) deserializes as `Other` and serializes back unchanged instead of failing. Like the
//!   TS reader, which casts `JSON.parse` output without validating it, a known tag with a malformed body is kept as
//!   `Other` too.
//!
//! - JSON text is read through `pi_json::parse` (lenient lone surrogates), then `serde_json::from_value`.
//!
//! Values of these types are owned plain data, moved and cloned rarely, so variant sizes are left as they are rather
//! than boxed (`clippy::large_enum_variant`).

#![expect(
	clippy::large_enum_variant,
	reason = "plain wire data; boxing would cost an allocation per message; not measured, revisit if profiles show it"
)]

mod agent_event;
mod agent_message;
mod classifier;
mod content;
mod images;
mod json_event;
mod literal;
mod message;
mod model;
mod record;
mod rpc;
mod session_entry;
mod session_event;
mod stream_event;
mod tool;
mod usage;

pub use agent_event::*;
pub use agent_message::*;
pub use classifier::*;
pub use content::*;
pub use images::*;
pub use json_event::*;
pub use literal::*;
pub use message::*;
pub use model::*;
pub use record::*;
pub use rpc::*;
pub use session_entry::*;
pub use session_event::*;
pub use stream_event::*;
pub use tool::*;
pub use usage::*;

use serde::{Deserialize, Deserializer};

/// Deserializes a field that is present as `Some`, `null` included: with `#[serde(default)]`, absent is `None` and
/// `"details": null` is `Some(JsonValue::Null)` (or the type's own null), so both round-trip.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
	T::deserialize(deserializer).map(Some)
}
