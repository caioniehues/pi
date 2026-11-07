//! The workspace's manifests as the checks read them: the member crates, every `Cargo.toml`, dependency tables.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One dependency table of a manifest: `[dependencies]`, `[dev-dependencies]` or `[build-dependencies]`, at the top
/// level or under `[target.<cfg>]`.
pub struct DependencyTable<'a> {
	/// The table's key path for messages, such as `dependencies` or `target.cfg(unix).dev-dependencies`.
	pub label: String,
	/// The section, without the target prefix.
	pub section: &'static str,
	/// The entries: dependency name to specifier.
	pub entries: &'a toml::Table,
}

/// Reads and parses a TOML file.
pub fn read_toml(path: &Path) -> Result<toml::Table> {
	let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
	text.parse().with_context(|| format!("parsing {}", path.display()))
}

/// The member crates named by `[workspace] members`: canonical directory to package name.
pub fn members(workspace: &Path) -> Result<BTreeMap<PathBuf, String>> {
	let manifest = read_toml(&workspace.join("Cargo.toml"))?;
	let listed = manifest
		.get("workspace")
		.and_then(|section| section.get("members"))
		.and_then(toml::Value::as_array)
		.context("Cargo.toml has no [workspace] members")?;
	let mut members = BTreeMap::new();
	for member in listed {
		let member = member.as_str().context("a workspace member is not a string")?;
		if member.contains(['*', '?', '[']) {
			bail!("workspace member {member:?}: globs are not supported, list each crate");
		}
		let directory = workspace.join(member);
		let package = read_toml(&directory.join("Cargo.toml"))?;
		let name = package
			.get("package")
			.and_then(|section| section.get("name"))
			.and_then(toml::Value::as_str)
			.with_context(|| format!("{member}/Cargo.toml has no package.name"))?;
		let canonical = directory
			.canonicalize()
			.with_context(|| format!("resolving {member}"))?;
		members.insert(canonical, name.to_owned());
	}
	Ok(members)
}

/// Directories that hold no workspace manifests: build output and vendored third-party sources.
const SKIPPED_DIRECTORIES: &[&str] = &["target", "vendor", ".git"];

/// Every `Cargo.toml` under `directory`, sorted, members or not.
pub fn all_manifests(directory: &Path) -> Result<Vec<PathBuf>> {
	let mut manifests = Vec::new();
	collect_manifests(directory, &mut manifests)?;
	manifests.sort();
	Ok(manifests)
}

fn collect_manifests(directory: &Path, manifests: &mut Vec<PathBuf>) -> Result<()> {
	for entry in fs::read_dir(directory).with_context(|| format!("reading {}", directory.display()))? {
		let entry = entry?;
		let name = entry.file_name();
		let kind = entry.file_type()?;
		if kind.is_dir() && !SKIPPED_DIRECTORIES.iter().any(|skipped| name == *skipped) {
			collect_manifests(&entry.path(), manifests)?;
		} else if kind.is_file() && name == "Cargo.toml" {
			manifests.push(entry.path());
		}
	}
	Ok(())
}

const DEPENDENCY_SECTIONS: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

/// Every dependency table of a crate manifest, top-level ones first.
pub fn dependency_tables(manifest: &toml::Table) -> Vec<DependencyTable<'_>> {
	let targets = manifest.get("target").and_then(toml::Value::as_table);
	let target_tables = targets
		.into_iter()
		.flatten()
		.filter_map(|(cfg, table)| Some((format!("target.{cfg}."), table.as_table()?)));
	let mut tables = Vec::new();
	for (prefix, table) in std::iter::once((String::new(), manifest)).chain(target_tables) {
		for &section in DEPENDENCY_SECTIONS {
			if let Some(entries) = table.get(section).and_then(toml::Value::as_table) {
				tables.push(DependencyTable {
					label: format!("{prefix}{section}"),
					section,
					entries,
				});
			}
		}
	}
	tables
}

/// The member crate a path dependency of the manifest at `manifest_path` points at, if it points at one.
pub fn path_target<'a>(manifest_path: &Path, path: &str, members: &'a BTreeMap<PathBuf, String>) -> Option<&'a String> {
	let directory = manifest_path.parent()?.join(path).canonicalize().ok()?;
	members.get(&directory)
}
