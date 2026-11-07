//! The tracker cross-apply corpus (rust/docs/slices/D0b.md): the Rust tracker runs the 200 editing scripts of
//! `rust/fixtures/chord-delta/tracker-scripts.jsonl` (generated from the TS tracker by `gen-tracker.mjs`), must reach
//! the value the TS draft reached and refuse the steps it refused with its message, and writes what it prepared to
//! `tracker-out.jsonl` for `check-tracker-out.mjs`, which replays the ops with the TS `apply` and `applyImmutable`.

use anyhow::{Context, Result, bail, ensure};
use pi_chord::delta::{Draft, DraftError, Op, Seg, Snapshot, Tracker, track};
use pi_json::{JsonValue, to_js_string};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

fn fixtures() -> PathBuf {
	PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/chord-delta"))
}

// TS: the cross-apply corpus of rust/docs/slices/D0b.md: every script ran through the TS tracker
// (`gen-tracker.mjs`), and `check-tracker-out.mjs` replays the Rust ops with the TS appliers.
#[test]
fn tracker_scripts_match_ts_and_replay_under_the_ts_appliers() -> Result<()> {
	let text = std::fs::read_to_string(fixtures().join("tracker-scripts.jsonl"))?;
	let mut out = String::new();
	let mut scripts = 0;
	for (index, line) in text.lines().enumerate() {
		let script = pi_json::parse(line)?;
		run_script(&script, &mut out).with_context(|| format!("tracker-scripts:{}", index + 1))?;
		scripts += 1;
	}
	ensure!(scripts == 200, "expected 200 scripts, found {scripts}");
	let out_path = fixtures().join("tracker-out.jsonl");
	std::fs::write(&out_path, out)?;
	check_with_node(&out_path)
}

/// Runs one script's changes and appends one `{base, ops, value}` line per change to `out`.
fn run_script(script: &JsonValue, out: &mut String) -> Result<()> {
	let mut tracker = track(Snapshot::from(field(script, "base")?));
	let changes = field(script, "changes")?.as_array().context("changes")?;
	for (index, change) in changes.iter().enumerate() {
		run_change(&mut tracker, change, out).with_context(|| format!("change {index}"))?;
	}
	Ok(())
}

fn run_change(tracker: &mut Tracker, change: &JsonValue, out: &mut String) -> Result<()> {
	let steps = field(change, "steps")?.as_array().context("steps")?;
	let prepared = match steps.as_slice() {
		[step] if field(step, "op")? == "replace" => tracker.prepare_replace(Snapshot::from(field(step, "value")?)),
		_ => {
			let mut draft_change = tracker.begin_change();
			for (index, step) in steps.iter().enumerate() {
				let outcome = run_step(draft_change.state(), step).with_context(|| format!("step {index}: {step}"))?;
				let expected = step.get("error").and_then(JsonValue::as_str);
				let actual = outcome.as_ref().err().map(ToString::to_string);
				ensure!(actual.as_deref() == expected, "step {index} {step}: Rust {outcome:?}");
			}
			draft_change.prepare()?
		}
	};
	let value = to_js_string(&prepared.value().to_value());
	let expected = to_js_string(field(change, "value")?);
	ensure!(value == expected, "Rust prepared {value}\n  TS prepared {expected}");
	let ops = JsonValue::Array(prepared.ops().iter().map(Op::to_json).collect());
	let base = to_js_string(&prepared.base().to_value());
	writeln!(out, r#"{{"base":{base},"ops":{},"value":{value}}}"#, to_js_string(&ops))?;
	tracker.adopt(&prepared)?;
	Ok(())
}

/// One draft edit; the outer `Result` is a malformed step, the inner one the draft's answer.
fn run_step(draft: &mut Draft, step: &JsonValue) -> Result<Result<(), DraftError>> {
	let op = field(step, "op")?.as_str().context("op")?;
	let path = path(field(step, "path")?)?;
	let snapshot = |key: &str| field(step, key).map(Snapshot::from);
	let index = |key: &str| field(step, key)?.as_i64().context("index");
	let end = || step.get("end").and_then(JsonValue::as_i64);
	let items = || -> Result<Vec<Snapshot>> {
		Ok(field(step, "items")?
			.as_array()
			.context("items")?
			.iter()
			.map(Snapshot::from)
			.collect())
	};
	Ok(match op {
		"set" => draft.set(&path, Some(snapshot("value")?)),
		"unset" => draft.set(&path, None),
		"delete" => draft.delete(&path),
		"append" => draft.append_text(&path, field(step, "text")?.as_str().context("text")?),
		"push" => draft.push(&path, items()?).map(drop),
		"pop" => draft.pop(&path).map(drop),
		"shift" => draft.shift(&path).map(drop),
		"unshift" => draft.unshift(&path, items()?).map(drop),
		"splice" => {
			let remove = field(step, "remove")?.as_u64().map(usize::try_from).transpose()?;
			draft.splice(&path, index("start")?, remove, items()?).map(drop)
		}
		"reverse" => draft.reverse(&path),
		"sort" => draft.sort_by(&path, |left, right| {
			let (left, right) = (to_js_string(&left.to_value()), to_js_string(&right.to_value()));
			left.encode_utf16().cmp(right.encode_utf16())
		}),
		"fill" => draft.fill(&path, &snapshot("value")?, index("start")?, end()),
		"copyWithin" => draft.copy_within(&path, index("target")?, index("start")?, end()),
		"length" => draft.set_len(
			&path,
			usize::try_from(field(step, "length")?.as_u64().context("length")?)?,
		),
		other => bail!("unknown step {other}"),
	})
}

fn field<'a>(record: &'a JsonValue, key: &str) -> Result<&'a JsonValue> {
	record.get(key).with_context(|| format!("record without {key}"))
}

fn path(value: &JsonValue) -> Result<Vec<Seg>> {
	value
		.as_array()
		.context("path")?
		.iter()
		.map(|segment| match segment {
			JsonValue::String(key) => Ok(Seg::Key(key.clone())),
			other => other.as_u64().map(Seg::Index).context("segment"),
		})
		.collect()
}

/// Replays the written changes with the TS appliers, when `node` is installed.
fn check_with_node(out: &std::path::Path) -> Result<()> {
	let script = fixtures().join("check-tracker-out.mjs");
	let output = match Command::new("node").arg(&script).arg(out).output() {
		Ok(output) => output,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
			eprintln!("skipping the TS replay of tracker-out.jsonl: node is not installed");
			return Ok(());
		}
		Err(error) => return Err(error.into()),
	};
	let stdout = String::from_utf8_lossy(&output.stdout);
	let stderr = String::from_utf8_lossy(&output.stderr);
	ensure!(
		output.status.success(),
		"check-tracker-out.mjs failed:\n{stdout}{stderr}"
	);
	eprintln!("{}", stdout.trim());
	Ok(())
}
