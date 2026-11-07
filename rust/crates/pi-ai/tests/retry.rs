//! Retry classification and the retry loop (`retry.rs`): ports `retry.test.ts`.

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
#![cfg(feature = "faux")]

use async_trait::async_trait;
use pi_ai::providers::faux::{FauxMessageOptions, faux_assistant_message, faux_text_message};
use pi_ai::retry::{RetryCallbacks, RetryPolicy, is_retryable_assistant_error, retry_assistant_call, retry_delay_ms};
use pi_types::{AssistantMessage, StopReason};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn error(message: &str) -> AssistantMessage {
	faux_assistant_message(
		Vec::new(),
		FauxMessageOptions {
			stop_reason: Some(StopReason::Error),
			error_message: Some(message.into()),
			..FauxMessageOptions::default()
		},
	)
}

fn retryable(message: &str) -> bool {
	is_retryable_assistant_error(&error(message))
}

// TS: retry.test.ts
#[test]
fn matches_explicit_provider_retry_guidance() {
	assert!(retryable(
		"An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message."
	));
	assert!(retryable(
		r#"{"message":"The system encountered an unexpected error during processing. Try your request again."}"#
	));
	assert!(retryable(
		"ResourceExhausted: Worker local total request limit reached (288/48)"
	));
}

// TS: retry.test.ts
#[test]
fn matches_bun_fetch_socket_drop_wording() {
	assert!(retryable(
		"The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()"
	));
}

// TS: retry.test.ts
#[test]
fn matches_upstream_request_buffer_exhaustion_wording() {
	assert!(retryable(
		"Error: exceeded request buffer limit while retrying upstream"
	));
}

// TS: retry.test.ts
#[test]
fn matches_dns_transport_failure_wording() {
	for message in [
		"The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)",
		"connect ENOTFOUND api.example.com",
		"EAI_AGAIN api.example.com",
		"getaddrinfo failed for api.example.com",
	] {
		assert!(retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn matches_http2_pending_stream_cancellation() {
	// Regression for #10379.
	for message in [
		"The pending stream has been canceled",
		"The pending stream has been canceled (caused by: socket closed)",
	] {
		assert!(retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn matches_service_unavailable_wording() {
	// Regression for #10965.
	for message in [
		"The service is temporarily unavailable. Please retry later.",
		"Service unavailable: The service is temporarily unavailable.",
	] {
		assert!(retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn matches_node_fetch_request_failure_wording() {
	// Regression for #10996.
	for message in [
		"request to https://oauth2.googleapis.com/token failed, reason:",
		"request to https://oauth2.googleapis.com/token failed, reason: connect ECONNREFUSED 127.0.0.1:9",
	] {
		assert!(retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn matches_gateway_upstream_failure_wording() {
	// Regression for #10920.
	for message in [
		"unknown: Unable to forward upstream stream",
		"upstream_error: Upstream request failed",
		"upstream_stream_read_error: Upstream response stream was interrupted",
		"Upstream HTTP/2 stream failed",
	] {
		assert!(retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn matches_gateway_wording_for_a_stream_cut_short_mid_reply() {
	// Regression for #11879.
	assert!(retryable(
		"Claude Code's stream from Anthropic was cut short after the reply had begun"
	));
}

// TS: retry.test.ts
#[test]
fn matches_openai_responses_streams_that_end_before_terminal_events() {
	assert!(retryable(
		"OpenAI Responses stream ended before a terminal response event"
	));
}

// TS: retry.test.ts
#[test]
fn matches_azure_peak_load_capacity_errors() {
	// Regression for #9669.
	assert!(retryable(
		"The system is currently experiencing high demand and cannot process your request. Your request exceeds the maximum usage size allowed during peak load. For improved capacity reliability, consider switching to Provisioned Throughput."
	));
}

// TS: retry.test.ts
#[test]
fn keeps_provider_limit_errors_non_retryable() {
	assert!(!retryable("429 quota exceeded"));
}

// TS: retry.test.ts
#[test]
fn keeps_the_chatgpt_subscription_usage_limit_non_retryable() {
	assert!(!retryable(
		r#"OpenAI API error (429): {"code":"subscription_sharing_usage_limit_exceeded","message":"Usage limit reached."}"#
	));
}

// TS: retry.test.ts
#[test]
fn keeps_the_chatgpt_business_workspace_usage_limit_non_retryable() {
	assert!(!retryable(
		r#"OpenAI API error (429): {"code":"subscription_sharing_workplace_individual_usage_limit_exceeded","message":"Usage limit reached."}"#
	));
}

// TS: retry.test.ts
#[test]
fn retries_temporary_chatgpt_subscription_errors() {
	for message in [
		"subscription_sharing_usage_unavailable: Usage cannot be checked.",
		"subscription_sharing_user_unavailable: User cannot be loaded.",
	] {
		assert!(retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn classifies_assistant_error_messages() {
	assert!(retryable("overloaded_error"));
	// Regression for #9627.
	assert!(retryable("520 status code (no body)"));
	assert!(retryable("524 status code (no body)"));
	assert!(!is_retryable_assistant_error(&faux_text_message("not an error")));
}

// TS: retry.test.ts
#[test]
fn does_not_match_status_codes_inside_ids_or_longer_numbers() {
	// Regression for #11057.
	for message in [
		r#"401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_011CU503xyz"}"#,
		"400 image exceeds 5 MB maximum: 5316784 bytes > 5242880 bytes",
	] {
		assert!(!retryable(message), "{message}");
	}
}

// TS: retry.test.ts
#[test]
fn caps_agent_retry_delay() {
	// Regression for #8826.
	assert_eq!(retry_delay_ms(2000, None, 6), 60_000);
	assert_eq!(retry_delay_ms(2000, Some(5000), 5), 5000);
	assert_eq!(retry_delay_ms(2000, Some(0), 5), 0);
}

const DISABLED: RetryPolicy = RetryPolicy {
	enabled: false,
	max_retries: 3,
	base_delay_ms: 0,
	max_agent_delay_ms: None,
};
const ENABLED: RetryPolicy = RetryPolicy {
	enabled: true,
	max_retries: 3,
	base_delay_ms: 0,
	max_agent_delay_ms: None,
};

/// Records every callback, as the `vi.fn()` spies do.
#[derive(Default)]
struct Spy {
	events: Mutex<Vec<String>>,
}

impl Spy {
	fn events(&self) -> Vec<String> {
		self.events.lock().unwrap().clone()
	}

	fn count(&self, prefix: &str) -> usize {
		self.events().iter().filter(|event| event.starts_with(prefix)).count()
	}
}

#[async_trait]
impl RetryCallbacks for Spy {
	async fn on_retry_scheduled(&self, attempt: u32, _max: u32, delay_ms: u64, _error: &str) {
		self.events.lock().unwrap().push(format!("retry:{attempt}:{delay_ms}"));
	}

	async fn on_retry_attempt_start(&self) {
		self.events.lock().unwrap().push("attempt-start".into());
	}

	async fn on_retry_finished(&self, success: bool, attempt: u32, final_error: Option<&str>) {
		self.events
			.lock()
			.unwrap()
			.push(format!("finished:{success}:{attempt}:{}", final_error.unwrap_or("-")));
	}
}

async fn run(responses: Vec<AssistantMessage>, policy: RetryPolicy, spy: &Spy) -> (AssistantMessage, u32) {
	let calls = AtomicU32::new(0);
	let responses = Arc::new(responses);
	let result = retry_assistant_call(
		|| {
			let index = calls.fetch_add(1, Ordering::SeqCst) as usize;
			let response = responses[index.min(responses.len() - 1)].clone();
			async move { response }
		},
		Some(policy),
		&CancellationToken::new(),
		spy,
	)
	.await;
	(result, calls.load(Ordering::SeqCst))
}

// TS: retry.test.ts
#[tokio::test]
async fn returns_a_successful_response_immediately_without_retrying() {
	let spy = Spy::default();
	let (result, calls) = run(vec![faux_text_message("ok")], ENABLED, &spy).await;
	assert_eq!((result.stop_reason, calls), (StopReason::Stop, 1));
}

// TS: retry.test.ts
#[tokio::test]
async fn does_not_retry_an_aborted_message() {
	let spy = Spy::default();
	let aborted = faux_assistant_message(
		Vec::new(),
		FauxMessageOptions {
			stop_reason: Some(StopReason::Aborted),
			..FauxMessageOptions::default()
		},
	);
	let (result, calls) = run(vec![aborted], ENABLED, &spy).await;
	assert_eq!(
		(result.stop_reason, calls, spy.count("retry")),
		(StopReason::Aborted, 1, 0)
	);
}

// TS: retry.test.ts
#[tokio::test]
async fn does_not_retry_a_non_retryable_error_quota_billing() {
	let spy = Spy::default();
	let (result, calls) = run(vec![error("insufficient_quota")], ENABLED, &spy).await;
	assert_eq!((result.stop_reason, calls), (StopReason::Error, 1));
	assert!(spy.events().is_empty());
}

// TS: retry.test.ts
#[tokio::test]
async fn retries_a_transient_error_up_to_max_retries_then_returns_the_final_error() {
	let spy = Spy::default();
	let (result, calls) = run(vec![error("terminated")], ENABLED, &spy).await;
	assert_eq!(
		(result.stop_reason, calls, spy.count("retry")),
		(StopReason::Error, 4, 3)
	);
	assert_eq!(spy.events().last().unwrap(), "finished:false:3:terminated");
}

// TS: retry.test.ts
#[tokio::test]
async fn reports_capped_retry_delays() {
	// Regression for #8826.
	let spy = Spy::default();
	let policy = RetryPolicy {
		enabled: true,
		max_retries: 4,
		base_delay_ms: 10,
		max_agent_delay_ms: Some(15),
	};
	run(
		vec![
			error("terminated"),
			error("terminated"),
			error("terminated"),
			error("terminated"),
			faux_text_message("recovered"),
		],
		policy,
		&spy,
	)
	.await;
	let delays: Vec<String> = spy
		.events()
		.into_iter()
		.filter(|event| event.starts_with("retry"))
		.collect();
	assert_eq!(delays, ["retry:1:10", "retry:2:15", "retry:3:15", "retry:4:15"]);
}

// TS: retry.test.ts
#[tokio::test]
async fn stops_retrying_once_a_call_succeeds() {
	let spy = Spy::default();
	let (result, calls) = run(
		vec![error("terminated"), error("terminated"), faux_text_message("recovered")],
		ENABLED,
		&spy,
	)
	.await;
	assert_eq!((result.stop_reason, calls), (StopReason::Stop, 3));
	assert_eq!(spy.events().last().unwrap(), "finished:true:2:-");
}

// TS: retry.test.ts
#[tokio::test]
async fn reports_an_aborted_retried_call_as_unsuccessful() {
	let spy = Spy::default();
	let aborted = faux_assistant_message(
		Vec::new(),
		FauxMessageOptions {
			stop_reason: Some(StopReason::Aborted),
			..FauxMessageOptions::default()
		},
	);
	let (result, calls) = run(vec![error("terminated"), aborted], ENABLED, &spy).await;
	assert_eq!((result.stop_reason, calls), (StopReason::Aborted, 2));
	assert_eq!(spy.events().last().unwrap(), "finished:false:1:-");
}

// TS: retry.test.ts
#[tokio::test]
async fn does_not_retry_when_policy_is_disabled() {
	let spy = Spy::default();
	let (result, calls) = run(vec![error("terminated")], DISABLED, &spy).await;
	assert_eq!((result.stop_reason, calls), (StopReason::Error, 1));
	assert!(spy.events().is_empty());
}

// TS: retry.test.ts
#[tokio::test]
async fn emits_on_retry_attempt_start_after_backoff_before_each_retried_call() {
	let spy = Spy::default();
	let (result, _) = run(
		vec![error("terminated"), error("terminated"), faux_text_message("recovered")],
		ENABLED,
		&spy,
	)
	.await;
	assert_eq!(result.stop_reason, StopReason::Stop);
	assert_eq!(
		spy.events()[..4],
		["retry:1:0", "attempt-start", "retry:2:0", "attempt-start"]
	);
}

// TS: retry.test.ts
#[tokio::test(start_paused = true)]
async fn aborts_backoff_sleep_via_signal_returns_an_aborted_message_and_emits_on_retry_finished_false() {
	let spy = Arc::new(Spy::default());
	let cancel = CancellationToken::new();
	let policy = RetryPolicy {
		enabled: true,
		max_retries: 5,
		base_delay_ms: 10_000,
		max_agent_delay_ms: None,
	};
	let calls = Arc::new(AtomicU32::new(0));
	let task = {
		let (spy, cancel, calls) = (Arc::clone(&spy), cancel.clone(), Arc::clone(&calls));
		tokio::spawn(async move {
			retry_assistant_call(
				|| {
					calls.fetch_add(1, Ordering::SeqCst);
					async { error("terminated") }
				},
				Some(policy),
				&cancel,
				spy.as_ref(),
			)
			.await
		})
	};
	while spy.count("retry") == 0 {
		tokio::task::yield_now().await;
	}
	cancel.cancel();
	let result = task.await.unwrap();
	assert_eq!(
		(result.stop_reason, result.error_message, calls.load(Ordering::SeqCst)),
		(StopReason::Aborted, None, 1)
	);
	assert_eq!(spy.events().last().unwrap(), "finished:false:1:terminated");
}
