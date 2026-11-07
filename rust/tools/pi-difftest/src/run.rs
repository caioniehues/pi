//! One scenario run against one API: a sandbox, the fake provider serving `provider.json`, each run's pi process
//! driven through its commands, and the normalized artefacts.

use crate::artefacts::{Artefacts, collect, latest_session};
use crate::driver::{Limits, PiCommand, RunOutput, drive};
use crate::environment::{RunRoot, Sandbox};
use crate::scenario::Scenario;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The outcome of a scenario run.
pub struct RunResult {
	/// The normalized artefacts named in the scenario's `compare`.
	pub artefacts: Artefacts,
	/// What went wrong though artefacts were collected: driver errors of runs that stopped early, and a provider
	/// script not consumed exactly. Empty for a clean run.
	pub problems: Vec<String>,
	/// The sandbox, when it was kept.
	pub kept: Option<PathBuf>,
}

/// Runs every pi process of `scenario` with the fake provider declared as `api`.
pub async fn run_scenario(scenario: &Scenario, api: &str, pi: &PiCommand, sandbox: Sandbox) -> Result<RunResult> {
	// Dropping `root` removes the sandbox, also when a `?` below returns early.
	let root = RunRoot::create(&scenario.dir.join("workspace"), &pi.package_dir, sandbox)?;
	let script = pi_fake_provider::load_script(&scenario.dir.join("provider.json"))?;
	let scripted = script.responses.len();
	let (addr, server) = pi_fake_provider::start(script, &root.provider_out()).await?;
	root.write_agent_files(api, addr, &scenario.spec.model, &scenario.spec.settings)?;
	let mut outputs: Vec<RunOutput> = Vec::new();
	for run in &scenario.spec.runs {
		let steps = scenario.steps(run)?;
		let mut args = scenario.spec.args.clone();
		for arg in &run.args {
			if arg.contains("{session}") {
				let session = latest_session(&root)?;
				args.push(arg.replace("{session}", &session.display().to_string()));
			} else {
				args.push(arg.clone());
			}
		}
		let output = drive(pi, &args, &root, &steps, &server, Limits::RUN)
			.await
			.with_context(|| format!("{} ({api}): run of {}", scenario.name, run.commands))?;
		let stopped = output.exit.driver_error.is_some();
		outputs.push(output);
		if stopped {
			break;
		}
	}
	let requested = server.served();
	server.shutdown().await;
	let artefacts = collect(&root, &addr.to_string(), &outputs, &scenario.spec.compare)?;
	let mut problems: Vec<String> = outputs
		.iter()
		.filter_map(|output| output.exit.driver_error.clone())
		.collect();
	if requested != scripted {
		problems.push(format!(
			"{} ({api}): pi made {requested} provider requests but provider.json scripts {scripted}",
			scenario.name
		));
	}
	Ok(RunResult {
		artefacts,
		problems,
		kept: root.kept().map(Path::to_path_buf),
	})
}
