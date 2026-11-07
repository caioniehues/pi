//! The formats TypeBox 1.3.27 checks with the WHATWG URL parser (`URL.canParse`): `url`, `iri` and `iri-reference`.
//! The `url` crate implements the same standard as Node's parser, with one decided divergence (rust/docs/slices.md,
//! "Decided divergences"): it rejects hosts with malformed A-labels (`http://xn--a.com/`, `http://xn--/`) that Node
//! accepts.

/// A URL the WHATWG parser accepts (TypeBox `IsUrl`).
pub(super) fn is_url(value: &str) -> bool {
	url::Url::parse(value).is_ok()
}

// Substituting an IPvFuture literal costs a scan of the string, so only strings under this length get it.
const IPV_FUTURE_MAX_LENGTH: usize = 2048;

/// An absolute IRI (RFC 3987, TypeBox `IsIri`): no unencoded whitespace or delimiters, well-formed percent escapes,
/// then the WHATWG parser, with an IPvFuture host (`[v1.x]`, which WHATWG rejects) replaced by `[::1]`.
pub(super) fn is_iri(value: &str) -> bool {
	let invalid = |c: char| c <= ' ' || matches!(c, '<' | '>' | '^' | '`' | '{' | '|' | '}' | '\\');
	if value.contains(invalid) || has_bad_percent(value) {
		return false;
	}
	if value.encode_utf16().count() < IPV_FUTURE_MAX_LENGTH {
		url::Url::parse(&narrow_ipv_future(value)).is_ok()
	} else {
		url::Url::parse(value).is_ok()
	}
}

/// An IRI or a relative reference (TypeBox `IsIriReference`): no unencoded spaces, control characters or backslashes,
/// no scheme-like prefix that lacks its colon (`httpx//a`), then the WHATWG parser against `http://example.com`.
pub(super) fn is_iri_reference(value: &str) -> bool {
	let invalid = |c: char| c <= ' ' || c == '\x7f' || c == '\\';
	if value.contains(invalid) || has_bad_percent(value) || has_malformed_scheme(value) {
		return false;
	}
	url::Url::parse("http://example.com")
		.and_then(|base| base.join(value))
		.is_ok()
}

/// A `%` not followed by two hex digits.
fn has_bad_percent(value: &str) -> bool {
	let b = value.as_bytes();
	b.iter()
		.enumerate()
		.any(|(i, c)| *c == b'%' && !(b.get(i + 1..i + 3).is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit))))
}

/// `^[a-zA-Z][a-zA-Z0-9+\-.]*\/\/`: a scheme's characters then `//`, without the colon between.
fn has_malformed_scheme(value: &str) -> bool {
	let b = value.as_bytes();
	if !b.first().is_some_and(u8::is_ascii_alphabetic) {
		return false;
	}
	let scheme = b[1..]
		.iter()
		.take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
		.count();
	b[1 + scheme..].starts_with(b"//")
}

/// `value` with its first `\[[vV][0-9a-fA-F]+\.[^\]]+\]` replaced by `[::1]`.
fn narrow_ipv_future(value: &str) -> String {
	let b = value.as_bytes();
	for start in (0..b.len()).filter(|&i| b[i] == b'[') {
		if let Some(end) = ipv_future_end(b, start) {
			return format!("{}[::1]{}", &value[..start], &value[end..]);
		}
	}
	value.to_owned()
}

/// The end (after `]`) of an IPvFuture literal starting at `b[start] == b'['`.
fn ipv_future_end(b: &[u8], start: usize) -> Option<usize> {
	let mut i = start + 1;
	if !matches!(b.get(i), Some(b'v' | b'V')) {
		return None;
	}
	i += 1;
	let hex = b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
	if hex == 0 || b.get(i + hex) != Some(&b'.') {
		return None;
	}
	i += hex + 1;
	let body = b[i..].iter().take_while(|c| **c != b']').count();
	if body == 0 || b.get(i + body) != Some(&b']') {
		return None;
	}
	Some(i + body + 1)
}
