//! The registry of providers (ports `Models` and `createModels` of `packages/ai/src/models.ts`, and
//! `withClientIdentity` of `utils/identity.ts`): model lookup, dynamic refresh, auth checks, login, and requests that
//! resolve auth and delegate to the provider owning the model.
//!
//! `Models` is a cheap handle: clones and identity wrappers share the providers, stores and refresh state. Request
//! methods return their stream at once and must be called inside a tokio runtime.

use crate::auth::context::DefaultAuthContext;
use crate::auth::credential_store::InMemoryCredentialStore;
use crate::auth::resolve::{
	AuthResolutionOverrides, modify_once_started, read_credential, refresh_stored_oauth_credential,
	resolve_provider_auth, wrap,
};
use crate::auth::types::{
	ApiKeyCredential, ApiKeyInput, AuthCheck, AuthContext, AuthInteraction, AuthResult, AuthType, Credential,
	CredentialStore, CredentialStoreError, LoginOptions,
};
use crate::cancel::race_with_cancel;
use crate::diagnostics::now_ms;
use crate::error::{ModelsError, ModelsErrorCode};
use crate::event_stream::AssistantMessageEventStream;
use crate::headers::{ProviderHeaders, merge_headers};
use crate::identity::{ClientIdentity, ClientIdentityTarget, get_opencode_session_headers, merge_client_identity};
use crate::lazy_stream::lazy_stream;
use crate::model_operations::{
	AnyModel, Failure, ModelType, assert_classifier_input_supported, classifier_error_result, image_error_result,
};
use crate::models_store::{InMemoryModelsStore, ModelsStore, ModelsStoreEntry};
use crate::options::{
	ClassifierOptions, DeferredFetchOptions, ImagesOptions, ProviderRequestOptions, RequestOptions,
	SimpleStreamOptions, StreamOptions,
};
use crate::provider::{ModelsPublication, Persist, Provider, RefreshModelsContext};
use crate::telemetry::{RequestKind, observe_model_request};
use crate::transcript::{Context, normalize_context};
use pi_types::{
	AssistantImages, AssistantMessage, ClassifierContext, ClassifierModel, ClassifierResult, DeferredHandle,
	ImageModel, ImagesContext, Model, Record,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use tokio_util::sync::CancellationToken;

/// What a `Models` is built with (`CreateModelsOptions`).
#[derive(Clone, Default)]
pub struct CreateModelsOptions {
	/// The credential store; in memory by default.
	pub credentials: Option<Arc<dyn CredentialStore>>,
	/// The models store; in memory by default.
	pub models_store: Option<Arc<dyn ModelsStore>>,
	/// The auth context; the process environment by default.
	pub auth_context: Option<Arc<dyn AuthContext>>,
	/// The default identity of every request; a request's identity overrides it field by field.
	pub identity: Option<ClientIdentity>,
}

/// Options of [`Models::refresh`] (`ModelsRefreshOptions`).
#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshOptions {
	/// Whether network access is allowed (default true).
	pub allow_network: Option<bool>,
	/// Restrict refresh to these provider ids.
	pub providers: Option<Vec<String>>,
	/// Bypass freshness checks.
	pub force: Option<bool>,
	/// Cancels the refresh.
	pub cancel: Option<CancellationToken>,
}

/// The outcome of [`Models::refresh`] (`ModelsRefreshResult`).
#[derive(Debug, Clone, Default)]
pub struct ModelsRefreshResult {
	/// Whether the caller cancelled it.
	pub aborted: bool,
	/// Failures by provider id, in provider order.
	pub errors: Vec<(String, ModelsError)>,
}

/// What `get_auth` resolves: a provider by id, or a model (whose headers are added).
#[derive(Debug, Clone, Copy)]
pub enum AuthTarget<'a> {
	/// A provider id.
	Provider(&'a str),
	/// A model.
	Model(&'a AnyModel),
}

/// The registry (`Models`, `MutableModels`); see the module comment.
#[derive(Clone)]
pub struct Models {
	inner: Arc<Inner>,
	/// The identity of a `with_client_identity` wrapper, over the one the registry was created with.
	identity: Option<ClientIdentity>,
}

struct Inner {
	providers: RwLock<Vec<Arc<dyn Provider>>>,
	credentials: Arc<dyn CredentialStore>,
	models_store: Arc<dyn ModelsStore>,
	auth_context: Arc<dyn AuthContext>,
	refresh: Mutex<RefreshState>,
}

#[derive(Default)]
struct RefreshState {
	generations: HashMap<String, u64>,
	controllers: HashMap<String, CancellationToken>,
	publication_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl Default for Models {
	fn default() -> Self {
		Self::new(CreateModelsOptions::default())
	}
}

impl Models {
	/// An empty registry (`createModels`).
	pub fn new(options: CreateModelsOptions) -> Self {
		Self {
			inner: Arc::new(Inner {
				providers: RwLock::new(Vec::new()),
				credentials: options
					.credentials
					.unwrap_or_else(|| Arc::new(InMemoryCredentialStore::new())),
				models_store: options
					.models_store
					.unwrap_or_else(|| Arc::new(InMemoryModelsStore::new())),
				auth_context: options.auth_context.unwrap_or_else(|| Arc::new(DefaultAuthContext)),
				refresh: Mutex::new(RefreshState::default()),
			}),
			identity: options.identity,
		}
	}

	/// This registry with `identity` as its requests' default identity (`withClientIdentity`): a request's identity
	/// overrides it, and it overrides the registry's. Providers and state stay shared.
	#[must_use]
	pub fn with_client_identity(&self, identity: &ClientIdentity) -> Self {
		Self {
			inner: Arc::clone(&self.inner),
			identity: merge_client_identity(&[self.identity.as_ref(), Some(identity)]),
		}
	}

	fn providers(&self) -> Vec<Arc<dyn Provider>> {
		self.inner
			.providers
			.read()
			.unwrap_or_else(PoisonError::into_inner)
			.clone()
	}

	/// Adds or replaces the provider with the same id (`setProvider`); a replaced one keeps its position.
	pub fn set_provider(&self, provider: Arc<dyn Provider>) {
		self.supersede_refresh(provider.id());
		let mut providers = self.inner.providers.write().unwrap_or_else(PoisonError::into_inner);
		match providers.iter_mut().find(|existing| existing.id() == provider.id()) {
			Some(slot) => *slot = provider,
			None => providers.push(provider),
		}
	}

	/// Removes a provider (`deleteProvider`).
	pub fn delete_provider(&self, id: &str) {
		self.supersede_refresh(id);
		self.inner
			.providers
			.write()
			.unwrap_or_else(PoisonError::into_inner)
			.retain(|provider| provider.id() != id);
	}

	/// Removes every provider (`clearProviders`).
	pub fn clear_providers(&self) {
		let mut ids: Vec<String> = self
			.providers()
			.iter()
			.map(|provider| provider.id().to_owned())
			.collect();
		ids.extend(self.lock_refresh().controllers.keys().cloned());
		for id in ids {
			self.supersede_refresh(&id);
		}
		self.inner
			.providers
			.write()
			.unwrap_or_else(PoisonError::into_inner)
			.clear();
	}

	/// The providers in registration order (`getProviders`).
	pub fn get_providers(&self) -> Vec<Arc<dyn Provider>> {
		self.providers()
	}

	/// The provider with `id` (`getProvider`).
	pub fn get_provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
		self.providers().into_iter().find(|provider| provider.id() == id)
	}

	fn selected(&self, provider: Option<&str>) -> Vec<Arc<dyn Provider>> {
		match provider {
			Some(id) => self.get_provider(id).into_iter().collect(),
			None => self.providers(),
		}
	}

	/// The last-known chat models of one provider or all (`getModels`).
	pub fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
		self.selected(provider)
			.iter()
			.flat_map(|provider| provider.get_models())
			.collect()
	}

	/// The last-known models of every type (`getAllModels`).
	pub fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
		self.selected(provider)
			.iter()
			.flat_map(|provider| provider.get_all_models())
			.collect()
	}

	/// The last-known models of one type (`getModelsOfType`).
	pub fn get_models_of_type(&self, model_type: ModelType, provider: Option<&str>) -> Vec<AnyModel> {
		self.get_all_models(provider)
			.into_iter()
			.filter(|model| model.is_type(model_type))
			.collect()
	}

	/// A chat model by provider and id (`getModel`).
	pub fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
		self.get_models(Some(provider)).into_iter().find(|model| model.id == id)
	}

	/// A model of one type by provider and id (`getModelOfType`).
	pub fn get_model_of_type(&self, model_type: ModelType, provider: &str, id: &str) -> Option<AnyModel> {
		self.get_models_of_type(model_type, Some(provider))
			.into_iter()
			.find(|model| model.id() == id)
	}

	fn lock_refresh(&self) -> std::sync::MutexGuard<'_, RefreshState> {
		self.inner.refresh.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn supersede_refresh(&self, provider_id: &str) -> u64 {
		let mut state = self.lock_refresh();
		let generation = state.generations.get(provider_id).copied().unwrap_or(0) + 1;
		state.generations.insert(provider_id.to_owned(), generation);
		if let Some(previous) = state.controllers.remove(provider_id) {
			previous.cancel();
		}
		generation
	}

	fn begin_refresh(&self, provider_id: &str, caller: &CancellationToken) -> (u64, CancellationToken) {
		let generation = self.supersede_refresh(provider_id);
		// `AbortSignal.any([caller, controller])`: the controller fires with the caller or when superseded.
		let controller = caller.child_token();
		self.lock_refresh()
			.controllers
			.insert(provider_id.to_owned(), controller.clone());
		(generation, controller)
	}

	fn is_current(&self, provider_id: &str, generation: u64) -> bool {
		self.lock_refresh().generations.get(provider_id) == Some(&generation)
	}

	/// Refreshes the selected configured dynamic providers concurrently (`refresh`); failures and cancellation are
	/// reported, never raised.
	pub async fn refresh(&self, options: ModelsRefreshOptions) -> ModelsRefreshResult {
		let allow_network = options.allow_network.unwrap_or(true);
		let caller = options.cancel.clone().unwrap_or_default();
		if caller.is_cancelled() {
			return ModelsRefreshResult {
				aborted: true,
				errors: Vec::new(),
			};
		}
		let refreshable: Vec<Arc<dyn Provider>> = self
			.providers()
			.into_iter()
			.filter(|provider| provider.capabilities().refresh_models)
			.filter(|provider| {
				options
					.providers
					.as_ref()
					.is_none_or(|ids| ids.iter().any(|id| id == provider.id()))
			})
			.collect();
		// Errors are recorded as each provider finishes, so the ones before an abort are kept, as in TS.
		let errors: Mutex<Vec<(usize, (String, ModelsError))>> = Mutex::new(Vec::new());
		let runs = refreshable.iter().enumerate().map(|(index, provider)| {
			let errors = &errors;
			let caller = &caller;
			async move {
				if let Some(error) = self
					.refresh_provider(Arc::clone(provider), allow_network, options.force, caller)
					.await
				{
					errors
						.lock()
						.unwrap_or_else(PoisonError::into_inner)
						.push((index, error));
				}
			}
		});
		let _ = race_with_cancel(
			async {
				futures_util::future::join_all(runs).await;
				Ok(())
			},
			&caller,
		)
		.await;
		let mut errors = errors.into_inner().unwrap_or_else(PoisonError::into_inner);
		errors.sort_by_key(|(index, _)| *index);
		let errors = errors.into_iter().map(|(_, error)| error).collect();
		ModelsRefreshResult {
			aborted: caller.is_cancelled(),
			errors,
		}
	}

	async fn refresh_provider(
		&self,
		provider: Arc<dyn Provider>,
		allow_network: bool,
		force: Option<bool>,
		caller: &CancellationToken,
	) -> Option<(String, ModelsError)> {
		let id = provider.id().to_owned();
		let (generation, signal) = self.begin_refresh(&id, caller);
		let operation = self.refresh_operation(&provider, allow_network, force, generation, &signal);
		let result = race_with_cancel(operation, &signal).await;
		{
			// The controller is still this refresh's while no newer refresh superseded it.
			let mut state = self.lock_refresh();
			if state.generations.get(&id) == Some(&generation) {
				state.controllers.remove(&id);
			}
		}
		match result {
			Err(error) if !signal.is_cancelled() => Some((id, error)),
			_ => None,
		}
	}

	async fn refresh_operation(
		&self,
		provider: &Arc<dyn Provider>,
		allow_network: bool,
		force: Option<bool>,
		generation: u64,
		signal: &CancellationToken,
	) -> Result<(), ModelsError> {
		let stored = read_credential(self.inner.credentials.as_ref(), provider.id(), signal).await;
		let (stored, credential_error) = match stored {
			Ok(stored) => (stored, None),
			Err(error) => (None, Some(error)),
		};
		// Restore cached provider state before auth resolution or network access.
		self.refresh_phase(provider, stored.clone(), false, None, generation, signal)
			.await?;
		if let Some(error) = credential_error {
			return Err(error);
		}
		if !allow_network || signal.is_cancelled() {
			return Ok(());
		}
		let Some(credential) = self.resolve_refresh_credential(provider, stored, signal).await? else {
			return Ok(());
		};
		self.refresh_phase(provider, Some(credential), true, force, generation, signal)
			.await
	}

	async fn refresh_phase(
		&self,
		provider: &Arc<dyn Provider>,
		credential: Option<Credential>,
		allow_network: bool,
		force: Option<bool>,
		generation: u64,
		signal: &CancellationToken,
	) -> Result<(), ModelsError> {
		let stored = self.inner.models_store.read(provider.id(), signal).await?;
		let stored = stored.map(|entry| ModelsStoreEntry {
			models: entry
				.models
				.into_iter()
				.filter(|model| model.model_type().is_some())
				.collect(),
			..entry
		});
		let (models, id, cancel) = (self.clone(), provider.id().to_owned(), signal.clone());
		let publish = Arc::new(move |publication: ModelsPublication| {
			let (models, id, cancel) = (models.clone(), id.clone(), cancel.clone());
			Box::pin(async move { models.publish(&id, generation, &cancel, publication).await })
				as futures_util::future::BoxFuture<'static, _>
		});
		provider
			.refresh_models(RefreshModelsContext {
				credential,
				stored,
				publish,
				allow_network,
				force: if allow_network { force } else { None },
				cancel: signal.clone(),
				base_url: provider.base_url().map(str::to_owned),
			})
			.await
	}

	/// Generation-checked, per-provider serialized publication of a refresh result.
	async fn publish(
		&self,
		provider_id: &str,
		generation: u64,
		cancel: &CancellationToken,
		publication: ModelsPublication,
	) -> Result<bool, ModelsError> {
		let lock = Arc::clone(
			self.lock_refresh()
				.publication_locks
				.entry(provider_id.to_owned())
				.or_default(),
		);
		race_with_cancel(
			async {
				let _guard = lock.lock().await;
				if cancel.is_cancelled() || !self.is_current(provider_id, generation) {
					return Ok(false);
				}
				match publication.persist {
					Persist::Keep => {}
					Persist::Delete => self.inner.models_store.delete(provider_id, cancel).await?,
					Persist::Write(entry) => self.inner.models_store.write(provider_id, entry, cancel).await?,
				}
				if cancel.is_cancelled() || !self.is_current(provider_id, generation) {
					return Ok(false);
				}
				if let Some(update) = publication.update {
					update();
				}
				Ok(true)
			},
			cancel,
		)
		.await
	}

	async fn resolve_refresh_credential(
		&self,
		provider: &Arc<dyn Provider>,
		stored: Option<Credential>,
		signal: &CancellationToken,
	) -> Result<Option<Credential>, ModelsError> {
		if let Some(Credential::OAuth(stored)) = stored {
			let Some(oauth) = &provider.auth().oauth else {
				return Ok(None);
			};
			if now_ms() < stored.expires {
				return Ok(Some(Credential::OAuth(stored)));
			}
			if signal.is_cancelled() {
				return Ok(None);
			}
			// A started refresh survives cancellation, so a rotated refresh token is always persisted.
			let needs_refresh = Arc::new(|current: &crate::auth::types::OAuthCredential| now_ms() >= current.expires);
			let refreshed =
				refresh_stored_oauth_credential(&self.inner.credentials, provider.id(), oauth, needs_refresh, signal)
					.await?;
			return Ok(refreshed.map(Credential::OAuth));
		}
		let Some(api_key) = &provider.auth().api_key else {
			return Ok(None);
		};
		let credential = match stored {
			Some(Credential::ApiKey(credential)) => Some(credential),
			_ => None,
		};
		let result = api_key
			.resolve(ApiKeyInput {
				ctx: self.inner.auth_context.as_ref(),
				credential: credential.as_ref(),
				cancel: signal,
			})
			.await?;
		Ok(result.map(|result| {
			Credential::ApiKey(ApiKeyCredential {
				key: result.auth.api_key,
				env: result.env,
			})
		}))
	}

	async fn check_provider_auth(
		&self,
		provider: &Arc<dyn Provider>,
		credential: Option<&Credential>,
		cancel: &CancellationToken,
	) -> Result<Option<AuthCheck>, ModelsError> {
		if let Some(Credential::OAuth(_)) = credential {
			return Ok(provider.auth().oauth.as_ref().map(|_| AuthCheck {
				source: Some("OAuth".into()),
				auth_type: AuthType::OAuth,
			}));
		}
		let Some(api_key) = &provider.auth().api_key else {
			return Ok(None);
		};
		let api_credential = match credential {
			Some(Credential::ApiKey(credential)) => Some(credential),
			_ => None,
		};
		let input = ApiKeyInput {
			ctx: self.inner.auth_context.as_ref(),
			credential: api_credential,
			cancel,
		};
		if let Some(check) = api_key.check(input).await {
			return check.map_err(|error| {
				wrap(
					ModelsErrorCode::Auth,
					format!("API key auth check failed for provider {}", provider.id()),
					error,
				)
			});
		}
		let overrides = AuthResolutionOverrides {
			cancel: Some(cancel.clone()),
			..AuthResolutionOverrides::default()
		};
		let resolution = resolve_provider_auth(
			provider.id(),
			provider.auth(),
			&self.inner.credentials,
			&self.inner.auth_context,
			&overrides,
		)
		.await?;
		Ok(resolution.map(|resolution| AuthCheck {
			source: resolution.source,
			auth_type: AuthType::ApiKey,
		}))
	}

	/// Whether a provider has complete auth, without refreshing OAuth (`checkAuth`).
	pub async fn check_auth(
		&self,
		provider_id: &str,
		cancel: Option<CancellationToken>,
	) -> Result<Option<AuthCheck>, ModelsError> {
		let cancel = cancel.unwrap_or_default();
		race_with_cancel(
			async {
				let Some(provider) = self.get_provider(provider_id) else {
					return Ok(None);
				};
				let credential = read_credential(self.inner.credentials.as_ref(), provider_id, &cancel).await?;
				self.check_provider_auth(&provider, credential.as_ref(), &cancel).await
			},
			&cancel,
		)
		.await
	}

	async fn authenticated_providers(
		&self,
		provider_id: Option<&str>,
		cancel: &CancellationToken,
	) -> Result<Vec<(Arc<dyn Provider>, Option<Credential>)>, ModelsError> {
		if cancel.is_cancelled() {
			return Err(ModelsError::aborted());
		}
		let checks = self.selected(provider_id).into_iter().map(|provider| async move {
			let credential = read_credential(self.inner.credentials.as_ref(), provider.id(), cancel).await?;
			let check = self.check_provider_auth(&provider, credential.as_ref(), cancel).await?;
			Ok::<_, ModelsError>(check.map(|_| (provider, credential)))
		});
		let results = futures_util::future::join_all(checks).await;
		let mut authenticated = Vec::new();
		for result in results {
			authenticated.extend(result?);
		}
		Ok(authenticated)
	}

	/// Chat models of providers with complete auth, filtered by credential (`getAvailable`).
	pub async fn get_available(
		&self,
		provider_id: Option<&str>,
		cancel: Option<CancellationToken>,
	) -> Result<Vec<Model>, ModelsError> {
		let cancel = cancel.unwrap_or_default();
		race_with_cancel(
			async {
				let providers = self.authenticated_providers(provider_id, &cancel).await?;
				Ok(providers
					.into_iter()
					.flat_map(|(provider, credential)| {
						provider.filter_models(provider.get_models(), credential.as_ref())
					})
					.collect())
			},
			&cancel,
		)
		.await
	}

	/// Models of one type of providers with complete auth (`getAvailableOfType`).
	pub async fn get_available_of_type(
		&self,
		model_type: ModelType,
		provider_id: Option<&str>,
		cancel: Option<CancellationToken>,
	) -> Result<Vec<AnyModel>, ModelsError> {
		let all = self.get_all_available(provider_id, cancel).await?;
		Ok(all.into_iter().filter(|model| model.is_type(model_type)).collect())
	}

	/// Models of every type of providers with complete auth (`getAllAvailable`).
	pub async fn get_all_available(
		&self,
		provider_id: Option<&str>,
		cancel: Option<CancellationToken>,
	) -> Result<Vec<AnyModel>, ModelsError> {
		let cancel = cancel.unwrap_or_default();
		race_with_cancel(
			async {
				let providers = self.authenticated_providers(provider_id, &cancel).await?;
				Ok(providers
					.into_iter()
					.flat_map(|(provider, credential)| {
						provider.filter_all_models(provider.get_all_models(), credential.as_ref())
					})
					.collect())
			},
			&cancel,
		)
		.await
	}

	/// Resolves a provider's auth, plus the model's headers for a model (`getAuth`); `None` when unknown or not
	/// configured.
	pub async fn get_auth(
		&self,
		target: AuthTarget<'_>,
		overrides: AuthResolutionOverrides,
	) -> Result<Option<AuthResult>, ModelsError> {
		let (provider_id, model_headers) = match target {
			AuthTarget::Provider(id) => (id, None),
			AuthTarget::Model(model) => (model.provider(), model_headers(model)),
		};
		let Some(provider) = self.get_provider(provider_id) else {
			return Ok(None);
		};
		let result = resolve_provider_auth(
			provider.id(),
			provider.auth(),
			&self.inner.credentials,
			&self.inner.auth_context,
			&overrides,
		)
		.await?;
		Ok(result.map(|mut result| {
			if let Some(headers) = model_headers {
				result.auth.headers = merge_headers(result.auth.headers.as_ref(), Some(&headers));
			}
			result
		}))
	}

	/// Runs a provider's login flow and stores the credential (`login`).
	pub async fn login(
		&self,
		provider_id: &str,
		auth_type: AuthType,
		interaction: &dyn AuthInteraction,
		options: &LoginOptions,
		cancel: Option<CancellationToken>,
	) -> Result<Credential, ModelsError> {
		let cancel = cancel.unwrap_or_default();
		if cancel.is_cancelled() {
			return Err(ModelsError::aborted());
		}
		let Some(provider) = self.get_provider(provider_id) else {
			return Err(ModelsError::new(
				ModelsErrorCode::Provider,
				format!("Unknown provider: {provider_id}"),
			));
		};
		let unsupported = || {
			let kind = match auth_type {
				AuthType::ApiKey => "api_key",
				AuthType::OAuth => "oauth",
			};
			ModelsError::new(
				ModelsErrorCode::Auth,
				format!("{} does not support {kind} login", provider.name()),
			)
		};
		let credential = match auth_type {
			AuthType::OAuth => {
				let oauth = provider.auth().oauth.clone().ok_or_else(unsupported)?;
				Credential::OAuth(race_with_cancel(oauth.login(interaction, options, &cancel), &cancel).await?)
			}
			AuthType::ApiKey => {
				let api_key = provider
					.auth()
					.api_key
					.clone()
					.filter(|api_key| api_key.supports_login())
					.ok_or_else(unsupported)?;
				Credential::ApiKey(race_with_cancel(api_key.login(interaction, &cancel), &cancel).await?)
			}
		};
		let stored = credential.clone();
		let update =
			Box::new(move |_| Box::pin(async move { Ok(Some(stored)) }) as futures_util::future::BoxFuture<'static, _>);
		modify_once_started(&self.inner.credentials, provider_id, update, &cancel).await?;
		Ok(credential)
	}

	/// Removes a provider's stored credential (`logout`).
	pub async fn logout(&self, provider_id: &str, cancel: Option<CancellationToken>) -> Result<(), ModelsError> {
		let cancel = cancel.unwrap_or_default();
		if cancel.is_cancelled() {
			return Err(ModelsError::aborted());
		}
		match self.inner.credentials.delete(provider_id, &cancel).await {
			Ok(()) => Ok(()),
			Err(_) if cancel.is_cancelled() => Err(ModelsError::aborted()),
			Err(CredentialStoreError::Aborted) => Err(ModelsError::aborted()),
			Err(CredentialStoreError::Callback(error)) => Err(error),
			Err(CredentialStoreError::Failed(error)) => Err(ModelsError::with_cause(
				ModelsErrorCode::Auth,
				format!("Credential store delete failed for {provider_id}"),
				error,
			)),
		}
	}

	fn require_provider(&self, provider_id: &str) -> Result<Arc<dyn Provider>, ModelsError> {
		self.get_provider(provider_id)
			.ok_or_else(|| ModelsError::new(ModelsErrorCode::Provider, format!("Unknown provider: {provider_id}")))
	}

	/// Resolves auth and merges it into the options; explicit options win per field, the header transform runs last
	/// (`applyAuth`). Returns the request's base URL override and the options.
	async fn apply_auth<O: RequestOptions>(
		&self,
		model: &AnyModel,
		base_url: &str,
		mut options: O,
	) -> Result<(Option<String>, O), ModelsError> {
		self.require_provider(model.provider())?;
		let request = options.request();
		let overrides = AuthResolutionOverrides {
			api_key: request.api_key.clone(),
			env: request.env.clone(),
			min_oauth_validity_ms: None,
			cancel: request.cancel.clone(),
		};
		let Some(resolution) = self.get_auth(AuthTarget::Model(model), overrides).await? else {
			return Err(ModelsError::new(
				ModelsErrorCode::Auth,
				format!("Provider is not configured: {}", model.provider()),
			));
		};
		let auth = resolution.auth;
		let request_base_url = auth.base_url.clone().filter(|url| !url.is_empty());
		let target = ClientIdentityTarget {
			provider: model.provider(),
			base_url: request_base_url.as_deref().unwrap_or(base_url),
		};
		let session = get_opencode_session_headers(target, options.session_id())
			.as_ref()
			.map(as_provider_headers);
		let identity = merge_client_identity(&[self.identity.as_ref(), options.request().identity.as_ref()]);
		let request = options.request_mut();
		if request.api_key.is_none() {
			request.api_key = auth.api_key;
		}
		let mut headers = merge_headers(
			merge_headers(session.as_ref(), auth.headers.as_ref()).as_ref(),
			request.headers.as_ref(),
		);
		if let Some(transform) = request.transform_headers.take() {
			headers = Some(transform(headers.unwrap_or_default()).await);
		}
		request.headers = headers;
		if resolution.env.is_some() || request.env.is_some() {
			let mut env = resolution.env.unwrap_or_default();
			for (name, value) in request.env.iter().flat_map(Record::iter) {
				env.insert(name, value.clone());
			}
			request.env = Some(env);
		}
		if identity.is_some() {
			request.identity = identity;
		}
		Ok((request_base_url, options))
	}

	/// Streams a request through the owning provider with auth applied (`stream`).
	pub fn stream(&self, model: &Model, context: Context, options: StreamOptions) -> AssistantMessageEventStream {
		let observer = observe_model_request(model, options.request.telemetry.as_ref(), None, RequestKind::Request);
		let cancel = options.request.cancel.clone();
		let (models, requested) = (self.clone(), model.clone());
		let transcript = normalize_context(context);
		let setup = async move {
			let provider = models.require_provider(&requested.provider)?;
			let (request_model, options) = models.prepare_chat(&requested, options).await?;
			Ok(provider.stream(&request_model, transcript, options))
		};
		lazy_stream(model, setup, observer, cancel)
	}

	/// The final message of [`Models::stream`] (`complete`).
	pub async fn complete(&self, model: &Model, context: Context, options: StreamOptions) -> AssistantMessage {
		self.stream(model, context, options).result().await
	}

	/// Streams with provider-neutral options (`streamSimple`).
	pub fn stream_simple(
		&self,
		model: &Model,
		context: Context,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		let observer = observe_model_request(
			model,
			options.stream.request.telemetry.as_ref(),
			options.reasoning,
			RequestKind::Request,
		);
		let cancel = options.stream.request.cancel.clone();
		let (models, requested) = (self.clone(), model.clone());
		let transcript = normalize_context(context);
		let setup = async move {
			let provider = models.require_provider(&requested.provider)?;
			let (request_model, options) = models.prepare_chat(&requested, options).await?;
			Ok(provider.stream_simple(&request_model, transcript, options))
		};
		lazy_stream(model, setup, observer, cancel)
	}

	/// The final message of [`Models::stream_simple`] (`completeSimple`).
	pub async fn complete_simple(
		&self,
		model: &Model,
		context: Context,
		options: SimpleStreamOptions,
	) -> AssistantMessage {
		self.stream_simple(model, context, options).result().await
	}

	/// Polls a deferred response as a stream (`streamDeferred`).
	pub fn stream_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: DeferredFetchOptions,
	) -> AssistantMessageEventStream {
		let observer = observe_model_request(model, options.request.telemetry.as_ref(), None, RequestKind::Poll);
		let cancel = options.request.cancel.clone();
		let (models, requested, handle) = (self.clone(), model.clone(), handle.clone());
		let setup = async move {
			let provider = models.require_provider(&requested.provider)?;
			if !provider.capabilities().fetch_deferred {
				return Err(ModelsError::new(
					ModelsErrorCode::Provider,
					format!("Provider {} does not support deferred responses", requested.provider),
				));
			}
			let (request_model, options) = models.prepare_chat(&requested, options).await?;
			Ok(provider.fetch_deferred(&request_model, &handle, options))
		};
		lazy_stream(model, setup, observer, cancel)
	}

	/// The final message of [`Models::stream_deferred`] (`fetchDeferred`).
	pub async fn fetch_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: DeferredFetchOptions,
	) -> AssistantMessage {
		self.stream_deferred(model, handle, options).result().await
	}

	/// Cancels a deferred response, best effort (`cancelDeferred`).
	pub async fn cancel_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: ProviderRequestOptions,
	) -> Result<(), ModelsError> {
		let provider = self.require_provider(&model.provider)?;
		if !provider.capabilities().cancel_deferred {
			return Err(ModelsError::new(
				ModelsErrorCode::Provider,
				format!("Provider {} does not support deferred responses", model.provider),
			));
		}
		let (request_model, options) = self.prepare_chat(model, options).await?;
		provider.cancel_deferred(&request_model, handle, options).await
	}

	async fn prepare_chat<O: RequestOptions>(&self, model: &Model, options: O) -> Result<(Model, O), ModelsError> {
		let any = AnyModel::Chat(model.clone());
		let (base_url, options) = self.apply_auth(&any, &model.base_url, options).await?;
		let mut request_model = model.clone();
		if let Some(base_url) = base_url {
			request_model.base_url = base_url;
		}
		Ok((request_model, options))
	}

	/// Generates images through the owning provider; never fails, errors are results (`generateImages`).
	pub async fn generate_images(
		&self,
		model: &ImageModel,
		context: &ImagesContext,
		options: ImagesOptions,
	) -> AssistantImages {
		let aborted = options.request.cancel.clone();
		let result = async {
			let provider = self.require_provider(&model.provider)?;
			if !provider.capabilities().generate_images {
				return Err(ModelsError::new(
					ModelsErrorCode::Provider,
					format!("Provider {} does not support image generation", model.provider),
				));
			}
			let any = AnyModel::Image(model.clone());
			let (base_url, options) = self.apply_auth(&any, &model.base_url, options).await?;
			let mut request_model = model.clone();
			if let Some(base_url) = base_url {
				request_model.base_url = base_url;
			}
			Ok(provider.generate_images(&request_model, context, options).await)
		}
		.await;
		result.unwrap_or_else(|error| image_error_result(model, &error.message, failure_of(aborted.as_ref())))
	}

	/// Classifies through the owning provider; never fails, errors are results (`classify`).
	pub async fn classify(
		&self,
		model: &ClassifierModel,
		context: &ClassifierContext,
		options: ClassifierOptions,
	) -> ClassifierResult {
		let aborted = options.request.cancel.clone();
		let result = async {
			assert_classifier_input_supported(model, context)?;
			let provider = self.require_provider(&model.provider)?;
			if !provider.capabilities().classify {
				return Err(ModelsError::new(
					ModelsErrorCode::Provider,
					format!("Provider {} does not support classification", model.provider),
				));
			}
			let any = AnyModel::Classifier(model.clone());
			let (base_url, options) = self.apply_auth(&any, &model.base_url, options).await?;
			let mut request_model = model.clone();
			if let Some(base_url) = base_url {
				request_model.base_url = base_url;
			}
			Ok(provider.classify(&request_model, context, options).await)
		}
		.await;
		result.unwrap_or_else(|error| classifier_error_result(model, &error.message, failure_of(aborted.as_ref())))
	}
}

fn failure_of(cancel: Option<&CancellationToken>) -> Failure {
	if cancel.is_some_and(CancellationToken::is_cancelled) {
		Failure::Aborted
	} else {
		Failure::Error
	}
}

fn as_provider_headers(headers: &Record<String>) -> ProviderHeaders {
	headers
		.iter()
		.map(|(name, value)| (name, Some(value.clone())))
		.collect()
}

fn model_headers(model: &AnyModel) -> Option<ProviderHeaders> {
	let headers = match model {
		AnyModel::Chat(model) => model.headers.as_ref(),
		AnyModel::Image(model) => model.headers.as_ref(),
		AnyModel::Classifier(model) => model.headers.as_ref(),
		AnyModel::Unknown(_) => None,
	}?;
	Some(as_provider_headers(headers))
}
