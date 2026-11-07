//! Credential and auth types (ports `packages/ai/src/auth/types.ts`). TS's optional interface methods (`login`,
//! `check`) are trait methods whose defaults say the provider lacks them.

use crate::error::{ModelsError, ModelsErrorCode};
use crate::headers::ProviderHeaders;
use crate::provider_env::ProviderEnv;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use pi_json::JsonObject;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Request auth of one model request (`ModelAuth`): anything else is provider config, not auth.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelAuth {
	/// The API key or token.
	pub api_key: Option<String>,
	/// Auth headers.
	pub headers: Option<ProviderHeaders>,
	/// A per-credential base URL (GitHub Copilot).
	pub base_url: Option<String>,
}

/// A stored api-key credential (`ApiKeyCredential`); `env` holds provider-scoped config such as account ids.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ApiKeyCredential {
	/// The key.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub key: Option<String>,
	/// Provider-scoped environment values.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub env: Option<ProviderEnv>,
}

/// A stored OAuth credential (`OAuthCredential`); provider-specific fields stay in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
	/// The refresh token.
	pub refresh: String,
	/// The access token.
	pub access: String,
	/// Epoch milliseconds when `access` expires.
	pub expires: u64,
	/// Provider fields (an enterprise domain, account ids), kept for the round trip.
	#[serde(flatten)]
	pub extra: JsonObject,
}

/// One credential per provider, tagged by `type` (`Credential`): the shape of `auth.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
	/// `type: "api_key"`.
	#[serde(rename = "api_key")]
	ApiKey(ApiKeyCredential),
	/// `type: "oauth"`.
	#[serde(rename = "oauth")]
	OAuth(OAuthCredential),
}

/// The kind of a credential or auth method (`AuthType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthType {
	/// `api_key`.
	#[serde(rename = "api_key")]
	ApiKey,
	/// `oauth`.
	#[serde(rename = "oauth")]
	OAuth,
}

impl Credential {
	/// The credential's type.
	pub fn auth_type(&self) -> AuthType {
		match self {
			Self::ApiKey(_) => AuthType::ApiKey,
			Self::OAuth(_) => AuthType::OAuth,
		}
	}
}

/// Non-secret credential metadata (`CredentialInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialInfo {
	/// The provider id.
	pub provider_id: String,
	/// The credential's type.
	pub auth_type: AuthType,
}

/// Why a credential store operation failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CredentialStoreError {
	/// The storage failed; `Models` wraps it with code `auth`.
	#[error("{0}")]
	Failed(Arc<dyn Error + Send + Sync>),
	/// The update callback of `modify` failed; its error propagates unchanged.
	#[error("{0}")]
	Callback(ModelsError),
	/// The cancellation token fired.
	#[error("The operation was aborted")]
	Aborted,
}

/// The update of [`CredentialStore::modify`]: sees the current credential, returns the new one or `None` to leave
/// it unchanged.
pub type CredentialUpdate =
	Box<dyn FnOnce(Option<Credential>) -> BoxFuture<'static, Result<Option<Credential>, ModelsError>> + Send>;

/// App-owned credential storage keyed by provider id (`CredentialStore`). `modify` is the only write path, a
/// serialized read-modify-write per provider id; the cancellation token cancels only the wait for the lock.
#[async_trait]
pub trait CredentialStore: Send + Sync {
	/// The stored credential, possibly expired; `None` when missing.
	async fn read(
		&self,
		provider_id: &str,
		cancel: &CancellationToken,
	) -> Result<Option<Credential>, CredentialStoreError>;

	/// Stored credential metadata, without secrets.
	async fn list(&self, cancel: &CancellationToken) -> Result<Vec<CredentialInfo>, CredentialStoreError>;

	/// Serialized write: runs `update` on the current credential under the provider's lock; returns the credential
	/// after the write.
	async fn modify(
		&self,
		provider_id: &str,
		update: CredentialUpdate,
		cancel: &CancellationToken,
	) -> Result<Option<Credential>, CredentialStoreError>;

	/// Removes a credential (logout), serialized against `modify`.
	async fn delete(&self, provider_id: &str, cancel: &CancellationToken) -> Result<(), CredentialStoreError>;
}

/// Environment access for auth resolution; injectable for tests (`AuthContext`).
#[async_trait]
pub trait AuthContext: Send + Sync {
	/// The variable's value; `None` when unset or blank.
	async fn env(&self, name: &str) -> Option<String>;

	/// Whether a file exists; a leading `~` is the home directory.
	async fn file_exists(&self, path: &str) -> bool;
}

/// Resolved auth of a provider (`AuthResult`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AuthResult {
	/// The request auth.
	pub auth: ModelAuth,
	/// Provider-scoped environment values from credentials and ambient context.
	pub env: Option<ProviderEnv>,
	/// A label for status UI: `ANTHROPIC_API_KEY`, `OAuth`, `~/.aws/credentials`.
	pub source: Option<String>,
}

/// Whether a provider is configured, without refreshing (`AuthCheck`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheck {
	/// The label for status UI.
	pub source: Option<String>,
	/// The auth method that is configured.
	pub auth_type: AuthType,
}

/// One option of a `select` prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPromptOption {
	/// The id `prompt` returns.
	pub id: String,
	/// The label.
	pub label: String,
	/// A description.
	pub description: Option<String>,
}

/// A prompt shown during login (`AuthPrompt`); `cancel` lets the flow withdraw it.
#[derive(Debug, Clone)]
pub enum AuthPrompt {
	/// Free text.
	Text {
		/// The question.
		message: String,
		/// A placeholder.
		placeholder: Option<String>,
		/// Withdraws the prompt.
		cancel: Option<CancellationToken>,
	},
	/// A secret.
	Secret {
		/// The question.
		message: String,
		/// A placeholder.
		placeholder: Option<String>,
		/// Withdraws the prompt.
		cancel: Option<CancellationToken>,
	},
	/// One of options.
	Select {
		/// The question.
		message: String,
		/// The options.
		options: Vec<AuthPromptOption>,
		/// Withdraws the prompt.
		cancel: Option<CancellationToken>,
	},
	/// A code pasted by hand, raced against a callback server.
	ManualCode {
		/// The question.
		message: String,
		/// A placeholder.
		placeholder: Option<String>,
		/// Withdraws the prompt.
		cancel: Option<CancellationToken>,
	},
}

/// A link in an info event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthInfoLink {
	/// The URL.
	pub url: String,
	/// The label.
	pub label: Option<String>,
}

/// A login progress event (`AuthEvent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
	/// Information.
	Info {
		/// The message.
		message: String,
		/// Links.
		links: Vec<AuthInfoLink>,
	},
	/// A URL to open.
	AuthUrl {
		/// The URL.
		url: String,
		/// Instructions.
		instructions: Option<String>,
	},
	/// A device code to enter.
	DeviceCode {
		/// The code.
		user_code: String,
		/// Where to enter it.
		verification_uri: String,
		/// Offer to open the browser (default true).
		open_browser_on_confirm: Option<bool>,
		/// Offer to copy the code (default true).
		copy_code_on_confirm: Option<bool>,
		/// Polling interval.
		interval_seconds: Option<u64>,
		/// Expiry.
		expires_in_seconds: Option<u64>,
	},
	/// Progress.
	Progress {
		/// The message.
		message: String,
	},
}

/// Login callbacks for api-key and OAuth flows (`AuthInteraction`).
#[async_trait]
pub trait AuthInteraction: Send + Sync {
	/// Shows a prompt; the entered text or the selected option id. Fails on cancel.
	async fn prompt(&self, prompt: AuthPrompt) -> Result<String, ModelsError>;

	/// Reports progress.
	fn notify(&self, event: AuthEvent);
}

/// App context of `Models::login` (`LoginOptions`).
#[derive(Clone, Default)]
pub struct LoginOptions {
	/// The stable id of this app installation.
	pub get_device_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
	/// The name the app introduces itself with during login.
	pub agent_name: Option<String>,
}

/// What an api-key resolution sees.
pub struct ApiKeyInput<'a> {
	/// The environment.
	pub ctx: &'a dyn AuthContext,
	/// The stored (or request) credential.
	pub credential: Option<&'a ApiKeyCredential>,
	/// Cancels the resolution.
	pub cancel: &'a CancellationToken,
}

/// Api-key auth: a stored key plus ambient sources (`ApiKeyAuth`).
#[async_trait]
pub trait ApiKeyAuth: Send + Sync {
	/// The display name, such as `Anthropic API key`.
	fn name(&self) -> &str;

	/// Whether `login` is supported; ambient-only providers have none.
	fn supports_login(&self) -> bool {
		false
	}

	/// Interactive setup.
	async fn login(
		&self,
		_interaction: &dyn AuthInteraction,
		_cancel: &CancellationToken,
	) -> Result<ApiKeyCredential, ModelsError> {
		Err(ModelsError::new(
			ModelsErrorCode::Auth,
			format!("{} does not support login", self.name()),
		))
	}

	/// A side-effect-free availability check; `None` means `Models` checks by resolving.
	async fn check(&self, _input: ApiKeyInput<'_>) -> Option<Result<Option<AuthCheck>, ModelsError>> {
		None
	}

	/// Resolves auth from the credential and ambient sources; `None` when not configured.
	async fn resolve(&self, input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError>;
}

/// OAuth auth (`OAuthAuth`): `refresh` produces a credential, `to_auth` derives request auth from the stored one.
#[async_trait]
pub trait OAuthAuth: Send + Sync {
	/// The display name.
	fn name(&self) -> &str;

	/// Whether access is backed by a provider subscription.
	fn is_subscription(&self) -> bool {
		false
	}

	/// The selector label of the login option.
	fn login_label(&self) -> Option<&str> {
		None
	}

	/// The login flow.
	async fn login(
		&self,
		interaction: &dyn AuthInteraction,
		options: &LoginOptions,
		cancel: &CancellationToken,
	) -> Result<OAuthCredential, ModelsError>;

	/// Exchanges the refresh token; `Models` runs it under the store lock.
	async fn refresh(
		&self,
		credential: &OAuthCredential,
		cancel: &CancellationToken,
	) -> Result<OAuthCredential, ModelsError>;

	/// Request auth of a valid credential, without side effects.
	async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError>;
}

/// A provider's auth methods; at least one is present (`ProviderAuth`).
#[derive(Clone, Default)]
pub struct ProviderAuth {
	/// Api-key auth.
	pub api_key: Option<Arc<dyn ApiKeyAuth>>,
	/// OAuth.
	pub oauth: Option<Arc<dyn OAuthAuth>>,
}
