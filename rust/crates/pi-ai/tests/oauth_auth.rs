//! OAuth resolution through `Models::get_auth` (`auth/resolve.rs`): ports the resolve parts of
//! `oauth-auth.test.ts` ("OAuth through Models.getAuth") with an OAuth adapter of the test's own; the concrete
//! provider flows (Anthropic, GitHub Copilot, ...) are C13b's.

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
	AuthInteraction, Credential, CredentialStore, CredentialUpdate, LoginOptions, ModelAuth, OAuthAuth,
	OAuthCredential, ProviderAuth,
};
use pi_ai::error::{ModelsError, ModelsErrorCode};
use pi_ai::event_stream::AssistantMessageEventStream;
use pi_ai::model_operations::AnyModel;
use pi_ai::models::{AuthTarget, CreateModelsOptions, Models};
use pi_ai::options::{SimpleStreamOptions, StreamOptions};
use pi_ai::provider::{CreateProviderOptions, ProviderApi, ProviderStreams, create_provider};
use pi_ai::transcript::TranscriptContext;
use pi_types::Model;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Derives the API key from the access token and, like GitHub Copilot, a per-credential base URL.
struct CopilotLike;

#[async_trait]
impl OAuthAuth for CopilotLike {
	fn name(&self) -> &str {
		"Copilot-like"
	}
	fn is_subscription(&self) -> bool {
		true
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
		Ok(credential.clone())
	}
	async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
		let domain = credential
			.extra
			.get("enterpriseUrl")
			.and_then(|value| value.as_str())
			.unwrap_or("individual.githubcopilot.com");
		Ok(ModelAuth {
			api_key: Some(credential.access.clone()),
			base_url: Some(format!("https://api.{domain}")),
			headers: None,
		})
	}
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

async fn models_with(credential: OAuthCredential) -> Models {
	let credentials = Arc::new(InMemoryCredentialStore::new());
	let update: CredentialUpdate = Box::new(move |_| async move { Ok(Some(Credential::OAuth(credential))) }.boxed());
	credentials
		.modify("copilot", update, &CancellationToken::new())
		.await
		.unwrap();
	let models = Models::new(CreateModelsOptions {
		credentials: Some(credentials as Arc<dyn CredentialStore>),
		..CreateModelsOptions::default()
	});
	models.set_provider(
		create_provider(CreateProviderOptions {
			id: "copilot".into(),
			auth: ProviderAuth {
				api_key: None,
				oauth: Some(Arc::new(CopilotLike)),
			},
			models: vec![AnyModel::Chat(common::model(
				"openai-completions",
				"copilot",
				"m",
				serde_json::json!({}),
			))],
			api: Some(ProviderApi::Single(Arc::new(NoStreams))),
			..CreateProviderOptions::default()
		})
		.unwrap(),
	);
	models
}

fn credential(extra: serde_json::Value) -> OAuthCredential {
	OAuthCredential {
		refresh: "r".into(),
		access: "token".into(),
		expires: pi_ai::diagnostics::now_ms() + 60 * 60_000,
		extra: common::object(extra),
	}
}

// TS: oauth-auth.test.ts
#[tokio::test]
async fn resolves_stored_oauth_credentials_via_the_flow() {
	let models = models_with(credential(serde_json::json!({}))).await;
	let result = models
		.get_auth(AuthTarget::Provider("copilot"), AuthResolutionOverrides::default())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(
		(result.auth.api_key.as_deref(), result.source.as_deref()),
		(Some("token"), Some("OAuth"))
	);
}

// TS: oauth-auth.test.ts
#[tokio::test]
async fn resolves_stored_oauth_credentials_including_per_credential_base_url() {
	let models = models_with(credential(serde_json::json!({ "enterpriseUrl": "company.ghe.com" }))).await;
	let result = models
		.get_auth(AuthTarget::Provider("copilot"), AuthResolutionOverrides::default())
		.await
		.unwrap()
		.unwrap();
	assert_eq!(result.auth.base_url.as_deref(), Some("https://api.company.ghe.com"));
}

// Rust-only.
#[test]
fn oauth_credentials_keep_provider_fields_in_their_json() {
	let text = r#"{"type":"oauth","refresh":"r","access":"a","expires":1,"enterpriseUrl":"x"}"#;
	let credential: Credential = common::read(text);
	assert_eq!(pi_json::to_js_string(&common::to_json(&credential)), text);
}
