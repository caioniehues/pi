//! The HTTP client every wire API sends through: one `reqwest::Client` factory with pi's defaults, and the `Fetch`
//! trait that request options can override so tests and fake providers intercept requests (the `fetch` option of
//! `ProviderRequestOptions`).
//!
//! Defaults follow `packages/coding-agent/src/core/http-dispatcher.ts` (undici's `EnvHttpProxyAgent`): HTTP/1.1 only
//! (`allowH2: false`), headers and body idle timeouts of 300 seconds (0 disables them), undici's 10-second connect
//! timeout and 4-second keep-alive, proxies from `HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`, gzip and zstd decoding.
//! Redirects are followed only to the same origin (scheme, host, port), up to undici's 20; a cross-origin 3xx is
//! returned as the response, so credentials such as `x-api-key`, which neither undici nor reqwest strips, never reach
//! another origin (decided in the C3a review). TLS is rustls with the Mozilla roots Node bundles (webpki-roots); no
//! OpenSSL.

use crate::headers::{ProviderHeaders, provider_headers_to_record};
use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use pi_types::Record;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A request ready to send.
#[derive(Debug, Clone)]
pub struct HttpRequest {
	/// The method.
	pub method: http::Method,
	/// The absolute URL.
	pub url: String,
	/// The headers.
	pub headers: HeaderMap,
	/// The body; empty for none.
	pub body: Bytes,
	/// How long to wait for the response headers (`timeoutMs`, as the SDKs' `fetchWithTimeout`); the body is bounded
	/// by the client's idle timeout only.
	pub timeout_ms: Option<u64>,
}

/// A response body as it arrives.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, HttpError>> + Send>>;

/// A response whose body has not been read.
pub struct HttpResponse {
	/// The status.
	pub status: u16,
	/// The headers.
	pub headers: HeaderMap,
	/// The body.
	pub body: ByteStream,
}

/// Why a request or its body failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HttpError {
	/// The request could not be built (a bad URL or header).
	#[error("{0}")]
	Request(String),
	/// The connection failed (TS: `fetch failed` with the cause).
	#[error("{0}")]
	Connect(String),
	/// A timeout elapsed.
	#[error("{0}")]
	Timeout(String),
	/// Reading the body failed.
	#[error("{0}")]
	Body(String),
	/// The cancellation token fired.
	#[error("Request was aborted")]
	Aborted,
}

/// Sends HTTP requests; the default is [`ReqwestFetch`], request options may supply another.
#[async_trait]
pub trait Fetch: Send + Sync {
	/// Sends `request`; resolves when the response head arrives.
	async fn fetch(&self, request: HttpRequest, cancel: &CancellationToken) -> Result<HttpResponse, HttpError>;
}

/// Client settings; the defaults are pi's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpClientOptions {
	/// Headers and body idle timeout in milliseconds; 0 disables it.
	pub idle_timeout_ms: u64,
	/// Connect timeout in milliseconds.
	pub connect_timeout_ms: u64,
}

/// `DEFAULT_HTTP_IDLE_TIMEOUT_MS` of the coding agent's dispatcher.
pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;
/// undici's default connect timeout.
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
/// undici's default keep-alive timeout.
const KEEP_ALIVE_TIMEOUT_MS: u64 = 4_000;

impl Default for HttpClientOptions {
	fn default() -> Self {
		Self {
			idle_timeout_ms: DEFAULT_HTTP_IDLE_TIMEOUT_MS,
			connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
		}
	}
}

/// A client with pi's defaults (see the module comment).
pub fn create_http_client(options: HttpClientOptions) -> Result<reqwest::Client, HttpError> {
	let mut builder = reqwest::Client::builder()
		.http1_only()
		.redirect(same_origin_redirects())
		.connect_timeout(Duration::from_millis(options.connect_timeout_ms))
		.pool_idle_timeout(Duration::from_millis(KEEP_ALIVE_TIMEOUT_MS))
		.gzip(true)
		.zstd(true);
	if options.idle_timeout_ms > 0 {
		builder = builder.read_timeout(Duration::from_millis(options.idle_timeout_ms));
	}
	builder.build().map_err(|error| HttpError::Request(error.to_string()))
}

/// undici's redirect limit.
const MAX_REDIRECTS: usize = 20;

/// Follows a redirect only to the origin of the first request, at most [`MAX_REDIRECTS`] times.
fn same_origin_redirects() -> reqwest::redirect::Policy {
	reqwest::redirect::Policy::custom(|attempt| {
		let origin = |url: &url::Url| {
			(
				url.scheme().to_owned(),
				url.host_str().map(str::to_owned),
				url.port_or_known_default(),
			)
		};
		let same = attempt
			.previous()
			.first()
			.is_some_and(|first| origin(first) == origin(attempt.url()));
		if same && attempt.previous().len() <= MAX_REDIRECTS {
			attempt.follow()
		} else {
			attempt.stop()
		}
	})
}

/// [`Fetch`] over a `reqwest::Client`.
#[derive(Clone)]
pub struct ReqwestFetch {
	client: reqwest::Client,
}

impl ReqwestFetch {
	/// Sends through `client`.
	pub fn new(client: reqwest::Client) -> Self {
		Self { client }
	}
}

#[async_trait]
impl Fetch for ReqwestFetch {
	async fn fetch(&self, request: HttpRequest, cancel: &CancellationToken) -> Result<HttpResponse, HttpError> {
		let send = self
			.client
			.request(request.method, &request.url)
			.headers(request.headers)
			.body(request.body)
			.send();
		let headers = async {
			match request.timeout_ms {
				Some(timeout) => tokio::time::timeout(Duration::from_millis(timeout), send)
					.await
					.map_err(|_| HttpError::Timeout(REQUEST_TIMED_OUT.into()))?,
				None => send.await,
			}
			.map_err(|error| map_reqwest_error(&error))
		};
		let response = tokio::select! {
			biased;
			() = cancel.cancelled() => return Err(HttpError::Aborted),
			response = headers => response?,
		};
		let status = response.status().as_u16();
		let headers = response.headers().clone();
		let body = response
			.bytes_stream()
			.map(|chunk| chunk.map_err(|error| map_reqwest_error(&error)));
		Ok(HttpResponse {
			status,
			headers,
			body: cancellable(Box::pin(body), cancel.clone()),
		})
	}
}

/// The SDKs' `APIConnectionTimeoutError` message.
const REQUEST_TIMED_OUT: &str = "Request timed out.";

/// `body` that ends with `HttpError::Aborted` once `cancel` fires, as a fetch signal aborts the body too.
pub fn cancellable(body: ByteStream, cancel: CancellationToken) -> ByteStream {
	let state = (body, cancel, false);
	Box::pin(futures_util::stream::unfold(
		state,
		|(mut body, cancel, finished)| async move {
			if finished {
				return None;
			}
			tokio::select! {
				biased;
				() = cancel.cancelled() => Some((Err(HttpError::Aborted), (body, cancel, true))),
				chunk = body.next() => chunk.map(|chunk| (chunk, (body, cancel, false))),
			}
		},
	))
}

fn map_reqwest_error(error: &reqwest::Error) -> HttpError {
	let mut message = error.to_string();
	let mut source = std::error::Error::source(error);
	while let Some(cause) = source {
		message = format!("{message}: {cause}");
		source = cause.source();
	}
	if error.is_timeout() {
		HttpError::Timeout(message)
	} else if error.is_connect() {
		HttpError::Connect(message)
	} else if error.is_body() || error.is_decode() {
		HttpError::Body(message)
	} else {
		HttpError::Request(message)
	}
}

/// The process-wide default fetch, created on first use with the default options.
pub fn default_fetch() -> Result<Arc<dyn Fetch>, HttpError> {
	static DEFAULT: OnceLock<Result<Arc<dyn Fetch>, HttpError>> = OnceLock::new();
	DEFAULT
		.get_or_init(|| {
			create_http_client(HttpClientOptions::default())
				.map(|client| Arc::new(ReqwestFetch::new(client)) as Arc<dyn Fetch>)
		})
		.clone()
}

/// Request headers from sources merged in order, case-insensitively, where a later `None` removes a header
/// (`providerHeadersToRecord`), as an `http::HeaderMap`.
pub fn request_headers(sources: &[Option<&ProviderHeaders>]) -> Result<HeaderMap, HttpError> {
	let record = provider_headers_to_record(sources).unwrap_or_default();
	header_map(&record)
}

/// A header map from a record; invalid names or values fail.
pub fn header_map(record: &Record<String>) -> Result<HeaderMap, HttpError> {
	let mut headers = HeaderMap::new();
	for (name, value) in record.iter() {
		let name = HeaderName::from_bytes(name.as_bytes())
			.map_err(|error| HttpError::Request(format!("invalid header name {name}: {error}")))?;
		let value = HeaderValue::from_str(value)
			.map_err(|error| HttpError::Request(format!("invalid header value for {name}: {error}")))?;
		headers.insert(name, value);
	}
	Ok(headers)
}
