//! The OpenAI and Anthropic SDK retry policy with an interruptible backoff (ports
//! `packages/ai/src/utils/provider-retry.ts`): APIs send each attempt with SDK retries off and wrap it here.

use crate::diagnostics::now_ms;
use http::HeaderMap;
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// The HTTP status and headers of a failed provider request, the fields `isProviderError` looks for.
#[derive(Debug, Clone, Copy)]
pub struct ProviderErrorMeta<'a> {
	/// The status; `None` for connection failures, which are retried.
	pub status: Option<u16>,
	/// The response headers, when there was a response.
	pub headers: Option<&'a HeaderMap>,
}

/// An error a provider request fails with; only errors with provider metadata are retried.
pub trait ProviderRequestError: fmt::Display {
	/// The status and headers, or `None` for an error that is not a provider HTTP error.
	fn provider_meta(&self) -> Option<ProviderErrorMeta<'_>>;
}

/// Options of [`retry_provider_request`] (`ProviderRetryOptions`).
#[derive(Debug, Clone, Default)]
pub struct ProviderRetryOptions {
	/// Retries after the first attempt; 0 when absent.
	pub max_retries: Option<u32>,
	/// Most server-requested delay; 60 seconds when absent, no limit at 0.
	pub max_retry_delay_ms: Option<u64>,
	/// HTTP statuses that fail at once although the policy would retry them.
	pub no_retry_statuses: Vec<u16>,
}

/// Why [`retry_provider_request`] failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRetryError<E> {
	/// The last attempt's error.
	#[error("{0}")]
	Request(E),
	/// The server asked for a longer delay than allowed; the message is TS's.
	#[error("{0}")]
	RetryDelayTooLong(String),
	/// The cancellation token fired (TS: `AbortError: Request aborted`).
	#[error("Request aborted")]
	Aborted,
}

const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

/// Runs `request`, retrying retryable provider errors with SDK backoff and `Retry-After` handling
/// (`retryProviderRequest`).
pub async fn retry_provider_request<T, E, F, Fut>(
	mut request: F,
	options: &ProviderRetryOptions,
	cancel: &CancellationToken,
) -> Result<T, ProviderRetryError<E>>
where
	E: ProviderRequestError,
	F: FnMut() -> Fut,
	Fut: Future<Output = Result<T, E>>,
{
	let max_retries = options.max_retries.unwrap_or(0);
	let mut remaining = max_retries;
	loop {
		let error = match request().await {
			Ok(value) => return Ok(value),
			Err(error) => error,
		};
		if cancel.is_cancelled() {
			return Err(ProviderRetryError::Aborted);
		}
		let Some(meta) = error.provider_meta() else {
			return Err(ProviderRetryError::Request(error));
		};
		let blocked = meta
			.status
			.is_some_and(|status| options.no_retry_statuses.contains(&status));
		if remaining == 0 || !is_retryable(meta) || blocked {
			return Err(ProviderRetryError::Request(error));
		}
		let retry_index = max_retries - remaining;
		remaining -= 1;
		let delay = retry_delay_ms(meta, &error.to_string(), retry_index, options.max_retry_delay_ms)?;
		let slept = tokio::select! {
			biased;
			() = cancel.cancelled() => false,
			() = tokio::time::sleep(delay_duration(delay)) => true,
		};
		if !slept {
			return Err(ProviderRetryError::Aborted);
		}
	}
}

/// The longest sleep: a server-requested delay above it (`maxRetryDelayMs: 0`) waits this long instead.
const MAX_SLEEP: Duration = Duration::from_secs(u32::MAX as u64);

/// A finite millisecond delay as a sleep: negative waits nothing, huge values wait [`MAX_SLEEP`]; wire input never
/// panics here.
fn delay_duration(delay_ms: f64) -> Duration {
	Duration::try_from_secs_f64(delay_ms.max(0.0) / 1000.0).map_or(MAX_SLEEP, |delay| delay.min(MAX_SLEEP))
}

fn header<'a>(meta: ProviderErrorMeta<'a>, name: &str) -> Option<&'a str> {
	meta.headers?.get(name)?.to_str().ok()
}

/// Mirrors the pinned OpenAI/Anthropic SDK retry policy; review when either SDK is upgraded.
fn is_retryable(meta: ProviderErrorMeta<'_>) -> bool {
	match header(meta, "x-should-retry") {
		Some("true") => return true,
		Some("false") => return false,
		_ => {}
	}
	meta.status
		.is_none_or(|status| status == 408 || status == 409 || status == 429 || status >= 500)
}

fn retry_delay_ms<E>(
	meta: ProviderErrorMeta<'_>,
	message: &str,
	retry_index: u32,
	max_retry_delay_ms: Option<u64>,
) -> Result<f64, ProviderRetryError<E>> {
	if let Some(value) = header(meta, "retry-after-ms").filter(|value| !value.is_empty())
		&& let Some(delay) = parse_float(value).filter(|delay| delay.is_finite())
	{
		return validate_server_delay(delay, max_retry_delay_ms, message);
	}
	if let Some(value) = header(meta, "retry-after").filter(|value| !value.is_empty()) {
		let delay = match parse_float(value) {
			Some(seconds) => Some(seconds * 1000.0),
			#[allow(
				clippy::cast_precision_loss,
				reason = "epoch milliseconds are below 2^53, as JS numbers"
			)]
			None => parse_http_date_ms(value).map(|at| at as f64 - now_ms() as f64),
		};
		if let Some(delay) = delay.filter(|delay| delay.is_finite()) {
			return validate_server_delay(delay, max_retry_delay_ms, message);
		}
	}
	let exponential = (0.5 * 2f64.powi(i32::try_from(retry_index).unwrap_or(i32::MAX))).min(8.0) * 1000.0;
	Ok(exponential * (1.0 - jitter() * 0.25))
}

fn validate_server_delay<E>(
	delay: f64,
	max_retry_delay_ms: Option<u64>,
	message: &str,
) -> Result<f64, ProviderRetryError<E>> {
	#[allow(clippy::cast_precision_loss, reason = "delays are far below 2^53")]
	let max = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS) as f64;
	if max > 0.0 && delay > max {
		return Err(ProviderRetryError::RetryDelayTooLong(format!(
			"Server requested {}s retry delay (max: {}s). {message}",
			(delay / 1000.0).ceil(),
			(max / 1000.0).ceil()
		)));
	}
	Ok(delay)
}

/// `Number.parseFloat`: the longest numeric prefix after leading whitespace; `None` for NaN.
fn parse_float(text: &str) -> Option<f64> {
	let text = text.trim_start();
	let bytes = text.as_bytes();
	let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
	let digits_start = end;
	while end < bytes.len() && bytes[end].is_ascii_digit() {
		end += 1;
	}
	if end < bytes.len() && bytes[end] == b'.' {
		end += 1;
		while end < bytes.len() && bytes[end].is_ascii_digit() {
			end += 1;
		}
	}
	if end == digits_start || &text[digits_start..end] == "." {
		return text[digits_start..].starts_with("Infinity").then(|| {
			if bytes.first() == Some(&b'-') {
				f64::NEG_INFINITY
			} else {
				f64::INFINITY
			}
		});
	}
	if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
		let mut exponent = end + 1;
		if exponent < bytes.len() && matches!(bytes[exponent], b'+' | b'-') {
			exponent += 1;
		}
		let exponent_digits = exponent;
		while exponent < bytes.len() && bytes[exponent].is_ascii_digit() {
			exponent += 1;
		}
		if exponent > exponent_digits {
			end = exponent;
		}
	}
	text[..end].parse().ok()
}

/// Epoch milliseconds of an IMF-fixdate (`Tue, 05 Nov 2024 12:00:00 GMT`), the `Retry-After` date form.
fn parse_http_date_ms(text: &str) -> Option<u64> {
	if let Some(time) = pi_json::parse_iso_ms(text) {
		return epoch_ms(time);
	}
	let parts: Vec<&str> = text.split_whitespace().collect();
	let [_, day, month, year, time, "GMT"] = parts.as_slice() else {
		return None;
	};
	let month = [
		"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
	]
	.iter()
	.position(|name| name == month)?;
	let iso = format!("{year}-{:02}-{:02}T{time}Z", month + 1, day.parse::<u32>().ok()?);
	pi_json::parse_iso_ms(&iso).and_then(epoch_ms)
}

fn epoch_ms(time: std::time::SystemTime) -> Option<u64> {
	let elapsed = time.duration_since(std::time::UNIX_EPOCH).ok()?;
	u64::try_from(elapsed.as_millis()).ok()
}

/// `Math.random()`.
fn jitter() -> f64 {
	let mut bytes = [0u8; 8];
	if getrandom::fill(&mut bytes).is_err() {
		return 0.5;
	}
	#[allow(clippy::cast_precision_loss, reason = "53 random bits and 2^53 fit an f64 exactly")]
	let unit = (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64;
	unit
}
