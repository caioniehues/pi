//! SDK-style provider retries (`provider_retry.rs`): ports `provider-retry.test.ts`, with tokio's paused clock as
//! the fake timers.

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	clippy::unreadable_literal,
	clippy::needless_pass_by_value,
	clippy::unnecessary_literal_bound,
	clippy::assert_is_empty,
	clippy::cast_possible_truncation,
	clippy::cast_possible_wrap,
	clippy::cast_precision_loss,
	clippy::cast_sign_loss,
	clippy::format_collect,
	clippy::ignored_unit_patterns,
	clippy::items_after_statements,
	clippy::too_many_lines,
	clippy::type_complexity,
	reason = "test code: literals copied from the TS tests, helpers that fail the test on bad fixtures"
)]

use http::HeaderMap;
use pi_ai::provider_retry::{
	ProviderErrorMeta, ProviderRequestError, ProviderRetryError, ProviderRetryOptions, retry_provider_request,
};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
struct ProviderError {
	status: Option<u16>,
	headers: HeaderMap,
}

impl fmt::Display for ProviderError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Provider error: {:?}", self.status)
	}
}

impl ProviderRequestError for ProviderError {
	fn provider_meta(&self) -> Option<ProviderErrorMeta<'_>> {
		Some(ProviderErrorMeta {
			status: self.status,
			headers: Some(&self.headers),
		})
	}
}

fn provider_error(status: u16, headers: &[(&'static str, &str)]) -> ProviderError {
	let mut map = HeaderMap::new();
	for (name, value) in headers {
		map.insert(*name, value.parse().unwrap());
	}
	ProviderError {
		status: Some(status),
		headers: map,
	}
}

/// A request that fails `failures` times with `error`, then succeeds; counts calls.
fn request(
	error: ProviderError,
	failures: u32,
	calls: &Arc<AtomicU32>,
) -> impl FnMut() -> std::future::Ready<Result<&'static str, ProviderError>> {
	let calls = Arc::clone(calls);
	move || {
		let call = calls.fetch_add(1, Ordering::SeqCst);
		std::future::ready(if call < failures { Err(error.clone()) } else { Ok("ok") })
	}
}

fn options(max_retries: u32, max_retry_delay_ms: Option<u64>, no_retry_statuses: Vec<u16>) -> ProviderRetryOptions {
	ProviderRetryOptions {
		max_retries: Some(max_retries),
		max_retry_delay_ms,
		no_retry_statuses,
	}
}

// TS: provider-retry.test.ts
#[tokio::test(start_paused = true)]
async fn retries_retryable_provider_errors() {
	let calls = Arc::new(AtomicU32::new(0));
	let started = tokio::time::Instant::now();
	let result = retry_provider_request(
		request(provider_error(429, &[("retry-after-ms", "1000")]), 1, &calls),
		&options(1, None, vec![]),
		&CancellationToken::new(),
	)
	.await;
	assert_eq!(result, Ok("ok"));
	assert_eq!(calls.load(Ordering::SeqCst), 2);
	assert_eq!(started.elapsed(), Duration::from_millis(1000));
}

// TS: provider-retry.test.ts
#[tokio::test]
async fn does_not_retry_errors_the_provider_marks_as_non_retryable() {
	let calls = Arc::new(AtomicU32::new(0));
	let error = provider_error(429, &[("x-should-retry", "false")]);
	let result = retry_provider_request(
		request(error.clone(), 9, &calls),
		&options(2, None, vec![]),
		&CancellationToken::new(),
	)
	.await;
	assert_eq!(result, Err(ProviderRetryError::Request(error)));
	assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// TS: provider-retry.test.ts
#[tokio::test]
async fn does_not_retry_statuses_listed_in_no_retry_statuses() {
	let calls = Arc::new(AtomicU32::new(0));
	let error = provider_error(504, &[("retry-after-ms", "0")]);
	let result = retry_provider_request(
		request(error.clone(), 9, &calls),
		&options(2, None, vec![504]),
		&CancellationToken::new(),
	)
	.await;
	assert_eq!(result, Err(ProviderRetryError::Request(error)));
	assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// TS: provider-retry.test.ts
#[tokio::test]
async fn rejects_a_provider_requested_retry_delay_above_the_limit() {
	let calls = Arc::new(AtomicU32::new(0));
	let result = retry_provider_request(
		request(provider_error(429, &[("retry-after", "277403")]), 9, &calls),
		&options(1, Some(1000), vec![]),
		&CancellationToken::new(),
	)
	.await;
	let Err(ProviderRetryError::RetryDelayTooLong(message)) = result else {
		panic!("expected a delay error")
	};
	assert!(
		message.contains("Server requested 277403s retry delay (max: 1s)"),
		"{message}"
	);
	assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// TS: provider-retry.test.ts
#[tokio::test(start_paused = true)]
async fn allows_disabling_the_provider_requested_retry_delay_cap() {
	let calls = Arc::new(AtomicU32::new(0));
	let started = tokio::time::Instant::now();
	let result = retry_provider_request(
		request(provider_error(429, &[("retry-after", "2")]), 1, &calls),
		&options(1, Some(0), vec![]),
		&CancellationToken::new(),
	)
	.await;
	assert_eq!(result, Ok("ok"));
	assert_eq!(started.elapsed(), Duration::from_millis(2000));
	assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// TS: provider-retry.test.ts
#[tokio::test(start_paused = true)]
async fn aborts_a_provider_requested_retry_delay() {
	let calls = Arc::new(AtomicU32::new(0));
	let cancel = CancellationToken::new();
	let task = {
		let (calls, cancel) = (Arc::clone(&calls), cancel.clone());
		tokio::spawn(async move {
			retry_provider_request(
				request(provider_error(429, &[("retry-after", "277403")]), 9, &calls),
				&options(2, Some(0), vec![]),
				&cancel,
			)
			.await
		})
	};
	tokio::time::sleep(Duration::from_millis(1)).await;
	assert_eq!(calls.load(Ordering::SeqCst), 1);
	cancel.cancel();
	assert_eq!(task.await.unwrap(), Err(ProviderRetryError::Aborted));
	assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Waits after one failure with `headers`; how long the paused clock advanced, or the error.
async fn delay_after(
	headers: &[(&'static str, &str)],
	max_retry_delay_ms: Option<u64>,
) -> Result<Duration, ProviderRetryError<ProviderError>> {
	let calls = Arc::new(AtomicU32::new(0));
	let started = tokio::time::Instant::now();
	retry_provider_request(
		request(provider_error(429, headers), 1, &calls),
		&options(1, max_retry_delay_ms, vec![]),
		&CancellationToken::new(),
	)
	.await?;
	Ok(started.elapsed())
}

#[tokio::test(start_paused = true)]
async fn non_finite_and_out_of_range_server_delays_never_panic() {
	// Rust-only: wire input. Non-finite values fall through to the SDK's exponential delay (at most 500 ms here).
	for value in ["Infinity", "NaN"] {
		assert!(
			delay_after(&[("retry-after-ms", value)], None).await.unwrap() <= Duration::from_millis(500),
			"{value}"
		);
		assert!(
			delay_after(&[("retry-after", value)], None).await.unwrap() <= Duration::from_millis(500),
			"{value}"
		);
	}
	assert!(matches!(
		delay_after(&[("retry-after-ms", "1e300")], None).await,
		Err(ProviderRetryError::RetryDelayTooLong(_))
	));
	assert!(delay_after(&[("retry-after-ms", "1e300")], Some(0)).await.unwrap() > Duration::from_secs(1_000_000));
	assert_eq!(
		delay_after(&[("retry-after-ms", "-5")], None).await.unwrap(),
		Duration::ZERO
	);
	assert_eq!(
		delay_after(&[("retry-after", "-5")], None).await.unwrap(),
		Duration::ZERO
	);
}
