# Rust port: the slice plan

Status: approved by the owner (2026-11-06). The full plan (context, architecture, technology choices, risks,
verification) lives in the coordinator's plan file; this file is the working record: which slice builds what, its
status, and (section "Carry-over") what a slice deferred to a later one.

Per slice: a spec written by the coordinator (`slices/<id>.md`), a build by an Opus 5.5 agent in a worktree off
`rust-port`, a review by a second agent and by the coordinator, fixes by the builder, one commit on `rust-port`.
Deferred work goes into "Carry-over" below, under the slice that must do it, in the same change. A slice's spec
includes every entry of its section; a done entry is deleted.

## CLI track

| # | Slice | Crates | Status |
| --- | --- | --- | --- |
| C0 | Workspace bootstrap, `pi-json`, xtask, `check.sh` | all skeletons, `pi-json`, `xtask` | building |
| C1 | Shared types + telemetry | `pi-types`, `pi-telemetry` | planned |
| C2 | pi-schema | `pi-schema` | planned |
| C3a | pi-ai core: catalog, registry, auth resolution, streams, transcript, retry, faux provider | `pi-ai` | planned |
| C3b | pi-ai wire APIs anthropic-messages and openai-completions | `pi-ai` | planned |
| C4 | Fake provider server + difftest driver, goldens from TS | `pi-fake-provider`, `pi-difftest` | planned |
| C5 | pi-agent | `pi-agent` | planned |
| C6a | pi-sdk core: extension, events, tools, contexts, api, settings, `docs/sdk.md` | `pi-sdk` | planned |
| C6b | pi-sdk theme and UI surface; pi-tui component contract and colors | `pi-sdk`, `pi-tui` | planned |
| C7 | pi-core foundation: config, settings, auth, lockfile, models, trust, keybindings | `pi-core` | planned |
| C8 | Session manager (JSONL v3, migration, tree, projection) | `pi-core` | planned |
| C9 | Tools | `pi-core` | planned |
| C10a | Agent-session I | `pi-core` | planned |
| C10b | Agent-session II | `pi-core` | planned |
| C11 | Compaction | `pi-core` | planned |
| C12 | M1: headless `pi` (print, json, rpc) | `pi-cli` | planned |
| C13a | Providers II (responses, azure, codex, google, vertex, pi-messages) | `pi-ai` | planned |
| C13b | Provider definitions + OAuth I | `pi-ai` | planned |
| C14 | Providers III (bedrock, mistral, images, classifiers, OAuth II, remote catalog) | `pi-ai`, `pi-core` | planned |
| C15a | pi-tui I (terminal, keys, width, colors, layout) | `pi-tui` | planned |
| C15b | pi-tui II (components, renderers) | `pi-tui` | planned |
| C16a | Markdown, highlighting, images | `pi-tui` | planned |
| C16b | Editor, clipboard | `pi-tui` | planned |
| C17 | Interactive I | `pi-interactive` | planned |
| C18a | Interactive II (selectors, dialogs, setup) | `pi-interactive` | planned |
| C18b | M2: interactive `pi` | `pi-interactive`, `pi-cli` | planned |
| C19 | pi-mcp | `pi-mcp` | planned |
| C20 | Built-ins mcp + tool-search | `pi-ext-mcp`, `pi-ext-tool-search` | planned |
| C21a | Codemode spike (wasmtime + quickjs.wasm) | `pi-codemode` | planned |
| C21b | Codemode + built-in | `pi-codemode`, `pi-ext-codemode` | planned |
| C22 | Subcommands, packages, export-html | `pi-core`, `pi-cli` | planned |
| C23 | Utils long tail | `pi-core` | planned |
| C24 | Hardening (regressions, Windows, perf) | all | planned |
| C25 | Parity sign-off, packaging | `pi-cli`, scripts | planned |

## Durable track

| # | Slice | Crates | Status |
| --- | --- | --- | --- |
| D0a | chord: context, json, delta ops | `pi-chord` | planned |
| D0b | chord: tracker, diff, watch, output, sqlite | `pi-chord` | planned |
| D1 | Durable types, memory storage, storage conformance | `pi-durable` | planned |
| D2a | Session I | `pi-durable` | planned |
| D2b | Session II | `pi-durable` | planned |
| D3 | jsonl + sqlite storage | `pi-durable` | planned |
| D4 | Env traits + local env + env conformance | `pi-durable` | planned |
| D5 | Remote env client | `pi-env-client` | planned |
| D6a | Harness I | `pi-durable` | planned |
| D6b | Harness II | `pi-durable` | planned |
| D7a | Built-in tasks I | `pi-durable` | planned |
| D7b | Built-in tasks II | `pi-durable` | planned |
| D8 | Tools + images | `pi-durable` | planned |
| D9 | Durable codemode | `pi-durable` | planned |
| D10 | Examples + sign-off | `pi-durable` | planned |

## Carry-over

Obligations a slice deliberately left to a later one, under the slice that must do them, with where they came from.
Whoever defers something adds it here in the same change; a slice's spec includes every entry of its section.

### C18b

- **Mermaid rendering**: diagrams render as a fenced code block; an ANSI renderer port of `lovely-mermaid` only if the
  owner asks (plan, 2026-11-06).

### After C25

- **llama.cpp built-in** (`packages/coding-agent/src/extensions/llama/`, 1.6k lines): provider, `/llama` command,
  Hugging Face downloads, TUI. Deferred by the owner (2026-11-06).
- **Dynamic extension loading**: `pi-ext-loader` (shared objects or WASM) if wanted; all SDK traits stay object-safe
  and `Send + Sync` so this remains possible (plan, 2026-11-06).
- **Windows PowerShell tool and `clip.exe` paths** are built in C9/C23 but only type-checked, not run, until a Windows
  machine is available (plan, 2026-11-06).
