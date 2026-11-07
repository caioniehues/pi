//! Provider authentication (ports `packages/ai/src/auth`): credential types, the auth traits providers implement,
//! resolution with locked OAuth refresh, env-var api-key auth, the default auth context and an in-memory store.

pub mod context;
pub mod credential_store;
pub mod helpers;
pub mod resolve;
pub mod types;
