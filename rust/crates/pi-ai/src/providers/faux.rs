//! The in-process faux provider every test builds on (ports `packages/ai/src/providers/faux.ts`): scripted responses,
//! token-by-token streaming with optional pacing, simulated prompt caching per session, and deferred responses.
//!
//! Lengths and chunk sizes are UTF-16 code units, as in TS; a chunk never splits a character.

use crate::auth::types::{ApiKeyAuth, ApiKeyInput, AuthResult, ProviderAuth};
use crate::diagnostics::now_ms;
use crate::error::ModelsError;
use crate::event_stream::{AssistantMessageEventStream, BlockKind, PartialMessage, TextKind};
use crate::lazy_stream::assistant_message;
use crate::model_operations::{AnyModel, ModelRef};
use crate::models::Models;
use crate::options::{
	DeferredFetchOptions, OnResponse, ProviderRequestOptions, ProviderResponse, SimpleStreamOptions, StreamOptions,
};
use crate::provider::{
	CreateProviderOptions, DeferredSupport, Provider, ProviderApi, ProviderStreams, create_provider,
};
use crate::streaming_json::ToolCallArgumentsStream;
use crate::text::get_system_message_text;
use crate::transcript::TranscriptContext;
use crate::utf16::{utf16_len, utf16_prefix};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use pi_json::{JsonObject, JsonValue};
use pi_types::{
	AssistantContentBlock, AssistantMessage, CacheRetention, ContentBlock, DeferredHandle, DoneReason, ErrorReason,
	Message, MessageContent, Model, ModelCost, ModelInputLimits, ModelInputModality, Record, StopReason, TextContent,
	ThinkingContent, ThinkingType, ToolCall, ToolCallType, Usage,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: u64 = 3;
const DEFAULT_MAX_TOKEN_SIZE: u64 = 5;

/// A faux model (`FauxModelDefinition`); absent fields take the defaults of `faux-1`.
#[derive(Debug, Clone, Default)]
pub struct FauxModelDefinition {
	/// The model id.
	pub id: String,
	/// The display name; the id when absent.
	pub name: Option<String>,
	/// Whether it reasons.
	pub reasoning: Option<bool>,
	/// Accepted inputs; text and images when absent.
	pub input: Option<Vec<ModelInputModality>>,
	/// Input limits.
	pub input_limits: Option<ModelInputLimits>,
	/// Prices; zero when absent.
	pub cost: Option<ModelCost>,
	/// Context window; 128000 when absent.
	pub context_window: Option<u64>,
	/// Output limit; 16384 when absent.
	pub max_tokens: Option<u64>,
}

/// A text block (`fauxText`).
pub fn faux_text(text: &str) -> AssistantContentBlock {
	AssistantContentBlock::Text(TextContent::new(text))
}

/// A thinking block (`fauxThinking`).
pub fn faux_thinking(thinking: &str) -> AssistantContentBlock {
	AssistantContentBlock::Thinking(ThinkingContent {
		kind: ThinkingType::Thinking,
		thinking: thinking.to_owned(),
		thinking_signature: None,
		redacted: None,
		extra: JsonObject::new(),
	})
}

/// A tool call block with a random id unless one is given (`fauxToolCall`).
pub fn faux_tool_call(name: &str, arguments: JsonObject, id: Option<&str>) -> AssistantContentBlock {
	AssistantContentBlock::ToolCall(ToolCall {
		kind: ToolCallType::ToolCall,
		id: id.map_or_else(|| random_id("tool"), str::to_owned),
		name: name.to_owned(),
		arguments,
		thought_signature: None,
		namespace: None,
		arguments_error: None,
		extra: JsonObject::new(),
	})
}

/// Options of [`faux_assistant_message`].
#[derive(Debug, Clone, Default)]
pub struct FauxMessageOptions {
	/// The stop reason; `stop` when absent.
	pub stop_reason: Option<StopReason>,
	/// A deferred handle.
	pub deferred: Option<DeferredHandle>,
	/// The error message.
	pub error_message: Option<String>,
	/// The response id.
	pub response_id: Option<String>,
	/// The timestamp; now when absent.
	pub timestamp: Option<u64>,
}

/// A scripted assistant message (`fauxAssistantMessage`).
pub fn faux_assistant_message(content: Vec<AssistantContentBlock>, options: FauxMessageOptions) -> AssistantMessage {
	let mut message = assistant_message(
		DEFAULT_API.into(),
		DEFAULT_PROVIDER.into(),
		DEFAULT_MODEL_ID.into(),
		options.stop_reason.unwrap_or(StopReason::Stop),
		options.timestamp.unwrap_or_else(now_ms),
	);
	message.content = content;
	message.deferred = options.deferred;
	message.error_message = options.error_message;
	message.response_id = options.response_id;
	message
}

/// A text-only scripted message: `fauxAssistantMessage("text")`.
pub fn faux_text_message(text: &str) -> AssistantMessage {
	faux_assistant_message(vec![faux_text(text)], FauxMessageOptions::default())
}

/// The faux provider's counters (`FauxProviderState`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FauxProviderState {
	/// Stream calls.
	pub call_count: u64,
	/// Deferred fetches.
	pub deferred_fetch_count: u64,
	/// Cancelled deferred handles.
	pub cancelled_deferred: Vec<DeferredHandle>,
}

/// Builds a response from the request (`FauxResponseFactory`); an `Err` becomes an error response.
pub type FauxResponseFactory = Arc<
	dyn Fn(
			&TranscriptContext,
			&SimpleStreamOptions,
			&FauxProviderState,
			&Model,
		) -> BoxFuture<'static, Result<AssistantMessage, String>>
		+ Send
		+ Sync,
>;

/// One scripted response: a message or a factory (`FauxResponseStep`).
#[derive(Clone)]
#[allow(
	clippy::large_enum_variant,
	reason = "scripted test data; boxing would only add noise"
)]
pub enum FauxResponseStep {
	/// A fixed message.
	Message(AssistantMessage),
	/// A function of the request.
	Factory(FauxResponseFactory),
}

impl From<AssistantMessage> for FauxResponseStep {
	fn from(message: AssistantMessage) -> Self {
		Self::Message(message)
	}
}

/// Deferred behavior (`RegisterFauxProviderOptions.deferred`).
#[derive(Debug, Clone, Copy, Default)]
pub struct FauxDeferredOptions {
	/// Fetches that return the handle again before the response is ready.
	pub pending_fetches: Option<u64>,
	/// `pollAfterMs` of the handles.
	pub poll_after_ms: Option<u64>,
}

/// Options of the faux provider (`RegisterFauxProviderOptions`).
#[derive(Debug, Clone, Default)]
pub struct FauxProviderOptions {
	/// The API id; a random `faux:...` id when absent.
	pub api: Option<String>,
	/// The provider id; `faux` when absent.
	pub provider: Option<String>,
	/// The models; one `faux-1` when empty.
	pub models: Vec<FauxModelDefinition>,
	/// Deferred behavior.
	pub deferred: FauxDeferredOptions,
	/// Pacing; unpaced chunks yield between each other.
	pub tokens_per_second: Option<f64>,
	/// Smallest chunk in tokens of four characters.
	pub token_size_min: Option<u64>,
	/// Largest chunk in tokens of four characters.
	pub token_size_max: Option<u64>,
}

/// A faux provider and its controls (`FauxProviderHandle`).
#[derive(Clone)]
pub struct FauxProviderHandle {
	/// The provider, for `Models::set_provider`.
	pub provider: Arc<dyn Provider>,
	/// The API id.
	pub api: String,
	/// The models.
	pub models: Vec<Model>,
	core: Arc<FauxCore>,
}

impl FauxProviderHandle {
	/// The first model.
	pub fn model(&self) -> &Model {
		// invariant: a faux provider has at least one model.
		&self.models[0]
	}

	/// A model by id.
	pub fn model_by_id(&self, id: &str) -> Option<&Model> {
		self.models.iter().find(|model| model.id == id)
	}

	/// The counters.
	pub fn state(&self) -> FauxProviderState {
		self.core.state()
	}

	/// Replaces the queued responses.
	pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
		self.core.lock().pending = responses.into();
	}

	/// Queues more responses.
	pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
		self.core.lock().pending.extend(responses);
	}

	/// The number of queued responses.
	pub fn pending_response_count(&self) -> usize {
		self.core.lock().pending.len()
	}
}

/// A faux provider registered on a `Models` (`FauxProviderRegistration`).
pub struct FauxProviderRegistration {
	/// The handle.
	pub handle: FauxProviderHandle,
	models: Models,
}

impl FauxProviderRegistration {
	/// Removes the provider from the registry.
	pub fn unregister(self) {
		self.models.delete_provider(self.handle.provider.id());
	}
}

/// Registers a new faux provider on `models` (`registerFauxProvider`).
pub fn register_faux_provider(models: &Models, options: &FauxProviderOptions) -> FauxProviderRegistration {
	let handle = faux_provider(options);
	models.set_provider(Arc::clone(&handle.provider));
	FauxProviderRegistration {
		handle,
		models: models.clone(),
	}
}

/// A faux provider for explicit `Models` collections (`fauxProvider`).
pub fn faux_provider(options: &FauxProviderOptions) -> FauxProviderHandle {
	let core = Arc::new_cyclic(|this| FauxCore::new(options, this.clone()));
	let provider_id = core.provider.clone();
	let api: Arc<dyn ProviderStreams> = Arc::clone(&core) as Arc<dyn ProviderStreams>;
	let models = core.models.clone();
	let provider = create_provider(CreateProviderOptions {
		id: provider_id,
		auth: ProviderAuth {
			api_key: Some(Arc::new(FauxAuth)),
			oauth: None,
		},
		models: models.iter().cloned().map(AnyModel::Chat).collect(),
		api: Some(ProviderApi::Single(api)),
		..CreateProviderOptions::default()
	});
	FauxProviderHandle {
		// invariant: the faux provider has a chat implementation, the one thing `create_provider` requires.
		provider: provider.unwrap_or_else(|_| unreachable!("faux provider has an API")),
		api: core.api.clone(),
		models,
		core,
	}
}

/// Auth that is always configured, with no key.
struct FauxAuth;

#[async_trait]
impl ApiKeyAuth for FauxAuth {
	fn name(&self) -> &'static str {
		"Faux"
	}

	async fn resolve(&self, _input: ApiKeyInput<'_>) -> Result<Option<AuthResult>, ModelsError> {
		Ok(Some(AuthResult::default()))
	}
}

struct DeferredEntry {
	handle: DeferredHandle,
	step: FauxResponseStep,
	context: TranscriptContext,
	options: SimpleStreamOptions,
	model: Model,
	pending_fetches: u64,
	cancelled: bool,
	final_message: Option<AssistantMessage>,
}

struct CoreState {
	pending: VecDeque<FauxResponseStep>,
	state: FauxProviderState,
	prompt_cache: HashMap<String, Vec<String>>,
	deferred: HashMap<String, DeferredEntry>,
}

struct FauxCore {
	api: String,
	provider: String,
	models: Vec<Model>,
	min_token_size: u64,
	max_token_size: u64,
	tokens_per_second: Option<f64>,
	deferred_options: FauxDeferredOptions,
	inner: Mutex<CoreState>,
	this: Weak<FauxCore>,
}

impl FauxCore {
	fn new(options: &FauxProviderOptions, this: Weak<Self>) -> Self {
		let api = options.api.clone().unwrap_or_else(|| random_id(DEFAULT_API));
		let provider = options.provider.clone().unwrap_or_else(|| DEFAULT_PROVIDER.into());
		let min = options
			.token_size_min
			.unwrap_or(DEFAULT_MIN_TOKEN_SIZE)
			.min(options.token_size_max.unwrap_or(DEFAULT_MAX_TOKEN_SIZE))
			.max(1);
		let max = options.token_size_max.unwrap_or(DEFAULT_MAX_TOKEN_SIZE).max(min);
		let definitions = if options.models.is_empty() {
			vec![FauxModelDefinition {
				id: DEFAULT_MODEL_ID.into(),
				name: Some(DEFAULT_MODEL_NAME.into()),
				..FauxModelDefinition::default()
			}]
		} else {
			options.models.clone()
		};
		let models = definitions
			.into_iter()
			.map(|definition| faux_model(definition, &api, &provider))
			.collect();
		Self {
			api,
			provider,
			models,
			min_token_size: min,
			max_token_size: max,
			tokens_per_second: options.tokens_per_second,
			deferred_options: options.deferred,
			inner: Mutex::new(CoreState {
				pending: VecDeque::new(),
				state: FauxProviderState::default(),
				prompt_cache: HashMap::new(),
				deferred: HashMap::new(),
			}),
			this,
		}
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, CoreState> {
		self.inner.lock().unwrap_or_else(PoisonError::into_inner)
	}

	fn state(&self) -> FauxProviderState {
		self.lock().state.clone()
	}

	fn pacing(&self) -> Pacing {
		Pacing {
			min: self.min_token_size,
			max: self.max_token_size,
			tokens_per_second: self.tokens_per_second,
		}
	}

	async fn resolve_response(
		&self,
		step: &FauxResponseStep,
		context: &TranscriptContext,
		options: &SimpleStreamOptions,
		model: &Model,
	) -> Result<AssistantMessage, String> {
		let resolved = match step {
			FauxResponseStep::Message(message) => message.clone(),
			FauxResponseStep::Factory(factory) => {
				let state = self.state();
				factory(context, options, &state, model).await?
			}
		};
		let mut message = resolved;
		message.api.clone_from(&self.api);
		message.provider.clone_from(&self.provider);
		message.model.clone_from(&model.id);
		Ok(self.with_usage_estimate(message, context, &options.stream))
	}

	fn with_usage_estimate(
		&self,
		mut message: AssistantMessage,
		context: &TranscriptContext,
		options: &StreamOptions,
	) -> AssistantMessage {
		// One text per message; the whole prompt joins them with blank lines.
		let prompt: Vec<String> = context
			.messages()
			.iter()
			.map(|message| format!("{}:{}", role(message), message_to_text(message)))
			.collect();
		let prompt_tokens = ceil_quarter(joined_length(&prompt, prompt.len()));
		let output = ceil_quarter(utf16_len(&assistant_content_to_text(&message.content)));
		let (mut input, mut cache_read, mut cache_write) = (prompt_tokens, 0, 0);
		if let Some(session) = options.session_id.as_ref().filter(|session| !session.is_empty())
			&& options.cache_retention != Some(CacheRetention::None)
		{
			let mut inner = self.lock();
			match inner.prompt_cache.get(session) {
				Some(previous) => {
					cache_read = ceil_quarter(common_prompt_prefix_length(previous, &prompt));
					cache_write = prompt_tokens.saturating_sub(cache_read);
				}
				None => cache_write = prompt_tokens,
			}
			// Every prompt token is either read from or written to the simulated cache.
			input = 0;
			inner.prompt_cache.insert(session.clone(), prompt);
		}
		let mut usage = Usage::zero();
		usage.input = input;
		usage.output = output;
		usage.cache_read = cache_read;
		usage.cache_write = cache_write;
		usage.total_tokens = Some(input + output + cache_read + cache_write);
		message.usage = usage;
		message
	}

	fn error_message(&self, error: &str, model_id: &str) -> AssistantMessage {
		let mut message = assistant_message(
			self.api.clone(),
			self.provider.clone(),
			model_id.to_owned(),
			StopReason::Error,
			now_ms(),
		);
		message.error_message = Some(error.to_owned());
		message
	}

	fn push_error(outer: &AssistantMessageEventStream, message: AssistantMessage) {
		outer.push_error(ErrorReason::Error, message.clone());
		outer.end(Some(message));
	}

	async fn run_stream(
		self: Arc<Self>,
		outer: AssistantMessageEventStream,
		model: Model,
		context: TranscriptContext,
		options: SimpleStreamOptions,
		step: Option<FauxResponseStep>,
	) {
		notify_response(options.stream.request.on_response.as_ref(), &model).await;
		let cancel = options.stream.request.cancel.clone();
		let Some(step) = step else {
			let message = self.error_message("No more faux responses queued", &model.id);
			let message = self.with_usage_estimate(message, &context, &options.stream);
			Self::push_error(&outer, message);
			return;
		};
		if options.deferred.is_some() {
			let handle = DeferredHandle {
				provider: model.provider.clone(),
				model_id: model.id.clone(),
				api: model.api.clone(),
				id: random_id("deferred"),
				expires_at: None,
				poll_after_ms: self.deferred_options.poll_after_ms,
				data: None,
				extra: JsonObject::new(),
			};
			self.lock().deferred.insert(
				handle.id.clone(),
				DeferredEntry {
					handle: handle.clone(),
					step,
					context,
					options,
					model: model.clone(),
					pending_fetches: self.deferred_options.pending_fetches.unwrap_or(0),
					cancelled: false,
					final_message: None,
				},
			);
			self.stream_or_error(&outer, deferred_message(&model, handle), cancel.as_ref(), &model.id)
				.await;
			return;
		}
		match self.resolve_response(&step, &context, &options, &model).await {
			Ok(message) => self.stream_or_error(&outer, message, cancel.as_ref(), &model.id).await,
			Err(error) => Self::push_error(&outer, self.error_message(&error, &model.id)),
		}
	}

	async fn stream_or_error(
		&self,
		outer: &AssistantMessageEventStream,
		message: AssistantMessage,
		cancel: Option<&CancellationToken>,
		model_id: &str,
	) {
		if let Err(error) = stream_with_deltas(outer, message, self.pacing(), cancel).await {
			Self::push_error(outer, self.error_message(&error, model_id));
		}
	}

	async fn run_fetch(
		self: Arc<Self>,
		outer: AssistantMessageEventStream,
		model: Model,
		handle: DeferredHandle,
		options: DeferredFetchOptions,
	) {
		notify_response(options.request.on_response.as_ref(), &model).await;
		let cancel = options.request.cancel.clone();
		match self.deferred_step(&handle) {
			Err(error) => Self::push_error(&outer, self.error_message(&error, &model.id)),
			Ok(DeferredStep::Pending(handle)) => {
				self.stream_or_error(&outer, deferred_message(&model, handle), cancel.as_ref(), &model.id)
					.await;
			}
			Ok(DeferredStep::Final(message)) => self.stream_or_error(&outer, message, cancel.as_ref(), &model.id).await,
			Ok(DeferredStep::Resolve(step, context, mut submission, entry_model)) => {
				// The submission's deferred flag, cancellation and response callback belong to the submission.
				submission.deferred = None;
				submission.stream.request.cancel = None;
				submission.stream.request.on_response = None;
				let message = match self.resolve_response(&step, &context, &submission, &entry_model).await {
					Ok(message) => message,
					Err(error) => self.error_message(&error, &entry_model.id),
				};
				if let Some(entry) = self.lock().deferred.get_mut(&handle.id) {
					entry.final_message = Some(message.clone());
				}
				self.stream_or_error(&outer, message, cancel.as_ref(), &model.id).await;
			}
		}
	}

	fn deferred_step(&self, handle: &DeferredHandle) -> Result<DeferredStep, String> {
		let mut inner = self.lock();
		let entry = inner
			.deferred
			.get_mut(&handle.id)
			.filter(|entry| {
				entry.handle.provider == handle.provider
					&& entry.handle.model_id == handle.model_id
					&& entry.handle.api == handle.api
			})
			.ok_or_else(|| format!("Unknown faux deferred response: {}", handle.id))?;
		if entry.cancelled {
			return Err(format!("Faux deferred response was cancelled: {}", handle.id));
		}
		if entry.pending_fetches > 0 {
			entry.pending_fetches -= 1;
			return Ok(DeferredStep::Pending(entry.handle.clone()));
		}
		Ok(match &entry.final_message {
			Some(message) => DeferredStep::Final(message.clone()),
			None => DeferredStep::Resolve(
				entry.step.clone(),
				entry.context.clone(),
				entry.options.clone(),
				entry.model.clone(),
			),
		})
	}
}

#[allow(clippy::large_enum_variant, reason = "short-lived, one per fetch")]
enum DeferredStep {
	Pending(DeferredHandle),
	Final(AssistantMessage),
	Resolve(FauxResponseStep, TranscriptContext, SimpleStreamOptions, Model),
}

#[async_trait]
impl ProviderStreams for FauxCore {
	fn deferred(&self) -> DeferredSupport {
		DeferredSupport {
			fetch: true,
			cancel: true,
		}
	}

	fn stream(&self, model: &Model, context: TranscriptContext, options: StreamOptions) -> AssistantMessageEventStream {
		self.stream_simple(
			model,
			context,
			SimpleStreamOptions {
				stream: options,
				..SimpleStreamOptions::default()
			},
		)
	}

	fn stream_simple(
		&self,
		model: &Model,
		context: TranscriptContext,
		options: SimpleStreamOptions,
	) -> AssistantMessageEventStream {
		let outer = AssistantMessageEventStream::new();
		let step = {
			let mut inner = self.lock();
			inner.state.call_count += 1;
			inner.pending.pop_front()
		};
		let core = self.self_arc();
		tokio::spawn(core.run_stream(outer.clone(), model.clone(), context, options, step));
		outer
	}

	fn fetch_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: DeferredFetchOptions,
	) -> AssistantMessageEventStream {
		let outer = AssistantMessageEventStream::new();
		self.lock().state.deferred_fetch_count += 1;
		tokio::spawn(
			self.self_arc()
				.run_fetch(outer.clone(), model.clone(), handle.clone(), options),
		);
		outer
	}

	async fn cancel_deferred(
		&self,
		model: &Model,
		handle: &DeferredHandle,
		options: ProviderRequestOptions,
	) -> Result<(), ModelsError> {
		{
			let mut inner = self.lock();
			inner.state.cancelled_deferred.push(handle.clone());
			if let Some(entry) = inner.deferred.get_mut(&handle.id) {
				entry.cancelled = true;
			}
		}
		notify_response(options.on_response.as_ref(), model).await;
		Ok(())
	}
}

impl FauxCore {
	/// The core as an `Arc`, for spawned work: trait methods take `&self`, and the core lives in the `Arc` that
	/// `faux_provider` made, which the provider holds while its methods run.
	fn self_arc(&self) -> Arc<Self> {
		// invariant: see above; the weak handle upgrades while any provider method runs.
		self.this
			.upgrade()
			.unwrap_or_else(|| unreachable!("faux core used after its provider was dropped"))
	}
}

/// How chunks are sized and paced.
#[derive(Debug, Clone, Copy)]
struct Pacing {
	min: u64,
	max: u64,
	tokens_per_second: Option<f64>,
}

async fn notify_response(on_response: Option<&OnResponse>, model: &Model) {
	if let Some(on_response) = on_response {
		on_response(
			ProviderResponse {
				status: 200,
				headers: Record::new(),
			},
			ModelRef::Chat(model),
		)
		.await;
	}
}

fn faux_model(definition: FauxModelDefinition, api: &str, provider: &str) -> Model {
	Model {
		name: definition.name.unwrap_or_else(|| definition.id.clone()),
		id: definition.id,
		api: api.to_owned(),
		provider: provider.to_owned(),
		base_url: DEFAULT_BASE_URL.into(),
		model_type: None,
		reasoning: definition.reasoning.unwrap_or(false),
		input: definition
			.input
			.unwrap_or_else(|| vec![ModelInputModality::Text, ModelInputModality::Image]),
		cost: definition.cost.unwrap_or_else(zero_cost),
		context_window: definition.context_window.unwrap_or(128_000),
		max_tokens: definition.max_tokens.unwrap_or(16_384),
		headers: None,
		compat: None,
		thinking_level_map: None,
		prompt_cache: None,
		input_limits: definition.input_limits,
		sampling_params: None,
		sampling_params_by_thinking_level: None,
		extra: JsonObject::new(),
	}
}

fn zero_cost() -> ModelCost {
	ModelCost {
		input: 0.0,
		output: 0.0,
		cache_read: 0.0,
		cache_write: 0.0,
		tiers: None,
		extra: JsonObject::new(),
	}
}

fn deferred_message(model: &Model, handle: DeferredHandle) -> AssistantMessage {
	let mut message = assistant_message(
		model.api.clone(),
		model.provider.clone(),
		model.id.clone(),
		StopReason::Deferred,
		now_ms(),
	);
	message.deferred = Some(handle);
	message
}

/// `${prefix}:${Date.now()}:${Math.random().toString(36).slice(2)}`.
fn random_id(prefix: &str) -> String {
	let mut bytes = [0u8; 8];
	let _ = getrandom::fill(&mut bytes);
	let mut value = u64::from_le_bytes(bytes);
	let mut digits = String::new();
	while value > 0 {
		let digit = u32::try_from(value % 36).unwrap_or(0);
		digits.push(char::from_digit(digit, 36).unwrap_or('0'));
		value /= 36;
	}
	format!("{prefix}:{}:{digits}", now_ms())
}

fn random_unit() -> f64 {
	let mut bytes = [0u8; 8];
	let _ = getrandom::fill(&mut bytes);
	#[allow(clippy::cast_precision_loss, reason = "53 random bits fit an f64 exactly")]
	let value = (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64;
	value
}

fn ceil_quarter(length: usize) -> i64 {
	i64::try_from(length.div_ceil(4)).unwrap_or(i64::MAX)
}

fn role(message: &Message) -> &'static str {
	match message {
		Message::System(_) => "system",
		Message::User(_) => "user",
		Message::Assistant(_) => "assistant",
		Message::ToolResult(_) => "toolResult",
	}
}

fn json_text<T: Serialize>(value: &T) -> String {
	serde_json::to_value(value)
		.map(|value| pi_json::to_js_string(&value))
		.unwrap_or_default()
}

fn content_to_text(blocks: &[ContentBlock]) -> String {
	blocks
		.iter()
		.map(|block| match block {
			ContentBlock::Text(text) => text.text.clone(),
			ContentBlock::Image(image) => format!("[image:{}:{}]", image.mime_type, utf16_len(&image.data)),
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn assistant_content_to_text(content: &[AssistantContentBlock]) -> String {
	content
		.iter()
		.map(|block| match block {
			AssistantContentBlock::Text(text) => text.text.clone(),
			AssistantContentBlock::Thinking(thinking) => thinking.thinking.clone(),
			AssistantContentBlock::ToolCall(call) => format!("{}:{}", call.name, json_text(&call.arguments)),
		})
		.collect::<Vec<_>>()
		.join("\n")
}

fn message_to_text(message: &Message) -> String {
	match message {
		Message::System(system) => {
			let mut parts = vec![get_system_message_text(system)];
			parts.extend(
				system
					.tools_removed
					.iter()
					.flatten()
					.map(|tool| format!("tool-:{}", json_text(tool))),
			);
			parts.extend(
				system
					.tools_added
					.iter()
					.flatten()
					.map(|tool| format!("tool+:{}", json_text(tool))),
			);
			parts.retain(|part| !part.is_empty());
			parts.join("\n")
		}
		Message::User(user) => match &user.content {
			MessageContent::Text(text) => text.clone(),
			MessageContent::Blocks(blocks) => content_to_text(blocks),
		},
		Message::Assistant(assistant) => assistant_content_to_text(&assistant.content),
		Message::ToolResult(result) => {
			let mut parts = vec![result.tool_name.clone()];
			parts.extend(
				result
					.content
					.iter()
					.map(|block| content_to_text(std::slice::from_ref(block))),
			);
			parts.join("\n")
		}
	}
}

/// The length of the prompt that joins the first `count` messages with blank lines.
fn joined_length(messages: &[String], count: usize) -> usize {
	let separators = if count > 0 { (count - 1) * 2 } else { 0 };
	separators
		+ messages[..count]
			.iter()
			.map(|message| utf16_len(message))
			.sum::<usize>()
}

/// The common prefix of two joined prompts: equal messages compared whole, characters from the first difference.
fn common_prompt_prefix_length(previous: &[String], current: &[String]) -> usize {
	let mut index = 0;
	while index < previous.len() && index < current.len() && previous[index] == current[index] {
		index += 1;
	}
	let rest = |messages: &[String]| {
		if index == messages.len() {
			return String::new();
		}
		let separator = if index > 0 { "\n\n" } else { "" };
		format!("{separator}{}", messages[index..].join("\n\n"))
	};
	let (a, b) = (rest(previous), rest(current));
	joined_length(previous, index)
		+ a.encode_utf16()
			.zip(b.encode_utf16())
			.take_while(|(x, y)| x == y)
			.count()
}

/// Chunks of `tokenSize * 4` UTF-16 units, a random token size per chunk; one empty chunk for empty text.
fn split_by_token_size(text: &str, pacing: Pacing) -> Vec<String> {
	let mut chunks = Vec::new();
	let mut rest = text;
	while !rest.is_empty() {
		#[allow(
			clippy::cast_precision_loss,
			clippy::cast_possible_truncation,
			clippy::cast_sign_loss,
			reason = "small token sizes"
		)]
		let token_size = pacing.min + (random_unit() * (pacing.max - pacing.min + 1) as f64).floor() as u64;
		let size = usize::try_from(token_size * 4).unwrap_or(usize::MAX).max(1);
		let chunk = utf16_prefix(rest, size);
		let chunk = if chunk.is_empty() {
			&rest[..rest.chars().next().map_or(0, char::len_utf8)]
		} else {
			chunk
		};
		chunks.push(chunk.to_owned());
		rest = &rest[chunk.len()..];
	}
	if chunks.is_empty() {
		chunks.push(String::new());
	}
	chunks
}

async fn schedule_chunk(chunk: &str, tokens_per_second: Option<f64>) {
	match tokens_per_second.filter(|rate| *rate > 0.0) {
		None => tokio::task::yield_now().await,
		Some(rate) => {
			#[allow(clippy::cast_precision_loss, reason = "chunk token counts are tiny")]
			let delay = ceil_quarter(utf16_len(chunk)) as f64 / rate;
			tokio::time::sleep(Duration::from_secs_f64(delay)).await;
		}
	}
}

/// Ends the stream as aborted, from the partial so far; `true` when it did.
fn abort_if_cancelled(
	stream: &AssistantMessageEventStream,
	partial: &PartialMessage,
	cancel: Option<&CancellationToken>,
) -> bool {
	if !cancel.is_some_and(CancellationToken::is_cancelled) {
		return false;
	}
	let mut aborted = partial.get().clone();
	aborted.stop_reason = StopReason::Aborted;
	aborted.error_message = Some("Request was aborted".into());
	aborted.timestamp = now_ms();
	stream.push_error(ErrorReason::Aborted, aborted.clone());
	stream.end(Some(aborted));
	true
}

/// Streams `message` block by block in chunks (`streamWithDeltas`); `Err` for a message without a stop reason.
async fn stream_with_deltas(
	stream: &AssistantMessageEventStream,
	message: AssistantMessage,
	pacing: Pacing,
	cancel: Option<&CancellationToken>,
) -> Result<(), String> {
	let mut start = message.clone();
	start.content = Vec::new();
	start.stop_reason = StopReason::Pending;
	let mut partial = PartialMessage::new(start);
	if abort_if_cancelled(stream, &partial, cancel) {
		return Ok(());
	}
	stream.push_start(&partial);
	for (index, block) in message.content.iter().enumerate() {
		if abort_if_cancelled(stream, &partial, cancel) {
			return Ok(());
		}
		let finished = match block {
			AssistantContentBlock::Thinking(thinking) => {
				stream_text_block(
					stream,
					&mut partial,
					index,
					TextKind::Thinking,
					&thinking.thinking,
					pacing,
					cancel,
				)
				.await
			}
			AssistantContentBlock::Text(text) => {
				stream_text_block(stream, &mut partial, index, TextKind::Text, &text.text, pacing, cancel).await
			}
			AssistantContentBlock::ToolCall(call) => {
				stream_tool_call(stream, &mut partial, index, call, pacing, cancel).await
			}
		};
		if !finished {
			return Ok(());
		}
	}
	match message.stop_reason {
		StopReason::Pending => Err("Faux response ended without a stop reason".into()),
		StopReason::Error | StopReason::Aborted => {
			let reason = if message.stop_reason == StopReason::Error {
				ErrorReason::Error
			} else {
				ErrorReason::Aborted
			};
			stream.push_error(reason, message.clone());
			stream.end(Some(message));
			Ok(())
		}
		reason => {
			let reason = match reason {
				StopReason::Length => DoneReason::Length,
				StopReason::ToolUse => DoneReason::ToolUse,
				StopReason::Deferred => DoneReason::Deferred,
				_ => DoneReason::Stop,
			};
			stream.push_done(reason, message.clone());
			stream.end(Some(message));
			Ok(())
		}
	}
}

/// Streams a text or thinking block; `false` when the stream was aborted.
async fn stream_text_block(
	stream: &AssistantMessageEventStream,
	partial: &mut PartialMessage,
	index: usize,
	kind: TextKind,
	text: &str,
	pacing: Pacing,
	cancel: Option<&CancellationToken>,
) -> bool {
	let (block, block_kind) = match kind {
		TextKind::Text => (faux_text(""), BlockKind::Text),
		TextKind::Thinking => (faux_thinking(""), BlockKind::Thinking),
	};
	partial.edit().content.push(block);
	stream.push_block_start(block_kind, index, partial);
	for chunk in split_by_token_size(text, pacing) {
		schedule_chunk(&chunk, pacing.tokens_per_second).await;
		if abort_if_cancelled(stream, partial, cancel) {
			return false;
		}
		match partial.edit().content.get_mut(index) {
			Some(AssistantContentBlock::Text(block)) => block.text.push_str(&chunk),
			Some(AssistantContentBlock::Thinking(block)) => block.thinking.push_str(&chunk),
			_ => {}
		}
		stream.push_block_delta(block_kind, index, &chunk, partial);
	}
	stream.push_text_end(kind, index, text, partial);
	true
}

/// Streams a tool call; partial arguments come from the parser real providers use. `false` when aborted.
async fn stream_tool_call(
	stream: &AssistantMessageEventStream,
	partial: &mut PartialMessage,
	index: usize,
	call: &ToolCall,
	pacing: Pacing,
	cancel: Option<&CancellationToken>,
) -> bool {
	partial
		.edit()
		.content
		.push(faux_tool_call(&call.name, JsonObject::new(), Some(&call.id)));
	stream.push_block_start(BlockKind::ToolCall, index, partial);
	let mut arguments = ToolCallArgumentsStream::new();
	for chunk in split_by_token_size(
		&pi_json::to_js_string(&JsonValue::Object(call.arguments.clone())),
		pacing,
	) {
		schedule_chunk(&chunk, pacing.tokens_per_second).await;
		if abort_if_cancelled(stream, partial, cancel) {
			return false;
		}
		if let Some(shown) = arguments.append(&chunk)
			&& let Some(AssistantContentBlock::ToolCall(block)) = partial.edit().content.get_mut(index)
		{
			block.arguments.clone_from(shown);
		}
		stream.push_block_delta(BlockKind::ToolCall, index, &chunk, partial);
	}
	if let Some(AssistantContentBlock::ToolCall(block)) = partial.edit().content.get_mut(index) {
		block.arguments.clone_from(&call.arguments);
	}
	stream.push_tool_call_end(index, call.clone(), partial);
	true
}
