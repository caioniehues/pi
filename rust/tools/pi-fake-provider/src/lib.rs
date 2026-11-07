//! Fake LLM provider server for the difference tests: serves a scenario script as the exact SSE byte stream of
//! the anthropic-messages and openai-completions APIs, with deterministic chunking, error injection, paused
//! streams and a record of every request. Has no dependency on `pi-ai`, so it cannot share a bug with it.

mod anthropic;
mod chunking;
mod openai_completions;
mod script;
mod server;

pub use anthropic::render_anthropic;
pub use chunking::{deltas, writes};
pub use openai_completions::render_openai_completions;
pub use script::{Block, Chunking, ErrorReply, Script, ScriptError, ScriptResponse, StopReason, Usage, load_script};
pub use server::{ServerError, ServerHandle, start, start_on};
