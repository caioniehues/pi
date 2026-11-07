//! Embeds the model catalog: checks `packages/ai/src/providers/data` (or `PI_MODEL_DATA_DIR`) the way
//! `scripts/model-data.ts` does and writes `$OUT_DIR/model_data.rs` with one `include_str!` per provider file.
//!
//! The data is generated and gitignored. Decided handling of its absence (C3a): an explicit `PI_MODEL_DATA_DIR` that
//! does not exist fails the build; a missing default directory warns with the same message and embeds an empty
//! catalog, so the rest of the crate builds and tests in a clean checkout. The `model_data` cfg tells the catalog
//! tests whether data was embedded.

#[path = "src/model_data.rs"]
#[allow(dead_code, reason = "build.rs uses only the reading and validation half")]
mod model_data;

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const MISSING_DATA: &str = "run `npm run hydrate:model-data` in packages/ai";

fn main() {
	println!("cargo::rustc-check-cfg=cfg(model_data)");
	println!("cargo::rustc-env=PI_RUSTC_VERSION={}", rustc_version());
	println!("cargo::rerun-if-env-changed=PI_MODEL_DATA_DIR");
	let explicit = env::var_os("PI_MODEL_DATA_DIR").map(PathBuf::from);
	let data_dir = explicit.clone().unwrap_or_else(default_data_dir);
	println!("cargo::rerun-if-changed={}", data_dir.display());
	// A directory that appears later must invalidate an empty-catalog build: watch its parent and its manifest too.
	if let Some(parent) = data_dir.parent() {
		println!("cargo::rerun-if-changed={}", parent.display());
	}
	println!(
		"cargo::rerun-if-changed={}",
		data_dir.join(model_data::MODEL_DATA_MANIFEST_FILE).display()
	);
	let out = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default()).join("model_data.rs");
	if !data_dir.is_dir() {
		assert!(
			explicit.is_none(),
			"model data directory {} does not exist: {MISSING_DATA}",
			data_dir.display()
		);
		println!(
			"cargo::warning=model data missing at {}: {MISSING_DATA}",
			data_dir.display()
		);
		write(&out, &generated(&[]));
		return;
	}
	let checked = check(&data_dir).and_then(|providers| {
		if explicit.is_none() {
			check_aggregator(&providers)?;
		}
		Ok(providers)
	});
	let providers = match checked {
		Ok(providers) => providers,
		Err(error) => panic!("model data in {} is not usable: {error}", data_dir.display()),
	};
	println!("cargo::rustc-cfg=model_data");
	write(&out, &generated(&providers));
}

/// The compiler's version (`1.99.1`), for the detailed `User-Agent`'s runtime part.
fn rustc_version() -> String {
	let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
	let output = std::process::Command::new(rustc).arg("--version").output();
	let text = output
		.map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
		.unwrap_or_default();
	text.split_whitespace().nth(1).unwrap_or("unknown").to_owned()
}

fn default_data_dir() -> PathBuf {
	let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
	manifest.join("../../../packages/ai/src/providers/data")
}

/// Validates the directory; returns each provider id with the absolute path of its file.
fn check(data_dir: &Path) -> Result<Vec<(String, PathBuf)>, model_data::ModelDataError> {
	let structure = model_data::read_model_data_structure(data_dir)?;
	model_data::validate_model_data_directory(&structure, data_dir)?;
	let mut providers = Vec::new();
	for provider in structure.keys() {
		let path = data_dir.join(format!("{provider}.json"));
		println!("cargo::rerun-if-changed={}", path.display());
		let path = fs::canonicalize(&path).map_err(|error| model_data::ModelDataError(error.to_string()))?;
		providers.push((provider.clone(), path));
	}
	println!(
		"cargo::rerun-if-changed={}",
		data_dir.join(model_data::MODEL_DATA_MANIFEST_FILE).display()
	);
	Ok(providers)
}

/// The default directory belongs to this checkout: its providers must be the ones `models.generated.ts` imports, as
/// `readModelDataStructure` requires.
fn check_aggregator(providers: &[(String, PathBuf)]) -> Result<(), model_data::ModelDataError> {
	let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
	let aggregator = manifest.join("../../../packages/ai/src/models.generated.ts");
	println!("cargo::rerun-if-changed={}", aggregator.display());
	let Ok(text) = fs::read_to_string(&aggregator) else {
		return Ok(());
	};
	let mut imported: Vec<&str> = text
		.lines()
		.filter_map(|line| {
			line.strip_prefix("import {")?
				.split("from \"./providers/")
				.nth(1)?
				.strip_suffix(".models.ts\";")
		})
		.collect();
	imported.sort_unstable();
	let actual: Vec<&str> = providers.iter().map(|(provider, _)| provider.as_str()).collect();
	model_data::assert_exact_model_ids("models.generated.ts provider", imported, actual)
}

fn generated(providers: &[(String, PathBuf)]) -> String {
	let mut source = String::from(
		"/// Provider ids and their catalog JSON (API groups of `type:id` keyed models), embedded by build.rs.\n\
		 pub(crate) const PROVIDER_DATA: &[(&str, &str)] = &[\n",
	);
	for (provider, path) in providers {
		// invariant: writing to a String cannot fail.
		let _ = writeln!(
			source,
			"\t({provider:?}, include_str!({:?})),",
			path.display().to_string()
		);
	}
	source.push_str("];\n");
	source
}

fn write(path: &Path, content: &str) {
	if let Err(error) = fs::write(path, content) {
		panic!("cannot write {}: {error}", path.display());
	}
}
