//! The faux provider through a `Models` registry, deferred path included: ports the `fauxProvider` block of
//! `providers.test.ts` (its other blocks test the wire providers, C3b and later).

#![cfg(feature = "faux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use futures_util::FutureExt;
use pi_ai::models::Models;
use pi_ai::options::{
	DeferredFetchOptions, DeferredRequest, DeferredWindow, ProviderRequestOptions, SimpleStreamOptions,
};
use pi_ai::providers::faux::{
	FauxDeferredOptions, FauxProviderOptions, FauxResponseFactory, FauxResponseStep, faux_provider, faux_text_message,
};
use pi_ai::transcript::Context;
use pi_types::StopReason;
use serde_json::json;
use std::sync::Arc;

fn context() -> Context {
	Context {
		messages: vec![common::user("hi", 1)],
		..Context::default()
	}
}

fn deferred(window: Option<DeferredWindow>) -> SimpleStreamOptions {
	SimpleStreamOptions {
		deferred: Some(DeferredRequest { window }),
		..SimpleStreamOptions::default()
	}
}

// TS: providers.test.ts > fauxProvider > streams queued responses through a Models collection
#[tokio::test]
async fn streams_queued_responses_through_a_models_collection() {
	let faux = faux_provider(&FauxProviderOptions::default());
	let models = Models::default();
	models.set_provider(Arc::clone(&faux.provider));
	faux.set_responses(vec![faux_text_message("hello from faux").into()]);
	let model = models.get_models(Some(faux.provider.id()))[0].clone();
	let result = models
		.complete_simple(&model, context(), SimpleStreamOptions::default())
		.await;
	assert_eq!(result.stop_reason, StopReason::Stop);
	assert_eq!(
		common::to_json(&result.content),
		json!([{ "type": "text", "text": "hello from faux" }])
	);
	assert_eq!(faux.state().call_count, 1);
}

// TS: providers.test.ts > fauxProvider > submits, polls, and redeems deferred responses
#[tokio::test]
async fn submits_polls_and_redeems_deferred_responses() {
	let faux = faux_provider(&FauxProviderOptions {
		deferred: FauxDeferredOptions {
			pending_fetches: Some(1),
			poll_after_ms: Some(25),
		},
		..FauxProviderOptions::default()
	});
	let models = Models::default();
	models.set_provider(Arc::clone(&faux.provider));
	faux.set_responses(vec![faux_text_message("ready").into()]);
	let model = faux.model().clone();
	let submission = models.stream_simple(&model, context(), deferred(Some(DeferredWindow::Hour1)));
	let mut types = Vec::new();
	while let Some(event) = submission.next().await {
		types.push(common::to_json(&event)["type"].as_str().unwrap().to_owned());
	}
	assert_eq!(types, ["start", "done"]);
	let submitted = submission.result().await;
	assert!(submitted.stop_reason == StopReason::Deferred && submitted.content.is_empty());
	let handle = submitted.deferred.clone().unwrap();
	let handle_json = common::to_json(&handle);
	assert_eq!(
		(
			handle_json["provider"].clone(),
			handle_json["modelId"].clone(),
			handle_json["api"].clone(),
			handle_json["pollAfterMs"].clone()
		),
		(json!(model.provider), json!(model.id), json!(model.api), json!(25))
	);
	assert!(handle_json["id"].is_string());
	assert_eq!(handle_json.as_object().unwrap().len(), 5);
	let pending = models
		.fetch_deferred(&model, &handle, DeferredFetchOptions::default())
		.await;
	assert_eq!(
		(pending.stop_reason, pending.deferred.as_ref()),
		(StopReason::Deferred, Some(&handle))
	);
	let ready = models
		.fetch_deferred(
			&model,
			&handle,
			DeferredFetchOptions {
				wait: Some(0),
				..DeferredFetchOptions::default()
			},
		)
		.await;
	assert_eq!(ready.stop_reason, StopReason::Stop);
	assert_eq!(
		common::to_json(&ready.content),
		json!([{ "type": "text", "text": "ready" }])
	);
	assert!(ready.usage.total_tokens.unwrap() > 0);
	let state = faux.state();
	assert_eq!((state.call_count, state.deferred_fetch_count), (1, 2));
}

// TS: providers.test.ts > fauxProvider > records cancellation and returns deferred fetch failures in-band
#[tokio::test]
async fn records_cancellation_and_returns_deferred_fetch_failures_in_band() {
	let faux = faux_provider(&FauxProviderOptions::default());
	let models = Models::default();
	models.set_provider(Arc::clone(&faux.provider));
	let failing: FauxResponseFactory = Arc::new(|_, _, _, _| async { Err("deferred failed".to_owned()) }.boxed());
	faux.set_responses(vec![
		FauxResponseStep::Factory(failing),
		faux_text_message("cancelled").into(),
	]);
	let model = faux.model().clone();
	let failed_submission = models.complete_simple(&model, context(), deferred(None)).await;
	let failed = models
		.fetch_deferred(
			&model,
			failed_submission.deferred.as_ref().unwrap(),
			DeferredFetchOptions::default(),
		)
		.await;
	assert_eq!(
		(failed.stop_reason, failed.error_message.as_deref()),
		(StopReason::Error, Some("deferred failed"))
	);
	let cancelled_submission = models.complete_simple(&model, context(), deferred(None)).await;
	let handle = cancelled_submission.deferred.unwrap();
	models
		.cancel_deferred(&model, &handle, ProviderRequestOptions::default())
		.await
		.unwrap();
	assert_eq!(faux.state().cancelled_deferred, std::slice::from_ref(&handle));
	let cancelled = models
		.fetch_deferred(&model, &handle, DeferredFetchOptions::default())
		.await;
	assert_eq!(cancelled.stop_reason, StopReason::Error);
	assert!(cancelled.error_message.unwrap().contains("was cancelled"));
}
