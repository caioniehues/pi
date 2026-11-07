# Slice C3a: `pi-ai` core (no wire APIs yet)

Read first: `rust/docs/conventions.md`, `rust/docs/slices.md`, `rust/crates/pi-json`, `rust/crates/pi-types`,
`rust/crates/pi-schema`, `rust/crates/pi-telemetry` (their `lib.rs` and docs).
TS references, read in full: `packages/ai/README.md`, `packages/ai/src/models.ts`, `packages/ai/src/model-catalog.ts`,
`packages/ai/src/models-store.ts`, `packages/ai/src/env-api-keys.ts`, `packages/ai/src/auth/{types,resolve,helpers,context,credential-store}.ts`,
`packages/ai/src/utils/{event-stream,streaming-json,json-parse,retry,provider-retry,overflow,transcript,identity,error-body,estimate,validation,text,diagnostics,headers,provider-env,sanitize-unicode,hash,abort,abort-signals,sleep,string-enum,model-operations,models-error,openrouter-cost}.ts`,
`packages/ai/src/api/{simple-options,transform-messages,constrained-sampling}.ts`, `packages/ai/src/providers/{faux,compat-schema,model-schema}.ts`,
`packages/ai/src/telemetry.ts`, `packages/ai/src/session-resources.ts`, `packages/ai/scripts/model-data.ts`,
`packages/ai/catalog/README.md`, and the tests named under Acceptance.

## Goal

Everything in pi-ai that is not a provider's wire protocol: the model catalog and registry, auth resolution for API
keys, the event stream, streaming tool-argument JSON, transcript and system-message replay, retry and overflow
detection, identity headers, cost, validation, and the in-process faux provider every later test uses.

## Model data

- `pi-ai/build.rs` reads `packages/ai/src/providers/data/*.json` (path overridable with `PI_MODEL_DATA_DIR`),
  verifies `.manifest.json` the way `scripts/model-data.ts` does (schema version 6, structure hash), and generates
  `$OUT_DIR/model_data.rs` with one `include_str!` per provider; a missing directory fails the build with
  "run `npm run hydrate:model-data` in packages/ai". To hydrate in your worktree: `npm ci --ignore-scripts` at the
  repo root, then `npm run hydrate:model-data`; never commit the data (it is gitignored).
- `catalog.rs` ports `model-catalog.ts`: groups by API, flattened into `Model`, `ImageModel`, `ClassifierModel`
  values with `provider` set; `MODELS`, `IMAGE_MODELS`, `CLASSIFIER_MODELS` lookups by provider id; model keys and
  ids as in TS (`model-data-validation.test.ts`, `model-catalog-types.test.ts`, `model-types.test.ts` define the
  invariants).
- `compat.rs` types the per-API `compat` objects from `compat-schema.ts` as serde structs that keep unknown keys
  (`openObject`), replacing the `Option<JsonObject>` placeholder of C1 in `pi-types` (edit `pi-types::model` in this
  slice; it is the one allowed cross-crate change).

## Registry and auth

- `models.rs`: `Models` (the registry: providers by id, `get_models`, `get_model`, `*_of_type`, `get_all_models`,
  `refresh`, `check_auth`, `get_available*`, `get_auth`, `login`, `logout`, `stream`, `complete`, `stream_simple`,
  `complete_simple`, `stream_deferred`, `fetch_deferred`, `cancel_deferred`, `generate_images`, `classify`), the
  `Provider` trait (object-safe, `Send + Sync`, async via `async_trait`; `stream` returns an
  `AssistantMessageEventStream`), `create_provider` with `CreateProviderOptions`, `calculate_cost` (tiers, 1h cache
  write at 2x), `get_supported_thinking_levels`, `clamp_thinking_level`, `models_are_equal`, request transforms
  (`on_payload`, `on_response`), telemetry observer (`AI_TELEMETRY_SCHEMA`, `observe_model_request`).
- `auth/`: `types.rs` (`Credential`, `ApiKeyAuth`, `OAuthAuth` as traits with the same methods, `AuthInteraction`,
  `AuthPrompt`, `AuthEvent`, `ProviderAuth`, `AuthResult`, `AuthCheck`, `CredentialStore` trait), `resolve.rs`
  (`resolve_provider_auth`, `refresh_stored_oauth_credential` calling the `OAuthAuth` trait; the concrete OAuth
  flows are C13b), `helpers.rs` (`env_api_key_auth`), `context.rs`, `credential_store.rs` (in-memory store;
  the file-backed `auth.json` store is C7 in `pi-core`). `env_api_keys.rs` ports the env-var tables and
  `find_env_keys`/`get_env_api_key`.
- `models_store.rs`: `ModelsStore` trait and `InMemoryModelsStore`.

## Streaming and request plumbing

- `event_stream.rs`: `EventStream<T, R>` as an async channel with the same semantics (push after done is dropped,
  `end(result)`, `result()` future) and `AssistantMessageEventStream` with its duration timing. The `partial`
  message: the Rust stream owns one `AssistantMessage` and every event carries an `Arc<AssistantMessage>` snapshot
  taken at push time (cheap clone of the Arc; the snapshot is made by cloning the partial only when a consumer is
  waiting or queued; document the cost decision in the file comment). `pi-types::stream_event` is adjusted in this
  slice to hold `Arc<AssistantMessage>` for `partial`.
- `streaming_json.rs`: `StreamingJsonParser` and `ToolCallArgumentsStream` ported exactly, including `JsonChange`
  events; `json_parse.rs`: `parse_json_with_repair` using the same partial-JSON repair rules as `partial-json`
  (port the subset `json-parse.ts` uses; test with `test/data/json-test-suite.json`).
- `transcript.rs`: `normalize_context`, `create_initial_system_message`, `get_current_system_message`,
  `get_current_tools`, `collapse_system_messages`, `resolve_transcript`, `to_tool_declaration`, `get_tool_state_changes`,
  `get_declared_tools`, `has_tool_redefinitions`, `has_non_additive_tool_changes`, `resolve_transcript_tools`.
- `retry.rs` and `provider_retry.rs`: policies, `retry_delay_ms`, `retry_assistant_call`, `is_retryable_assistant_error`,
  `retry_provider_request` with `Retry-After` handling; `overflow.rs`: `is_context_overflow`, `is_recoverable_length`,
  `get_overflow_patterns`; `error_body.rs`: `normalize_provider_error`, `format_provider_error`, 4000-char
  truncation, `with_cause_detail`; `estimate.rs`; `identity.rs` (`ClientIdentity`, `PI_ATTRIBUTION`,
  `format_user_agent`, attribution headers per target, `with_client_identity`); `validation.rs` (`validate_tool_call`
  through `pi-schema` with formats, coercion and `unresolved_refs: Reject`); `text.rs`; `diagnostics.rs`;
  `sanitize_unicode.rs`; `hash.rs` (`short_hash`); `headers.rs`; `provider_env.rs`; `session_resources.rs`;
  `simple_options.rs` (thinking budgets, `clamp_max_tokens_to_context`, `adjust_max_tokens_for_thinking`,
  `build_base_options`), `transform_messages.rs` (cross-provider handoff, error tool-result image splitting),
  `constrained_sampling.rs`.
- `sse.rs`: a Server-Sent Events decoder over a `Stream<Item = Result<Bytes>>` with the exact line and event rules
  of `iterateSseMessages` in `anthropic-messages.ts` (CR, LF, CRLF; `data:` concatenation; `event:`; ignoring
  comments; a final unterminated event flushed at end of body). Used by every SSE API in C3b and later.
- `http.rs`: one `reqwest::Client` factory with pi's defaults (rustls, gzip, zstd, proxy from env, no automatic
  redirects for auth-bearing requests as in undici defaults, connect and idle timeouts as `http-dispatcher.ts`
  sets them: read `packages/coding-agent/src/core/http-dispatcher.ts`), `ProviderRequestOptions` plumbing
  (`fetch` override as a trait object so tests and the fake provider can intercept), `timeout_ms`, `headers` with
  `null` removal.

## Faux provider

`providers/faux.rs`: a full port of `faux.ts`: scripted responses (`FauxResponseStep` as either a message or a
closure), token-by-token streaming with `tokens_per_second` and `token_size`, deferred handles with `pending_fetches`
and `poll_after_ms`, `state` counters, `register_faux_provider` on a `Models`, `faux_text`, `faux_thinking`,
`faux_tool_call`, `faux_assistant_message`. Behind feature `faux` (default off, enabled by tests and by
`pi-core`'s testing harness).

## Acceptance

- Port case by case: `event-stream`, `streaming-json`, `streaming-tool-arguments`, `json-parse`, `retry`,
  `provider-retry`, `overflow`, `context-overflow`, `context-estimate`, `tokens`, `total-tokens`, `system-message-replay`,
  `transcript-tool-changes`, `identity`, `error-body`, `validation`, `validation-probes`, `text`, `unicode-surrogate`,
  `model-cost-tiers`, `model-types`, `model-catalog-types`, `model-data-validation`, `models-runtime`, `models-entry`,
  `env-api-keys`, `compat-schema`, `compat-env`, `sampling-options`, `clamp-max-tokens`, `reasoning-options`,
  `max-thinking`, `xhigh`, `supports-xhigh`, `faux-provider`, `telemetry`, `telemetry-options`, `uuid`, `message-types`,
  `lax-message-content`, `constrained-sampling`, `transform-messages-*`, `cross-provider-handoff`,
  `tool-call-id-normalization` (the parts that do not need a wire API), `oauth-auth` (the resolve parts).
  Name each Rust test after its TS test.
- `cargo test -p pi-ai` runs without network and without hydrated data for everything except the catalog tests,
  which are `#[ignore]`d with a message when `PI_MODEL_DATA_DIR` and the default directory are both absent.

## Carry-over inputs

None.
