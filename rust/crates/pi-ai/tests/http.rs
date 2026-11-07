//! The HTTP client and fetch (`http.rs`): client defaults, header merging, `timeout_ms` on the response head only,
//! cancellation of the body, and same-origin redirects. Servers are raw HTTP/1.1 on loopback.

#![allow(
	clippy::unwrap_used,
	clippy::expect_used,
	reason = "test code: helpers that fail the test"
)]

use bytes::Bytes;
use futures_util::StreamExt;
use pi_ai::http::{
	Fetch, HttpClientOptions, HttpError, HttpRequest, ReqwestFetch, create_http_client, request_headers,
};
use std::future::Future;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// Serves every connection with `handle(path, stream)`; the base URL.
async fn serve<F, Fut>(handle: F) -> String
where
	F: Fn(String, TcpStream) -> Fut + Send + Sync + 'static,
	Fut: Future<Output = ()> + Send + 'static,
{
	let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
	let address = listener.local_addr().unwrap();
	let handle = std::sync::Arc::new(handle);
	tokio::spawn(async move {
		loop {
			let Ok((mut stream, _)) = listener.accept().await else {
				return;
			};
			let handle = std::sync::Arc::clone(&handle);
			tokio::spawn(async move {
				let mut head = Vec::new();
				let mut byte = [0u8; 1];
				while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap_or(0) == 1 {
					head.push(byte[0]);
				}
				let text = String::from_utf8_lossy(&head);
				let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
				handle(path, stream).await;
			});
		}
	});
	format!("http://{address}")
}

fn fetch() -> ReqwestFetch {
	ReqwestFetch::new(create_http_client(HttpClientOptions::default()).unwrap())
}

fn get(url: &str, timeout_ms: Option<u64>) -> HttpRequest {
	HttpRequest {
		method: http::Method::GET,
		url: url.to_owned(),
		headers: http::HeaderMap::new(),
		body: Bytes::new(),
		timeout_ms,
	}
}

async fn write(stream: &mut TcpStream, text: &str) {
	stream.write_all(text.as_bytes()).await.unwrap();
	stream.flush().await.unwrap();
}

const CHUNKED: &str = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n";

#[test]
fn builds_the_default_client_without_network() {
	assert!(create_http_client(HttpClientOptions::default()).is_ok());
	assert!(
		create_http_client(HttpClientOptions {
			idle_timeout_ms: 0,
			connect_timeout_ms: 1000
		})
		.is_ok()
	);
}

#[test]
fn request_headers_merge_case_insensitively_and_null_removes_a_default() {
	let defaults = [("User-Agent", Some("pi".to_owned())), ("X-Keep", Some("1".to_owned()))]
		.into_iter()
		.collect();
	let request = [("user-agent", None), ("x-keep", Some("2".to_owned()))]
		.into_iter()
		.collect();
	let headers = request_headers(&[Some(&defaults), Some(&request)]).unwrap();
	assert!(headers.get("user-agent").is_none());
	assert_eq!(headers.get("x-keep").unwrap(), "2");
	assert_eq!(headers.len(), 1);
}

#[tokio::test]
async fn timeout_ms_bounds_the_wait_for_the_response_head() {
	let base = serve(|_, mut stream| async move {
		tokio::time::sleep(Duration::from_millis(500)).await;
		write(&mut stream, CHUNKED).await;
	})
	.await;
	let result = fetch().fetch(get(&base, Some(100)), &CancellationToken::new()).await;
	assert!(matches!(result, Err(HttpError::Timeout(_))));
}

#[tokio::test]
async fn timeout_ms_does_not_bound_a_slow_body() {
	let base = serve(|_, mut stream| async move {
		write(&mut stream, CHUNKED).await;
		write(&mut stream, "1\r\na\r\n").await;
		tokio::time::sleep(Duration::from_millis(300)).await;
		write(&mut stream, "1\r\nb\r\n0\r\n\r\n").await;
	})
	.await;
	let response = fetch()
		.fetch(get(&base, Some(100)), &CancellationToken::new())
		.await
		.unwrap();
	let mut body = Vec::new();
	let mut chunks = response.body;
	while let Some(chunk) = chunks.next().await {
		body.extend_from_slice(&chunk.unwrap());
	}
	assert_eq!(body, b"ab");
}

#[tokio::test]
async fn cancellation_stops_reading_the_body() {
	let base = serve(|_, mut stream| async move {
		write(&mut stream, CHUNKED).await;
		write(&mut stream, "1\r\na\r\n").await;
		tokio::time::sleep(Duration::from_secs(30)).await;
	})
	.await;
	let cancel = CancellationToken::new();
	let response = fetch().fetch(get(&base, None), &cancel).await.unwrap();
	let mut body = response.body;
	assert_eq!(body.next().await.unwrap().unwrap(), Bytes::from_static(b"a"));
	cancel.cancel();
	let next = tokio::time::timeout(Duration::from_secs(5), body.next()).await.unwrap();
	assert!(matches!(next, Some(Err(HttpError::Aborted))));
	assert!(body.next().await.is_none());
}

#[tokio::test]
async fn follows_same_origin_redirects_and_returns_cross_origin_ones() {
	let other = serve(|_, mut stream| async move {
		write(
			&mut stream,
			"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nother",
		)
		.await;
	})
	.await;
	let base = serve(move |path, mut stream| {
		let other = other.clone();
		async move {
			let response = match path.as_str() {
				"/same" => {
					"HTTP/1.1 302 Found\r\nlocation: /end\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
				}
				"/cross" => format!(
					"HTTP/1.1 302 Found\r\nlocation: {other}/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
				),
				_ => "HTTP/1.1 200 OK\r\ncontent-length: 3\r\nconnection: close\r\n\r\nend".to_owned(),
			};
			write(&mut stream, &response).await;
		}
	})
	.await;
	let same = fetch()
		.fetch(get(&format!("{base}/same"), None), &CancellationToken::new())
		.await
		.unwrap();
	assert_eq!(same.status, 200);
	let cross = fetch()
		.fetch(get(&format!("{base}/cross"), None), &CancellationToken::new())
		.await
		.unwrap();
	assert_eq!(cross.status, 302);
}
