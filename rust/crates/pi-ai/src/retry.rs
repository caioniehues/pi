//! Retry of failed assistant calls: the transient-error classifier and the policy-driven retry loop (ports
//! `packages/ai/src/utils/retry.ts`; the TS file cites the provider message behind each pattern).

use crate::js_regex::JsRegexSet;
use async_trait::async_trait;
use pi_types::{AssistantMessage, StopReason};
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Subscription, quota and billing limits: never retried, even when a retryable pattern also matches.
static NON_RETRYABLE_PROVIDER_LIMIT_ERROR: JsRegexSet = JsRegexSet::new(
	&[
		"GoUsageLimitError|FreeUsageLimitError|Monthly usage limit reached|available balance|insufficient_quota|out of budget|quota exceeded|billing|subscription_sharing_usage_limit_exceeded|subscription_sharing_workplace_individual_usage_limit_exceeded|\"code\"\\s*:\\s*\"?1308(?![0-9])",
	],
	"i",
);

/// Transient provider, gateway and transport failures.
static RETRYABLE_PROVIDER_ERROR: JsRegexSet = JsRegexSet::new(
	&[concat!(
		"overloaded|server_busy|servers are currently busy|currently experiencing high demand|model is at capacity|",
		"rate.?limit|too many requests|(?<![a-z0-9])(?:429|500|502|503|504|520|524)(?![a-z0-9])|",
		"service.?(?:is.?)?(?:temporarily.?)?unavailable|server.?error|internal.?error|",
		"provider.?returned.?(?:an.?)?error|exceeded request buffer limit while retrying upstream|",
		"unable to forward upstream|upstream.?request.?failed|upstream_stream_read_error|stream was interrupted|",
		"http/?2 stream failed|network.?error|connection.?error|connection.?refused|connection.?lost|",
		"other side closed|fetch failed|request to \\S+ failed, reason:|getaddrinfo|ENOTFOUND|EAI_AGAIN|",
		"upstream.?connect|reset before headers|socket hang up|socket connection was closed|timed? out|timeout|",
		"terminated|websocket.?closed|websocket.?error|ended without|stream ended before message_stop|",
		"stream ended before a terminal response event|http2 request did not get a response|",
		"stream disconnected before completion|cut short after the reply had begun|",
		"pending stream has been canceled|retry delay|you can retry your request|try your request again|",
		"please retry your request|ResourceExhausted|subscription_sharing_usage_unavailable|",
		"subscription_sharing_user_unavailable"
	)],
	"i",
);

/// Bounded attempts with exponential backoff (`RetryPolicy`, coding-agent's `settings.retry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
	/// Whether to retry at all.
	pub enabled: bool,
	/// Most retries; the initial call is not one.
	pub max_retries: u32,
	/// Base delay; attempt `n` waits `base_delay_ms * 2^(n-1)`.
	pub base_delay_ms: u64,
	/// Cap of each delay; 60 seconds when absent.
	pub max_agent_delay_ms: Option<u64>,
}

/// The default cap of an agent-level retry delay.
pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// `Number.MAX_SAFE_INTEGER`, where TS caps a delay that overflows.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// The delay before retry `attempt` (1-based), capped (`retryDelayMs`).
pub fn retry_delay_ms(base_delay_ms: u64, max_agent_delay_ms: Option<u64>, attempt: u32) -> u64 {
	let factor = 1u64.checked_shl(attempt.saturating_sub(1)).unwrap_or(u64::MAX);
	let delay = base_delay_ms.saturating_mul(factor).min(MAX_SAFE_INTEGER);
	delay.min(max_agent_delay_ms.unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS))
}

/// Callbacks around each retry of [`retry_assistant_call`] (`RetryCallbacks`); every method defaults to nothing.
#[async_trait]
pub trait RetryCallbacks: Send + Sync {
	/// Before the backoff sleep of retry `attempt` (1-based).
	async fn on_retry_scheduled(&self, _attempt: u32, _max_attempts: u32, _delay_ms: u64, _error_message: &str) {}

	/// After the backoff sleep, right before the retried call starts.
	async fn on_retry_attempt_start(&self) {}

	/// Once when the loop ends after at least one retry: `success` when a later call completed normally.
	async fn on_retry_finished(&self, _success: bool, _attempt: u32, _final_error: Option<&str>) {}
}

/// Callbacks that do nothing.
pub struct NoRetryCallbacks;

impl RetryCallbacks for NoRetryCallbacks {}

/// Runs one assistant call with bounded retry on transient errors (`retryAssistantCall`): success and aborts
/// return at once, non-retryable errors fail fast, an abort during backoff returns the error turned aborted.
pub async fn retry_assistant_call<F, Fut>(
	mut produce: F,
	policy: Option<RetryPolicy>,
	cancel: &CancellationToken,
	callbacks: &dyn RetryCallbacks,
) -> AssistantMessage
where
	F: FnMut() -> Fut,
	Fut: Future<Output = AssistantMessage>,
{
	let max_attempts = policy
		.filter(|policy| policy.enabled)
		.map_or(0, |policy| policy.max_retries);
	let mut attempt = 0;
	let mut last_retry: Option<u32> = None;
	loop {
		let response = produce().await;
		if response.stop_reason != StopReason::Error {
			if let Some(attempt) = last_retry {
				callbacks
					.on_retry_finished(response.stop_reason != StopReason::Aborted, attempt, None)
					.await;
			}
			return response;
		}
		if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
			if let Some(attempt) = last_retry {
				callbacks
					.on_retry_finished(false, attempt, response.error_message.as_deref())
					.await;
			}
			return response;
		}
		attempt += 1;
		last_retry = Some(attempt);
		let error_message = response
			.error_message
			.clone()
			.filter(|message| !message.is_empty())
			.unwrap_or_else(|| "Unknown error".into());
		let policy = policy.unwrap_or(RetryPolicy {
			enabled: false,
			max_retries: 0,
			base_delay_ms: 0,
			max_agent_delay_ms: None,
		});
		let delay_ms = retry_delay_ms(policy.base_delay_ms, policy.max_agent_delay_ms, attempt);
		callbacks
			.on_retry_scheduled(attempt, max_attempts, delay_ms, &error_message)
			.await;
		let slept = tokio::select! {
			biased;
			() = cancel.cancelled() => false,
			() = tokio::time::sleep(Duration::from_millis(delay_ms)) => true,
		};
		if !slept {
			callbacks.on_retry_finished(false, attempt, Some(&error_message)).await;
			// Aborts during backoff take the shape of a provider stream abort: the error message goes.
			let mut aborted = response;
			aborted.error_message = None;
			aborted.stop_reason = StopReason::Aborted;
			return aborted;
		}
		callbacks.on_retry_attempt_start().await;
	}
}

/// Whether a failed response looks like a transient provider or transport error (`isRetryableAssistantError`).
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
	let Some(error) = message.error_message.as_deref().filter(|error| !error.is_empty()) else {
		return false;
	};
	message.stop_reason == StopReason::Error
		&& !NON_RETRYABLE_PROVIDER_LIMIT_ERROR.test(error)
		&& RETRYABLE_PROVIDER_ERROR.test(error)
}
