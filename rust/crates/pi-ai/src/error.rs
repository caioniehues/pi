//! The error of registry, auth and request setup operations (ports `ModelsError` of
//! `packages/ai/src/utils/models-error.ts`).

use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// What kind of operation failed (`ModelsErrorCode`), plus the Rust-only `Aborted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelsErrorCode {
	/// `model_source`: a model list could not be read or fetched.
	ModelSource,
	/// `model_validation`: a model definition is invalid.
	ModelValidation,
	/// `provider`: the provider is unknown or lacks the operation.
	Provider,
	/// `stream`: a stream could not be dispatched.
	Stream,
	/// `auth`: api-key resolution or the credential store failed.
	Auth,
	/// `oauth`: an OAuth refresh or auth derivation failed.
	Oauth,
	/// The operation's cancellation token fired (TS rejects with the signal's `AbortError`).
	Aborted,
}

/// A failed operation: its code and a message that carries the underlying reason.
#[derive(Debug, Clone)]
pub struct ModelsError {
	/// What kind of operation failed.
	pub code: ModelsErrorCode,
	/// The message, with the cause's detail appended (`withCauseDetail`).
	pub message: String,
	cause: Option<Arc<dyn Error + Send + Sync>>,
}

/// TS parity: the message of a default `AbortError`.
const ABORTED_MESSAGE: &str = "The operation was aborted";

impl ModelsError {
	/// An error without a cause.
	pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
		Self {
			code,
			message: message.into(),
			cause: None,
		}
	}

	/// An error caused by `cause`, whose message is appended unless `message` already contains it.
	pub fn with_cause(
		code: ModelsErrorCode,
		message: impl Into<String>,
		cause: impl Into<Arc<dyn Error + Send + Sync>>,
	) -> Self {
		let cause = cause.into();
		Self {
			code,
			message: with_cause_detail(message.into(), &cause.to_string()),
			cause: Some(cause),
		}
	}

	/// The error of an operation whose cancellation token fired.
	pub fn aborted() -> Self {
		Self::new(ModelsErrorCode::Aborted, ABORTED_MESSAGE)
	}

	/// Whether this is the error of a cancelled operation.
	pub fn is_aborted(&self) -> bool {
		self.code == ModelsErrorCode::Aborted
	}
}

/// Callers surface `message` only, so the underlying reason is kept in it.
fn with_cause_detail(message: String, cause: &str) -> String {
	let detail = cause.trim();
	if detail.is_empty() || message.contains(detail) {
		message
	} else {
		format!("{message}: {detail}")
	}
}

impl fmt::Display for ModelsError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.message)
	}
}

impl Error for ModelsError {
	fn source(&self) -> Option<&(dyn Error + 'static)> {
		self.cause.as_deref().map(|cause| cause as &(dyn Error + 'static))
	}
}

/// A plain error with a message, for causes and failures that carry nothing else (a thrown `new Error(message)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageError(pub String);

impl fmt::Display for MessageError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

impl Error for MessageError {}

impl From<MessageError> for Arc<dyn Error + Send + Sync> {
	fn from(error: MessageError) -> Self {
		Arc::new(error)
	}
}

impl From<ModelsError> for Arc<dyn Error + Send + Sync> {
	fn from(error: ModelsError) -> Self {
		Arc::new(error)
	}
}
