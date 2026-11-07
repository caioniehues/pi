//! `check-pins`: the Rust side of `scripts/check-pinned-deps.mjs`. External dependencies are pinned exactly, once, in
//! `[workspace.dependencies]`; crates use them with `workspace = true`, and path dependencies name member crates.

use crate::workspace::{all_manifests, dependency_tables, members, path_target, read_toml};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every violation in the workspace under `workspace`, after a heading line; empty when there is none.
pub fn check_pins(workspace: &Path) -> Result<Vec<String>> {
	let root = workspace.join("Cargo.toml");
	let mut violations = Vec::new();
	check_workspace_dependencies(&root, &read_toml(&root)?, &mut violations);
	let members = members(workspace)?;
	for path in all_manifests(workspace)?.iter().filter(|path| **path != root) {
		check_crate_manifest(path, &read_toml(path)?, &members, &mut violations);
	}
	if !violations.is_empty() {
		violations.insert(
			0,
			"Dependencies must be pinned in [workspace.dependencies] and used with workspace = true:".into(),
		);
	}
	Ok(violations)
}

fn check_workspace_dependencies(path: &Path, manifest: &toml::Table, violations: &mut Vec<String>) {
	let pinned = manifest
		.get("workspace")
		.and_then(|section| section.get("dependencies"))
		.and_then(toml::Value::as_table);
	for (name, spec) in pinned.into_iter().flatten() {
		let version = spec
			.as_str()
			.or_else(|| spec.get("version").and_then(toml::Value::as_str));
		if !version.is_some_and(is_exact_version) {
			violations.push(format!(
				"  {}: workspace.dependencies.{name} must be pinned with =x.y.z, found {spec}",
				path.display()
			));
		}
	}
}

/// `=` and a full semver version, with optional pre-release and build metadata.
fn is_exact_version(version: &str) -> bool {
	let Some(exact) = version.strip_prefix('=') else {
		return false;
	};
	let core = exact.split(['-', '+']).next().unwrap_or_default();
	let parts: Vec<&str> = core.split('.').collect();
	parts.len() == 3
		&& parts.iter().all(|part| {
			!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) && (*part == "0" || !part.starts_with('0'))
		})
}

fn check_crate_manifest(
	path: &Path,
	manifest: &toml::Table,
	members: &BTreeMap<PathBuf, String>,
	violations: &mut Vec<String>,
) {
	for table in dependency_tables(manifest) {
		for (name, spec) in table.entries {
			if let Some(problem) = dependency_problem(path, spec, members) {
				violations.push(format!(
					"  {}: {}.{name} {problem}, found {spec}",
					path.display(),
					table.label
				));
			}
		}
	}
}

/// What is wrong with one dependency of the manifest at `path`, if anything.
fn dependency_problem(path: &Path, spec: &toml::Value, members: &BTreeMap<PathBuf, String>) -> Option<&'static str> {
	if spec.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
		return None;
	}
	let Some(target) = spec.get("path") else {
		return Some("must use workspace = true");
	};
	let member = target.as_str().and_then(|target| path_target(path, target, members));
	member.is_none().then_some("must be a path to a workspace member")
}
