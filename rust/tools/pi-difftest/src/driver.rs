//! Drives one pi process in RPC mode through a commands file: sends each command and waits for its response,
//! executes the driver-only records, closes stdin and collects every stdout record, the exit status and stderr.
//! pi runs in its own process group, killed when the run ends, so no tool process outlives it (Unix only; the
//! tool does not run on Windows, where a job object would be needed).

use crate::environment::{MODEL, PROVIDER, RunRoot};
use crate::scenario::{DEFAULT_TIMEOUT_MS, Step};
use anyhow::{Context, Result, bail};
use pi_fake_provider::ServerHandle;
use pi_json::JsonValue;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// How a pi is started: a program and the arguments before pi's own (`node cli.js`, or a binary and none).
#[derive(Debug, Clone)]
pub struct PiCommand {
	/// The program.
	pub program: PathBuf,
	/// Arguments placed before pi's arguments.
	pub prefix: Vec<String>,
	/// The pi's package directory (docs, examples, `package.json`), reached through the sandbox's
	/// `PI_PACKAGE_DIR` link.
	pub package_dir: PathBuf,
}

/// What one pi process produced.
pub struct RunOutput {
	/// Every stdout record, as written (LF framing, CR stripped).
	pub records: Vec<String>,
	/// How it ended.
	pub exit: ExitInfo,
}

/// How a pi process ended.
pub struct ExitInfo {
	/// The exit code; absent when a signal ended it.
	pub code: Option<i32>,
	/// Whether the driver had to kill it after stdin closed.
	pub killed: bool,
	/// Everything written to stderr.
	pub stderr: String,
	/// Why the driver stopped early: a missing response or event, or a write to a closed stdin.
	pub driver_error: Option<String>,
}

/// How long the end of a run may take.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
	/// How long pi may take to exit after stdin closes before its process group is killed.
	pub exit: Duration,
	/// How long reading the rest of stdout and stderr may take after pi ended; a process that left the group
	/// and still holds a pipe cannot hang the driver.
	pub drain: Duration,
}

impl Limits {
	/// The limits of every scenario run.
	pub const RUN: Self = Self {
		exit: Duration::from_secs(30),
		drain: Duration::from_secs(5),
	};
}

/// Runs `pi --mode rpc --provider fake --model fake-model <args>` in the root's cwd through `steps`.
pub async fn drive(
	pi: &PiCommand,
	args: &[String],
	root: &RunRoot,
	steps: &[Step],
	server: &ServerHandle,
	limits: Limits,
) -> Result<RunOutput> {
	let mut child = Command::new(&pi.program)
		.args(&pi.prefix)
		.args(["--mode", "rpc", "--provider", PROVIDER, "--model", MODEL])
		.args(args)
		.current_dir(root.cwd())
		.env_clear()
		.envs(root.env())
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.kill_on_drop(true)
		.process_group(0)
		.spawn()
		.with_context(|| format!("cannot start {}", pi.program.display()))?;
	// A spawned child has a pid until it is waited for; pi leads its own group, so the pid names the group.
	let group = child.id().context("pi exited before the driver saw it")?;
	let stdin = child.stdin.take().context("no stdin pipe")?;
	let (sender, lines) = mpsc::unbounded_channel();
	tokio::spawn(read_lines(child.stdout.take().context("no stdout pipe")?, sender));
	let stderr = tokio::spawn(read_all(child.stderr.take().context("no stderr pipe")?));
	let mut session = Session {
		stdin: Some(stdin),
		lines,
		records: Vec::new(),
		parsed: Vec::new(),
		cursor: 0,
	};
	let driver_error = session.run(steps, server).await.err().map(|error| format!("{error:#}"));
	session.stdin = None;
	if driver_error.is_some() {
		// A run that went wrong is not waited for: pi may never exit on its own.
		kill_group(group);
	}
	let finished = finish(&mut child, group, &mut session, limits).await;
	kill_group(group);
	let (code, killed) = finished?;
	let stderr = tokio::time::timeout(limits.drain, stderr)
		.await
		.ok()
		.and_then(Result::ok)
		.unwrap_or_default();
	Ok(RunOutput {
		records: session.records,
		exit: ExitInfo {
			code,
			killed,
			stderr,
			driver_error,
		},
	})
}

/// The driver's view of a running pi: stdin, the records read so far, and the `$wait` cursor.
struct Session {
	stdin: Option<ChildStdin>,
	lines: mpsc::UnboundedReceiver<String>,
	records: Vec<String>,
	/// `records` parsed; `Null` for a line that is not JSON.
	parsed: Vec<JsonValue>,
	/// `$wait` searches from here and moves it past its match; responses do not move it, because a command's
	/// events (an abort's `agent_settled`) may come before its response.
	cursor: usize,
}

impl Session {
	async fn run(&mut self, steps: &[Step], server: &ServerHandle) -> Result<()> {
		for step in steps {
			match step {
				Step::Command { line, id, command } => {
					let from = self.records.len();
					let stdin = self.stdin.as_mut().context("stdin is closed")?;
					stdin
						.write_all(format!("{line}\n").as_bytes())
						.await
						.context("cannot write to pi's stdin")?;
					stdin.flush().await?;
					let is_response = |record: &JsonValue| is_response_to(record, id.as_deref(), command);
					self.wait_for(from, is_response, DEFAULT_TIMEOUT_MS)
						.await
						.with_context(|| format!("no response to {line}"))?;
				}
				Step::Wait {
					record_type,
					subset,
					timeout_ms,
				} => {
					let matches = |record: &JsonValue| {
						record.get("type").and_then(JsonValue::as_str) == Some(record_type)
							&& subset.as_ref().is_none_or(|subset| contains(record, subset))
					};
					let found = self
						.wait_for(self.cursor, matches, *timeout_ms)
						.await
						.with_context(|| format!("no {record_type} record"))?;
					self.cursor = found + 1;
				}
				Step::Sleep(ms) => tokio::time::sleep(Duration::from_millis(*ms)).await,
				Step::Release => server.release(),
			}
		}
		Ok(())
	}

	/// The index of the first record at or after `from` that matches, reading more records as needed.
	async fn wait_for(&mut self, from: usize, matches: impl Fn(&JsonValue) -> bool, timeout_ms: u64) -> Result<usize> {
		let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
		let mut next = from;
		loop {
			if let Some(offset) = self.parsed[next.min(self.parsed.len())..].iter().position(&matches) {
				return Ok(next + offset);
			}
			next = self.parsed.len().max(from);
			match tokio::time::timeout_at(deadline, self.lines.recv()).await {
				Ok(Some(line)) => self.push(line),
				Ok(None) => bail!("pi closed stdout"),
				Err(_) => bail!("timed out after {timeout_ms} ms"),
			}
		}
	}

	fn push(&mut self, line: String) {
		// Wire input: a line that is not JSON is kept as text and matches nothing.
		self.parsed.push(pi_json::parse(&line).unwrap_or(JsonValue::Null));
		self.records.push(line);
	}
}

fn is_response_to(record: &JsonValue, id: Option<&str>, command: &str) -> bool {
	if record.get("type").and_then(JsonValue::as_str) != Some("response") {
		return false;
	}
	match id {
		Some(id) => record.get("id").and_then(JsonValue::as_str) == Some(id),
		None => record.get("command").and_then(JsonValue::as_str) == Some(command),
	}
}

/// Whether `value` holds everything in `subset`: objects key by key, anything else by equality.
fn contains(value: &JsonValue, subset: &JsonValue) -> bool {
	match (value, subset) {
		(JsonValue::Object(value), JsonValue::Object(subset)) => subset
			.iter()
			.all(|(key, expected)| value.get(key).is_some_and(|actual| contains(actual, expected))),
		(value, subset) => value == subset,
	}
}

/// Waits for exit with stdin closed (killing the group after `limits.exit`) and reads the rest of stdout for at
/// most `limits.drain`.
async fn finish(child: &mut Child, group: u32, session: &mut Session, limits: Limits) -> Result<(Option<i32>, bool)> {
	let (status, killed) = if let Ok(status) = tokio::time::timeout(limits.exit, child.wait()).await {
		(status?, false)
	} else {
		kill_group(group);
		(child.wait().await?, true)
	};
	let deadline = tokio::time::Instant::now() + limits.drain;
	while let Ok(Some(line)) = tokio::time::timeout_at(deadline, session.lines.recv()).await {
		session.push(line);
	}
	Ok((status.code(), killed))
}

/// Sends SIGKILL to the process group `group`; an already empty group is fine.
fn kill_group(group: u32) {
	// `kill(1)` rather than libc: the crate forbids unsafe code, and this runs once per pi process.
	let _ = std::process::Command::new("kill")
		.args(["-KILL", "--", &format!("-{group}")])
		.stdout(Stdio::null())
		.stderr(Stdio::null())
		.status();
}

/// Splits stdout on LF only (U+2028 and U+2029 are not separators) and strips one CR before each LF.
async fn read_lines(mut stdout: impl AsyncRead + Unpin, sender: mpsc::UnboundedSender<String>) {
	let mut pending = Vec::new();
	let mut buffer = vec![0_u8; 64 * 1024];
	loop {
		let read = match stdout.read(&mut buffer).await {
			Ok(0) | Err(_) => break,
			Ok(read) => read,
		};
		pending.extend_from_slice(&buffer[..read]);
		while let Some(end) = pending.iter().position(|&byte| byte == b'\n') {
			let mut line: Vec<u8> = pending.drain(..=end).collect();
			line.pop();
			if line.last() == Some(&b'\r') {
				line.pop();
			}
			if sender.send(String::from_utf8_lossy(&line).into_owned()).is_err() {
				return;
			}
		}
	}
	if !pending.is_empty() {
		let _ = sender.send(String::from_utf8_lossy(&pending).into_owned());
	}
}

async fn read_all(mut stream: impl AsyncRead + Unpin) -> String {
	let mut bytes = Vec::new();
	// A read error ends the capture; what was read so far is kept.
	let _ = stream.read_to_end(&mut bytes).await;
	String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::environment::Sandbox;
	use serde_json::json;
	use std::os::unix::fs::PermissionsExt;
	use std::path::Path;

	/// A pi that never answers, ignores stdin EOF and leaves a child holding its stdout; it writes its pid (its
	/// process group) to `pid` in its cwd.
	const STUCK_PI: &str = "#!/bin/sh\necho $$ > pid\nsleep 300 &\nsleep 300\n";

	/// Drives the stuck pi through `steps` with short limits; returns the output, the time taken and its group.
	async fn drive_stuck(name: &str, steps: &[Step]) -> (RunOutput, Duration, u32) {
		let scratch = std::env::temp_dir().join(format!("pi-difftest-driver-{}-{name}", std::process::id()));
		std::fs::create_dir_all(&scratch).unwrap();
		let program = scratch.join("stuck-pi.sh");
		std::fs::write(&program, STUCK_PI).unwrap();
		std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
		let root = RunRoot::create(Path::new("/nonexistent"), &scratch, Sandbox::Remove).unwrap();
		let script = pi_fake_provider::Script {
			delay_ms: 0,
			responses: Vec::new(),
		};
		let (_, server) = pi_fake_provider::start(script, &root.provider_out()).await.unwrap();
		let pi = PiCommand {
			program,
			prefix: Vec::new(),
			package_dir: scratch.clone(),
		};
		let limits = Limits {
			exit: Duration::from_millis(500),
			drain: Duration::from_millis(500),
		};
		let started = std::time::Instant::now();
		let output = drive(&pi, &[], &root, steps, &server, limits).await.unwrap();
		let elapsed = started.elapsed();
		let group = std::fs::read_to_string(root.cwd().join("pid"))
			.unwrap()
			.trim()
			.parse()
			.unwrap();
		server.shutdown().await;
		std::fs::remove_dir_all(&scratch).unwrap();
		(output, elapsed, group)
	}

	fn group_alive(group: u32) -> bool {
		std::process::Command::new("kill")
			.args(["-0", "--", &format!("-{group}")])
			.stderr(Stdio::null())
			.status()
			.unwrap()
			.success()
	}

	#[tokio::test]
	async fn a_pi_that_never_answers_is_killed_with_its_children() {
		let wait = Step::Wait {
			record_type: "agent_settled".into(),
			subset: None,
			timeout_ms: 300,
		};
		let (output, elapsed, group) = drive_stuck("wait", &[wait]).await;
		assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
		assert!(
			output
				.exit
				.driver_error
				.as_deref()
				.is_some_and(|error| error.contains("timed out after 300 ms"))
		);
		assert!(!group_alive(group), "a process of group {group} survived");
	}

	#[tokio::test]
	async fn a_pi_that_ignores_stdin_eof_is_killed_after_the_exit_limit() {
		let (output, elapsed, group) = drive_stuck("eof", &[]).await;
		assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
		assert!(output.exit.killed);
		assert!(output.exit.driver_error.is_none());
		assert!(!group_alive(group), "a process of group {group} survived");
	}

	#[test]
	fn subset_matching_is_deep_for_objects() {
		let record =
			json!({ "type": "message_update", "assistantMessageEvent": { "type": "text_delta", "delta": "x" } });
		assert!(contains(
			&record,
			&json!({ "assistantMessageEvent": { "type": "text_delta" } })
		));
		assert!(!contains(
			&record,
			&json!({ "assistantMessageEvent": { "type": "text_end" } })
		));
		assert!(!contains(&record, &json!({ "missing": 1 })));
	}

	#[test]
	fn responses_match_by_id_or_command() {
		let response = json!({ "id": "a", "type": "response", "command": "get_state", "success": true });
		assert!(is_response_to(&response, Some("a"), "get_state"));
		assert!(!is_response_to(&response, Some("b"), "get_state"));
		assert!(is_response_to(&response, None, "get_state"));
		assert!(!is_response_to(&json!({ "type": "agent_end" }), None, "agent_end"));
	}

	#[tokio::test]
	async fn lines_split_on_lf_only() {
		let input: &[u8] = "a\r\nb\u{2028}c\nlast".as_bytes();
		let (sender, mut receiver) = mpsc::unbounded_channel();
		read_lines(input, sender).await;
		let mut lines = Vec::new();
		while let Some(line) = receiver.recv().await {
			lines.push(line);
		}
		assert_eq!(lines, vec!["a", "b\u{2028}c", "last"]);
	}
}
