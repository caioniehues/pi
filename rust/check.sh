#!/usr/bin/env bash
# The gate every Rust slice must pass (rust/docs/slices/C0.md): format, lint, test, deny, pins, dependency rules,
# vendor checksums, release smoke. Runs from any cwd; stops at the first failing step with its full output.
# `--locked` everywhere a command resolves dependencies: the gate checks the committed Cargo.lock, never rewrites it.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
export INSTA_UPDATE=no

step() {
	echo "==> $*"
	"$@"
}

step cargo fmt --all --check
step cargo clippy --locked --workspace --all-targets -- -D warnings
step cargo test --locked --workspace
if ! cargo deny --version >/dev/null 2>&1; then
	echo "install cargo-deny: cargo install cargo-deny" >&2
	exit 1
fi
step cargo deny --locked check
step cargo run --locked -p xtask -- check-pins
step cargo run --locked -p xtask -- check-dag
step cargo run --locked -p xtask -- verify-vendor
step cargo build --locked --release -p pi-cli
step "${CARGO_TARGET_DIR:-target}/release/pi" --version
echo "check.sh: all green"
