//! `pi`: the coding agent's command-line entry point (ports `packages/coding-agent/src/cli.ts` and `main.ts`).
//! C0 answers `--version` only, so the release build can be smoke-tested.

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
	// TS parity: `pi --version` and `pi -v` print the bare version (main.ts).
	if matches!(env::args().nth(1).as_deref(), Some("--version" | "-v")) {
		println!("{}", env!("CARGO_PKG_VERSION"));
		return ExitCode::SUCCESS;
	}
	eprintln!("pi: this build only supports --version");
	ExitCode::FAILURE
}
