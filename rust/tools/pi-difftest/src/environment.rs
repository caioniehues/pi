//! The sandbox of one scenario run: a temp root holding the working directory (a copy of `workspace/`), the agent
//! dir with `settings.json`, `models.json` and `auth.json` for the fake provider, a home directory and the
//! provider's output, plus the environment the pi under test gets.

use anyhow::{Context, Result};
use pi_json::{JsonObject, JsonValue};
use serde_json::json;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The provider id every scenario uses.
pub const PROVIDER: &str = "fake";
/// The model id every scenario uses.
pub const MODEL: &str = "fake-model";
/// The API key written to `auth.json`.
const API_KEY: &str = "fake-key";

/// A run's temp root; removed when dropped, on success and on every early error alike, unless kept.
pub struct RunRoot {
	path: PathBuf,
	sandbox: Sandbox,
}

/// Whether the sandbox outlives the run, for debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
	/// Delete it when the run ends.
	Remove,
	/// Leave it on disk.
	Keep,
}

static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

impl RunRoot {
	/// Creates the root and its directories, links `package` as `pi-package`, and copies `workspace` (when it
	/// exists) into the working directory.
	pub fn create(workspace: &Path, package: &Path, sandbox: Sandbox) -> Result<Self> {
		// Paths of a fixed length, whatever TMPDIR and the pid: they reach the system prompt (cwd, docs), whose
		// length feeds pi's token estimates, so a longer path would change compaction numbers.
		let id = (u64::from(std::process::id()) << 16) | (NEXT_ROOT.fetch_add(1, Ordering::Relaxed) as u64 & 0xffff);
		let path = PathBuf::from(format!("/tmp/pi-difftest-{:012x}", id & 0xffff_ffff_ffff));
		if path.exists() {
			std::fs::remove_dir_all(&path)?;
		}
		std::fs::create_dir_all(&path)?;
		// The canonical form is what pi sees as its cwd (macOS /tmp is a symlink), so it is the one to normalize.
		let root = Self {
			path: std::fs::canonicalize(&path)?,
			sandbox,
		};
		for dir in [root.cwd(), root.agent_dir(), root.home(), root.provider_out()] {
			std::fs::create_dir_all(dir)?;
		}
		std::os::unix::fs::symlink(package, root.package_link())
			.with_context(|| format!("cannot link {}", package.display()))?;
		if workspace.is_dir() {
			copy_tree(workspace, &root.cwd())?;
		}
		Ok(root)
	}

	/// The root directory.
	pub fn path(&self) -> &Path {
		&self.path
	}

	/// The working directory pi runs in.
	pub fn cwd(&self) -> PathBuf {
		self.path.join("cwd")
	}

	/// `PI_CODING_AGENT_DIR`.
	pub fn agent_dir(&self) -> PathBuf {
		self.path.join("agent")
	}

	/// `HOME`, empty, so nothing of the user's leaks in.
	pub fn home(&self) -> PathBuf {
		self.path.join("home")
	}

	/// `PI_PACKAGE_DIR`: a link to the pi's package directory, so the paths pi prints are the same for every
	/// checkout and implementation.
	pub fn package_link(&self) -> PathBuf {
		self.path.join("pi-package")
	}

	/// Where the fake provider writes `requests.jsonl`.
	pub fn provider_out(&self) -> PathBuf {
		self.path.join("provider")
	}

	/// Writes `settings.json`, `models.json` and `auth.json` declaring provider `fake` at `server` with `api`.
	pub fn write_agent_files(
		&self,
		api: &str,
		server: SocketAddr,
		model: &JsonObject,
		settings: &JsonObject,
	) -> Result<()> {
		let mut base_settings = json!({
			"enableInstallTelemetry": false,
			"enableAnalytics": false,
			"quietStartup": true,
		});
		merge(&mut base_settings, &JsonValue::Object(settings.clone()));
		let mut model_entry = json!({
			"id": MODEL,
			"name": "Fake Model",
			"reasoning": false,
			"input": ["text"],
			"cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
			"contextWindow": 200_000,
			"maxTokens": 16_384,
		});
		for (key, value) in model {
			model_entry[key] = value.clone();
		}
		let models = json!({
			"providers": { PROVIDER: { "baseUrl": base_url(api, server), "api": api, "models": [model_entry] } }
		});
		let auth = json!({ PROVIDER: { "type": "api_key", "key": API_KEY } });
		let agent = self.agent_dir();
		write_json(&agent.join("settings.json"), &base_settings)?;
		write_json(&agent.join("models.json"), &models)?;
		write_json(&agent.join("auth.json"), &auth)
	}

	/// The whole environment of the pi under test: nothing inherited but `PATH`, so no provider key or user
	/// setting leaks in.
	pub fn env(&self) -> Vec<(String, String)> {
		let path = std::env::var("PATH").unwrap_or_default();
		vec![
			("PATH".into(), path),
			("HOME".into(), self.home().display().to_string()),
			("PI_CODING_AGENT_DIR".into(), self.agent_dir().display().to_string()),
			("PI_PACKAGE_DIR".into(), self.package_link().display().to_string()),
			("PI_OFFLINE".into(), "1".into()),
			("PI_TELEMETRY".into(), "0".into()),
			("TZ".into(), "UTC".into()),
			("LANG".into(), "C.UTF-8".into()),
		]
	}

	/// The root's path when it is kept after the run.
	pub fn kept(&self) -> Option<&Path> {
		(self.sandbox == Sandbox::Keep).then_some(self.path.as_path())
	}
}

impl Drop for RunRoot {
	fn drop(&mut self) {
		if self.sandbox == Sandbox::Remove {
			// A sandbox that cannot be removed only costs disk space under /tmp.
			let _ = std::fs::remove_dir_all(&self.path);
		}
	}
}

/// The SDKs append their own paths: Anthropic's adds `/v1/messages` to the base URL, OpenAI's `/chat/completions`.
fn base_url(api: &str, server: SocketAddr) -> String {
	if api == "openai-completions" {
		format!("http://{server}/v1")
	} else {
		format!("http://{server}")
	}
}

/// Merges `overlay` into `base`: objects key by key, anything else replaces.
fn merge(base: &mut JsonValue, overlay: &JsonValue) {
	match (base, overlay) {
		(JsonValue::Object(base), JsonValue::Object(overlay)) => {
			for (key, value) in overlay {
				match base.get_mut(key) {
					Some(existing) => merge(existing, value),
					None => {
						base.insert(key.clone(), value.clone());
					}
				}
			}
		}
		(base, overlay) => *base = overlay.clone(),
	}
}

fn write_json(path: &Path, value: &JsonValue) -> Result<()> {
	let text = pi_json::to_js_string_pretty(value, 2) + "\n";
	std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
	for entry in std::fs::read_dir(from)? {
		let entry = entry?;
		let target = to.join(entry.file_name());
		if entry.file_type()?.is_dir() {
			std::fs::create_dir_all(&target)?;
			copy_tree(&entry.path(), &target)?;
		} else {
			std::fs::copy(entry.path(), &target)?;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn merge_is_deep_for_objects_only() {
		let mut base = json!({ "a": { "b": 1, "c": 2 }, "d": [1] });
		merge(&mut base, &json!({ "a": { "c": 3 }, "d": [2], "e": true }));
		assert_eq!(base, json!({ "a": { "b": 1, "c": 3 }, "d": [2], "e": true }));
	}

	#[test]
	fn base_url_matches_each_sdk_path() {
		let server = SocketAddr::from(([127, 0, 0, 1], 9));
		assert_eq!(base_url("anthropic-messages", server), "http://127.0.0.1:9");
		assert_eq!(base_url("openai-completions", server), "http://127.0.0.1:9/v1");
	}
}
