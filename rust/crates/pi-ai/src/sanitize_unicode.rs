//! Removal of unpaired surrogates before text reaches a provider (ports `packages/ai/src/utils/sanitize-unicode.ts`).
//!
//! A Rust `str` cannot hold an unpaired surrogate: JSON read through `pi_json::parse` turns each lone surrogate
//! escape into U+FFFD (a decided divergence), so the text that TS sanitizes never exists here and the function
//! returns its input unchanged. It is kept so the request builders of C3b read like their TS sources.

/// `sanitizeSurrogates`: text without unpaired surrogates, which a Rust string never has.
pub fn sanitize_surrogates(text: &str) -> &str {
	text
}
