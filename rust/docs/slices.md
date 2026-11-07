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
| C0 | Workspace bootstrap, `pi-json`, xtask, `check.sh` | all skeletons, `pi-json`, `xtask` | done |
| C1 | Shared types + telemetry | `pi-types`, `pi-telemetry` | building |
| C2 | pi-schema | `pi-schema` | done |
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
| D0a | chord: context, json, delta ops | `pi-chord` | done |
| D0b | chord: tracker, diff, watch, output, sqlite | `pi-chord` | building |
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

### C1

- **Lenient surrogates**: pi-types tests and readers must use `pi_json::parse` (lenient surrogates), not
  `serde_json::from_str` (D0a review, 2026-11-06).

### C3a

- **Tool-call arguments with unpaired surrogates**: decided in D0a: `pi_json::parse` parses leniently and maps a lone
  surrogate escape (`"\ud800"`) to U+FFFD, where `serde_json` rejects it and `JSON.parse` keeps it. pi-ai's argument
  parsing must go through `pi_json::parse`. One unpaired surrogate and U+FFFD both count as one code point, so
  pi-schema's length checks agree with TS (C2 review, 2026-11-06).

### C18b

- **Mermaid rendering**: diagrams render as a fenced code block; an ANSI renderer port of `lovely-mermaid` only if the
  owner asks (plan, 2026-11-06).

### C23

- **`Date.parse` of offset-less date-times and non-ISO text**: `pi_json::parse_iso_ms` reads the ECMA-262 Date Time
  String Format as V8 does, but returns `None` for date-times without an offset (`2024-01-01T12:00`, local time in
  JS) and for V8's legacy non-ISO forms (`Tue, 05 Nov 2024 12:00:00 GMT`, `2024-1-01`); port them, with the local
  time zone, where a TS caller parses such text (C0, 2026-11-06).
- **`iso_ms` past the JS range**: for times beyond ±8.64e15 ms, where `toISOString` throws, `iso_ms` prints the
  proleptic Gregorian date instead of failing. Acceptable while every `SystemTime` comes from a JS-range source;
  revisit if one can come from elsewhere (C0, 2026-11-06).

### C24

- **pi-schema checks that allocate**: `check()` allocates nothing for valid values except where `regress` runs
  (`pattern`, `patternProperties`, the regex formats: one allocation per match), `propertyNames` (each key becomes a
  `JsonValue`), `unevaluated*` (the sets of evaluated names) and `$dynamicRef` (the scope stack); remove them if
  profiles show them (C2, 2026-11-06).
- **pi-schema `parse()`/`errors()` compile per call**: TS keeps its explainer and coercer after the first use; the
  port compiles the schema again for each call, because that compiler borrows the validator's schema. `parse()` runs
  `coerce()` on every tool call: measured, the edit-tool schema checks in 0.04 µs and parses in 2.7 µs; an MCP-style
  schema with three patterns checks in 0.3 µs and parses in 10.5 µs, because the pattern regexes recompile per call.
  Acceptable against model latency; `errors()` runs only on failure. Cache the compiler if profiles show it (C2
  review, 2026-11-06).
- **No shared regex cache across validators**: TS's `sharedRegex` shares one `RegExp` per pattern source across all
  compiled schemas; the port compiles each pattern once per `compile` (and per `parse`/`errors` call, above). Add a
  shared cache if many tools repeat patterns (C2 review, 2026-11-06).

### D0b

- **`TelemetrySpan` from C1**: replace `pi_chord::context::TelemetrySpan` (a local trait with `add_event`,
  `set_attributes`, `start_child`) with `pi_telemetry::TelemetrySpan` once C1 has landed, add `trace_in` (TS `traceIn`,
  which needs `SpanOptions` and span ends), and port the `traceIn` halves of `context.test.ts` (D0a, 2026-11-06).
- **`PendingOps` fuzz tests**: `PendingOps` and its adversarial, cap, ownership, size and cost tests landed in D0a;
  the four "PendingOps folding" fuzz tests of `delta-pending.test.ts` need the tracker and `diff_revisions`, so they
  are D0b's. The folding rules are done; D0b changes them only if those tests show a difference (D0a, 2026-11-06).
- **`overlap` counts UTF-16 code units**, as TS does (the corpus checks it); `append_text`'s prefix detection must
  convert its result to a byte offset into the Rust string (D0a, 2026-11-06).
- **Op payloads do not share containers with revisions**: ops hold `JsonValue` payloads, so a prepared change's ops
  cannot share containers with the revision (TS README: payloads may be the same objects as parts of
  `prepared.value`); measured memory of op backlogs plus revisions decides in D2b whether a `Snapshot`-payload op
  representation is needed (D0a review, 2026-11-06).
- **Tracker-dependent tests**: port `delta-apply-immutable.test.ts` "handles tracker-produced batches across arbitrary
  revision boundaries" with the tracker (D0a, 2026-11-06).
- **A direct `Snapshot` serializer**: snapshot bytes go through `to_value` and `pi_json::to_js_string` (a deep copy);
  write a serializer over `Snapshot` if the watch or sqlite ports need it (D0a review, 2026-11-06).
- **Appending to a `Snapshot` string copies it**: strings are `Arc<str>`, so each `a` applied to a snapshot costs the
  string's whole length (V8 appends to a rope in O(chunk)), and N appends to one string cost O(N x length). Measure
  with the watch and output ports; if streamed output makes it show, give `Snapshot` a chunked string (D0a,
  2026-11-06).

### D1

- **Abort reasons**: `AbortSignal` carries no reason; Durable's `harness/util.ts:11,19`, `codemode/codemode.ts:464` and
  Chord's remote provider need `signal.reason`: give `AbortSignal` an optional reason (a `ChordError`-like value) set
  at cancellation in D1 (D0a review, 2026-11-06).

### After C25

- **llama.cpp built-in** (`packages/coding-agent/src/extensions/llama/`, 1.6k lines): provider, `/llama` command,
  Hugging Face downloads, TUI. Deferred by the owner (2026-11-06).
- **Dynamic extension loading**: `pi-ext-loader` (shared objects or WASM) if wanted; all SDK traits stay object-safe
  and `Send + Sync` so this remains possible (plan, 2026-11-06).
- **Windows PowerShell tool and `clip.exe` paths** are built in C9/C23 but only type-checked, not run, until a Windows
  machine is available (plan, 2026-11-06).

## Decided divergences

Behaviour of the Rust port that deliberately differs from the TS reference, with the decision that settled it.

- **Lone surrogates read as U+FFFD** (D0a review, 2026-11-06): a JS string may hold an unpaired UTF-16 surrogate and
  `JSON.stringify` writes it as an escape; `serde_json` rejects such text. `pi_json::parse` maps every lone surrogate
  escape to U+FFFD and never fails on one, so a pair split across two `a` ops and joined by TS reads as two U+FFFD in
  Rust. Well-formed text round-trips exactly. Likewise a `t` count that cuts a surrogate pair leaves U+FFFD where JS
  leaves the low half (`pi_chord::delta`, D0a).
- **Integers of 2^64 and more in ops** (D0a, 2026-11-06): TS accepts any non-negative integral number; `pi-chord`
  rejects those of 2^64 and more with the TS message of the field (path segments, `t` counts, `p` index and remove
  count, `#` ids and path ids, permutation entries). No producer writes one. Indices above 2^53 stay exact where a JS
  number rounds them, so an index segment on an object names the key of its exact digits.
- **Malformed A-labels in URL hosts** (C2 review, 2026-11-06): the `url`, `iri` and `iri-reference` formats reject
  hosts with malformed A-labels (`http://xn--a.com/`, `http://xn--/`, `http://xn--ab-.com`) that Node's
  `URL.canParse` accepts; the `url` crate validates IDNA strictly. Hostile input only (`pi_schema::formats`, C2).
