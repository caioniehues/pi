#!/usr/bin/env bash
# The TS pi as a single executable, for `compare --pi` and `live --pi`: validates the driver against itself
# (`compare --all --pi tools/pi-difftest/ts-pi.sh` must pass). PI_TS_REPO overrides the checkout it runs.
set -euo pipefail
repo="${PI_TS_REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
exec node "$repo/packages/coding-agent/dist/cli.js" "$@"
