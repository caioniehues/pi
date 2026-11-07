//! The HTTP server: serves the script's responses in request order on the provider endpoints, records every
//! request to `requests.jsonl`, and holds paused streams until the driver releases them.

use crate::anthropic::render_anthropic;
use crate::chunking::writes;
use crate::openai_completions::render_openai_completions;
use crate::script::{Script, ScriptResponse};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pi_json::{JsonObject, JsonValue};
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;

/// Why the server could not start.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
	/// The output directory or `requests.jsonl` could not be created.
	#[error("cannot create {path}: {source}")]
	Output {
		/// The path.
		path: String,
		/// The I/O error.
		source: std::io::Error,
	},
	/// The listening socket could not be bound.
	#[error("cannot listen on {addr}: {source}")]
	Bind {
		/// The address.
		addr: SocketAddr,
		/// The I/O error.
		source: std::io::Error,
	},
}

/// A running server: its address, the release gate for paused streams, and shutdown.
pub struct ServerHandle {
	addr: SocketAddr,
	state: Arc<ServerState>,
	shutdown: oneshot::Sender<()>,
	task: JoinHandle<()>,
}

impl ServerHandle {
	/// The address the server listens on.
	pub fn addr(&self) -> SocketAddr {
		self.addr
	}

	/// Lets one paused stream (the current or the next one to pause) continue.
	pub fn release(&self) {
		self.state.gate.add_permits(1);
	}

	/// How many provider requests the server has answered from the script so far.
	pub fn served(&self) -> usize {
		*self.state.next.lock().unwrap_or_else(PoisonError::into_inner)
	}

	/// Stops accepting connections and waits for the server task to end; paused streams are let go first, so a
	/// stream nobody released (an aborted request) cannot keep the graceful shutdown waiting.
	pub async fn shutdown(self) {
		self.state.gate.close();
		// A send fails only when the server task has already ended, which is the goal anyway.
		let _ = self.shutdown.send(());
		let _ = self.task.await;
	}
}

/// Starts a server for `script` on an ephemeral port of 127.0.0.1, recording requests to `<out>/requests.jsonl`.
pub async fn start(script: Script, out: &Path) -> Result<(SocketAddr, ServerHandle), ServerError> {
	start_on(script, out, SocketAddr::from(([127, 0, 0, 1], 0))).await
}

/// `start` on a given address.
pub async fn start_on(script: Script, out: &Path, addr: SocketAddr) -> Result<(SocketAddr, ServerHandle), ServerError> {
	let requests_path = out.join("requests.jsonl");
	let output_error = |source| ServerError::Output {
		path: requests_path.display().to_string(),
		source,
	};
	std::fs::create_dir_all(out).map_err(output_error)?;
	let requests = File::create(&requests_path).map_err(output_error)?;
	let state = Arc::new(ServerState {
		script,
		next: Mutex::new(0),
		requests: Mutex::new(requests),
		gate: Semaphore::new(0),
	});
	let listener = tokio::net::TcpListener::bind(addr)
		.await
		.map_err(|source| ServerError::Bind { addr, source })?;
	let addr = listener
		.local_addr()
		.map_err(|source| ServerError::Bind { addr, source })?;
	let app = Router::new()
		.route("/v1/messages", post(anthropic_messages))
		.route("/v1/chat/completions", post(openai_completions))
		.route("/v1/responses", post(responses_reserved))
		.route("/control/release", post(release))
		.fallback(unknown)
		.with_state(state.clone());
	let (shutdown, stopped) = oneshot::channel::<()>();
	let task = tokio::spawn(async move {
		let serve = axum::serve(listener, app).with_graceful_shutdown(async {
			let _ = stopped.await;
		});
		// Serving fails only on an accept error of the bound socket; the driver sees it as missing responses.
		let _ = serve.await;
	});
	Ok((
		addr,
		ServerHandle {
			addr,
			state: state.clone(),
			shutdown,
			task,
		},
	))
}

struct ServerState {
	script: Script,
	/// The index of the response the next provider request gets.
	next: Mutex<usize>,
	requests: Mutex<File>,
	/// Permits for paused streams; `release` adds one.
	gate: Semaphore,
}

/// The wire formats the provider endpoints render.
#[derive(Clone, Copy)]
enum Api {
	AnthropicMessages,
	OpenAiCompletions,
}

async fn anthropic_messages(
	State(state): State<Arc<ServerState>>,
	method: Method,
	uri: Uri,
	headers: HeaderMap,
	body: Bytes,
) -> Response {
	serve(
		Api::AnthropicMessages,
		&state,
		&record(&state, &method, &uri, &headers, &body),
	)
}

async fn openai_completions(
	State(state): State<Arc<ServerState>>,
	method: Method,
	uri: Uri,
	headers: HeaderMap,
	body: Bytes,
) -> Response {
	serve(
		Api::OpenAiCompletions,
		&state,
		&record(&state, &method, &uri, &headers, &body),
	)
}

/// `/v1/responses` (openai-responses) is reserved for a later slice: the route exists so the request is recorded.
async fn responses_reserved(
	State(state): State<Arc<ServerState>>,
	method: Method,
	uri: Uri,
	headers: HeaderMap,
	body: Bytes,
) -> Response {
	record(&state, &method, &uri, &headers, &body);
	plain(
		StatusCode::NOT_IMPLEMENTED,
		"fake provider: /v1/responses is not implemented yet",
	)
}

async fn release(State(state): State<Arc<ServerState>>) -> StatusCode {
	state.gate.add_permits(1);
	StatusCode::NO_CONTENT
}

/// Any other path is recorded too, so a wrong `baseUrl` shows up in `requests.jsonl`.
async fn unknown(
	State(state): State<Arc<ServerState>>,
	method: Method,
	uri: Uri,
	headers: HeaderMap,
	body: Bytes,
) -> Response {
	record(&state, &method, &uri, &headers, &body);
	plain(
		StatusCode::NOT_FOUND,
		&format!("fake provider: no route for {method} {uri}"),
	)
}

/// Appends the request to `requests.jsonl` and returns its parsed body (`Null` when it is not JSON).
fn record(state: &ServerState, method: &Method, uri: &Uri, headers: &HeaderMap, body: &Bytes) -> JsonValue {
	let mut grouped: BTreeMap<&str, Vec<String>> = BTreeMap::new();
	for (name, value) in headers {
		grouped
			.entry(name.as_str())
			.or_default()
			.push(String::from_utf8_lossy(value.as_bytes()).into_owned());
	}
	let mut header_object = JsonObject::new();
	for (name, values) in grouped {
		header_object.insert(name.to_owned(), JsonValue::String(values.join(", ")));
	}
	let text = String::from_utf8_lossy(body);
	let mut entry = JsonObject::new();
	entry.insert("method".into(), JsonValue::String(method.to_string()));
	entry.insert("url".into(), JsonValue::String(uri.to_string()));
	entry.insert("headers".into(), JsonValue::Object(header_object));
	// Wire input: the body is whatever the pi under test sent.
	let parsed = pi_json::parse(&text).unwrap_or(JsonValue::Null);
	if parsed.is_null() {
		entry.insert("body".into(), JsonValue::String(text.into_owned()));
	} else {
		entry.insert("body".into(), parsed.clone());
		// The record re-serializes the body; keep the sent text when that would hide a formatting difference.
		if pi_json::to_js_string(&parsed) != text {
			entry.insert("bodyText".into(), JsonValue::String(text.into_owned()));
		}
	}
	let line = pi_json::to_js_string(&JsonValue::Object(entry)) + "\n";
	let mut file = state.requests.lock().unwrap_or_else(PoisonError::into_inner);
	// A failed write leaves the record out of requests.jsonl, which the comparison then reports.
	let _ = file.write_all(line.as_bytes()).and_then(|()| file.flush());
	parsed
}

fn serve(api: Api, state: &Arc<ServerState>, body: &JsonValue) -> Response {
	let index = {
		let mut next = state.next.lock().unwrap_or_else(PoisonError::into_inner);
		let index = *next;
		*next += 1;
		index
	};
	let Some(response) = state.script.responses.get(index) else {
		let message = format!(
			"fake provider: script exhausted: request {} but the script has {} responses",
			index + 1,
			state.script.responses.len()
		);
		return plain(StatusCode::INTERNAL_SERVER_ERROR, &message);
	};
	if let Some(error) = &response.error {
		return error_reply(error.status, &error.body, error.retry_after);
	}
	let model = body.get("model").and_then(JsonValue::as_str).unwrap_or_default();
	let number = index + 1;
	let events = match api {
		Api::AnthropicMessages => render_anthropic(response, &format!("msg_fake_{number}"), model),
		Api::OpenAiCompletions => render_openai_completions(response, &format!("chatcmpl-fake-{number}"), model),
	};
	let delay = Duration::from_millis(response.delay_ms.unwrap_or(state.script.delay_ms));
	stream(response, writes(events, response.chunking), delay, state.clone())
}

fn error_reply(status: u16, body: &JsonValue, retry_after: Option<u64>) -> Response {
	// `load_script` admits 400 to 599 only, all valid; a `Script` built in code with another status gets a 500.
	let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
	let mut response = match body {
		JsonValue::String(text) => plain(status, text),
		other => {
			let mut response = (status, pi_json::to_js_string(other)).into_response();
			response
				.headers_mut()
				.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
			response
		}
	};
	if let Some(seconds) = retry_after {
		response
			.headers_mut()
			.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
	}
	response
}

/// The writes as an SSE body: `delay` before each write, and a stop for a release after `pauseAfter` writes.
fn stream(response: &ScriptResponse, writes: Vec<Bytes>, delay: Duration, state: Arc<ServerState>) -> Response {
	let pause_after = response.pause_after;
	let pieces = futures_util::stream::unfold((writes.into_iter(), 0_usize), move |(mut rest, sent)| {
		let state = state.clone();
		async move {
			let piece = rest.next()?;
			if pause_after == Some(sent)
				&& let Ok(permit) = state.gate.acquire().await
			{
				permit.forget();
			}
			if !delay.is_zero() {
				tokio::time::sleep(delay).await;
			}
			Some((Ok::<Bytes, Infallible>(piece), (rest, sent + 1)))
		}
	});
	let mut response = Body::from_stream(pieces).into_response();
	let headers = response.headers_mut();
	headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
	headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
	response
}

fn plain(status: StatusCode, text: &str) -> Response {
	let mut response = (status, text.to_owned()).into_response();
	response.headers_mut().insert(
		header::CONTENT_TYPE,
		HeaderValue::from_static("text/plain; charset=utf-8"),
	);
	response
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::script::{Block, ErrorReply, StopReason, Usage};
	use serde_json::json;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	fn text(text: &str, pause_after: Option<usize>) -> ScriptResponse {
		ScriptResponse {
			content: vec![Block::Text { text: text.into() }],
			stop_reason: StopReason::Stop,
			usage: Usage::default(),
			delay_ms: None,
			chunking: None,
			pause_after,
			error: None,
		}
	}

	fn out_dir(name: &str) -> std::path::PathBuf {
		let dir = std::env::temp_dir().join(format!("pi-fake-provider-test-{}-{name}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		dir
	}

	/// One HTTP/1.1 request with `Connection: close`; the whole response as text.
	async fn post(addr: SocketAddr, path: &str, body: &str) -> String {
		let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
		let request = format!(
			"POST {path} HTTP/1.1\r\nhost: {addr}\r\nx-api-key: k\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
			body.len()
		);
		stream.write_all(request.as_bytes()).await.unwrap();
		let mut response = String::new();
		stream.read_to_string(&mut response).await.unwrap();
		response
	}

	#[tokio::test]
	async fn serves_responses_in_order_and_records_requests() {
		let out = out_dir("order");
		let error = ScriptResponse {
			error: Some(ErrorReply {
				status: 429,
				body: json!({ "error": "slow down" }),
				retry_after: Some(2),
			}),
			..text("", None)
		};
		let script = Script {
			delay_ms: 0,
			responses: vec![error, text("hi", None)],
		};
		let (addr, handle) = start(script, &out).await.unwrap();
		let first = post(addr, "/v1/messages?beta=true", r#"{"model":"m"}"#).await;
		assert!(first.starts_with("HTTP/1.1 429"), "{first}");
		assert!(first.contains("retry-after: 2\r\n"));
		assert!(first.ends_with(r#"{"error":"slow down"}"#));
		let second = post(addr, "/v1/chat/completions", r#"{"model": "m"}"#).await;
		assert!(second.contains("content-type: text/event-stream"));
		assert!(
			second
				.contains(r#"data: {"id":"chatcmpl-fake-2","object":"chat.completion.chunk","created":0,"model":"m""#)
		);
		assert!(second.contains(r#""delta":{"content":"hi"}"#));
		let third = post(addr, "/v1/messages", "{}").await;
		assert!(third.starts_with("HTTP/1.1 500"));
		assert!(third.contains("script exhausted: request 3 but the script has 2 responses"));
		assert_eq!(handle.served(), 3);
		handle.shutdown().await;
		let records: Vec<JsonValue> = std::fs::read_to_string(out.join("requests.jsonl"))
			.unwrap()
			.lines()
			.map(|line| pi_json::parse(line).unwrap())
			.collect();
		assert_eq!(records.len(), 3);
		assert_eq!(records[0]["url"], "/v1/messages?beta=true");
		assert_eq!(records[0]["headers"]["x-api-key"], "k");
		assert_eq!(records[0]["body"], json!({ "model": "m" }));
		assert!(records[0].get("bodyText").is_none());
		// The body was not sent in `JSON.stringify` form, so the record keeps the text.
		assert_eq!(records[1]["bodyText"], r#"{"model": "m"}"#);
		let _ = std::fs::remove_dir_all(&out);
	}

	#[tokio::test]
	async fn a_paused_stream_continues_after_release() {
		let out = out_dir("pause");
		let (addr, handle) = start(
			Script {
				delay_ms: 0,
				responses: vec![text("hi", Some(1))],
			},
			&out,
		)
		.await
		.unwrap();
		let request = tokio::spawn(post(addr, "/v1/messages", r#"{"model":"m"}"#));
		tokio::time::sleep(Duration::from_millis(100)).await;
		assert!(!request.is_finished());
		handle.release();
		let response = request.await.unwrap();
		assert!(response.contains("event: message_stop"));
		handle.shutdown().await;
		let _ = std::fs::remove_dir_all(&out);
	}

	#[tokio::test]
	async fn unknown_paths_are_recorded_and_refused() {
		let out = out_dir("unknown");
		let (addr, handle) = start(
			Script {
				delay_ms: 0,
				responses: Vec::new(),
			},
			&out,
		)
		.await
		.unwrap();
		assert!(post(addr, "/v1/responses", "{}").await.starts_with("HTTP/1.1 501"));
		assert!(post(addr, "/messages", "{}").await.starts_with("HTTP/1.1 404"));
		assert_eq!(handle.served(), 0);
		handle.shutdown().await;
		assert_eq!(
			std::fs::read_to_string(out.join("requests.jsonl"))
				.unwrap()
				.lines()
				.count(),
			2
		);
		let _ = std::fs::remove_dir_all(&out);
	}
}
