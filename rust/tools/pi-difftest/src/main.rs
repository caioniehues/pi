//! Difference test driver: runs the TS pi (the oracle, at `TS_PIN`) and the Rust pi against the fake provider
//! through scripted RPC scenarios, normalizes what they produce, and compares it with goldens or with each other.
//! Subcommands: `ts-run`, `rust-run`, `regen`, `compare`, `live` (see README.md).

mod artefacts;
mod diff;
mod driver;
mod environment;
mod golden;
mod normalize;
mod run;
mod scenario;

use anyhow::{Context, Result, bail};
use artefacts::write_dir;
use diff::diff_artefacts;
use driver::PiCommand;
use environment::Sandbox;
use golden::{GoldenStore, check_ts_repo, node_version, ts_cli, ts_pin};
use run::{RunResult, run_scenario};
use scenario::Scenario;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage:
  pi-difftest ts-run (--all | <scenario>...) [--api <api>] [--out <dir>] [--keep] [--ts-repo <dir>]
  pi-difftest rust-run (--all | <scenario>...) --pi <path> [--package-dir <dir>] [--api <api>] [--out <dir>] [--keep]
  pi-difftest regen (--all | <scenario>...) [--api <api>] [--ts-repo <dir>]
  pi-difftest compare (--all | <scenario>...) --pi <path> [--package-dir <dir>] [--api <api>] [--keep]
  pi-difftest live (--all | <scenario>...) --pi <path> [--package-dir <dir>] [--api <api>] [--ts-repo <dir>] [--keep]";

#[tokio::main]
async fn main() -> Result<ExitCode> {
	let mut args = std::env::args().skip(1);
	let subcommand = args.next().unwrap_or_default();
	let options = Options::parse(args)?;
	let tool = ToolPaths::locate()?;
	match subcommand.as_str() {
		"ts-run" => single_run(&tool, &options, &ts_pi(&options.ts_repo(&tool)), "ts").await,
		"rust-run" => single_run(&tool, &options, &options.rust_pi(&tool)?, "rust").await,
		"regen" => regen(&tool, &options).await,
		"compare" => compare(&tool, &options).await,
		"live" => live(&tool, &options).await,
		_ => bail!("{USAGE}"),
	}
}

/// The parsed options shared by all subcommands.
struct Options {
	scenarios: Vec<String>,
	all: bool,
	api: Option<String>,
	pi: Option<PathBuf>,
	out: Option<PathBuf>,
	ts_repo: Option<PathBuf>,
	package_dir: Option<PathBuf>,
	sandbox: Sandbox,
}

impl Options {
	fn parse(mut args: impl Iterator<Item = String>) -> Result<Self> {
		let mut options = Self {
			scenarios: Vec::new(),
			all: false,
			api: None,
			pi: None,
			out: None,
			ts_repo: None,
			package_dir: None,
			sandbox: Sandbox::Remove,
		};
		while let Some(arg) = args.next() {
			let mut value = || args.next().with_context(|| format!("{arg} needs a value\n{USAGE}"));
			match arg.as_str() {
				"--all" => options.all = true,
				"--keep" => options.sandbox = Sandbox::Keep,
				"--api" => options.api = Some(value()?),
				"--pi" => options.pi = Some(PathBuf::from(value()?)),
				"--out" => options.out = Some(PathBuf::from(value()?)),
				"--ts-repo" => options.ts_repo = Some(PathBuf::from(value()?)),
				"--package-dir" => options.package_dir = Some(PathBuf::from(value()?)),
				flag if flag.starts_with("--") => bail!("unknown option {flag}\n{USAGE}"),
				name => options.scenarios.push(name.to_owned()),
			}
		}
		Ok(options)
	}

	fn ts_repo(&self, tool: &ToolPaths) -> PathBuf {
		self.ts_repo.clone().unwrap_or_else(|| tool.repo.clone())
	}

	fn rust_pi(&self, tool: &ToolPaths) -> Result<PiCommand> {
		let program = self.pi.clone().context("--pi <path> is required")?;
		// An absolute path, because pi runs in the sandbox's cwd.
		let program = std::fs::canonicalize(&program).with_context(|| format!("no pi at {}", program.display()))?;
		let package_dir = self
			.package_dir
			.clone()
			.unwrap_or_else(|| tool.repo.join("packages/coding-agent"));
		let package_dir =
			std::fs::canonicalize(&package_dir).with_context(|| format!("no package dir {}", package_dir.display()))?;
		Ok(PiCommand {
			program,
			prefix: Vec::new(),
			package_dir,
		})
	}

	/// The scenarios named, or all of them with `--all`.
	fn selected(&self, tool: &ToolPaths) -> Result<Vec<Scenario>> {
		let names = if self.all {
			Scenario::names(&tool.scenarios)?
		} else {
			self.scenarios.clone()
		};
		if names.is_empty() {
			bail!("name a scenario or pass --all\n{USAGE}");
		}
		names.iter().map(|name| Scenario::load(&tool.scenarios, name)).collect()
	}

	/// The APIs of `scenario` that `--api` lets through.
	fn apis<'a>(&self, scenario: &'a Scenario) -> Vec<&'a str> {
		let apis = scenario.spec.apis.iter().map(String::as_str);
		apis.filter(|api| self.api.as_deref().is_none_or(|wanted| wanted == *api))
			.collect()
	}
}

/// Where the tool's files are: its directory (scenarios, goldens, `TS_PIN`) and the repository root.
struct ToolPaths {
	dir: PathBuf,
	scenarios: PathBuf,
	repo: PathBuf,
}

impl ToolPaths {
	fn locate() -> Result<Self> {
		let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
		let repo = std::fs::canonicalize(dir.join("../../.."))?;
		Ok(Self {
			scenarios: dir.join("scenarios"),
			dir,
			repo,
		})
	}

	fn goldens(&self) -> GoldenStore {
		GoldenStore::new(self.dir.join("golden"))
	}
}

fn ts_pi(repo: &Path) -> PiCommand {
	PiCommand {
		program: PathBuf::from("node"),
		prefix: vec![ts_cli(repo).display().to_string()],
		package_dir: repo.join("packages/coding-agent"),
	}
}

/// `ts-run` and `rust-run`: the normalized artefacts of each scenario and API, written under `--out`.
async fn single_run(tool: &ToolPaths, options: &Options, pi: &PiCommand, side: &str) -> Result<ExitCode> {
	let out_root = options
		.out
		.clone()
		.unwrap_or_else(|| std::env::temp_dir().join("pi-difftest-out").join(side));
	let mut clean = true;
	for scenario in options.selected(tool)? {
		for api in options.apis(&scenario) {
			let result = run_scenario(&scenario, api, pi, options.sandbox).await?;
			let out = out_root.join(&scenario.name).join(api);
			write_dir(&out, &result.artefacts)?;
			println!(
				"{} {api} ({}): {}",
				scenario.name,
				scenario.spec.description,
				out.display()
			);
			clean &= report_run(&result);
		}
	}
	Ok(if clean { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Prints the problems and the kept sandbox of a run; true when the run was clean.
fn report_run(result: &RunResult) -> bool {
	for problem in &result.problems {
		eprintln!("  problem: {problem}");
	}
	if let Some(path) = &result.kept {
		eprintln!("  sandbox kept at {}", path.display());
	}
	result.problems.is_empty()
}

/// `regen`: runs the TS pi at `TS_PIN` and rewrites the goldens. Every run must be clean (no driver error, the
/// script consumed exactly); goldens and `META.json` are written only after all runs succeeded, so a failure
/// leaves the store as it was.
async fn regen(tool: &ToolPaths, options: &Options) -> Result<ExitCode> {
	let repo = options.ts_repo(tool);
	let pin = ts_pin(&tool.dir)?;
	check_ts_repo(&repo, &pin)?;
	let pi = ts_pi(&repo);
	let mut results = Vec::new();
	for scenario in options.selected(tool)? {
		for api in options.apis(&scenario) {
			let result = run_scenario(&scenario, api, &pi, options.sandbox).await?;
			if !report_run(&result) {
				bail!(
					"{} ({api}): the TS pi did not complete the scenario; no golden written",
					scenario.name
				);
			}
			results.push((scenario.name.clone(), api.to_owned(), result.artefacts));
		}
	}
	let store = tool.goldens();
	let mut changed = false;
	for (scenario, api, artefacts) in &results {
		let updated = store.write(scenario, api, artefacts)?;
		changed |= updated;
		println!(
			"regen {scenario} {api}: {}",
			if updated { "updated" } else { "unchanged" }
		);
	}
	store.write_meta(&pin, &node_version()?, changed)?;
	Ok(ExitCode::SUCCESS)
}

/// `compare`: runs `--pi` and diffs its normalized artefacts against the goldens.
async fn compare(tool: &ToolPaths, options: &Options) -> Result<ExitCode> {
	let pi = options.rust_pi(tool)?;
	let store = tool.goldens();
	let mut failed = 0;
	for scenario in options.selected(tool)? {
		for api in options.apis(&scenario) {
			let golden = store.read(&scenario.name, api)?;
			let result = run_scenario(&scenario, api, &pi, options.sandbox).await?;
			report_run(&result);
			let diff = diff_artefacts(&golden, &result.artefacts, "golden", "actual");
			failed += usize::from(print_outcome(&format!("{}/{api}", scenario.name), &diff));
		}
	}
	Ok(summary(failed))
}

/// `live`: runs the TS pi and `--pi` and diffs their normalized artefacts directly.
async fn live(tool: &ToolPaths, options: &Options) -> Result<ExitCode> {
	let ts = ts_pi(&options.ts_repo(tool));
	let rust = options.rust_pi(tool)?;
	let mut failed = 0;
	for scenario in options.selected(tool)? {
		for api in options.apis(&scenario) {
			let expected = run_scenario(&scenario, api, &ts, options.sandbox).await?;
			report_run(&expected);
			let actual = run_scenario(&scenario, api, &rust, options.sandbox).await?;
			report_run(&actual);
			let diff = diff_artefacts(&expected.artefacts, &actual.artefacts, "ts", "rust");
			failed += usize::from(print_outcome(&format!("{}/{api}", scenario.name), &diff));
		}
	}
	Ok(summary(failed))
}

/// Prints `ok` or the diff; true when there is a diff.
fn print_outcome(label: &str, diff: &str) -> bool {
	if diff.is_empty() {
		println!("ok   {label}");
		return false;
	}
	println!("DIFF {label}\n{diff}");
	true
}

fn summary(failed: usize) -> ExitCode {
	if failed == 0 {
		println!("all scenarios match");
		ExitCode::SUCCESS
	} else {
		println!("{failed} scenario run(s) differ");
		ExitCode::FAILURE
	}
}
