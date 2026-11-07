//! A scenario on disk: `scenarios/<name>/{scenario.json, commands*.jsonl, provider.json, workspace/}`, read and
//! checked (a trust boundary: the files are written by hand).

use anyhow::{Context, Result, bail};
use pi_json::{JsonObject, JsonValue};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The APIs a scenario runs against when `scenario.json` names none.
pub const ALL_APIS: [&str; 2] = ["anthropic-messages", "openai-completions"];

/// The artefacts a run produces, in the order the normalizer reads them (ids are numbered in this order).
pub const ALL_ARTEFACTS: [&str; 5] = ["events", "session", "requests", "fs", "exit"];

/// A loaded scenario.
#[derive(Debug, Clone)]
pub struct Scenario {
	/// The directory name.
	pub name: String,
	/// The scenario directory.
	pub dir: PathBuf,
	/// The parsed `scenario.json`.
	pub spec: ScenarioSpec,
}

/// `scenario.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioSpec {
	/// What the scenario exercises, for readers.
	pub description: String,
	/// The APIs the fake provider is declared with, one golden set each; both when absent.
	#[serde(default = "all_apis")]
	pub apis: Vec<String>,
	/// CLI arguments for every run, after `--mode rpc --provider fake --model fake-model`.
	#[serde(default)]
	pub args: Vec<String>,
	/// Fields merged over the fake model's `models.json` entry.
	#[serde(default)]
	pub model: JsonObject,
	/// Settings merged (deeply) over the base `settings.json`.
	#[serde(default)]
	pub settings: JsonObject,
	/// The artefacts written to and compared with the goldens; all when absent.
	#[serde(default = "all_artefacts")]
	pub compare: Vec<String>,
	/// The pi processes started one after the other against the same agent dir and provider script.
	#[serde(default = "one_run")]
	pub runs: Vec<RunSpec>,
}

/// One pi process of a scenario.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunSpec {
	/// The commands file in the scenario directory.
	pub commands: String,
	/// CLI arguments of this run, after the scenario's; `{session}` is the session file of the runs before.
	#[serde(default)]
	pub args: Vec<String>,
}

fn all_apis() -> Vec<String> {
	ALL_APIS.iter().map(|api| (*api).to_owned()).collect()
}

fn all_artefacts() -> Vec<String> {
	ALL_ARTEFACTS.iter().map(|name| (*name).to_owned()).collect()
}

fn one_run() -> Vec<RunSpec> {
	vec![RunSpec {
		commands: "commands.jsonl".into(),
		args: Vec::new(),
	}]
}

/// One line of a commands file: an RPC command sent to pi, or a driver-only record.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
	/// Send this line on stdin, then wait for its response.
	Command {
		/// The line as written.
		line: String,
		/// The command's `id`, which its response repeats.
		id: Option<String>,
		/// The command's `type`, which its response names in `command`.
		command: String,
	},
	/// `{"$wait": type, "where"?: subset, "timeoutMs"?: n}`: consume records until one of that type matches.
	Wait {
		/// The record `type` to wait for.
		record_type: String,
		/// A JSON subset the record must contain.
		subset: Option<JsonValue>,
		/// How long to wait before failing.
		timeout_ms: u64,
	},
	/// `{"$sleep": ms}`.
	Sleep(u64),
	/// `{"$release": true}`: let the fake provider's paused stream continue.
	Release,
}

/// How long a `$wait` or a command's response may take unless the step says otherwise.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

impl Scenario {
	/// Reads `<scenarios>/<name>/scenario.json`.
	pub fn load(scenarios: &Path, name: &str) -> Result<Self> {
		let dir = scenarios.join(name);
		let path = dir.join("scenario.json");
		let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
		let spec: ScenarioSpec = pi_json::parse(&text)
			.and_then(serde_json::from_value)
			.with_context(|| format!("invalid {}", path.display()))?;
		if let Some(unknown) = spec.compare.iter().find(|name| !ALL_ARTEFACTS.contains(&name.as_str())) {
			bail!("{}: unknown artefact {unknown:?} in compare", path.display());
		}
		if let Some(unknown) = spec.apis.iter().find(|api| !ALL_APIS.contains(&api.as_str())) {
			bail!("{}: unknown api {unknown:?}", path.display());
		}
		Ok(Self {
			name: name.to_owned(),
			dir,
			spec,
		})
	}

	/// Every scenario directory under `scenarios`, sorted by name.
	pub fn names(scenarios: &Path) -> Result<Vec<String>> {
		let mut names = Vec::new();
		for entry in std::fs::read_dir(scenarios).with_context(|| format!("cannot list {}", scenarios.display()))? {
			let entry = entry?;
			if entry.path().join("scenario.json").is_file() {
				names.push(entry.file_name().to_string_lossy().into_owned());
			}
		}
		names.sort();
		Ok(names)
	}

	/// The steps of one run's commands file.
	pub fn steps(&self, run: &RunSpec) -> Result<Vec<Step>> {
		let path = self.dir.join(&run.commands);
		let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
		text.lines()
			.enumerate()
			.filter(|(_, line)| !line.trim().is_empty())
			.map(|(index, line)| parse_step(line).with_context(|| format!("{}:{}", path.display(), index + 1)))
			.collect()
	}
}

fn parse_step(line: &str) -> Result<Step> {
	let value = pi_json::parse(line)?;
	let Some(object) = value.as_object() else {
		bail!("a step must be a JSON object");
	};
	if let Some(record_type) = object.get("$wait") {
		let record_type = record_type.as_str().context("$wait takes a record type")?.to_owned();
		let timeout_ms = object
			.get("timeoutMs")
			.and_then(JsonValue::as_u64)
			.unwrap_or(DEFAULT_TIMEOUT_MS);
		return Ok(Step::Wait {
			record_type,
			subset: object.get("where").cloned(),
			timeout_ms,
		});
	}
	if let Some(ms) = object.get("$sleep") {
		return Ok(Step::Sleep(ms.as_u64().context("$sleep takes milliseconds")?));
	}
	if object.contains_key("$release") {
		return Ok(Step::Release);
	}
	let command = object
		.get("type")
		.and_then(JsonValue::as_str)
		.context("a command needs a type")?;
	let id = object.get("id").and_then(JsonValue::as_str).map(str::to_owned);
	Ok(Step::Command {
		line: line.to_owned(),
		id,
		command: command.to_owned(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	#[test]
	fn parses_driver_records_and_commands() {
		assert_eq!(
			parse_step(r#"{"$wait":"agent_settled"}"#).unwrap(),
			Step::Wait {
				record_type: "agent_settled".into(),
				subset: None,
				timeout_ms: DEFAULT_TIMEOUT_MS
			}
		);
		assert_eq!(
			parse_step(r#"{"$wait":"message_update","where":{"a":1},"timeoutMs":5}"#).unwrap(),
			Step::Wait {
				record_type: "message_update".into(),
				subset: Some(json!({"a":1})),
				timeout_ms: 5
			}
		);
		assert_eq!(parse_step(r#"{"$sleep":20}"#).unwrap(), Step::Sleep(20));
		assert_eq!(parse_step(r#"{"$release":true}"#).unwrap(), Step::Release);
		assert_eq!(
			parse_step(r#"{"id":"s","type":"get_state"}"#).unwrap(),
			Step::Command {
				line: r#"{"id":"s","type":"get_state"}"#.into(),
				id: Some("s".into()),
				command: "get_state".into()
			}
		);
		assert!(parse_step("[]").is_err());
		assert!(parse_step(r#"{"id":"x"}"#).is_err());
	}
}
