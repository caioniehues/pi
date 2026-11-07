//! The golden store: `golden/<scenario>/<api>/<artefact>` generated from the TS pi at `TS_PIN`, and
//! `golden/META.json` saying which TS commit and node version produced them and when.

use crate::artefacts::{Artefacts, read_dir, write_dir};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// The goldens under `<tool dir>/golden`.
pub struct GoldenStore {
	root: PathBuf,
}

impl GoldenStore {
	/// The store at `root`.
	pub fn new(root: PathBuf) -> Self {
		Self { root }
	}

	/// The golden directory of one scenario and API.
	pub fn dir(&self, scenario: &str, api: &str) -> PathBuf {
		self.root.join(scenario).join(api)
	}

	/// Replaces the goldens of `scenario` and `api`; true when any file changed.
	pub fn write(&self, scenario: &str, api: &str, artefacts: &Artefacts) -> Result<bool> {
		let dir = self.dir(scenario, api);
		let before = if dir.is_dir() {
			read_dir(&dir)?
		} else {
			Artefacts::new()
		};
		write_dir(&dir, artefacts)?;
		Ok(&before != artefacts)
	}

	/// The goldens of `scenario` and `api`.
	pub fn read(&self, scenario: &str, api: &str) -> Result<Artefacts> {
		let dir = self.dir(scenario, api);
		if !dir.is_dir() {
			bail!("no goldens at {}; run `regen {scenario}` first", dir.display());
		}
		read_dir(&dir)
	}

	/// Writes `META.json` when goldens changed or the commit or node version differ; the date stays otherwise,
	/// so regenerating unchanged goldens changes nothing.
	pub fn write_meta(&self, ts_commit: &str, node: &str, changed: bool) -> Result<()> {
		let path = self.root.join("META.json");
		let current = std::fs::read_to_string(&path)
			.ok()
			.and_then(|text| pi_json::parse(&text).ok());
		let same_source = current
			.as_ref()
			.is_some_and(|meta| meta["tsCommit"] == ts_commit && meta["node"] == node);
		if same_source && !changed {
			return Ok(());
		}
		let date = pi_json::iso_ms(SystemTime::now())[..10].to_owned();
		let meta = json!({ "tsCommit": ts_commit, "node": node, "date": date });
		std::fs::write(&path, pi_json::to_js_string_pretty(&meta, 2) + "\n")
			.with_context(|| format!("cannot write {}", path.display()))
	}
}

/// The pinned TS commit in `<tool dir>/TS_PIN`.
pub fn ts_pin(tool_dir: &Path) -> Result<String> {
	let path = tool_dir.join("TS_PIN");
	let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
	Ok(text.trim().to_owned())
}

/// Fails unless `repo`'s `packages/` (working tree included) are those of `pin` and the CLI is built.
pub fn check_ts_repo(repo: &Path, pin: &str) -> Result<()> {
	let same = Command::new("git")
		.arg("-C")
		.arg(repo)
		.args(["diff", "--quiet", pin, "--", "packages"])
		.status()
		.context("cannot run git")?;
	if !same.success() {
		bail!(
			"{}: packages/ differ from TS_PIN {pin} (or the commit is missing); goldens must come from the pin",
			repo.display()
		);
	}
	let cli = ts_cli(repo);
	if !cli.is_file() {
		bail!("{} is missing; build the TS pi first (see README.md)", cli.display());
	}
	Ok(())
}

/// The TS pi's entry point in `repo`.
pub fn ts_cli(repo: &Path) -> PathBuf {
	repo.join("packages/coding-agent/dist/cli.js")
}

/// `node --version`, trimmed.
pub fn node_version() -> Result<String> {
	let output = Command::new("node")
		.arg("--version")
		.output()
		.context("cannot run node")?;
	Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
