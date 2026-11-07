//! `hydrate-check`: the generated model data (`packages/ai/src/providers/data/*.json`) exists; pi-ai's catalog
//! tests need it from C3.

use std::fs;
use std::path::Path;

/// One line telling how to generate the data when `repository` has none; empty otherwise.
pub fn hydrate_check(repository: &Path) -> Vec<String> {
	let data = repository.join("packages/ai/src/providers/data");
	let has_json = fs::read_dir(&data).is_ok_and(|entries| {
		entries
			.flatten()
			.any(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
	});
	if has_json {
		return Vec::new();
	}
	vec![format!(
		"{} has no *.json: run `npm run hydrate:model-data`",
		data.display()
	)]
}
