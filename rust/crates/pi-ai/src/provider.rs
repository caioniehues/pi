//! The `Provider` trait, the runtime unit a `Models` holds, and `create_provider`, which builds one from parts (ports
//! the `Provider` interface, `ProviderStreams`, `ProviderImages`, `ProviderClassifier`, `RefreshModelsContext`,
//! `ModelsPublication` and `createProvider` of `packages/ai/src/models.ts` and `types.ts`).
//!
//! TS marks optional methods by their absence; here they are trait methods with defaults that report the missing
//! operation, and `capabilities()` says which ones a provider implements.

use crate::auth::types::{Credential, ProviderAuth};
use crate::diagnostics::now_ms;
use crate::error::{ModelsError, ModelsErrorCode};
use crate::event_stream::AssistantMessageEventStream;
use crate::headers::ProviderHeaders;
use crate::lazy_stream::{NoObserver, lazy_stream};
use crate::model_operations::{AnyModel, Failure, ModelType, classifier_error_result, image_error_result};
use crate::models_store::ModelsStoreEntry;
use crate::options::{
	ClassifierOptions, DeferredFetchOptions, ImagesOptions, ProviderRequestOptions, SimpleStreamOptions, StreamOptions,
};
use crate::transcript::TranscriptContext;
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use pi_types::{
	AssistantImages, ClassifierContext, ClassifierModel, ClassifierResult, DeferredHandle, ImageModel, ImagesContext,
	Model,
};
use std::sync::{Arc, Mutex, PoisonError};
use tokio_util::sync::CancellationToken;

/// Which optional operations a provider implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
	clippy::struct_excessive_bools,
	reason = "independent capability flags, not a state machine"
)]
pub struct ProviderCapabilities {
	/// `refresh_models`: a dynamic model list.
	pub refresh_models: bool,
	/// `fetch_deferred`.
	pub fetch_deferred: bool,
	/// `cancel_deferred`.
	pub cancel_deferred: bool,
	/// `generate_images`.
	pub generate_images: bool,
	/// `classify`.
	pub classify: bool,
}

/// What a provider's `refresh_models` gets (`RefreshModelsContext`).
#[derive(Clone)]
pub struct RefreshModelsContext {
	/// The effective credential; OAuth credentials are refreshed before network access.
	pub credential: Option<Credential>,
	/// The provider's stored catalog, read before this phase, models of unknown types dropped.
	pub stored: Option<ModelsStoreEntry>,
	/// Generation-checked publication; `false` when superseded or cancelled.
	pub publish: Publish,
	/// False during offline or cache-only initialization.
	pub allow_network: bool,
	/// Bypass freshness checks (only with network).
	pub force: Option<bool>,
	/// Always present.
	pub cancel: CancellationToken,
	/// The registered provider's base URL.
	pub base_url: Option<String>,
}

/// Publishes a refresh's result.
pub type Publish = Arc<dyn Fn(ModelsPublication) -> BoxFuture<'static, Result<bool, ModelsError>> + Send + Sync>;

/// What a refresh persists (`ModelsPublication.persist`).
pub enum Persist {
	/// Leave storage unchanged.
	Keep,
	/// Delete the stored catalog.
	Delete,
	/// Store this catalog.
	Write(ModelsStoreEntry),
}

/// A refresh result (`ModelsPublication`): what to persist, then a synchronous in-memory update.
pub struct ModelsPublication {
	/// The persistence change.
	pub persist: Persist,
	/// Runs only after the persistence change, unless superseded.
	pub update: Option<Box<dyn FnOnce() + Send>>,
}

/// A provider (`Provider`): its metadata, auth, models and operations.
#[async_trait]
pub trait Provider: Send + Sync {
	/// The provider id.
	fn id(&self) -> &str;
	/// The display name.
	fn name(&self) -> &str;
	/// The base URL.
	fn base_url(&self) -> Option<&str> {
		None
	}
	/// Provider headers.
	fn headers(&self) -> Option<&ProviderHeaders> {
		None
	}
	/// The auth methods.
	fn auth(&self) -> &ProviderAuth;
	/// The optional operations it implements.
	fn capabilities(&self) -> ProviderCapabilities {
		ProviderCapabilities::default()
	}
	/// Current chat models.
	fn get_models(&self) -> Vec<Model>;
	/// Current models of every type; chat models only by default.
	fn get_all_models(&self) -> Vec<AnyModel> {
		self.get_models().into_iter().map(AnyModel::Chat).collect()
	}
	/// Restores the stored list and optionally fetches a newer one (dynamic providers).
	async fn refresh_models(&self, _context: RefreshModelsContext) -> Result<(), ModelsError> {
		Ok(())
	}
	/// Credential-specific chat model availability; all by default.
	fn filter_models(&self, models: Vec<Model>, _credential: Option<&Credential>) -> Vec<Model> {
		models
	}
	/// Credential-specific availability of every model type; by default `filter_models` on chat models.
	fn filter_all_models(&self, models: Vec<AnyModel>, credential: Option<&Credential>) -> Vec<AnyModel> {
		let available: Vec<String> = self
			.filter_models(self.get_models(), credential)
			.into_iter()
			.map(|model| model.id)
			.collect();
		models
			.into_iter()
			.filter(|model| !model.is_type(ModelType::Chat) || available.iter().any(|id| id == model.id()))
			.collect()
	}
	/// Streams a normalized transcript.
	fn stream(&self, model: &Model, context: TranscriptContext, options: StreamOptions) -> AssistantMessageEventStream;
	/// Streams with provider-neutral options.
	fn stream_simple(
		&self,
		model: &Model,
		context: TranscriptContext,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream;
	/// Fetches a deferred response.
	fn fetch_deferred(
		&self,
		model: &Model,
		_handle: &DeferredHandle,
		options: DeferredFetchOptions,
	) -> AssistantMessageEventStream {
		let error = ModelsError::new(
			ModelsErrorCode::Provider,
			format!("Provider {} does not support deferred responses", self.id()),
		);
		error_stream(model, error, options.request.cancel)
	}
	/// Cancels a deferred response, best effort.
	async fn cancel_deferred(
		&self,
		_model: &Model,
		_handle: &DeferredHandle,
		_options: ProviderRequestOptions,
	) -> Result<(), ModelsError> {
		Err(ModelsError::new(
			ModelsErrorCode::Provider,
			format!("Provider {} does not support deferred responses", self.id()),
		))
	}
	/// Generates images; never fails, errors are results.
	async fn generate_images(
		&self,
		model: &ImageModel,
		_context: &ImagesContext,
		_options: ImagesOptions,
	) -> AssistantImages {
		image_error_result(
			model,
			&format!("Provider {} does not support image generation", self.id()),
			Failure::Error,
		)
	}
	/// Classifies; never fails, errors are results.
	async fn classify(
		&self,
		model: &ClassifierModel,
		_context: &ClassifierContext,
		_options: ClassifierOptions,
	) -> ClassifierResult {
		classifier_error_result(
			model,
			&format!("Provider {} does not support classification", self.id()),
			Failure::Error,
		)
	}
}

/// A stream that ends at once with `error` (a setup failure).
pub fn error_stream(
	model: &Model,
	error: ModelsError,
	cancel: Option<CancellationToken>,
) -> AssistantMessageEventStream {
	lazy_stream(model, async move { Err(error) }, Box::new(NoObserver), cancel)
}

/// The stream implementation of an API (`ProviderStreams`): every wire API module provides one.
#[async_trait]
pub trait ProviderStreams: Send + Sync {
	/// Whether `fetch_deferred` and `cancel_deferred` are implemented.
	fn deferred(&self) -> DeferredSupport {
		DeferredSupport::default()
	}
	/// Streams a normalized transcript.
	fn stream(&self, model: &Model, context: TranscriptContext, options: StreamOptions) -> AssistantMessageEventStream;
	/// Streams with provider-neutral options.
	fn stream_simple(
		&self,
		model: &Model,
		context: TranscriptContext,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream;
	/// Fetches a deferred response.
	fn fetch_deferred(
		&self,
		model: &Model,
		_handle: &DeferredHandle,
		options: DeferredFetchOptions,
	) -> AssistantMessageEventStream {
		error_stream(
			model,
			ModelsError::new(ModelsErrorCode::Provider, "API does not support deferred responses"),
			options.request.cancel,
		)
	}
	/// Cancels a deferred response.
	async fn cancel_deferred(
		&self,
		_model: &Model,
		_handle: &DeferredHandle,
		_options: ProviderRequestOptions,
	) -> Result<(), ModelsError> {
		Err(ModelsError::new(
			ModelsErrorCode::Provider,
			"API cannot cancel deferred responses",
		))
	}
}

/// Which deferred operations an API implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeferredSupport {
	/// `fetch_deferred`.
	pub fetch: bool,
	/// `cancel_deferred`.
	pub cancel: bool,
}

/// The image implementation of an image API (`ProviderImages`).
#[async_trait]
pub trait ProviderImages: Send + Sync {
	/// Generates images; never fails.
	async fn generate_images(
		&self,
		model: &ImageModel,
		context: &ImagesContext,
		options: ImagesOptions,
	) -> AssistantImages;
}

/// The classifier implementation of a classifier API (`ProviderClassifier`).
#[async_trait]
pub trait ProviderClassifier: Send + Sync {
	/// Classifies; never fails.
	async fn classify(
		&self,
		model: &ClassifierModel,
		context: &ClassifierContext,
		options: ClassifierOptions,
	) -> ClassifierResult;
}

/// The chat implementation of a created provider: one for every model, or one per `model.api`.
#[derive(Clone)]
pub enum ProviderApi {
	/// One implementation for every chat model.
	Single(Arc<dyn ProviderStreams>),
	/// Implementations keyed by `model.api`.
	ByApi(Vec<(String, Arc<dyn ProviderStreams>)>),
}

/// Fetches a dynamic model overlay.
pub type FetchModels =
	Arc<dyn Fn(&RefreshModelsContext) -> BoxFuture<'static, Result<Vec<AnyModel>, ModelsError>> + Send + Sync>;

/// Filters chat models by credential.
pub type FilterModels = Arc<dyn Fn(Vec<Model>, Option<&Credential>) -> Vec<Model> + Send + Sync>;

/// Filters models of every type by credential.
pub type FilterAllModels = Arc<dyn Fn(Vec<AnyModel>, Option<&Credential>) -> Vec<AnyModel> + Send + Sync>;

/// The parts of a provider (`CreateProviderOptions`).
#[derive(Clone, Default)]
pub struct CreateProviderOptions {
	/// The id.
	pub id: String,
	/// The display name; the id when absent.
	pub name: Option<String>,
	/// The base URL.
	pub base_url: Option<String>,
	/// Provider headers.
	pub headers: Option<ProviderHeaders>,
	/// The auth methods.
	pub auth: ProviderAuth,
	/// Static models of every type.
	pub models: Vec<AnyModel>,
	/// A dynamic overlay, restored and published transactionally.
	pub fetch_models: Option<FetchModels>,
	/// Credential-specific chat availability.
	pub filter_models: Option<FilterModels>,
	/// Credential-specific availability of every type.
	pub filter_all_models: Option<FilterAllModels>,
	/// The chat implementation.
	pub api: Option<ProviderApi>,
	/// Image implementations keyed by `model.api`.
	pub images: Vec<(String, Arc<dyn ProviderImages>)>,
	/// Classifier implementations keyed by `model.api`.
	pub classifiers: Vec<(String, Arc<dyn ProviderClassifier>)>,
}

/// Builds a provider from parts (`createProvider`); fails when it has no chat, image or classifier implementation.
pub fn create_provider(options: CreateProviderOptions) -> Result<Arc<dyn Provider>, ModelsError> {
	let has_streams = match &options.api {
		Some(ProviderApi::Single(_)) => true,
		Some(ProviderApi::ByApi(apis)) => !apis.is_empty(),
		None => false,
	};
	if !has_streams && options.images.is_empty() && options.classifiers.is_empty() {
		return Err(ModelsError::new(
			ModelsErrorCode::Provider,
			format!(
				"Provider {}: at least one of \"api\", \"images\", or \"classifiers\" is required.",
				options.id
			),
		));
	}
	let name = options.name.clone().unwrap_or_else(|| options.id.clone());
	Ok(Arc::new(CreatedProvider {
		name,
		dynamic: Arc::new(Mutex::new(Vec::new())),
		options,
	}))
}

struct CreatedProvider {
	options: CreateProviderOptions,
	name: String,
	dynamic: Arc<Mutex<Vec<AnyModel>>>,
}

impl CreatedProvider {
	fn current_models(&self) -> Vec<AnyModel> {
		let mut merged = self.options.models.clone();
		for model in self.dynamic.lock().unwrap_or_else(PoisonError::into_inner).iter() {
			match merged
				.iter_mut()
				.find(|entry| entry.model_type() == model.model_type() && entry.id() == model.id())
			{
				Some(slot) => *slot = model.clone(),
				None => merged.push(model.clone()),
			}
		}
		merged
	}

	fn api_for(&self, model: &Model) -> Option<&Arc<dyn ProviderStreams>> {
		match self.options.api.as_ref()? {
			ProviderApi::Single(api) => Some(api),
			ProviderApi::ByApi(apis) => apis
				.iter()
				.find(|(api, _)| *api == model.api)
				.map(|(_, implementation)| implementation),
		}
	}

	fn streams(&self) -> Vec<&Arc<dyn ProviderStreams>> {
		match &self.options.api {
			Some(ProviderApi::Single(api)) => vec![api],
			Some(ProviderApi::ByApi(apis)) => apis.iter().map(|(_, api)| api).collect(),
			None => Vec::new(),
		}
	}

	fn no_api(&self, model: &Model, cancel: Option<CancellationToken>) -> AssistantMessageEventStream {
		let error = ModelsError::new(
			ModelsErrorCode::Stream,
			format!(
				"Provider {} has no API implementation for \"{}\"",
				self.options.id, model.api
			),
		);
		error_stream(model, error, cancel)
	}
}

#[async_trait]
impl Provider for CreatedProvider {
	fn id(&self) -> &str {
		&self.options.id
	}

	fn name(&self) -> &str {
		&self.name
	}

	fn base_url(&self) -> Option<&str> {
		self.options.base_url.as_deref()
	}

	fn headers(&self) -> Option<&ProviderHeaders> {
		self.options.headers.as_ref()
	}

	fn auth(&self) -> &ProviderAuth {
		&self.options.auth
	}

	fn capabilities(&self) -> ProviderCapabilities {
		let streams = self.streams();
		ProviderCapabilities {
			refresh_models: self.options.fetch_models.is_some(),
			fetch_deferred: streams.iter().any(|api| api.deferred().fetch),
			cancel_deferred: streams.iter().any(|api| api.deferred().cancel),
			generate_images: !self.options.images.is_empty(),
			classify: !self.options.classifiers.is_empty(),
		}
	}

	fn get_models(&self) -> Vec<Model> {
		self.current_models()
			.into_iter()
			.filter_map(|model| match model {
				AnyModel::Chat(model) => Some(model),
				_ => None,
			})
			.collect()
	}

	fn get_all_models(&self) -> Vec<AnyModel> {
		self.current_models()
	}

	async fn refresh_models(&self, context: RefreshModelsContext) -> Result<(), ModelsError> {
		let Some(fetch_models) = &self.options.fetch_models else {
			return Ok(());
		};
		if let Some(stored) = &context.stored {
			let restored: Vec<AnyModel> = stored
				.models
				.iter()
				.filter(|model| model.provider() == self.options.id)
				.cloned()
				.collect();
			let dynamic = Arc::clone(&self.dynamic);
			let update = Box::new(move || *dynamic.lock().unwrap_or_else(PoisonError::into_inner) = restored);
			if !(context.publish)(ModelsPublication {
				persist: Persist::Keep,
				update: Some(update),
			})
			.await?
			{
				return Ok(());
			}
		}
		if !context.allow_network || context.cancel.is_cancelled() {
			return Ok(());
		}
		let fetched = fetch_models(&context).await?;
		if context.cancel.is_cancelled() {
			return Ok(());
		}
		let refreshed: Vec<AnyModel> = fetched
			.into_iter()
			.filter(|model| model.model_type().is_some())
			.collect();
		let dynamic = Arc::clone(&self.dynamic);
		let entry = ModelsStoreEntry {
			models: refreshed.clone(),
			checked_at: Some(now_ms()),
			..ModelsStoreEntry::default()
		};
		let update = Box::new(move || *dynamic.lock().unwrap_or_else(PoisonError::into_inner) = refreshed);
		(context.publish)(ModelsPublication {
			persist: Persist::Write(entry),
			update: Some(update),
		})
		.await?;
		Ok(())
	}

	fn filter_models(&self, models: Vec<Model>, credential: Option<&Credential>) -> Vec<Model> {
		match &self.options.filter_models {
			Some(filter) => filter(models, credential),
			None => models,
		}
	}

	fn filter_all_models(&self, models: Vec<AnyModel>, credential: Option<&Credential>) -> Vec<AnyModel> {
		if let Some(filter) = &self.options.filter_all_models {
			return filter(models, credential);
		}
		if self.options.filter_models.is_none() {
			return models;
		}
		let available: Vec<String> = self
			.filter_models(self.get_models(), credential)
			.into_iter()
			.map(|model| model.id)
			.collect();
		models
			.into_iter()
			.filter(|model| !model.is_type(ModelType::Chat) || available.iter().any(|id| id == model.id()))
			.collect()
	}

	fn stream(&self, model: &Model, context: TranscriptContext, options: StreamOptions) -> AssistantMessageEventStream {
		match self.api_for(model) {
			Some(api) => api.stream(model, context, options),
			None => self.no_api(model, options.request.cancel),
		}
	}

	fn stream_simple(
		&self,
		model: &Model,
		context: TranscriptContext,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		match self.api_for(model) {
			Some(api) => api.stream_simple(model, context, options),
			None => self.no_api(model, options.stream.request.cancel),
		}
	}

	fn fetch_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: DeferredFetchOptions,
	) -> AssistantMessageEventStream {
		if let Some(api) = self.api_for(model).filter(|api| api.deferred().fetch) {
			return api.fetch_deferred(model, handle, options);
		}
		let error = ModelsError::new(
			ModelsErrorCode::Provider,
			format!(
				"Provider {} does not support deferred responses for \"{}\"",
				self.options.id, model.api
			),
		);
		error_stream(model, error, options.request.cancel)
	}

	async fn cancel_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: ProviderRequestOptions,
	) -> Result<(), ModelsError> {
		match self.api_for(model).filter(|api| api.deferred().cancel) {
			Some(api) => api.cancel_deferred(model, handle, options).await,
			None => Err(ModelsError::new(
				ModelsErrorCode::Provider,
				format!(
					"Provider {} cannot cancel deferred responses for \"{}\"",
					self.options.id, model.api
				),
			)),
		}
	}

	async fn generate_images(
		&self,
		model: &ImageModel,
		context: &ImagesContext,
		options: ImagesOptions,
	) -> AssistantImages {
		match self.options.images.iter().find(|(api, _)| *api == model.api) {
			Some((_, implementation)) => implementation.generate_images(model, context, options).await,
			None => image_error_result(
				model,
				&format!(
					"Provider {} has no image generation implementation for \"{}\"",
					self.options.id, model.api
				),
				Failure::Error,
			),
		}
	}

	async fn classify(
		&self,
		model: &ClassifierModel,
		context: &ClassifierContext,
		options: ClassifierOptions,
	) -> ClassifierResult {
		match self.options.classifiers.iter().find(|(api, _)| *api == model.api) {
			Some((_, implementation)) => implementation.classify(model, context, options).await,
			None => classifier_error_result(
				model,
				&format!(
					"Provider {} has no classifier implementation for \"{}\"",
					self.options.id, model.api
				),
				Failure::Error,
			),
		}
	}
}
