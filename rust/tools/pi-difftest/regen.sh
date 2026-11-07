#!/usr/bin/env bash
# Regenerates the goldens from the TS pi at TS_PIN (rust/docs/slices/C4.md): builds the TS pi in a fresh
# `git worktree` at the pinned commit under a temp directory, runs `pi-difftest regen` against it, removes the
# worktree. Needs network for `npm ci` and `npm run hydrate:model-data` (the build needs the hydrated model data,
# although the fake provider itself is declared in models.json).
#
#   ./regen.sh [scenario...]      default: --all
#   ./regen.sh --here [...]       use this checkout's built TS pi instead (its packages/ must equal TS_PIN)
set -euo pipefail

tool_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
rust_dir="$(cd "$tool_dir/../.." && pwd)"
repo_dir="$(cd "$rust_dir/.." && pwd)"
pin="$(tr -d '[:space:]' < "$tool_dir/TS_PIN")"

if [ "${1:-}" = "--here" ]; then
	shift
	ts_repo="$repo_dir"
	cleanup() { :; }
else
	work="$(mktemp -d "${TMPDIR:-/tmp}/pi-ts-pin.XXXXXX")"
	ts_repo="$work/pi"
	cleanup() {
		git -C "$repo_dir" worktree remove --force "$ts_repo" 2>/dev/null || true
		rm -rf "$work"
	}
	trap cleanup EXIT
	git -C "$repo_dir" worktree add --detach "$ts_repo" "$pin"
	(
		cd "$ts_repo"
		npm ci --ignore-scripts
		npm run hydrate:model-data
		npm run build:offline
	)
fi

if [ "$#" -eq 0 ]; then
	set -- --all
fi
cd "$rust_dir"
cargo run --locked -p pi-difftest -- regen "$@" --ts-repo "$ts_repo"
