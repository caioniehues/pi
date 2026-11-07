//! Header records (ports `packages/ai/src/utils/headers.ts`) and the provider header type with `null` removal.

use http::HeaderMap;
use pi_types::Record;

/// `Record<string, string | null>`: request headers where `None` removes a default header of the same name.
pub type ProviderHeaders = Record<Option<String>>;

/// The headers of a response as a record, names lowercase (`headersToRecord`).
pub fn headers_to_record(headers: &HeaderMap) -> Record<String> {
	headers
		.iter()
		.map(|(name, value)| {
			(
				name.as_str().to_owned(),
				String::from_utf8_lossy(value.as_bytes()).into_owned(),
			)
		})
		.collect()
}

/// Merges header sources in order, case-insensitively: a later name replaces an earlier one (keeping the later
/// spelling, at the end), `None` removes it; `None` when nothing is left (`providerHeadersToRecord`).
pub fn provider_headers_to_record(sources: &[Option<&ProviderHeaders>]) -> Option<Record<String>> {
	let mut merged: Vec<(String, String, String)> = Vec::new();
	for source in sources.iter().flatten() {
		for (name, value) in source.iter() {
			let lower = name.to_lowercase();
			merged.retain(|(existing, _, _)| *existing != lower);
			if let Some(value) = value {
				merged.push((lower, name.to_owned(), value.clone()));
			}
		}
	}
	(!merged.is_empty()).then(|| merged.into_iter().map(|(_, name, value)| (name, value)).collect())
}

/// Merges `override_headers` over `base` case-insensitively, keeping `None` entries (the `mergeHeaders` of
/// `models.ts`); `None` when both are absent.
pub fn merge_headers(
	base: Option<&ProviderHeaders>,
	override_headers: Option<&ProviderHeaders>,
) -> Option<ProviderHeaders> {
	if base.is_none() && override_headers.is_none() {
		return None;
	}
	let mut merged: Vec<(String, Option<String>)> = base
		.map(|base| {
			base.iter()
				.map(|(name, value)| (name.to_owned(), value.clone()))
				.collect()
		})
		.unwrap_or_default();
	for (name, value) in override_headers.iter().flat_map(|headers| headers.iter()) {
		let lower = name.to_lowercase();
		merged.retain(|(existing, _)| existing.to_lowercase() != lower);
		merged.push((name.to_owned(), value.clone()));
	}
	Some(merged.into_iter().collect())
}
