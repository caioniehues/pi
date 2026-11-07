//! xtask: repository checks for the Rust workspace, run as `cargo run -p xtask -- <command>` (`rust/check.sh`).

mod check_dag;
mod check_pins;
mod hydrate_check;
mod verify_vendor;
mod workspace;

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::ExitCode;

fn main() -> Result<ExitCode> {
	// Paths in messages are relative to `rust/`, wherever cargo was started.
	std::env::set_current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).context("entering rust/")?;
	let workspace = Path::new(".");
	let failures = match std::env::args().nth(1).as_deref() {
		Some("check-pins") => check_pins::check_pins(workspace)?,
		Some("check-dag") => check_dag::check_dag(workspace)?,
		Some("verify-vendor") => verify_vendor::verify_vendor(workspace)?,
		Some("hydrate-check") => hydrate_check::hydrate_check(Path::new("..")),
		_ => bail!("usage: cargo run -p xtask -- <check-pins | check-dag | verify-vendor | hydrate-check>"),
	};
	if failures.is_empty() {
		return Ok(ExitCode::SUCCESS);
	}
	for failure in failures {
		eprintln!("{failure}");
	}
	Ok(ExitCode::FAILURE)
}
