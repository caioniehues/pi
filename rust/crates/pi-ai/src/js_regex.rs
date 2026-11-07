//! ECMAScript regular expressions for the error classifiers (`retry.ts`, `overflow.ts`): their patterns use
//! lookbehind and lookahead, which only an ECMAScript engine (`regress`, also pi-schema's) runs as TS does.

use regress::Regex;
use std::sync::OnceLock;

/// Patterns compiled on first use with their JS flags; `test` is true when any of them matches.
pub(crate) struct JsRegexSet {
	sources: &'static [&'static str],
	flags: &'static str,
	compiled: OnceLock<Vec<Regex>>,
}

impl JsRegexSet {
	/// `/source/flags` for each source, compiled on first use.
	pub(crate) const fn new(sources: &'static [&'static str], flags: &'static str) -> Self {
		Self {
			sources,
			flags,
			compiled: OnceLock::new(),
		}
	}

	/// Whether any pattern matches (`RegExp.prototype.test`).
	pub(crate) fn test(&self, text: &str) -> bool {
		self.compiled
			.get_or_init(|| {
				self.sources
					.iter()
					.filter_map(|source| {
						let compiled = Regex::with_flags(source, self.flags);
						// invariant: the patterns are constants of this crate, covered by its tests.
						debug_assert!(compiled.is_ok(), "invalid constant pattern {source}");
						compiled.ok()
					})
					.collect()
			})
			.iter()
			.any(|regex| regex.find(text).is_some())
	}
}
