//! pi-ai: the unified LLM API (ports `packages/ai` without its wire protocols): the model catalog and the `Models`
//! registry, provider auth, the assistant event stream, streaming tool-argument JSON, transcript replay, retry and
//! overflow classification, request plumbing (identity, headers, HTTP client, SSE), and the faux provider (feature
//! `faux`).

pub mod auth;
pub mod cancel;
pub mod catalog;
pub mod constrained_sampling;
pub mod diagnostics;
pub mod env_api_keys;
pub mod error;
pub mod error_body;
pub mod estimate;
pub mod event_stream;
pub mod hash;
pub mod headers;
pub mod http;
pub mod identity;
mod js_regex;
pub mod json_parse;
pub mod lazy_stream;
pub mod model_data;
pub mod model_operations;
pub mod models;
pub mod models_store;
pub mod openrouter_cost;
pub mod options;
pub mod overflow;
mod partial_json;
pub mod provider;
pub mod provider_env;
pub mod provider_retry;
pub mod providers;
pub mod retry;
pub mod sanitize_unicode;
pub mod session_resources;
pub mod simple_options;
pub mod sse;
pub mod streaming_json;
pub mod telemetry;
pub mod text;
pub mod transcript;
pub mod transform_messages;
pub mod utf16;
pub mod validation;
