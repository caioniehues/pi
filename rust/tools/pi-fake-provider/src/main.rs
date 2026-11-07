//! Command-line entry point of the fake provider: `pi-fake-provider --script scenario.json --out dir --port 0`
//! prints the bound port as its first stdout line, then serves until SIGINT or SIGTERM.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;

const USAGE: &str = "usage: pi-fake-provider --script <scenario.json> --out <dir> [--port <n>]";

#[tokio::main]
async fn main() -> Result<()> {
	let options = parse_options(std::env::args().skip(1))?;
	let script = pi_fake_provider::load_script(&options.script)?;
	let addr = SocketAddr::from(([127, 0, 0, 1], options.port));
	let (addr, handle) = pi_fake_provider::start_on(script, &options.out, addr).await?;
	let mut stdout = std::io::stdout();
	writeln!(stdout, "{}", addr.port())?;
	stdout.flush()?;
	wait_for_signal().await?;
	handle.shutdown().await;
	Ok(())
}

struct Options {
	script: PathBuf,
	out: PathBuf,
	port: u16,
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options> {
	let (mut script, mut out, mut port) = (None, None, 0);
	while let Some(flag) = args.next() {
		let value = args.next().with_context(|| format!("{flag} needs a value\n{USAGE}"))?;
		match flag.as_str() {
			"--script" => script = Some(PathBuf::from(value)),
			"--out" => out = Some(PathBuf::from(value)),
			"--port" => port = value.parse().with_context(|| format!("invalid port {value}"))?,
			_ => bail!("unknown option {flag}\n{USAGE}"),
		}
	}
	match (script, out) {
		(Some(script), Some(out)) => Ok(Options { script, out, port }),
		_ => bail!("{USAGE}"),
	}
}

async fn wait_for_signal() -> Result<()> {
	let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
	tokio::select! {
		result = tokio::signal::ctrl_c() => result?,
		_ = terminate.recv() => {}
	}
	Ok(())
}
