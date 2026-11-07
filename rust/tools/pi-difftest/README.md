# pi-difftest

A black-box oracle for the Rust port. The TypeScript `pi` at the commit in `TS_PIN` is driven over RPC against
the scripted fake provider (`../pi-fake-provider`); its normalized output is stored as goldens, and the Rust
`pi` must reproduce them byte for byte.

## Commands

Run from `rust/`:

```bash
cargo run -p pi-difftest -- compare --all --pi target/release/pi    # Rust pi against the goldens; exit 1 on a diff
cargo run -p pi-difftest -- regen --all                               # TS pi of this checkout -> goldens
cargo run -p pi-difftest -- live --all --pi target/release/pi        # TS and Rust side by side, no goldens
cargo run -p pi-difftest -- ts-run text-reply --keep                  # normalized artefacts of one TS run
cargo run -p pi-difftest -- rust-run text-reply --pi target/release/pi
```

Options: `--all` or scenario names; `--api anthropic-messages|openai-completions` runs one API only; `--keep`
leaves each run's sandbox on disk and prints its path; `--out <dir>` (ts-run, rust-run) is where the normalized
artefacts go; `--ts-repo <dir>` runs `node <dir>/packages/coding-agent/dist/cli.js` instead of this checkout's;
`--package-dir <dir>` is the directory `PI_PACKAGE_DIR` reaches for `--pi` (default: this checkout's
`packages/coding-agent`, whose docs and examples the Rust pi can share).

The driver is validated against itself: `compare --all --pi tools/pi-difftest/ts-pi.sh` runs the TS pi through the Rust-side path and must pass.

## Regenerating goldens

`regen` refuses to run unless the TS checkout's `packages/` equal `TS_PIN` (`git diff --quiet <pin> -- packages`)
and `dist/cli.js` exists, and fails if the TS pi does not complete a scenario. `regen.sh` does the whole job in a
fresh `git worktree` at the pin under a temp directory: `npm ci --ignore-scripts`, `npm run hydrate:model-data`
(network; `build:offline` needs the hydrated model data although the fake provider is declared in `models.json`),
`npm run build:offline`, then `regen --ts-repo <worktree>`. `./regen.sh --here` uses this checkout instead; it
checks that `packages/` equal the pin but trusts the existing `dist/` to have been built from them, so rebuild
after switching branches.

`regen` writes nothing until every run of the invocation succeeded: a run with a driver error or a provider
script not consumed exactly (pi made more or fewer requests than `provider.json` scripts) fails it with the
scenario and the counts, and the goldens and `META.json` stay as they were. `ts-run` and `rust-run` exit 1 on the
same problems. `golden/META.json` records the TS commit, the node version and the date; it is rewritten only when a golden
changed or the commit or node version differ, so regenerating unchanged goldens leaves the tree clean. A second
`regen --all` must produce zero diff; review a changed golden like code.

## A run

For each scenario and each API in its `apis`, the driver

1. creates a sandbox `/tmp/pi-difftest-<12 hex digits>/` with `cwd/` (a copy of `workspace/`), `agent/`, `home/`,
   `provider/` and `pi-package`, a link to the pi's package directory. The path has a fixed length, and
   `PI_PACKAGE_DIR` points at the link, because the cwd and the docs paths are in the system prompt, whose length
   feeds pi's token estimates (`estimatedTokensAfter`, `contextUsage`): a checkout or TMPDIR of another length
   would change them;
2. starts the fake provider in process, serving `provider.json`, recording requests to `provider/requests.jsonl`;
3. writes `agent/settings.json` (install telemetry and analytics off, `quietStartup`, merged with the scenario's
   `settings`), `agent/models.json` (provider `fake`, `api` as the run says, `baseUrl` `http://127.0.0.1:<port>`
   for Anthropic, whose SDK appends `/v1/messages`, and `http://127.0.0.1:<port>/v1` for OpenAI, whose SDK appends
   `/chat/completions`; model `fake-model` merged with the scenario's `model`) and `agent/auth.json` (an
   `api_key` for `fake`);
4. starts `pi --mode rpc --provider fake --model fake-model <args> <run args>` in `cwd/` with an empty
   environment apart from `PATH`, `HOME=home/`, `PI_CODING_AGENT_DIR=agent/`, `PI_PACKAGE_DIR=pi-package`, `PI_OFFLINE=1`, `PI_TELEMETRY=0`,
   `TZ=UTC`, `LANG=C.UTF-8` (no provider key can leak in), and drives it through the commands file;
5. closes stdin, waits for exit (30 s), kills pi's process group (pi runs in its own group, so no tool process
   outlives it; a run with a driver error is killed at once), reads the rest of stdout and stderr for at most 5 s,
   and collects and normalizes the artefacts. The sandbox is removed on every exit path unless `--keep`. The
   tool is Unix-only (process groups, the package link); on Windows a job object would be needed.

## Scenario format

`scenarios/<name>/`:

- `scenario.json`: `description` (required); `apis` (default both); `args` (CLI arguments of every run);
  `model` (fields over the fake model, e.g. `{"reasoning": true}`); `settings` (deep-merged over the base
  settings); `compare` (artefacts written and compared, default all: `events`, `session`, `requests`, `fs`,
  `exit`); `runs` (default `[{"commands": "commands.jsonl"}]`; each run is a new pi process on the same agent
  dir and provider script, `args` may contain `{session}`, the newest session file so far).
- `commands.jsonl`: one RPC command per line, sent on stdin; the driver waits for its `response` (matched by
  `id`, or by `command` when the command has no id; 30 s) before the next line. Driver-only records:
  - `{"$wait": "<type>", "where"?: {...}, "timeoutMs"?: n}` consumes records until one of that `type` that
    contains the `where` subset. `$wait` keeps a cursor: it searches after the previous `$wait`'s match, and
    responses do not move it, because a command's events can precede its response (an abort's `agent_settled`).
  - `{"$sleep": ms}` waits.
  - `{"$release": true}` lets the fake provider's paused stream (`pauseAfter`) continue. A release with no stream
    paused is banked: the next stream to pause continues at once.
- `provider.json`: the fake provider script, `{"delayMs"?: n, "responses": [...]}`; request `n` gets response
  `n`; the top-level `delayMs` applies to every response that sets none. A response
  is `{content: [blocks], stopReason?: "stop"|"length"|"toolUse", usage?: {input, output, cacheRead, cacheWrite},
  delayMs?, chunking?: {"tokens": n} | {"bytes": n}, pauseAfter?: n, error?: {status, body, retryAfter?}}`;
  blocks are `{type: "text", text}`, `{type: "thinking", thinking, signature?}`, `{type: "toolCall", id, name,
  arguments}`. `tokens` cuts every delta into pieces of n characters, `bytes` cuts the whole SSE stream into n-byte
  writes; `delayMs` waits before each write; `pauseAfter` stops the stream after n writes until `$release`; `error`
  answers with that status, the body (a string as text, anything else as JSON) and `retry-after`. A request past
  the end of the script gets a 500 naming it. An error `status` outside 400-599 is rejected when the script loads.
The Anthropic `message_start` carries the input and cache counts and `message_delta` only `output_tokens`, so
pi takes the path of `anthropic-messages.ts` that keeps the start counts when a proxy omits them in the delta.
See `../pi-fake-provider/src/script.rs`.
- `workspace/`: copied into the sandbox's cwd (optional).

Every commands file ends with the state commands (`get_state`, `get_messages`, `get_session_stats`, `get_tree`),
so their responses are golden too.

### Determinism

Any command sent while pi is streaming must be sent while the stream is paused: give the response a
`pauseAfter`, `$wait` for the record that shows the pause point was reached (e.g. the first `text_delta`), send
the command (its response arrives while the stream is held), then `$release`. `steer`, `follow-up` and
`abort-mid-stream` do this.

Every script sets `"delayMs": 20`. The TS pi serializes a `message_update` record's `usage` from the live
message object some ticks after the event: when several SSE events arrive in one read, the earlier records
already show the usage of a later event (the final output tokens, or OpenAI's usage chunk), and whether they
arrive in one read depends on timing. A 20 ms gap between writes lets pi finish each write before the next
arrives, so each record shows the usage as of its event, which is also what an implementation that serializes
at emit time writes.

### Notes on the first scenarios

- `retry-429`: `retry.provider.maxRetries` is unset, so the SDK does not retry and the `retry-after` header is
  not used; the 429 reaches the session, which retries after `retry.baseDelayMs` (set to 50 ms) and emits
  `auto_retry_start`/`auto_retry_end`.
- `overflow-auto-compaction` and `manual-compact` set `compaction.keepRecentTokens` to 1; the cut then splits the
  last turn, so each compaction makes two summary requests (history, then turn prefix), and the overflow retry
  carries only the summary. `overflow-auto-compaction-turn` sets it to 10: the cut lands on the first answer,
  one turn-prefix summary covers the first prompt, and the retried request re-sends the first answer and the kept
  last prompt. (At TS_PIN no `keepRecentTokens` cuts exactly at the last prompt: up to 5 the cut falls on the
  failed answer after it, from 6 on the first answer.)
- `steer` and `follow-up` stream a text and bash tool call, paused after the first text delta: the steer arrives
  in request 2 after the tool result; the follow-up waits for the answer to the tool result and arrives in
  request 3.
- Bash commands in scenarios write their output in one `write` (`echo "files: $(ls)"`, not `echo a && ls`): pi
  emits one `tool_execution_update` per output read, so output written in two pieces gives one or two updates
  depending on timing.
- `tool-bash` streams the tool arguments in 3-character deltas; `text-reply` writes its non-ASCII reply in 7-byte
  pieces that cut UTF-8 sequences and SSE events, which pi reassembles into the same single `text_delta`.
- `abort-mid-stream` never releases its paused stream; the server lets paused streams go on shutdown.
- `thinking-blocks` declares the model with `reasoning: true` and runs with `--thinking high`; the OpenAI form
  streams the thinking as `reasoning_content` (the signature is Anthropic-only).

## Artefacts and normalization

Per run, in `golden/<scenario>/<api>/`:

- `events.jsonl`: every stdout record in order; with several runs each run starts with `{"$run": n}`.
- `session.jsonl`: every file under `agent/sessions/`, sorted by path, each after `{"$file": "<path>"}`.
- `requests.jsonl`: one record per provider request: `method`, `url`, `headers` (sorted by name, repeated values
  joined with `, `), `body` (parsed), and `bodyText` when the body is not the `JSON.stringify` form of its value.
  The normalizer drops the headers the SDKs and undici add (`x-stainless-*`, `accept-language`, `sec-fetch-mode`,
  `accept-encoding`, `connection`, `content-length`, `host`, `user-agent`: a decided divergence in
  `rust/docs/slices.md`); the headers pi sets and the URL stay compared.
- `fs.json`: the sandbox cwd after the run, `{"<path>": "<sha256>" | "dir"}` sorted by path.
- `exit.json`: per run `{code, stderrTail}` (last 20 stderr lines), plus `killed` and `driverError` when they
  happened.

The normalizer reads the artefacts in that order with one mapping per run, so cross-references survive:

- UUIDs become `uuid-1`, `uuid-2`, ... and whole strings of 8 lowercase hex digits (entry ids) `id-1`, ... in
  order of first appearance (in keys too);
- ISO timestamps become `$TIME`, session-file timestamps (`2026-11-06T10-00-00-000Z`) `$FILE_TIME`;
- the sandbox root (and its encoded form in session directory names) becomes `$ROOT`, the server address
  `$SERVER`;
- values by key: `timestamp` numbers `$EPOCH_MS`; `durationMs` and `wall_time_seconds` `$DURATION`; `version`
  strings `$VERSION`; `responseId` `$RESPONSE_ID`.
  `delayMs` is kept: every emitted delay (session retries, summarization retries) is deterministic at `TS_PIN`.

Nothing else is masked. Lines are re-serialized with `pi_json::to_js_string` in their own key order and compared
as text, so key order differences show (the Anthropic `Usage`, where TS writes `cacheWrite1h` after `cost`, is
compared as TS writes it). A line that is not the `JSON.stringify` form of its own value (`1.0`, extra spaces,
not JSON) becomes `{"$noncanonical": "<line>"}` with only the text rules applied, so formatting differences show.

## Adding a scenario

1. Create `scenarios/<name>/` with `scenario.json`, `provider.json`, `commands.jsonl` ending with the four state
   commands, and `workspace/` if the scenario needs files.
2. `cargo run -p pi-difftest -- ts-run <name> --keep` and read the artefacts; check `exit.json` has no
   `driverError`; a run that does not consume the script exactly is reported (a 500 "script exhausted" in the
   artefacts means pi asked for more, for example a turn-prefix summary after a split turn).
3. `regen <name>` twice: the second must report `unchanged`.
4. `compare <name> --pi tools/pi-difftest/ts-pi.sh` must pass.
5. Commit the scenario and its goldens together.
