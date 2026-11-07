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
| C1 | Shared types + telemetry | `pi-types`, `pi-telemetry` | done |
| C2 | pi-schema | `pi-schema` | done |
| C3a | pi-ai core: catalog, registry, auth resolution, streams, transcript, retry, faux provider | `pi-ai` | building |
| C3b | pi-ai wire APIs anthropic-messages and openai-completions | `pi-ai` | planned |
| C4 | Fake provider server + difftest driver, goldens from TS | `pi-fake-provider`, `pi-difftest` | done |
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
| C15a | pi-tui I (terminal, keys, width, colors, layout) | `pi-tui` | building |
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
| D0b | chord: tracker, diff, watch, output, sqlite | `pi-chord` | done |
| D1 | Durable types, memory storage, storage conformance | `pi-durable` | building |
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

### C3b

- **Anthropic `Usage` key order**: `pi_types::Usage` writes the TS constructor order (`input`, `output`,
  `cacheRead`, `cacheWrite`, `cacheWrite1h`, `reasoning`, `totalTokens`, `cost`), but `anthropic-messages.ts` assigns
  `cacheWrite1h` (and `reasoning` when reported) after `cost` on every message, so TS-written Anthropic assistant
  lines rewrite with a different key order. C3b writes `Usage` in that provider's order for its messages (C1 review,
  2026-11-06).
  Decided in C4: the difftest normalizer compares lines as text in their own key order, so the goldens hold TS's
  order (`cacheWrite1h` after `cost` in every Anthropic message's usage) and the Rust writer must produce it
  (C4, 2026-11-07).
- **Request shape in the difftest goldens**: `golden/*/requests.jsonl` holds the URL (Anthropic posts to
  `/v1/messages?beta=true`), the body in the SDKs' key order, and the headers pi sets (`anthropic-version`,
  `anthropic-beta`, `anthropic-dangerous-direct-browser-access`, `content-type`, `accept`, the shape of
  `x-api-key`/`authorization`, pi's identity and attribution headers). C3b matches the URL, the body key order
  and those headers; the SDK and undici headers are not reproduced (see "Decided divergences") (C4 review,
  2026-11-07).
- **Wire plumbing from C3a**: SSE bodies go through `pi_ai::sse::SseDecoder`, requests through `pi_ai::http::Fetch`
  (`ProviderRequestOptions::fetch` or `http::default_fetch()`), retries through `retry_provider_request` (implement
  `ProviderRequestError` for the API's HTTP errors), error display through `error_body::SdkError` and
  `format_provider_error`. An API's own options (`AnthropicOptions`, ...) travel in `StreamOptions::api_options`
  (`Arc<dyn Any>`), downcast by the API. Provider-scoped proxy variables (`ProviderEnv` `HTTP_PROXY`/`HTTPS_PROXY`)
  are not applied by `http.rs` (it reads the process environment); decide per API whether a scoped client is needed.
  reqwest exposes no happy-eyeballs attempt timeout (the dispatcher's 2 seconds) (C3a, 2026-11-07).
- **Tests C3a left for the wire APIs**: `sampling-options.test.ts`, `xhigh.test.ts`, the "client identity in
  requests" header-capture cases of `identity.test.ts`, the real `openai` SDK connection-failure case of
  `error-body.test.ts`, the anthropic and openai-completions payload cases of `transcript-tool-changes.test.ts`, the
  anthropic and openai-completions paths of `streaming-tool-arguments.test.ts` and its "tool-call stream state on
  errors" case. The live suites (`context-overflow`, `tokens`, `total-tokens`, `unicode-surrogate`,
  `cross-provider-handoff`, the live parts of `tool-call-id-normalization`) need wire APIs and keys; C3a did not
  port them; port them `#[ignore]`d with the env var they need, for the providers each slice adds (C3a, 2026-11-07).

### C7

- **models.json validation schemas**: `compat-schema.ts` and `model-schema.ts` as JSON Schemas (with their `default`
  annotations and the `ProviderCompatSchema` superset) for validating models.json through pi-schema, and the port of
  `compat-schema.test.ts`; C3a typed the compat objects (`pi_types::compat`) but did not port the schemas (C3a,
  2026-11-07).
- **Credential store**: the `auth.json` store implements `pi_ai::auth::types::CredentialStore`; `modify` must cancel
  only the wait for its lock (Models relies on it through `modify_once_started`) (C3a, 2026-11-07).

### C8

- **Pre-v3 lines and migration key order**: v1 entries have no `id`/`parentId` and v1 `compaction` has
  `firstKeptEntryIndex`, so both fixtures' entries read as `SessionEntry::Other`; a v2 `hookMessage` message is
  `AgentMessage::Other`. TS `migrateV1ToV2` mutates the parsed objects: it appends `id` and `parentId` at the end of
  each entry, appends `firstKeptEntryId` at the end of a compaction (and deletes the index), and appends `version` last
  to an unversioned header. The typed `pi-types` structs write `id`/`parentId` right after `type`, so typed
  re-serialization of a migrated v1 entry is not byte-identical to TS's rewrite (C1's fixture test: 0 of 2,020 v1
  entries are). `migrate_session_entries` works on the JSON objects as TS does, and the rewrite after migration must
  write those objects in TS's order (keep the migrated objects for `_rewriteFile`, or write entries in their read
  order) for the migration golden to be byte-identical (C1, 2026-11-06).
- **Malformed known entries**: a known `type` with a malformed body silently reads as `Other` (TS parity: TS does not
  validate). The C8 loader logs a diagnostic when an entry is `Other` but its `type` is a known kind. Token counts in
  `Usage` are `i64` because TS can write negatives (`google-generative-ai.ts`/`google-vertex.ts` subtract cached from
  prompt tokens without clamping), so such messages stay typed (C1 review, 2026-11-06).

### C11

- **`Usage::total_tokens` is `Option<i64>`**: sessions from before the field existed lack it. Port
  `calculateContextTokens` with TS's `usage.totalTokens || sum` (a `0` also falls back to the sum) (C1, 2026-11-06).

### C12

- **Release builds need model data**: `check.sh`'s release build and the packaging must have hydrated model data:
  a release build where `pi_ai::catalog::is_hydrated()` is false must fail (C12 for the `pi` binary's build, C25 for
  packaging) (C3a review, 2026-11-07).
- **Typed compat key order**: `pi_types::Model.compat` is typed per API (`ModelCompat`); its fields serialize in the
  schema's order, then unknown keys, while the catalog writes compat keys in varying orders, so a re-serialized model
  (RPC `get_available_models`, the models store) can order `compat` differently from TS, and the difftest compares
  lines as text. Keep the read order when writing models (C3a, 2026-11-07).
- **Ids the difftest normalizer maps**: entry ids must be 8 lowercase hex characters and session ids UUIDs (as TS
  makes them), or the normalizer leaves them raw and every golden differs (C4 review, 2026-11-07).
- **Difftest environment**: the driver runs pi with only `PATH`, `HOME`, `PI_CODING_AGENT_DIR`, `PI_PACKAGE_DIR`
  (a link in the sandbox), `PI_OFFLINE=1`, `PI_TELEMETRY=0`, `TZ=UTC`, `LANG=C.UTF-8`. The Rust pi must honor
  `PI_PACKAGE_DIR` for the docs, README and examples paths of its system prompt (TS `getPackageDir`), since the
  goldens hold `$ROOT/pi-package/...` and the prompt's length feeds token estimates; `--package-dir` sets the link's
  target (C4, 2026-11-07).
- **Unknown RPC commands**: `RpcCommand` rejects an unknown `type`; RPC mode must still echo `id` and answer
  `Unknown command: <type>`, so it reads `id` and `type` from the raw object before typed parsing (C1 review,
  2026-11-06).

### C13a

- **Responses-path tests from C3a**: the Responses cases of `constrained-sampling.test.ts` (convertResponsesTools,
  replayed grammar calls, custom tool input streaming), the Codex payload case of `max-thinking.test.ts`, the Responses
  and pi-messages paths of `streaming-tool-arguments.test.ts` and the Responses payload cases of
  `transcript-tool-changes.test.ts` (C3a, 2026-11-07).
- **Fake provider `/v1/responses`**: the route exists and answers 501; add the openai-responses SSE renderer
  (`packages/ai/src/api/openai-responses-shared.ts`) beside `anthropic.rs` and `openai_completions.rs`, and the
  API to `ALL_APIS` in pi-difftest (C4, 2026-11-07).

### C13b

- **OAuth flows**: the provider cases of `oauth-auth.test.ts` (Anthropic, Codex, OpenRouter, xAI, GitHub Copilot
  `toAuth`/`refresh`); C3a ported the `Models::get_auth` resolve chain with a test adapter. TS's `lazyOAuth` has no
  port (Rust links the flows) (C3a, 2026-11-07).

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
- **Assistant partial snapshots**: events carry `Arc<AssistantMessage>`; an edit copies the message while a queued
  event still holds the previous snapshot, so a lagging consumer pays one message copy per edit (O(n^2) for a long
  response). Measure with the agent loop; give content blocks shared storage if it shows (C3a, 2026-11-07).
- **Streaming JSON snapshot cost**: snapshots copy strings and completed containers (a JS snapshot shares them), so
  the copy credit counts bytes and a large document's preview lags behind its text (bounded, linear total work).
  serde_json recurses on clone and drop, so values nested a few thousand levels deep overflow the stack (the parser
  itself does not recurse; `pi_json::parse` stops at 128 levels) (C3a, 2026-11-07).
- **Tool validation compiles per call**: `validate_tool_arguments` compiles the schema on every call (TS caches per
  schema object in a `WeakMap`); cache per tool if profiles show it (C3a, 2026-11-07).

### C25

- **Release builds need model data**: without hydrated data `pi-ai`'s build.rs warns and embeds an empty catalog (so
  slices build in clean checkouts); packaging must hydrate first and fail a release build that has no catalog (C3a,
  2026-11-07).

### D1

- **`TelemetrySpan` from C1**: replace `pi_chord::context::TelemetrySpan` (a local trait with `add_event`,
  `set_attributes`, `start_child`) with the landed `pi_telemetry::TelemetrySpan`. In pi-telemetry,
  `TelemetrySpan: TelemetryContext`, `start_span` returns a `Box<dyn TelemetrySpan>`, the span settles on
  `end(self: Box<Self>)` or drop, and `handle()` returns an `Arc<dyn TelemetryContext>` without `add_event` or
  `set_attributes`. Since `Context` records events and attributes on its span, it holds `Arc<dyn TelemetrySpan>`
  (made from the box; the span settles when the last clone drops) or a recording handle of its own; `handle()` alone
  is not enough. Add `trace_in` (TS `traceIn`, with `SpanOptions` and span ends) and port the `traceIn` halves of
  `context.test.ts` (D0a, C1 review, 2026-11-06; D0b, 2026-11-07).
- **Abort callbacks for watches**: a `CancellationToken` has no callback, so `Watch::observe_abort` is observed by the
  futures that wait on the watch (delivery and `closed`); an aborted watch nobody waits on ends, and detaches, on the
  next poll. When `AbortSignal` gets its reason, give it a way to run a callback on abort (or a spawner) so a watch
  ends the moment its context is aborted, as in TS (D0b, 2026-11-07).
- **Object safety of the watch and SQLite traits**: `WatchHandle`, `NativeWatchHandle`, `Watchable`,
  `NativeWatchable`, `SqliteExecutor` and `SqliteDatabase` are not object-safe (generic `start` and `transaction`,
  `impl Future` returns). Durable decides whether it needs boxed (`dyn`) versions and adds them (D0b review,
  2026-11-07).
- **Abort reasons**: `AbortSignal` carries no reason; Durable's `harness/util.ts:11,19`, `codemode/codemode.ts:464` and
  Chord's remote provider need `signal.reason`: give `AbortSignal` an optional reason (a `ChordError`-like value) set
  at cancellation in D1 (D0a review, 2026-11-06).

### D2b

- **Op payloads do not share containers with revisions**: ops hold `JsonValue` payloads, so a prepared change's ops
  cannot share containers with the revision (TS README: payloads may be the same objects as parts of
  `prepared.value`); measured memory of op backlogs plus revisions decides whether a `Snapshot`-payload op
  representation is needed (D0a review, 2026-11-06).
- **Appending to a `Snapshot` string copies it**: strings are `Arc<str>`, so each `append_text` copies the string and
  N appends to one string cost O(N x length). Measured in D0b: a 500 KB stream in 40-byte chunks (12,500 changes, one
  tracker change each) takes 0.35 s in a debug build, 0.28 s in release. If Durable's streamed output shows it, give
  `Snapshot` a chunked string (D0a, measured D0b, 2026-11-07).

- **Per-change cost grows with the width of every container on the path**: an edit copies each container on its path
  (`Arc::make_mut`, O(width) for the first edit of a revision) and `prepare` scans each of them (O(width) diff), where
  the TS overlay costs O(edits). Measured by the D0b review, 2,000 changes of one append each:

  | Container on the path | Time |
  | --- | --- |
  | array of 10,000 elements | 0.197 s |
  | object of 10,000 keys | 1.63 s |

  Durable's documents with wide arrays or maps (conversation entries, task records) pay this per change; if a session
  shows it, track dirty children in the draft so the diff visits only them, and give wide containers a persistent
  (chunked) representation so the copy is O(log width) (D0b review, 2026-11-07).

### D3

- **A direct `Snapshot` serializer**: snapshot bytes go through `to_value` and `pi_json::to_js_string` (a deep copy);
  the watch and sqlite ports of D0b did not need one. Write a serializer over `Snapshot` if jsonl or sqlite storage
  measures the copy (D0a review, 2026-11-06).

### D6a

- **A runtime for `Progress`**: `pi_chord::output::Progress` paces on a `ProgressClock` that also spawns the commit
  future (TS uses `Date.now`, `setTimeout` and the event loop); pi-chord has no runtime, so there is no default clock.
  Durable's harness supplies a tokio-backed one (D0b, 2026-11-07).

### After C25

- **Telemetry store conformance** (out of scope): `packages/telemetry/test/store-conformance.ts` is shared by the
  in-memory and the Chord SQLite telemetry stores. C1 ported its cases as in-memory tests
  (`pi-telemetry/tests/memory.rs`); no planned slice builds Chord's SQLite telemetry store, so the reusable
  `pi_telemetry::testing` suite (parameterised by a store factory, with an injectable clock for the exact-time cases)
  waits until a slice needs that store; if Durable does, move this entry to that slice (C1, 2026-11-06; filed by the
  D0b review, 2026-11-07).
- **llama.cpp built-in** (`packages/coding-agent/src/extensions/llama/`, 1.6k lines): provider, `/llama` command,
  Hugging Face downloads, TUI. Deferred by the owner (2026-11-06).
- **Dynamic extension loading**: `pi-ext-loader` (shared objects or WASM) if wanted; all SDK traits stay object-safe
  and `Send + Sync` so this remains possible (plan, 2026-11-06).
- **Windows PowerShell tool and `clip.exe` paths** are built in C9/C23 but only type-checked, not run, until a Windows
  machine is available (plan, 2026-11-06).

## Resolved points

- **The Rust tracker's op batches** (D0b, 2026-11-07; spec `slices/D0b.md`): the TS tracker is a `Proxy` over a piece
  table; Rust has an explicit `Draft` API over a copy-on-write `Snapshot`, diffed against its base at `prepare` with
  the draft's hints (the text of each `append_text`, the base position of each element of a structurally edited array,
  whole placements). Ops need not match TS batches byte for byte: they must replay under the TS `apply` and
  `applyImmutable` to the prepared value, be valid ops, come from the value the TS draft reaches, and be reasonably
  minimal (`append_text` one `a`, a `set` one `s`, a `push` one `p`, an element edited in place still diffed in place
  after its array was spliced or reordered, no root replacement but `prepare_replace` or past 4,096 ops). The
  cross-apply corpus (`fixtures/chord-delta/tracker-scripts.jsonl`, `check-tracker-out.mjs`) checks all of it against
  the TS implementation. As the TS tracker, the Rust one replays a key deleted and set again with `d` and `s`; as TS
  `diffRevisions`, `diff_revisions` ignores key order (D0b review, 2026-11-07).

## Decided divergences

Behaviour of the Rust port that deliberately differs from the TS reference, with the decision that settled it.

- **SDK and undici request headers** (coordinator, C4 review, 2026-11-07): headers that the TS SDKs or undici add,
  not pi (`x-stainless-*`, `accept-language`, `sec-fetch-mode`, `accept-encoding`, `connection`, `content-length`,
  `host`, `user-agent`), are no parity requirement; the difftest normalizer drops them from `requests.jsonl`.
  Headers pi sets and the URL, `?beta=true` included, stay compared.

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
- **Streaming JSON lone surrogates** (C3a, 2026-11-07): `StreamingJsonParser` holds a `\uD800`-`\uDBFF` escape back
  until the next escape shows whether it pairs, and a lone one reads as U+FFFD; TS shows the lone high surrogate while
  its low half is pending. Numbers `f64` cannot hold (`1e400`) are dropped where JS shows `Infinity`.
- **JSON parse error messages** (C3a, 2026-11-07): `argumentsError` and other parse failures carry serde_json's
  message (`EOF while parsing a string at line 1 column 9`) where TS carries V8's.
- **User-Agent runtime** (C3a, 2026-11-07): the detailed `User-Agent` is `pi/<version> (<platform>; rust/<rustc
  version>; <arch>)` with Node's platform and arch names, where TS names `node/<version>`.
- **Same-origin redirects only** (C3a review, 2026-11-07): undici's fetch follows up to 20 redirects and strips
  `authorization`, `proxy-authorization`, `cookie` and `host` when the origin changes (scheme included); reqwest's
  default policy strips sensitive headers only on a host or port change; neither strips `x-api-key`-style headers.
  The port's client (`pi_ai::http`) therefore follows redirects only to the same origin (scheme, host, port), up to
  20, and returns a cross-origin 3xx as the response.
- **Ill-typed compat fields** (C3a review, 2026-11-07): `pi_types::compat` reads compat objects field by field; a
  field whose value does not fit its type (`"supportsStore": "yes"`) is kept raw in `extra` and reads as unset, where
  TS casts the object and uses the ill-typed value as it is (a truthy string enables the flag). The valid fields read
  as in TS.
