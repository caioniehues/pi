//! Auth resolution shared by every operation of a `Models` (ports `packages/ai/src/auth/resolve.ts`): a stored
//! credential owns the provider, ambient sources count only when nothing is stored, and expiring OAuth tokens are
//! refreshed once under the credential store's lock.

use crate::auth::types::{
	ApiKeyAuth, ApiKeyCredential, ApiKeyInput, AuthContext, AuthResult, Credential, CredentialStore,
	CredentialStoreError, CredentialUpdate, OAuthAuth, OAuthCredential, ProviderAuth,
};
use crate::cancel::race_with_cancel;
use crate::diagnostics::now_ms;
use crate::error::{ModelsError, ModelsErrorCode};
use crate::provider_env::ProviderEnv;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Request overrides of auth resolution (`AuthResolutionOverrides`).
#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
	/// An explicit API key, resolved as a credential.
	pub api_key: Option<String>,
	/// Environment values over the auth context's and the stored credential's.
	pub env: Option<ProviderEnv>,
	/// Required remaining OAuth validity; at least five minutes.
	pub min_oauth_validity_ms: Option<u64>,
	/// Cancels the resolution.
	pub cancel: Option<CancellationToken>,
}

/// Resolves a provider's auth (`resolveProviderAuth`); `None` when the provider is not configured.
pub async fn resolve_provider_auth(
	provider_id: &str,
	auth: &ProviderAuth,
	credentials: &Arc<dyn CredentialStore>,
	auth_context: &Arc<dyn AuthContext>,
	overrides: &AuthResolutionOverrides,
) -> Result<Option<AuthResult>, ModelsError> {
	let cancel = overrides.cancel.clone().unwrap_or_default();
	race_with_cancel(
		resolve(provider_id, auth, credentials, auth_context, overrides, &cancel),
		&cancel,
	)
	.await
}

async fn resolve(
	provider_id: &str,
	auth: &ProviderAuth,
	credentials: &Arc<dyn CredentialStore>,
	auth_context: &Arc<dyn AuthContext>,
	overrides: &AuthResolutionOverrides,
	cancel: &CancellationToken,
) -> Result<Option<AuthResult>, ModelsError> {
	let overlay = overrides.env.clone().map(|env| OverlayAuthContext {
		base: Arc::clone(auth_context),
		env,
	});
	let context: &dyn AuthContext = match &overlay {
		Some(overlay) => overlay,
		None => auth_context.as_ref(),
	};
	if let (Some(key), Some(api_key)) = (&overrides.api_key, &auth.api_key) {
		let credential = ApiKeyCredential {
			key: Some(key.clone()),
			env: overrides.env.clone(),
		};
		return resolve_api_key(context, api_key.as_ref(), provider_id, Some(&credential), cancel).await;
	}
	match read_credential(credentials.as_ref(), provider_id, cancel).await? {
		Some(Credential::OAuth(stored)) => match &auth.oauth {
			Some(oauth) => {
				resolve_stored_oauth(
					credentials,
					provider_id,
					oauth,
					stored,
					cancel,
					overrides.min_oauth_validity_ms,
				)
				.await
			}
			None => Ok(None),
		},
		Some(Credential::ApiKey(stored)) => match &auth.api_key {
			Some(api_key) => {
				let credential = with_env_overrides(stored, overrides.env.as_ref());
				resolve_api_key(context, api_key.as_ref(), provider_id, Some(&credential), cancel).await
			}
			None => Ok(None),
		},
		// Ambient: env vars, AWS profiles, ADC files.
		None => match &auth.api_key {
			Some(api_key) => resolve_api_key(context, api_key.as_ref(), provider_id, None, cancel).await,
			None => Ok(None),
		},
	}
}

fn with_env_overrides(mut stored: ApiKeyCredential, env: Option<&ProviderEnv>) -> ApiKeyCredential {
	if let Some(env) = env {
		let merged = stored.env.get_or_insert_with(ProviderEnv::new);
		for (name, value) in env.iter() {
			merged.insert(name, value.clone());
		}
	}
	stored
}

/// The auth context with request env values over it (`overlayEnvAuthContext`).
struct OverlayAuthContext {
	base: Arc<dyn AuthContext>,
	env: ProviderEnv,
}

#[async_trait]
impl AuthContext for OverlayAuthContext {
	async fn env(&self, name: &str) -> Option<String> {
		match self.env.get(name).filter(|value| !value.is_empty()) {
			Some(value) => Some(value.clone()),
			None => self.base.env(name).await,
		}
	}

	async fn file_exists(&self, path: &str) -> bool {
		self.base.file_exists(path).await
	}
}

const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: u64 = 5 * 60 * 1000;
const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: u64 = 15_000;

/// Whether an OAuth credential needs a refresh, checked again under the lock.
pub type NeedsRefresh = Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>;

/// Refreshes a stored OAuth credential under the store lock and persists it before the lock is released
/// (`refreshStoredOAuthCredential`); `None` when the provider no longer has an OAuth credential.
///
/// `cancel` cancels only the wait for the lock. Once the refresh starts, the provider may already have rotated the
/// refresh token, so the refresh and its persistence run on a task of their own, bounded only by a timeout, and
/// finish even when the caller stops waiting.
pub async fn refresh_stored_oauth_credential(
	credentials: &Arc<dyn CredentialStore>,
	provider_id: &str,
	oauth: &Arc<dyn OAuthAuth>,
	needs_refresh: NeedsRefresh,
	cancel: &CancellationToken,
) -> Result<Option<OAuthCredential>, ModelsError> {
	let oauth = Arc::clone(oauth);
	let id = provider_id.to_owned();
	let update: CredentialUpdate = Box::new(move |current| {
		Box::pin(async move {
			let Some(Credential::OAuth(current)) = current else {
				return Ok(None);
			}; // logged out meanwhile
			if !needs_refresh(&current) {
				return Ok(None); // another process or request refreshed
			}
			let timeout = CancellationToken::new();
			let refresh = oauth.refresh(&current, &timeout);
			let limit = Duration::from_millis(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS);
			let refreshed = tokio::time::timeout(limit, refresh).await.unwrap_or_else(|_| {
				timeout.cancel();
				Err(ModelsError::new(
					ModelsErrorCode::Aborted,
					"The operation was aborted due to timeout",
				))
			});
			refreshed
				.map(|credential| Some(Credential::OAuth(credential)))
				.map_err(|error| {
					ModelsError::with_cause(ModelsErrorCode::Oauth, format!("OAuth refresh failed for {id}"), error)
				})
		})
	});
	let post = modify_once_started(credentials, provider_id, update, cancel).await?;
	Ok(match post {
		Some(Credential::OAuth(credential)) => Some(credential),
		_ => None,
	})
}

/// Runs `modify` on a task of its own: `cancel` cancels the wait for the lock only; once `update` starts, the
/// write completes even if the caller stops waiting. Errors are mapped as TS maps them.
pub async fn modify_once_started(
	credentials: &Arc<dyn CredentialStore>,
	provider_id: &str,
	update: CredentialUpdate,
	cancel: &CancellationToken,
) -> Result<Option<Credential>, ModelsError> {
	let started = Arc::new(AtomicBool::new(false));
	let lock_wait = CancellationToken::new();
	let relay_done = CancellationToken::new();
	{
		let (started, lock_wait, relay_done, cancel) = (
			Arc::clone(&started),
			lock_wait.clone(),
			relay_done.clone(),
			cancel.clone(),
		);
		tokio::spawn(async move {
			tokio::select! {
				() = cancel.cancelled() => if !started.load(Ordering::SeqCst) { lock_wait.cancel() },
				() = relay_done.cancelled() => {}
			}
		});
	}
	let marked = {
		let started = Arc::clone(&started);
		let cancel = cancel.clone();
		Box::new(move |current| {
			started.store(true, Ordering::SeqCst);
			if cancel.is_cancelled() {
				return Box::pin(async { Err(ModelsError::aborted()) }) as futures_util::future::BoxFuture<'static, _>;
			}
			update(current)
		}) as CredentialUpdate
	};
	let task = {
		let (credentials, provider_id, lock_wait) =
			(Arc::clone(credentials), provider_id.to_owned(), lock_wait.clone());
		tokio::spawn(async move { credentials.modify(&provider_id, marked, &lock_wait).await })
	};
	let result = task.await;
	relay_done.cancel();
	match result {
		Ok(Ok(credential)) => Ok(credential),
		Ok(Err(CredentialStoreError::Callback(error))) => Err(error),
		Ok(Err(CredentialStoreError::Aborted)) => Err(ModelsError::aborted()),
		Ok(Err(CredentialStoreError::Failed(error))) => {
			if cancel.is_cancelled() {
				return Err(ModelsError::aborted());
			}
			Err(ModelsError::with_cause(
				ModelsErrorCode::Auth,
				format!("Credential store modify failed for {provider_id}"),
				error,
			))
		}
		Err(join) => Err(ModelsError::new(
			ModelsErrorCode::Auth,
			format!("Credential store modify failed for {provider_id}: {join}"),
		)),
	}
}

/// Resolves stored OAuth with double-checked locking: a token expiring within the minimum validity is refreshed.
async fn resolve_stored_oauth(
	credentials: &Arc<dyn CredentialStore>,
	provider_id: &str,
	oauth: &Arc<dyn OAuthAuth>,
	stored: OAuthCredential,
	cancel: &CancellationToken,
	min_validity_ms: Option<u64>,
) -> Result<Option<AuthResult>, ModelsError> {
	let minimum = DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_validity_ms.unwrap_or(0));
	let expires_soon: NeedsRefresh =
		Arc::new(move |credential: &OAuthCredential| now_ms() + minimum >= credential.expires);
	let mut credential = stored;
	if expires_soon(&credential) {
		let Some(post) =
			refresh_stored_oauth_credential(credentials, provider_id, oauth, Arc::clone(&expires_soon), cancel).await?
		else {
			return Ok(None); // logged out meanwhile
		};
		credential = post;
		// The five-minute window triggers a refresh; only explicit callers require the minimum afterwards.
		if min_validity_ms.is_some() && expires_soon(&credential) {
			return Err(ModelsError::new(
				ModelsErrorCode::Oauth,
				format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
			));
		}
	}
	match oauth.to_auth(&credential).await {
		Ok(auth) => Ok(Some(AuthResult {
			auth,
			env: None,
			source: Some("OAuth".into()),
		})),
		Err(error) => Err(wrap(
			ModelsErrorCode::Oauth,
			format!("OAuth auth derivation failed for {provider_id}"),
			error,
		)),
	}
}

async fn resolve_api_key(
	context: &dyn AuthContext,
	api_key: &dyn ApiKeyAuth,
	provider_id: &str,
	credential: Option<&ApiKeyCredential>,
	cancel: &CancellationToken,
) -> Result<Option<AuthResult>, ModelsError> {
	api_key
		.resolve(ApiKeyInput {
			ctx: context,
			credential,
			cancel,
		})
		.await
		.map_err(|error| {
			wrap(
				ModelsErrorCode::Auth,
				format!("API key auth failed for provider {provider_id}"),
				error,
			)
		})
}

/// Wraps an error with context; a cancellation stays a cancellation.
pub(crate) fn wrap(code: ModelsErrorCode, message: String, error: ModelsError) -> ModelsError {
	if error.is_aborted() {
		error
	} else {
		ModelsError::with_cause(code, message, error)
	}
}

/// Reads the stored credential, wrapping store failures with code `auth`.
pub(crate) async fn read_credential(
	credentials: &dyn CredentialStore,
	provider_id: &str,
	cancel: &CancellationToken,
) -> Result<Option<Credential>, ModelsError> {
	match credentials.read(provider_id, cancel).await {
		Ok(credential) => Ok(credential),
		Err(CredentialStoreError::Aborted) => Err(ModelsError::aborted()),
		Err(CredentialStoreError::Callback(error)) => Err(error),
		Err(CredentialStoreError::Failed(error)) => Err(ModelsError::with_cause(
			ModelsErrorCode::Auth,
			format!("Credential store read failed for {provider_id}"),
			error,
		)),
	}
}
