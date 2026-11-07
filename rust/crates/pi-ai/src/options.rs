//! Request options of every operation (ports `ProviderRequestOptions`, `StreamOptions`, `SimpleStreamOptions`,
//! `DeferredFetchOptions`, `ImagesOptions`, `ClassifierOptions` and `ModelsRequestTransforms` of
//! `packages/ai/src/types.ts` and `models.ts`).
//!
//! TS's per-API option types (`AnthropicOptions`, ...) extend `StreamOptions`; here an API's own options travel in
//! `StreamOptions::api_options` and the API downcasts them.

use crate::headers::ProviderHeaders;
use crate::http::Fetch;
use crate::identity::ClientIdentity;
use crate::model_operations::ModelRef;
use crate::provider_env::ProviderEnv;
use futures_util::future::BoxFuture;
use pi_json::{JsonObject, JsonValue};
use pi_telemetry::TelemetryContext;
use pi_types::{CacheRetention, Model, Record, ThinkingLevel};
use std::any::Any;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The status and headers of a provider response (`ProviderResponse`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderResponse {
	/// The status.
	pub status: u16,
	/// The headers, names lowercase.
	pub headers: Record<String>,
}

/// Inspects or replaces a request payload before it is sent; `None` keeps it (`onPayload`).
pub type OnPayload = Arc<dyn Fn(JsonValue, ModelRef<'_>) -> BoxFuture<'static, Option<JsonValue>> + Send + Sync>;

/// Called when a response arrives, before its body is read (`onResponse`).
pub type OnResponse = Arc<dyn Fn(ProviderResponse, ModelRef<'_>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Observes each parsed provider stream event before normalization (`onProviderStreamEvent`).
pub type OnProviderStreamEvent = Arc<dyn Fn(&JsonValue, &Model) -> BoxFuture<'static, ()> + Send + Sync>;

/// Transforms the assembled request headers before dispatch; `Models` only (`transformHeaders`).
pub type TransformHeaders = Arc<dyn Fn(ProviderHeaders) -> BoxFuture<'static, ProviderHeaders> + Send + Sync>;

/// Authentication, transport and lifecycle options shared by every provider request (`ProviderRequestOptions`).
#[derive(Clone, Default)]
pub struct ProviderRequestOptions {
	/// Cancels the request (`signal`).
	pub cancel: Option<CancellationToken>,
	/// The parent span of the request's telemetry.
	pub telemetry: Option<Arc<dyn TelemetryContext>>,
	/// The API key; wins over resolved auth.
	pub api_key: Option<String>,
	/// Sends the HTTP requests instead of the default client.
	pub fetch: Option<Arc<dyn Fetch>>,
	/// Provider-scoped environment values over the process environment.
	pub env: Option<ProviderEnv>,
	/// Inspects or replaces the payload.
	pub on_payload: Option<OnPayload>,
	/// Called on each HTTP response.
	pub on_response: Option<OnResponse>,
	/// Extra headers over the defaults; `None` values remove a default.
	pub headers: Option<ProviderHeaders>,
	/// How the request identifies the calling app.
	pub identity: Option<ClientIdentity>,
	/// HTTP request timeout in milliseconds.
	pub timeout_ms: Option<u64>,
	/// Client-side retries.
	pub max_retries: Option<u32>,
	/// Most server-requested retry delay; 0 disables the cap.
	pub max_retry_delay_ms: Option<u64>,
	/// Applied by `Models` to the assembled headers, then cleared before dispatch.
	pub transform_headers: Option<TransformHeaders>,
}

/// Preferred transport (`Transport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
	/// Server-sent events.
	Sse,
	/// A WebSocket.
	Websocket,
	/// A cached WebSocket.
	WebsocketCached,
	/// The provider's choice.
	Auto,
}

/// Options of a stream request (`StreamOptions`).
#[derive(Clone, Default)]
pub struct StreamOptions {
	/// The shared request options.
	pub request: ProviderRequestOptions,
	/// Observes raw provider stream events.
	pub on_provider_stream_event: Option<OnProviderStreamEvent>,
	/// Sampling temperature.
	pub temperature: Option<f64>,
	/// Parameters merged into the request body as-is, over `Model::sampling_params`.
	pub sampling_params: Option<JsonObject>,
	/// Output token limit.
	pub max_tokens: Option<u64>,
	/// Preferred transport.
	pub transport: Option<Transport>,
	/// Prompt cache retention (default short).
	pub cache_retention: Option<CacheRetention>,
	/// Session id for session-aware caching and routing.
	pub session_id: Option<String>,
	/// Do not use or record server-side continuation state.
	pub disable_continuation: Option<bool>,
	/// WebSocket connect timeout in milliseconds.
	pub websocket_connect_timeout_ms: Option<u64>,
	/// Metadata providers pick fields from.
	pub metadata: Option<JsonObject>,
	/// An API's own options (`AnthropicOptions`, ...), downcast by that API.
	pub api_options: Option<Arc<dyn Any + Send + Sync>>,
}

/// Provider-neutral tool selection (`ToolChoice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChoice {
	/// The model decides.
	Auto,
	/// No tools.
	None,
}

/// How long a deferred response may take (`deferred.window`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredWindow {
	/// 15 minutes.
	Minutes15,
	/// One hour.
	Hour1,
	/// One day.
	Hours24,
}

/// A request to continue asynchronously (`deferred: true | { window }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeferredRequest {
	/// The window, when chosen.
	pub window: Option<DeferredWindow>,
}

/// Token budgets per thinking level for token-based providers (`ThinkingBudgets`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ThinkingBudgets {
	/// `minimal`.
	pub minimal: Option<u64>,
	/// `low`.
	pub low: Option<u64>,
	/// `medium`.
	pub medium: Option<u64>,
	/// `high`.
	pub high: Option<u64>,
}

/// Options of the provider-neutral simple stream (`SimpleStreamOptions`).
#[derive(Clone, Default)]
pub struct SimpleStreamOptions {
	/// The stream options.
	pub stream: StreamOptions,
	/// Tool selection.
	pub tool_choice: Option<ToolChoice>,
	/// The thinking level.
	pub reasoning: Option<ThinkingLevel>,
	/// Ask for a deferred response.
	pub deferred: Option<DeferredRequest>,
	/// Custom thinking budgets.
	pub thinking_budgets: Option<ThinkingBudgets>,
}

/// Options of a deferred-response fetch (`DeferredFetchOptions`).
#[derive(Clone, Default)]
pub struct DeferredFetchOptions {
	/// The shared request options.
	pub request: ProviderRequestOptions,
	/// Longest provider long-poll in milliseconds; 0 checks once.
	pub wait: Option<u64>,
}

/// Options of image generation (`ImagesOptions`).
#[derive(Clone, Default)]
pub struct ImagesOptions {
	/// The shared request options.
	pub request: ProviderRequestOptions,
	/// Metadata providers pick fields from.
	pub metadata: Option<JsonObject>,
}

/// Options of a classification (`ClassifierOptions`).
#[derive(Clone, Default)]
pub struct ClassifierOptions {
	/// The shared request options.
	pub request: ProviderRequestOptions,
	/// Divides answer logits before normalization; positive.
	pub temperature: Option<f64>,
}

/// Options that carry the shared request options, so `Models` applies auth to every kind the same way.
pub trait RequestOptions: Clone + Send + Sync + 'static {
	/// The shared request options.
	fn request(&self) -> &ProviderRequestOptions;
	/// The shared request options, to change.
	fn request_mut(&mut self) -> &mut ProviderRequestOptions;
	/// The session id, for the session header of `opencode.ai`; only stream options have one.
	fn session_id(&self) -> Option<&str> {
		None
	}
}

impl RequestOptions for ProviderRequestOptions {
	fn request(&self) -> &ProviderRequestOptions {
		self
	}
	fn request_mut(&mut self) -> &mut ProviderRequestOptions {
		self
	}
}

impl RequestOptions for StreamOptions {
	fn request(&self) -> &ProviderRequestOptions {
		&self.request
	}
	fn request_mut(&mut self) -> &mut ProviderRequestOptions {
		&mut self.request
	}
	fn session_id(&self) -> Option<&str> {
		self.session_id.as_deref()
	}
}

impl RequestOptions for SimpleStreamOptions {
	fn request(&self) -> &ProviderRequestOptions {
		&self.stream.request
	}
	fn request_mut(&mut self) -> &mut ProviderRequestOptions {
		&mut self.stream.request
	}
	fn session_id(&self) -> Option<&str> {
		self.stream.session_id.as_deref()
	}
}

impl RequestOptions for DeferredFetchOptions {
	fn request(&self) -> &ProviderRequestOptions {
		&self.request
	}
	fn request_mut(&mut self) -> &mut ProviderRequestOptions {
		&mut self.request
	}
}

impl RequestOptions for ImagesOptions {
	fn request(&self) -> &ProviderRequestOptions {
		&self.request
	}
	fn request_mut(&mut self) -> &mut ProviderRequestOptions {
		&mut self.request
	}
}

impl RequestOptions for ClassifierOptions {
	fn request(&self) -> &ProviderRequestOptions {
		&self.request
	}
	fn request_mut(&mut self) -> &mut ProviderRequestOptions {
		&mut self.request
	}
}
