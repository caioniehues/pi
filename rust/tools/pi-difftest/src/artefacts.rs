//! The artefacts of a scenario run (`events.jsonl`, `session.jsonl`, `requests.jsonl`, `fs.json`, `exit.json`):
//! gathered from the run's sandbox, normalized with one shared id mapping, and written to or read from a directory.

use crate::driver::RunOutput;
use crate::environment::RunRoot;
use crate::normalize::Normalizer;
use crate::scenario::ALL_ARTEFACTS;
use anyhow::{Context, Result};
use pi_json::{JsonObject, JsonValue};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Normalized artefacts, keyed by file name.
pub type Artefacts = BTreeMap<String, String>;

/// How many trailing stderr lines `exit.json` keeps.
const STDERR_TAIL_LINES: usize = 20;

/// Gathers and normalizes every artefact of a finished scenario run, then keeps those named in `compare`.
pub fn collect(root: &RunRoot, server: &str, runs: &[RunOutput], compare: &[String]) -> Result<Artefacts> {
	let mut normalizer = Normalizer::new(root.path(), server);
	let mut artefacts = Artefacts::new();
	for name in ALL_ARTEFACTS {
		let text = match name {
			"events" => normalizer.jsonl(&events(runs)),
			"session" => normalizer.jsonl(&sessions(&root.agent_dir().join("sessions"))?),
			"requests" => normalizer.requests_jsonl(&read_optional(&root.provider_out().join("requests.jsonl"))?),
			"fs" => pretty(&mut normalizer, listing(&root.cwd())?),
			_ => pretty(&mut normalizer, exits(runs)),
		};
		if compare.iter().any(|wanted| wanted == name) {
			artefacts.insert(file_name(name).to_owned(), text);
		}
	}
	Ok(artefacts)
}

/// The file an artefact is stored in.
pub fn file_name(artefact: &str) -> &'static str {
	match artefact {
		"events" => "events.jsonl",
		"session" => "session.jsonl",
		"requests" => "requests.jsonl",
		"fs" => "fs.json",
		_ => "exit.json",
	}
}

/// Every stdout record; with several runs, each run's records follow a `{"$run": n}` line.
fn events(runs: &[RunOutput]) -> String {
	let mut text = String::new();
	for (index, run) in runs.iter().enumerate() {
		if runs.len() > 1 {
			let _ = writeln!(text, "{{\"$run\":{}}}", index + 1);
		}
		for record in &run.records {
			text.push_str(record);
			text.push('\n');
		}
	}
	text
}

/// Every session file under `sessions`, sorted by path, each after a `{"$file": "<path under sessions>"}` line
/// so the file's name and directory are compared too.
fn sessions(sessions: &Path) -> Result<String> {
	let mut files = session_files(sessions)?;
	files.sort();
	let mut text = String::new();
	for file in files {
		let relative = file.strip_prefix(sessions).unwrap_or(&file).display().to_string();
		let header = json!({ "$file": relative });
		text.push_str(&pi_json::to_js_string(&header));
		text.push('\n');
		text.push_str(&std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?);
	}
	Ok(text)
}

/// The newest session file under the agent dir, for `{session}` in a later run's arguments.
pub fn latest_session(root: &RunRoot) -> Result<PathBuf> {
	// Session file names start with their creation time, so the greatest name is the newest file.
	session_files(&root.agent_dir().join("sessions"))?
		.into_iter()
		.max_by_key(|path| path.file_name().map(std::ffi::OsStr::to_owned))
		.context("no session file for {session}")
}

/// The `.jsonl` files under `sessions`, recursively; none when it does not exist.
fn session_files(sessions: &Path) -> Result<Vec<PathBuf>> {
	let mut files = Vec::new();
	if sessions.is_dir() {
		walk(sessions, &mut files)?;
	}
	files.retain(|path| path.extension().is_some_and(|extension| extension == "jsonl"));
	Ok(files)
}

/// `{ "<path>": "<sha256>" | "dir" }` for everything under the working directory, sorted by path.
fn listing(cwd: &Path) -> Result<JsonValue> {
	let mut paths = Vec::new();
	walk_all(cwd, &mut paths)?;
	paths.sort();
	let mut object = JsonObject::new();
	for path in paths {
		let relative = path.strip_prefix(cwd).unwrap_or(&path).display().to_string();
		let entry = if path.is_dir() {
			"dir".to_owned()
		} else {
			let bytes = std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
			hex(&Sha256::digest(&bytes))
		};
		object.insert(relative, JsonValue::String(entry));
	}
	Ok(JsonValue::Object(object))
}

/// One object per run: the exit code, the stderr tail, and the driver's error or kill when there was one.
fn exits(runs: &[RunOutput]) -> JsonValue {
	let entries = runs.iter().map(|run| {
		let lines: Vec<&str> = run.exit.stderr.lines().collect();
		let tail = &lines[lines.len().saturating_sub(STDERR_TAIL_LINES)..];
		let mut entry = json!({ "code": run.exit.code, "stderrTail": tail });
		if run.exit.killed {
			entry["killed"] = json!(true);
		}
		if let Some(error) = &run.exit.driver_error {
			entry["driverError"] = json!(error);
		}
		entry
	});
	JsonValue::Array(entries.collect())
}

fn pretty(normalizer: &mut Normalizer, mut value: JsonValue) -> String {
	normalizer.value(&mut value);
	pi_json::to_js_string_pretty(&value, 2) + "\n"
}

fn read_optional(path: &Path) -> Result<String> {
	if path.exists() {
		return std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()));
	}
	Ok(String::new())
}

/// The files under `dir`, recursively.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
	for entry in std::fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
		let path = entry?.path();
		if path.is_dir() {
			walk(&path, files)?;
		} else {
			files.push(path);
		}
	}
	Ok(())
}

/// The files and directories under `dir`, recursively.
fn walk_all(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
	for entry in std::fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
		let path = entry?.path();
		paths.push(path.clone());
		if path.is_dir() {
			walk_all(&path, paths)?;
		}
	}
	Ok(())
}

fn hex(bytes: &[u8]) -> String {
	bytes
		.iter()
		.fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
			let _ = write!(out, "{byte:02x}");
			out
		})
}

/// Writes `artefacts` into `dir`, removing artefact files of earlier runs that are no longer produced.
pub fn write_dir(dir: &Path, artefacts: &Artefacts) -> Result<()> {
	std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
	for name in ALL_ARTEFACTS {
		let path = dir.join(file_name(name));
		match artefacts.get(file_name(name)) {
			Some(text) => std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?,
			None if path.exists() => std::fs::remove_file(&path)?,
			None => {}
		}
	}
	Ok(())
}

/// Reads the artefact files present in `dir`.
pub fn read_dir(dir: &Path) -> Result<Artefacts> {
	let mut artefacts = Artefacts::new();
	for name in ALL_ARTEFACTS {
		let path = dir.join(file_name(name));
		if path.exists() {
			let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
			artefacts.insert(file_name(name).to_owned(), text);
		}
	}
	Ok(artefacts)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hex_is_lowercase_and_padded() {
		assert_eq!(hex(&[0, 15, 255]), "000fff");
	}
}
