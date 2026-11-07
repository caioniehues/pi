//! The `Models` registry (`models.rs`, `provider.rs`, `auth/*`, `models_store.rs`, `model_operations.rs`): ports
//! `models-runtime.test.ts` case by case, plus the unknown-type case of `models-entry.test.ts` and
//! `compat-env.test.ts` (the request key reaches the provider).

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

mod common;

use async_trait::async_trait;
use futures_util::FutureExt;
use pi_ai::auth::credential_store::InMemoryCredentialStore;
use pi_ai::auth::resolve::AuthResolutionOverrides;
use pi_ai::auth::types::{
	ApiKeyAuth, ApiKeyCredential, ApiKeyInput, AuthCheck, AuthEvent, AuthInteraction, AuthPrompt, AuthResult, AuthType,
	Credential, CredentialInfo, CredentialStore, CredentialStoreError, CredentialUpdate, LoginOptions, ModelAuth,
	OAuthAuth, OAuthCredential, ProviderAuth,
};
use pi_ai::diagnostics::now_ms;
use pi_ai::error::{ModelsError, ModelsErrorCode};
use pi_ai::event_stream::{AssistantMessageEventStream, PartialMessage};
use pi_ai::headers::ProviderHeaders;
use pi_ai::model_operations::{AnyModel, calculate_cost, has_api};
use pi_ai::models::{AuthTarget, CreateModelsOptions, Models, ModelsRefreshOptions};
use pi_ai::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
use pi_ai::options::{SimpleStreamOptions, StreamOptions, TransformHeaders};
use pi_ai::provider::{
	CreateProviderOptions, ModelsPublication, Persist, Provider, ProviderApi, ProviderCapabilities, ProviderStreams,
	RefreshModelsContext, create_provider,
};
use pi_ai::transcript::{Context, TranscriptContext};
use pi_types::{DoneReason, Model, StopReason};
use serde_json::json;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn test_model(provider: &str, id: &str) -> Model {
	common::model(
		"test-api",
		provider,
		id,
		json!({ "contextWindow": 10000, "maxTokens": 1000 }),
	)
}

fn context() -> Context {
	Context {
		messages: vec![common::user("hi", 1)],
		..Context::default()
	}
}

/// What a provider request received.
#[derive(Clone)]
struct ProviderCall {
	model: Model,
	options: StreamOptions,
}

type Calls = Arc<Mutex<Vec<ProviderCall>>>;

type RefreshFn = Arc<
	dyn Fn(RefreshModelsContext) -> futures_util::future::BoxFuture<'static, Result<(), ModelsError>> + Send + Sync,
>;

/// A handwritten provider, like the TS `testProvider`.
struct TestProvider {
	id: String,
	base_url: Option<String>,
	auth: ProviderAuth,
	models: Arc<Mutex<Vec<Model>>>,
	refresh: Option<RefreshFn>,
	calls: Calls,
}

fn ambient() -> ProviderAuth {
	ProviderAuth {
		api_key: Some(Arc::new(EnvKeyAuth {
			key: None,
			ambient: true,
		})),
		oauth: None,
	}
}

impl TestProvider {
	fn new(id: &str) -> Self {
		Self {
			id: id.into(),
			base_url: None,
			auth: ambient(),
			models: Arc::new(Mutex::new(vec![test_model(id, "model-a")])),
			refresh: None,
			calls: Calls::default(),
		}
	}

	fn auth(mut self, auth: ProviderAuth) -> Self {
		self.auth = auth;
		self
	}

	fn models(self, models: Vec<Model>) -> Self {
		*self.models.lock().unwrap() = models;
		self
	}

	fn refresh(mut self, refresh: RefreshFn) -> Self {
		self.refresh = Some(refresh);
		self
	}

	fn respond(&self, model: &Model, options: StreamOptions) -> AssistantMessageEventStream {
		self.calls.lock().unwrap().push(ProviderCall {
			model: model.clone(),
			options,
		});
		let stream = AssistantMessageEventStream::new();
		let mut message = common::assistant(
			json!({ "content": [{ "type": "text", "text": "ok" }], "api": model.api, "provider": model.provider, "model": model.id, "timestamp": now_ms() }),
		);
		message.stop_reason = StopReason::Stop;
		stream.push_start(&PartialMessage::new(message.clone()));
		stream.push_done(DoneReason::Stop, message.clone());
		stream.end(Some(message));
		stream
	}
}

#[async_trait]
impl Provider for TestProvider {
	fn id(&self) -> &str {
		&self.id
	}
	fn name(&self) -> &str {
		&self.id
	}
	fn base_url(&self) -> Option<&str> {
		self.base_url.as_deref()
	}
	fn auth(&self) -> &ProviderAuth {
		&self.auth
	}
	fn capabilities(&self) -> ProviderCapabilities {
		ProviderCapabilities {
			refresh_models: self.refresh.is_some(),
			..ProviderCapabilities::default()
		}
	}
	fn get_models(&self) -> Vec<Model> {
		self.models.lock().unwrap().clone()
	}
	async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), ModelsError> {
		match &self.refresh {
			Some(refresh) => refresh(context).await,
			None => Ok(()),
		}
	}
	fn stream(
		&self,
		model: &Model,
		_context: TranscriptContext,
		options: StreamOptions,
	) -> AssistantMessageEventStream {
		self.respond(model, options)
	}
	fn stream_simple(
		&self,
		model: &Model,
		_context: TranscriptContext,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		self.respond(model, options.stream)
	}
}

/// Resolves a stored key, else `key` (the TS `envKeyAuth`); with `ambient`, resolves configured with no values.
struct EnvKeyAuth {
	key: Option<String>,
	ambient: bool,
}

fn env_key_auth(key: Option<&str>) -> Arc<dyn ApiKeyAuth> {
	Arc::new(EnvKeyAuth {
		key: key.map(str::to_owned),
		ambient: false,
	})
}

#[async_trait]
impl ApiKeyAuth for EnvKeyAuth {
	fn name(&self) -> &str {
		"Test API key"
	}

	async fn resolve(&self, input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		if self.ambient {
			return Ok(Some(AuthResult::default()));
		}
		let resolved = input
			.credential
			.and_then(|credential| credential.key.clone())
			.or_else(|| self.key.clone());
		Ok(resolved.map(|key| AuthResult {
			auth: ModelAuth {
				api_key: Some(key),
				..ModelAuth::default()
			},
			env: None,
			source: Some(if input.credential.is_some() { "stored" } else { "env" }.into()),
		}))
	}
}

type RefreshOAuth = Arc<
	dyn Fn(OAuthCredential) -> futures_util::future::BoxFuture<'static, Result<OAuthCredential, ModelsError>>
		+ Send
		+ Sync,
>;

/// OAuth whose refresh is a closure (the TS `testOAuth`).
struct TestOAuth {
	refresh: RefreshOAuth,
	refreshes: AtomicU32,
}

fn test_oauth(refresh: RefreshOAuth) -> Arc<TestOAuth> {
	Arc::new(TestOAuth {
		refresh,
		refreshes: AtomicU32::new(0),
	})
}

fn identity_refresh() -> RefreshOAuth {
	Arc::new(|credential| async move { Ok(credential) }.boxed())
}

#[async_trait]
impl OAuthAuth for TestOAuth {
	fn name(&self) -> &str {
		"Test OAuth"
	}
	async fn login(
		&self,
		_interaction: &dyn AuthInteraction,
		_options: &LoginOptions,
		_cancel: &CancellationToken,
	) -> Result<OAuthCredential, ModelsError> {
		Err(ModelsError::new(ModelsErrorCode::Auth, "not used"))
	}
	async fn refresh(
		&self,
		credential: &OAuthCredential,
		_cancel: &CancellationToken,
	) -> Result<OAuthCredential, ModelsError> {
		self.refreshes.fetch_add(1, Ordering::SeqCst);
		(self.refresh)(credential.clone()).await
	}
	async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
		Ok(ModelAuth {
			api_key: Some(credential.access.clone()),
			..ModelAuth::default()
		})
	}
}

fn oauth_credential(access: &str, refresh: &str, expires: u64) -> Credential {
	Credential::OAuth(OAuthCredential {
		refresh: refresh.into(),
		access: access.into(),
		expires,
		extra: pi_json::JsonObject::new(),
	})
}

async fn store(credentials: &dyn CredentialStore, provider: &str, credential: Credential) {
	let update: CredentialUpdate = Box::new(move |_| async move { Ok(Some(credential)) }.boxed());
	credentials
		.modify(provider, update, &CancellationToken::new())
		.await
		.unwrap();
}

fn with_store(credentials: &Arc<InMemoryCredentialStore>) -> Models {
	Models::new(CreateModelsOptions {
		credentials: Some(Arc::clone(credentials) as Arc<dyn CredentialStore>),
		..CreateModelsOptions::default()
	})
}

async fn api_key_of(models: &Models, target: AuthTarget<'_>) -> Option<String> {
	models
		.get_auth(target, AuthResolutionOverrides::default())
		.await
		.unwrap()
		.and_then(|result| result.auth.api_key)
}

fn access_of(credential: Option<Credential>) -> String {
	match credential {
		Some(Credential::OAuth(credential)) => credential.access,
		_ => panic!("an OAuth credential"),
	}
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn enumerates_credential_metadata_without_exposing_secrets() {
	let credentials = InMemoryCredentialStore::new();
	store(
		&credentials,
		"api-provider",
		Credential::ApiKey(ApiKeyCredential {
			key: Some("secret".into()),
			env: None,
		}),
	)
	.await;
	store(
		&credentials,
		"oauth-provider",
		oauth_credential("access", "refresh", now_ms() + 60_000),
	)
	.await;
	assert_eq!(
		credentials.list(&CancellationToken::new()).await.unwrap(),
		[
			CredentialInfo {
				provider_id: "api-provider".into(),
				auth_type: AuthType::ApiKey
			},
			CredentialInfo {
				provider_id: "oauth-provider".into(),
				auth_type: AuthType::OAuth
			},
		]
	);
}

// TS: models-runtime.test.ts
#[test]
fn applies_request_wide_pricing_tiers_above_the_configured_input_threshold() {
	let mut model = test_model("openai", "gpt-5.6-sol");
	model.cost = common::from_json(
		json!({ "input": 5, "output": 30, "cacheRead": 0.5, "cacheWrite": 6.25, "tiers": [{ "inputTokensAbove": 272000, "input": 10, "output": 45, "cacheRead": 1, "cacheWrite": 12.5 }] }),
	);
	let usage = |cache_write: i64| {
		common::from_json::<pi_types::Usage>(
			json!({ "input": 200000, "output": 100000, "cacheRead": 72000, "cacheWrite": cache_write, "totalTokens": 372000 + cache_write, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }),
		)
	};
	let short = calculate_cost(&model.cost, &mut usage(0));
	assert_eq!(
		[short.input, short.output, short.cache_read, short.cache_write],
		[1.0, 3.0, 0.036, 0.0]
	);
	let long = calculate_cost(&model.cost, &mut usage(1));
	assert_eq!(
		[long.input, long.output, long.cache_read, long.cache_write],
		[2.0, 4.5, 0.072, 0.000_012_5]
	);
}

// TS: models-runtime.test.ts
#[test]
fn marks_catalog_derived_totals_as_not_provider_reported() {
	let mut usage: pi_types::Usage = common::from_json(
		json!({ "input": 1_000_000, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 1_000_000, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 9, "totalFromProvider": true } }),
	);
	calculate_cost(&test_model("openai", "gpt-5.6-sol").cost, &mut usage);
	assert_eq!(usage.cost.total_from_provider, Some(false));
}

// Rust-only.
#[test]
fn applies_the_1h_cache_write_at_twice_the_input_rate() {
	let model = common::model(
		"anthropic-messages",
		"anthropic",
		"m",
		json!({ "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 } }),
	);
	let mut usage: pi_types::Usage = common::from_json(
		json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 1_000_000, "cacheWrite1h": 400_000, "totalTokens": 1_000_000, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } }),
	);
	let cost = calculate_cost(&model.cost, &mut usage);
	assert!((cost.cache_write - (3.75 * 0.6 + 3.0 * 2.0 * 0.4)).abs() < 1e-9);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn registers_replaces_and_deletes_providers() {
	let models = Models::default();
	models.set_provider(Arc::new(TestProvider::new("p1")));
	models.set_provider(Arc::new(TestProvider::new("p2")));
	assert_eq!(
		models
			.get_providers()
			.iter()
			.map(|provider| provider.id().to_owned())
			.collect::<Vec<_>>(),
		["p1", "p2"]
	);
	let replacement: Arc<dyn Provider> = Arc::new(TestProvider::new("p1"));
	models.set_provider(Arc::clone(&replacement));
	assert!(Arc::ptr_eq(&models.get_provider("p1").unwrap(), &replacement));
	assert_eq!(models.get_providers().len(), 2);
	models.delete_provider("p1");
	assert!(models.get_provider("p1").is_none());
	models.clear_providers();
	assert!(models.get_providers().is_empty());
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn lists_and_finds_models_per_provider() {
	let models = Models::default();
	models.set_provider(Arc::new(
		TestProvider::new("p1").models(vec![test_model("p1", "m1"), test_model("p1", "m2")]),
	));
	models.set_provider(Arc::new(TestProvider::new("p2").models(vec![test_model("p2", "m3")])));
	let ids = |list: Vec<Model>| list.into_iter().map(|model| model.id).collect::<Vec<_>>();
	assert_eq!(ids(models.get_models(None)), ["m1", "m2", "m3"]);
	assert_eq!(ids(models.get_models(Some("p1"))), ["m1", "m2"]);
	assert!(models.get_models(Some("nope")).is_empty());
	assert_eq!(models.get_model("p2", "m3").map(|model| model.id), Some("m3".into()));
	assert!(models.get_model("p2", "missing").is_none());
	let found = AnyModel::Chat(models.get_model("p2", "m3").unwrap());
	assert!(!has_api(&found, "openai-completions"));
	assert!(has_api(&found, "test-api"));
}

fn refresh_fn<F>(refresh: F) -> RefreshFn
where
	F: Fn(RefreshModelsContext) -> futures_util::future::BoxFuture<'static, Result<(), ModelsError>>
		+ Send
		+ Sync
		+ 'static,
{
	Arc::new(refresh)
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn refresh_updates_every_configured_dynamic_provider_and_reports_failures() {
	let provider = TestProvider::new("dyn").models(vec![test_model("dyn", "before")]);
	let list = Arc::clone(&provider.models);
	let refreshes = Arc::new(AtomicU32::new(0));
	let counter = Arc::clone(&refreshes);
	let provider = provider.refresh(refresh_fn(move |context| {
		let (list, counter) = (Arc::clone(&list), Arc::clone(&counter));
		async move {
			if !context.allow_network {
				return Ok(());
			}
			counter.fetch_add(1, Ordering::SeqCst);
			let update = Box::new(move || *list.lock().unwrap() = vec![test_model("dyn", "after")]);
			(context.publish)(ModelsPublication {
				persist: Persist::Keep,
				update: Some(update),
			})
			.await?;
			Ok(())
		}
		.boxed()
	}));
	let models = Models::default();
	models.set_provider(Arc::new(provider));
	models.set_provider(Arc::new(
		TestProvider::new("static").models(vec![test_model("static", "s1")]),
	));
	assert!(models.get_model("dyn", "before").is_some());
	let first = models.refresh(ModelsRefreshOptions::default()).await;
	assert!(first.errors.is_empty());
	assert_eq!(refreshes.load(Ordering::SeqCst), 1);
	assert!(models.get_model("dyn", "after").is_some() && models.get_model("dyn", "before").is_none());

	let flaky = TestProvider::new("flaky").refresh(refresh_fn(|context| {
		async move {
			if context.allow_network {
				Err(ModelsError::new(ModelsErrorCode::ModelSource, "fetch failed"))
			} else {
				Ok(())
			}
		}
		.boxed()
	}));
	models.set_provider(Arc::new(flaky));
	let second = models.refresh(ModelsRefreshOptions::default()).await;
	assert_eq!(refreshes.load(Ordering::SeqCst), 2);
	assert_eq!(
		second
			.errors
			.iter()
			.find(|(id, _)| id == "flaky")
			.map(|(_, error)| error.message.as_str()),
		Some("fetch failed")
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn restricts_refresh_work_to_selected_providers() {
	let calls = Arc::new(Mutex::new(Vec::new()));
	let models = Models::default();
	for id in ["one", "two"] {
		let calls = Arc::clone(&calls);
		let provider = TestProvider::new(id).refresh(refresh_fn(move |context| {
			calls.lock().unwrap().push(format!(
				"{id}:{}",
				if context.allow_network { "network" } else { "cache" }
			));
			async { Ok(()) }.boxed()
		}));
		models.set_provider(Arc::new(provider));
	}
	let result = models
		.refresh(ModelsRefreshOptions {
			providers: Some(vec!["two".into(), "unknown".into()]),
			..ModelsRefreshOptions::default()
		})
		.await;
	assert!(result.errors.is_empty());
	assert_eq!(*calls.lock().unwrap(), ["two:cache", "two:network"]);
}

struct NoStreams;

#[async_trait]
impl ProviderStreams for NoStreams {
	fn stream(
		&self,
		_model: &Model,
		_context: TranscriptContext,
		_options: StreamOptions,
	) -> AssistantMessageEventStream {
		AssistantMessageEventStream::new()
	}
	fn stream_simple(
		&self,
		_model: &Model,
		_context: TranscriptContext,
		_options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		AssistantMessageEventStream::new()
	}
}

fn dynamic_provider(fetch: Arc<dyn Fn() -> Result<Vec<AnyModel>, ModelsError> + Send + Sync>) -> Arc<dyn Provider> {
	create_provider(CreateProviderOptions {
		id: "dynamic".into(),
		auth: ProviderAuth {
			api_key: Some(env_key_auth(None)),
			oauth: None,
		},
		fetch_models: Some(Arc::new(move |_| {
			let result = fetch();
			async move { result }.boxed()
		})),
		api: Some(ProviderApi::Single(Arc::new(NoStreams))),
		..CreateProviderOptions::default()
	})
	.unwrap()
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn persists_dynamic_catalogs_and_restores_them_without_network_access() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let models_store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
	store(
		credentials.as_ref(),
		"dynamic",
		Credential::ApiKey(ApiKeyCredential {
			key: Some("key".into()),
			env: None,
		}),
	)
	.await;
	let options = || CreateModelsOptions {
		credentials: Some(Arc::clone(&credentials) as Arc<dyn CredentialStore>),
		models_store: Some(Arc::clone(&models_store)),
		..CreateModelsOptions::default()
	};
	let online = Models::new(options());
	online.set_provider(dynamic_provider(Arc::new(|| {
		Ok(vec![AnyModel::Chat(test_model("dynamic", "fetched"))])
	})));
	assert!(online.refresh(ModelsRefreshOptions::default()).await.errors.is_empty());
	assert!(online.get_model("dynamic", "fetched").is_some());
	let offline = Models::new(options());
	offline.set_provider(dynamic_provider(Arc::new(|| {
		Err(ModelsError::new(ModelsErrorCode::ModelSource, "must not fetch"))
	})));
	assert!(
		offline
			.refresh(ModelsRefreshOptions {
				allow_network: Some(false),
				..ModelsRefreshOptions::default()
			})
			.await
			.errors
			.is_empty()
	);
	assert!(offline.get_model("dynamic", "fetched").is_some());
}

// TS: models-entry.test.ts
#[tokio::test]
async fn models_entry_stored_and_fetched_models_of_unknown_types_are_dropped_instead_of_failing_the_refresh() {
	let models_store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
	let image = json!({ "type": "image", "id": "stored-image", "name": "i", "api": "test-images", "provider": "dyn", "baseUrl": "b", "input": ["text"], "output": ["image"], "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 } });
	let mut embedding = common::to_json(&test_model("dyn", "future-embedding"));
	embedding["type"] = json!("embedding");
	let stored: ModelsStoreEntry =
		common::from_json(json!({ "models": [common::to_json(&test_model("dyn", "stored-chat")), image, embedding] }));
	models_store
		.write("dyn", stored, &CancellationToken::new())
		.await
		.unwrap();
	let fetched: Arc<Mutex<Vec<AnyModel>>> = Arc::default();
	let source = Arc::clone(&fetched);
	let models = Models::new(CreateModelsOptions {
		models_store: Some(Arc::clone(&models_store)),
		..CreateModelsOptions::default()
	});
	models.set_provider(
		create_provider(CreateProviderOptions {
			id: "dyn".into(),
			auth: ambient(),
			fetch_models: Some(Arc::new(move |_| {
				let models = source.lock().unwrap().clone();
				async move { Ok(models) }.boxed()
			})),
			api: Some(ProviderApi::Single(Arc::new(NoStreams))),
			..CreateProviderOptions::default()
		})
		.unwrap(),
	);
	let restored = models
		.refresh(ModelsRefreshOptions {
			providers: Some(vec!["dyn".into()]),
			allow_network: Some(false),
			..ModelsRefreshOptions::default()
		})
		.await;
	assert!(restored.errors.is_empty());
	assert_eq!(
		models
			.get_all_models(Some("dyn"))
			.iter()
			.map(AnyModel::id)
			.collect::<Vec<_>>(),
		["stored-chat", "stored-image"]
	);
	let mut video = common::to_json(&test_model("dyn", "fetched-video"));
	video["type"] = json!("video");
	*fetched.lock().unwrap() = vec![
		AnyModel::Chat(test_model("dyn", "fetched-chat")),
		common::from_json(video),
	];
	let refreshed = models
		.refresh(ModelsRefreshOptions {
			providers: Some(vec!["dyn".into()]),
			..ModelsRefreshOptions::default()
		})
		.await;
	assert!(refreshed.errors.is_empty());
	assert_eq!(
		models
			.get_all_models(Some("dyn"))
			.iter()
			.map(AnyModel::id)
			.collect::<Vec<_>>(),
		["fetched-chat"]
	);
	let persisted = models_store
		.read("dyn", &CancellationToken::new())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		persisted.models.iter().map(AnyModel::id).collect::<Vec<_>>(),
		["fetched-chat"]
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn passes_effective_api_key_credentials_and_refresh_options_while_skipping_unconfigured_providers() {
	let seen: Arc<Mutex<Option<(Option<Credential>, Option<bool>)>>> = Arc::default();
	let unconfigured = Arc::new(AtomicU32::new(0));
	let models = Models::default();
	let record = Arc::clone(&seen);
	models.set_provider(Arc::new(
		TestProvider::new("configured")
			.auth(ProviderAuth {
				api_key: Some(env_key_auth(Some("ambient-key"))),
				oauth: None,
			})
			.refresh(refresh_fn(move |context| {
				if context.allow_network {
					*record.lock().unwrap() = Some((context.credential.clone(), context.force));
				}
				async { Ok(()) }.boxed()
			})),
	));
	let counter = Arc::clone(&unconfigured);
	models.set_provider(Arc::new(
		TestProvider::new("unconfigured")
			.auth(ProviderAuth {
				api_key: Some(env_key_auth(None)),
				oauth: None,
			})
			.refresh(refresh_fn(move |context| {
				if context.allow_network {
					counter.fetch_add(1, Ordering::SeqCst);
				}
				async { Ok(()) }.boxed()
			})),
	));
	models
		.refresh(ModelsRefreshOptions {
			force: Some(true),
			..ModelsRefreshOptions::default()
		})
		.await;
	let (credential, force) = seen.lock().unwrap().clone().unwrap();
	assert_eq!(
		credential,
		Some(Credential::ApiKey(ApiKeyCredential {
			key: Some("ambient-key".into()),
			env: None
		}))
	);
	assert_eq!(force, Some(true));
	assert_eq!(unconfigured.load(Ordering::SeqCst), 0);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn passes_the_registered_providers_base_url_including_wrapper_overrides_to_refresh_models() {
	let urls: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
	let record = Arc::clone(&urls);
	let mut provider = TestProvider::new("dynamic").refresh(refresh_fn(move |context| {
		record.lock().unwrap().push(context.base_url.clone());
		async { Ok(()) }.boxed()
	}));
	provider.base_url = Some("https://regional.example.test/v1".into());
	let models = Models::default();
	models.set_provider(Arc::new(provider));
	models.refresh(ModelsRefreshOptions::default()).await;
	assert_eq!(
		*urls.lock().unwrap(),
		[
			Some("https://regional.example.test/v1".into()),
			Some("https://regional.example.test/v1".into())
		]
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn returns_aborted_state_without_reporting_cancellation_as_a_provider_error() {
	let cancel = CancellationToken::new();
	let trigger = cancel.clone();
	let models = Models::default();
	models.set_provider(Arc::new(TestProvider::new("dynamic").refresh(refresh_fn(move |_| {
		trigger.cancel();
		async { Ok(()) }.boxed()
	}))));
	let result = models
		.refresh(ModelsRefreshOptions {
			cancel: Some(cancel),
			..ModelsRefreshOptions::default()
		})
		.await;
	assert!(result.aborted);
	assert!(result.errors.is_empty());
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn stops_waiting_on_abort_when_a_provider_ignores_its_signal() {
	let cancel = CancellationToken::new();
	let started = Arc::new(tokio::sync::Notify::new());
	let notify = Arc::clone(&started);
	let models = Models::default();
	models.set_provider(Arc::new(TestProvider::new("dynamic").refresh(refresh_fn(move |_| {
		let notify = Arc::clone(&notify);
		async move {
			notify.notify_one();
			std::future::pending::<()>().await;
			Ok(())
		}
		.boxed()
	}))));
	let pending = {
		let (models, cancel) = (models.clone(), cancel.clone());
		tokio::spawn(async move {
			models
				.refresh(ModelsRefreshOptions {
					cancel: Some(cancel),
					..ModelsRefreshOptions::default()
				})
				.await
		})
	};
	started.notified().await;
	cancel.cancel();
	let result = pending.await.unwrap();
	assert!(result.aborted && result.errors.is_empty());
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn rejects_late_publication_from_a_superseded_non_cooperative_provider() {
	let models_store: Arc<dyn ModelsStore> = Arc::new(InMemoryModelsStore::new());
	let state = Arc::new(Mutex::new("initial".to_owned()));
	let calls = Arc::new(AtomicU32::new(0));
	let (first_started, first_release) = (
		Arc::new(tokio::sync::Notify::new()),
		Arc::new(tokio::sync::Notify::new()),
	);
	let (state_ref, calls_ref, started_ref, release_ref) = (
		Arc::clone(&state),
		Arc::clone(&calls),
		Arc::clone(&first_started),
		Arc::clone(&first_release),
	);
	let provider = TestProvider::new("dynamic").refresh(refresh_fn(move |context| {
		let (state, calls, started, release) = (
			Arc::clone(&state_ref),
			Arc::clone(&calls_ref),
			Arc::clone(&started_ref),
			Arc::clone(&release_ref),
		);
		async move {
			if !context.allow_network {
				return Ok(());
			}
			let current = calls.fetch_add(1, Ordering::SeqCst) + 1;
			if current == 1 {
				started.notify_one();
				release.notified().await;
			}
			let value = format!("generation-{current}");
			let entry = ModelsStoreEntry {
				models: vec![AnyModel::Chat(test_model("dynamic", &value))],
				..ModelsStoreEntry::default()
			};
			let update = Box::new(move || *state.lock().unwrap() = value);
			(context.publish)(ModelsPublication {
				persist: Persist::Write(entry),
				update: Some(update),
			})
			.await?;
			Ok(())
		}
		.boxed()
	}));
	let models = Models::new(CreateModelsOptions {
		models_store: Some(Arc::clone(&models_store)),
		..CreateModelsOptions::default()
	});
	models.set_provider(Arc::new(provider));
	let selected = || ModelsRefreshOptions {
		providers: Some(vec!["dynamic".into()]),
		..ModelsRefreshOptions::default()
	};
	let first = {
		let models = models.clone();
		tokio::spawn(async move { models.refresh(selected()).await })
	};
	first_started.notified().await;
	models.refresh(selected()).await;
	first.await.unwrap();
	first_release.notify_one();
	tokio::task::yield_now().await;
	assert_eq!(*state.lock().unwrap(), "generation-2");
	assert_eq!(
		models_store
			.read("dynamic", &CancellationToken::new())
			.await
			.unwrap()
			.unwrap()
			.models[0]
			.id(),
		"generation-2"
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn cancels_queued_credential_mutations_without_running_them_later() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let release = Arc::new(tokio::sync::Notify::new());
	let gate = Arc::clone(&release);
	let first = {
		let credentials = Arc::clone(&credentials);
		tokio::spawn(async move {
			let update: CredentialUpdate = Box::new(move |_| {
				async move {
					gate.notified().await;
					Ok(Some(Credential::ApiKey(ApiKeyCredential {
						key: Some("first".into()),
						env: None,
					})))
				}
				.boxed()
			});
			credentials.modify("p1", update, &CancellationToken::new()).await
		})
	};
	tokio::task::yield_now().await;
	let cancel = CancellationToken::new();
	let ran = Arc::new(AtomicU32::new(0));
	let counter = Arc::clone(&ran);
	let second = {
		let (credentials, cancel) = (Arc::clone(&credentials), cancel.clone());
		tokio::spawn(async move {
			let update: CredentialUpdate = Box::new(move |_| {
				counter.fetch_add(1, Ordering::SeqCst);
				async {
					Ok(Some(Credential::ApiKey(ApiKeyCredential {
						key: Some("second".into()),
						env: None,
					})))
				}
				.boxed()
			});
			credentials.modify("p1", update, &cancel).await
		})
	};
	tokio::task::yield_now().await;
	cancel.cancel();
	assert!(matches!(second.await.unwrap(), Err(CredentialStoreError::Aborted)));
	release.notify_one();
	first.await.unwrap().unwrap();
	assert_eq!(ran.load(Ordering::SeqCst), 0);
	assert_eq!(
		credentials.read("p1", &CancellationToken::new()).await.unwrap(),
		Some(Credential::ApiKey(ApiKeyCredential {
			key: Some("first".into()),
			env: None
		}))
	);
}

// Radius bug report 01a10855-43d8-7447-a710-2acf5ee0b2ee: refresh_token_invalidated after a cancelled refresh.
// TS: models-runtime.test.ts
#[tokio::test]
async fn persists_an_oauth_refresh_that_started_before_the_request_was_cancelled() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	store(credentials.as_ref(), "p1", oauth_credential("old", "old-refresh", 0)).await;
	let cancel = CancellationToken::new();
	let trigger = cancel.clone();
	let oauth = test_oauth(Arc::new(move |credential| {
		// The provider has rotated old-refresh by the time the request is cancelled.
		trigger.cancel();
		async move {
			Ok(OAuthCredential {
				access: "new".into(),
				refresh: "new-refresh".into(),
				expires: now_ms() + 60_000,
				..credential
			})
		}
		.boxed()
	}));
	let models = with_store(&credentials);
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: None,
		oauth: Some(oauth),
	})));
	let result = models
		.get_auth(
			AuthTarget::Provider("p1"),
			AuthResolutionOverrides {
				cancel: Some(cancel),
				..AuthResolutionOverrides::default()
			},
		)
		.await;
	assert!(result.unwrap_err().is_aborted());
	for _ in 0..100 {
		if let Some(Credential::OAuth(credential)) = credentials.read("p1", &CancellationToken::new()).await.unwrap()
			&& credential.refresh == "new-refresh"
		{
			return;
		}
		tokio::task::yield_now().await;
	}
	panic!("the started refresh was not persisted");
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn resolves_auth_stored_credential_owns_the_provider_ambient_only_when_nothing_stored() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let models = with_store(&credentials);
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(env_key_auth(Some("env-key"))),
		oauth: Some(test_oauth(identity_refresh())),
	})));
	let model = AnyModel::Chat(test_model("p1", "model-a"));
	assert_eq!(
		api_key_of(&models, AuthTarget::Model(&model)).await.as_deref(),
		Some("env-key")
	);
	assert_eq!(
		api_key_of(&models, AuthTarget::Provider("p1")).await.as_deref(),
		Some("env-key")
	);
	let explicit = models
		.get_auth(
			AuthTarget::Model(&model),
			AuthResolutionOverrides {
				api_key: Some("explicit-key".into()),
				..AuthResolutionOverrides::default()
			},
		)
		.await
		.unwrap();
	assert_eq!(
		explicit.and_then(|result| result.auth.api_key).as_deref(),
		Some("explicit-key")
	);
	store(
		credentials.as_ref(),
		"p1",
		oauth_credential("oauth-token", "r", now_ms() + 10 * 60_000),
	)
	.await;
	let resolution = models
		.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		(resolution.auth.api_key.as_deref(), resolution.source.as_deref()),
		(Some("oauth-token"), Some("OAuth"))
	);
	store(
		credentials.as_ref(),
		"p1",
		Credential::ApiKey(ApiKeyCredential {
			key: Some("stored-key".into()),
			env: None,
		}),
	)
	.await;
	let resolution = models
		.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		(resolution.auth.api_key.as_deref(), resolution.source.as_deref()),
		(Some("stored-key"), Some("stored"))
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn checks_provider_auth_without_refreshing_oauth_and_filters_available_models() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let models = with_store(&credentials);
	let oauth = test_oauth(identity_refresh());
	models.set_provider(Arc::new(TestProvider::new("ambient").auth(ProviderAuth {
		api_key: Some(env_key_auth(Some("env-key"))),
		oauth: None,
	})));
	models.set_provider(Arc::new(TestProvider::new("missing").auth(ProviderAuth {
		api_key: Some(env_key_auth(None)),
		oauth: None,
	})));
	models.set_provider(Arc::new(TestProvider::new("oauth").auth(ProviderAuth {
		api_key: None,
		oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
	})));
	store(credentials.as_ref(), "oauth", oauth_credential("expired", "refresh", 0)).await;
	assert_eq!(
		models.check_auth("ambient", None).await.unwrap(),
		Some(AuthCheck {
			source: Some("env".into()),
			auth_type: AuthType::ApiKey
		})
	);
	assert_eq!(models.check_auth("missing", None).await.unwrap(), None);
	assert_eq!(
		models.check_auth("oauth", None).await.unwrap(),
		Some(AuthCheck {
			source: Some("OAuth".into()),
			auth_type: AuthType::OAuth
		})
	);
	assert_eq!(oauth.refreshes.load(Ordering::SeqCst), 0);
	let providers = |list: Vec<Model>| list.into_iter().map(|model| model.provider).collect::<Vec<_>>();
	assert_eq!(
		providers(models.get_available(None, None).await.unwrap()),
		["ambient", "oauth"]
	);
	assert_eq!(
		providers(models.get_available(Some("ambient"), None).await.unwrap()),
		["ambient"]
	);
}

struct LoginKey;

#[async_trait]
impl ApiKeyAuth for LoginKey {
	fn name(&self) -> &str {
		"Login key"
	}
	fn supports_login(&self) -> bool {
		true
	}
	async fn login(
		&self,
		_interaction: &dyn AuthInteraction,
		_cancel: &CancellationToken,
	) -> Result<ApiKeyCredential, ModelsError> {
		Ok(ApiKeyCredential {
			key: Some("logged-in".into()),
			env: None,
		})
	}
	async fn resolve(&self, _input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		Ok(None)
	}
}

struct Silent;

#[async_trait]
impl AuthInteraction for Silent {
	async fn prompt(&self, _prompt: AuthPrompt) -> Result<String, ModelsError> {
		Ok("unused".into())
	}
	fn notify(&self, _event: AuthEvent) {}
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn runs_provider_login_and_logout_through_the_credential_store() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let models = with_store(&credentials);
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(Arc::new(LoginKey)),
		oauth: None,
	})));
	let credential = models
		.login("p1", AuthType::ApiKey, &Silent, &LoginOptions::default(), None)
		.await
		.unwrap();
	assert_eq!(
		credential,
		Credential::ApiKey(ApiKeyCredential {
			key: Some("logged-in".into()),
			env: None
		})
	);
	assert_eq!(
		credentials.read("p1", &CancellationToken::new()).await.unwrap(),
		Some(credential)
	);
	models.logout("p1", None).await.unwrap();
	assert_eq!(credentials.read("p1", &CancellationToken::new()).await.unwrap(), None);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn a_stored_credential_without_a_matching_handler_blocks_ambient_fallback() {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let models = with_store(&credentials);
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(env_key_auth(Some("env-key"))),
		oauth: None,
	})));
	store(credentials.as_ref(), "p1", oauth_credential("a", "r", 0)).await;
	assert!(
		models
			.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
			.await
			.unwrap()
			.is_none()
	);
}

fn rotating(access: &'static str) -> RefreshOAuth {
	Arc::new(move |credential| {
		async move {
			Ok(OAuthCredential {
				access: access.into(),
				expires: now_ms() + 60 * 60_000,
				..credential
			})
		}
		.boxed()
	})
}

async fn oauth_models(
	credential: Credential,
	refresh: RefreshOAuth,
) -> (Models, Arc<InMemoryCredentialStore>, Arc<TestOAuth>) {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	store(credentials.as_ref(), "p1", credential).await;
	let oauth = test_oauth(refresh);
	let models = with_store(&credentials);
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: None,
		oauth: Some(Arc::clone(&oauth) as Arc<dyn OAuthAuth>),
	})));
	(models, credentials, oauth)
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn refreshes_expired_oauth_credentials_and_persists_the_rotated_credential() {
	let (models, credentials, _) = oauth_models(oauth_credential("old-token", "r", 0), rotating("new-token")).await;
	assert_eq!(
		api_key_of(&models, AuthTarget::Provider("p1")).await.as_deref(),
		Some("new-token")
	);
	assert_eq!(
		access_of(credentials.read("p1", &CancellationToken::new()).await.unwrap()),
		"new-token"
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn refreshes_oauth_credentials_with_less_than_five_minutes_remaining() {
	let (models, _, oauth) = oauth_models(
		oauth_credential("old-token", "r", now_ms() + 60_000),
		rotating("new-token"),
	)
	.await;
	assert_eq!(
		api_key_of(&models, AuthTarget::Provider("p1")).await.as_deref(),
		Some("new-token")
	);
	assert_eq!(oauth.refreshes.load(Ordering::SeqCst), 1);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn honors_a_callers_longer_oauth_minimum_validity() {
	let (models, _, oauth) = oauth_models(
		oauth_credential("old-token", "r", now_ms() + 10 * 60_000),
		rotating("new-token"),
	)
	.await;
	let overrides = AuthResolutionOverrides {
		min_oauth_validity_ms: Some(30 * 60_000),
		..AuthResolutionOverrides::default()
	};
	assert_eq!(
		models
			.get_auth(AuthTarget::Provider("p1"), overrides)
			.await
			.unwrap()
			.and_then(|result| result.auth.api_key)
			.as_deref(),
		Some("new-token")
	);
	assert_eq!(oauth.refreshes.load(Ordering::SeqCst), 1);
}

fn failing(message: &'static str) -> RefreshOAuth {
	Arc::new(move |_| async move { Err(ModelsError::new(ModelsErrorCode::Oauth, message)) }.boxed())
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn rejects_with_code_oauth_when_refresh_fails_preserving_the_stored_credential() {
	let (models, credentials, _) = oauth_models(oauth_credential("old", "r", 0), failing("invalid_grant")).await;
	let error = models
		.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
		.await
		.unwrap_err();
	assert_eq!(error.code, ModelsErrorCode::Oauth);
	assert_eq!(
		access_of(credentials.read("p1", &CancellationToken::new()).await.unwrap()),
		"old"
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn serializes_concurrent_oauth_refreshes_through_store_modify_no_double_refresh() {
	let refresh: RefreshOAuth = Arc::new(|credential| {
		async move {
			tokio::time::sleep(std::time::Duration::from_millis(10)).await;
			Ok(OAuthCredential {
				access: "new-1".into(),
				refresh: "r2".into(),
				expires: now_ms() + 60 * 60_000,
				..credential
			})
		}
		.boxed()
	});
	let (models, _, oauth) = oauth_models(oauth_credential("old", "r1", 0), refresh).await;
	let (a, b) = tokio::join!(
		api_key_of(&models, AuthTarget::Provider("p1")),
		api_key_of(&models, AuthTarget::Provider("p1"))
	);
	assert_eq!(oauth.refreshes.load(Ordering::SeqCst), 1);
	assert_eq!((a.as_deref(), b.as_deref()), (Some("new-1"), Some("new-1")));
}

/// A store that counts `modify` calls, or fails reads or writes.
struct ProbeStore {
	base: InMemoryCredentialStore,
	modifies: AtomicU32,
	fail_read: bool,
	fail_modify: bool,
	fixed: Option<Credential>,
}

impl ProbeStore {
	fn new() -> Self {
		Self {
			base: InMemoryCredentialStore::new(),
			modifies: AtomicU32::new(0),
			fail_read: false,
			fail_modify: false,
			fixed: None,
		}
	}
}

fn disk_on_fire() -> CredentialStoreError {
	CredentialStoreError::Failed(Arc::new(pi_ai::error::MessageError("disk on fire".into())))
}

#[async_trait]
impl CredentialStore for ProbeStore {
	async fn read(
		&self,
		provider_id: &str,
		cancel: &CancellationToken,
	) -> Result<Option<Credential>, CredentialStoreError> {
		if self.fail_read {
			return Err(disk_on_fire());
		}
		match &self.fixed {
			Some(fixed) => Ok(Some(fixed.clone())),
			None => self.base.read(provider_id, cancel).await,
		}
	}
	async fn list(&self, cancel: &CancellationToken) -> Result<Vec<CredentialInfo>, CredentialStoreError> {
		self.base.list(cancel).await
	}
	async fn modify(
		&self,
		provider_id: &str,
		update: CredentialUpdate,
		cancel: &CancellationToken,
	) -> Result<Option<Credential>, CredentialStoreError> {
		self.modifies.fetch_add(1, Ordering::SeqCst);
		if self.fail_modify {
			return Err(disk_on_fire());
		}
		self.base.modify(provider_id, update, cancel).await
	}
	async fn delete(&self, provider_id: &str, cancel: &CancellationToken) -> Result<(), CredentialStoreError> {
		self.base.delete(provider_id, cancel).await
	}
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn valid_oauth_tokens_resolve_without_touching_modify() {
	let probe = Arc::new(ProbeStore::new());
	store(
		&probe.base,
		"p1",
		oauth_credential("valid", "r", now_ms() + 10 * 60_000),
	)
	.await;
	let models = Models::new(CreateModelsOptions {
		credentials: Some(Arc::clone(&probe) as Arc<dyn CredentialStore>),
		..CreateModelsOptions::default()
	});
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: None,
		oauth: Some(test_oauth(identity_refresh())),
	})));
	assert_eq!(
		api_key_of(&models, AuthTarget::Provider("p1")).await.as_deref(),
		Some("valid")
	);
	assert_eq!(probe.modifies.load(Ordering::SeqCst), 0);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn wraps_credential_store_failures_in_models_error() {
	let read_failing = Arc::new(ProbeStore {
		fail_read: true,
		..ProbeStore::new()
	});
	let models = Models::new(CreateModelsOptions {
		credentials: Some(read_failing as Arc<dyn CredentialStore>),
		..CreateModelsOptions::default()
	});
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(env_key_auth(Some("env-key"))),
		oauth: None,
	})));
	assert_eq!(
		models
			.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
			.await
			.unwrap_err()
			.code,
		ModelsErrorCode::Auth
	);
	let modify_failing = Arc::new(ProbeStore {
		fail_modify: true,
		fixed: Some(oauth_credential("old", "r", 0)),
		..ProbeStore::new()
	});
	let oauth_models = Models::new(CreateModelsOptions {
		credentials: Some(modify_failing as Arc<dyn CredentialStore>),
		..CreateModelsOptions::default()
	});
	oauth_models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: None,
		oauth: Some(test_oauth(identity_refresh())),
	})));
	assert_eq!(
		oauth_models
			.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
			.await
			.unwrap_err()
			.code,
		ModelsErrorCode::Auth
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn keeps_the_underlying_reason_in_wrapped_oauth_refresh_errors() {
	let (models, _, _) = oauth_models(
		oauth_credential("old", "r", 0),
		failing("token refresh failed (400): invalid_grant"),
	)
	.await;
	let error = models
		.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
		.await
		.unwrap_err();
	assert!(
		error
			.message
			.contains("OAuth refresh failed for p1: token refresh failed (400): invalid_grant"),
		"{}",
		error.message
	);
}

struct FailingKey;

#[async_trait]
impl ApiKeyAuth for FailingKey {
	fn name(&self) -> &str {
		"Failing"
	}
	async fn resolve(&self, _input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		Err(ModelsError::new(ModelsErrorCode::Auth, "nope"))
	}
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn wraps_api_key_auth_failures_in_models_error() {
	let models = Models::default();
	models.set_provider(Arc::new(TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(Arc::new(FailingKey)),
		oauth: None,
	})));
	let error = models
		.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
		.await
		.unwrap_err();
	assert_eq!(error.code, ModelsErrorCode::Auth);
	assert_eq!(error.message, "API key auth failed for provider p1: nope");
}

/// Resolves a key and a base URL from the credential's or context's `ACCOUNT_ID`.
struct ScopedKey;

#[async_trait]
impl ApiKeyAuth for ScopedKey {
	fn name(&self) -> &str {
		"Scoped"
	}
	async fn resolve(&self, input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		let from_credential = input
			.credential
			.and_then(|credential| credential.env.as_ref())
			.and_then(|env| env.get("ACCOUNT_ID").cloned());
		let account = match from_credential {
			Some(account) => Some(account),
			None => input.ctx.env("ACCOUNT_ID").await,
		};
		let (Some(key), Some(account)) = (input.credential.and_then(|credential| credential.key.clone()), account)
		else {
			return Ok(None);
		};
		Ok(Some(AuthResult {
			auth: ModelAuth {
				api_key: Some(key),
				base_url: Some(format!("https://example.test/{account}")),
				headers: None,
			},
			env: Some([("ACCOUNT_ID", account)].into_iter().collect()),
			source: None,
		}))
	}
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn uses_explicit_request_api_key_and_env_during_provider_auth_resolution() {
	let provider = TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(Arc::new(ScopedKey)),
		oauth: None,
	});
	let calls = Arc::clone(&provider.calls);
	let models = Models::default();
	models.set_provider(Arc::new(provider));
	let mut options = SimpleStreamOptions::default();
	options.stream.request.api_key = Some("explicit-key".into());
	options.stream.request.env = Some([("ACCOUNT_ID", "acct".to_owned())].into_iter().collect());
	models
		.complete_simple(&test_model("p1", "model-a"), context(), options)
		.await;
	let call = calls.lock().unwrap()[0].clone();
	assert_eq!(call.model.base_url, "https://example.test/acct");
	assert_eq!(call.options.request.api_key.as_deref(), Some("explicit-key"));
	assert_eq!(
		call.options.request.env,
		Some([("ACCOUNT_ID", "acct".to_owned())].into_iter().collect())
	);
}

/// Resolves fixed auth with headers and a base URL.
struct HeaderKey;

#[async_trait]
impl ApiKeyAuth for HeaderKey {
	fn name(&self) -> &str {
		"Test"
	}
	async fn resolve(&self, _input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		let headers: ProviderHeaders = [
			("Authorization", Some("Bearer resolved-key".to_owned())),
			("x-a", Some("auth".into())),
			("x-b", Some("auth".into())),
		]
		.into_iter()
		.collect();
		Ok(Some(AuthResult {
			auth: ModelAuth {
				api_key: Some("resolved-key".into()),
				headers: Some(headers),
				base_url: Some("https://auth.test/v1".into()),
			},
			..AuthResult::default()
		}))
	}
}

fn headers(pairs: &[(&str, &str)]) -> ProviderHeaders {
	pairs
		.iter()
		.map(|(name, value)| (*name, Some((*value).to_owned())))
		.collect()
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn merges_resolved_auth_into_stream_options_explicit_options_win_per_field() {
	let provider = TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(Arc::new(HeaderKey)),
		oauth: None,
	});
	let calls = Arc::clone(&provider.calls);
	let models = Models::default();
	models.set_provider(Arc::new(provider));
	let model = test_model("p1", "model-a");
	let mut options = SimpleStreamOptions::default();
	options.stream.request.api_key = Some("explicit-key".into());
	options.stream.request.headers = Some(headers(&[("authorization", "Explicit token"), ("x-b", "explicit")]));
	assert_eq!(
		models.complete_simple(&model, context(), options).await.stop_reason,
		StopReason::Stop
	);
	let call = calls.lock().unwrap()[0].clone();
	assert_eq!(call.options.request.api_key.as_deref(), Some("explicit-key"));
	assert_eq!(
		call.options.request.headers,
		Some(headers(&[
			("x-a", "auth"),
			("authorization", "Explicit token"),
			("x-b", "explicit")
		]))
	);
	assert_eq!(call.model.base_url, "https://auth.test/v1");
	models
		.complete_simple(&model, context(), SimpleStreamOptions::default())
		.await;
	assert_eq!(
		calls.lock().unwrap()[1].options.request.api_key.as_deref(),
		Some("resolved-key")
	);
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn adds_model_headers_only_for_model_auth_and_transforms_assembled_headers_once() {
	let provider = TestProvider::new("p1").auth(ProviderAuth {
		api_key: Some(env_key_auth(Some("key"))),
		oauth: None,
	});
	let calls = Arc::clone(&provider.calls);
	let models = Models::default();
	models.set_provider(Arc::new(provider));
	let mut model = test_model("p1", "model-a");
	model.headers = Some(
		[("x-model", "model".to_owned()), ("x-shared", "model".to_owned())]
			.into_iter()
			.collect(),
	);
	assert_eq!(
		models
			.get_auth(AuthTarget::Provider("p1"), AuthResolutionOverrides::default())
			.await
			.unwrap()
			.unwrap()
			.auth
			.headers,
		None
	);
	let any = AnyModel::Chat(model.clone());
	assert_eq!(
		models
			.get_auth(AuthTarget::Model(&any), AuthResolutionOverrides::default())
			.await
			.unwrap()
			.unwrap()
			.auth
			.headers,
		Some(headers(&[("x-model", "model"), ("x-shared", "model")]))
	);
	let transforms = Arc::new(AtomicU32::new(0));
	let counter = Arc::clone(&transforms);
	let transform: TransformHeaders = Arc::new(move |assembled| {
		counter.fetch_add(1, Ordering::SeqCst);
		assert_eq!(
			assembled,
			headers(&[
				("x-model", "model"),
				("x-explicit", "explicit"),
				("X-Shared", "explicit")
			])
		);
		let mut assembled = assembled;
		assembled.insert("x-transformed", Some("yes".into()));
		async move { assembled }.boxed()
	});
	let mut options = SimpleStreamOptions::default();
	options.stream.request.headers = Some(headers(&[("x-explicit", "explicit"), ("X-Shared", "explicit")]));
	options.stream.request.transform_headers = Some(transform);
	models.complete_simple(&model, context(), options).await;
	assert_eq!(transforms.load(Ordering::SeqCst), 1);
	let call = calls.lock().unwrap()[0].clone();
	assert_eq!(
		call.options.request.headers,
		Some(headers(&[
			("x-model", "model"),
			("x-explicit", "explicit"),
			("X-Shared", "explicit"),
			("x-transformed", "yes")
		]))
	);
	assert!(call.options.request.transform_headers.is_none());
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn produces_an_error_stream_for_unknown_providers_instead_of_throwing() {
	let result = Models::default()
		.complete_simple(
			&test_model("ghost", "model-a"),
			context(),
			SimpleStreamOptions::default(),
		)
		.await;
	assert_eq!(result.stop_reason, StopReason::Error);
	assert!(result.error_message.unwrap().contains("Unknown provider: ghost"));
}

// TS: models-runtime.test.ts
#[tokio::test]
async fn streams_through_the_provider() {
	let models = Models::default();
	models.set_provider(Arc::new(TestProvider::new("p1")));
	let stream = models.stream_simple(&test_model("p1", "model-a"), context(), SimpleStreamOptions::default());
	let mut types = Vec::new();
	while let Some(event) = stream.next().await {
		types.push(common::to_json(&event)["type"].as_str().unwrap().to_owned());
	}
	assert_eq!(types, ["start", "done"]);
	assert_eq!(stream.result().await.stop_reason, StopReason::Stop);
}

// TS: compat-env.test.ts
#[tokio::test]
async fn compat_env_dispatches_the_request_api_key_to_the_provider() {
	let provider = TestProvider::new("custom-openai");
	let calls = Arc::clone(&provider.calls);
	let models = Models::default();
	models.set_provider(Arc::new(provider));
	let mut options = StreamOptions::default();
	options.request.api_key = Some("request-key".into());
	models
		.complete(
			&common::model("openai-responses", "custom-openai", "test-model", json!({})),
			context(),
			options,
		)
		.await;
	assert_eq!(
		calls.lock().unwrap()[0].options.request.api_key.as_deref(),
		Some("request-key")
	);
}

// Rust-only.
#[tokio::test]
async fn keeps_errors_recorded_before_an_abort() {
	// TS: models-runtime.test.ts semantics of `refresh` (errors are recorded per provider as they finish).
	let cancel = CancellationToken::new();
	let models = Models::default();
	models.set_provider(Arc::new(TestProvider::new("fails").refresh(refresh_fn(|context| {
		async move {
			if context.allow_network {
				Err(ModelsError::new(ModelsErrorCode::ModelSource, "fetch failed"))
			} else {
				Ok(())
			}
		}
		.boxed()
	}))));
	let trigger = cancel.clone();
	models.set_provider(Arc::new(TestProvider::new("hangs").refresh(refresh_fn(
		move |context| {
			let trigger = trigger.clone();
			async move {
				if context.allow_network {
					tokio::task::yield_now().await;
					trigger.cancel();
					std::future::pending::<()>().await;
				}
				Ok(())
			}
			.boxed()
		},
	))));
	let result = models
		.refresh(ModelsRefreshOptions {
			cancel: Some(cancel),
			..ModelsRefreshOptions::default()
		})
		.await;
	assert!(result.aborted);
	assert_eq!(
		result.errors.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
		["fails"]
	);
}
