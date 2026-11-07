# Slice C3b: `pi-ai` wire APIs anthropic-messages and openai-completions, providers anthropic and openai

Read first: C3a's spec and crate. TS references, read in full: `packages/ai/src/api/anthropic-messages.ts`,
`packages/ai/src/api/openai-completions.ts`, `packages/ai/src/api/openai-prompt-cache.ts`,
`packages/ai/src/api/github-copilot-headers.ts`, `packages/ai/src/providers/{anthropic,openai,all}.ts`,
`packages/ai/src/utils/openrouter-cost.ts`, the parts of `node_modules/@anthropic-ai/sdk` and `node_modules/openai`
that the two files use (client construction, default headers, retry and timeout defaults, the `X-Stainless-*`
headers, the `asResponse()`/`withResponse()` paths, how `maxRetries` and `timeout` behave, `APIError` shapes), and
every test listed under Acceptance.

## Goal

Two wire protocols, byte-compatible on the request side with what the TypeScript pi sends, and identical on the
event side for the same SSE bodies. Everything the TS gets from the vendor SDKs is written explicitly here.

## Deliverables

- `api/anthropic_messages.rs` (+ `api/anthropic/` submodules if one concept per file demands it: `params.rs`,
  `convert.rs`, `betas.rs`, `threads.rs`, `federation.rs`, `stream.rs`): `stream` and `stream_simple` with
  `AnthropicOptions`; request building (`buildParams`, `convertMessages`, `convertTools`, tool-name normalization
  and the Claude Code stealth tool-name mapping, cache control and retention, beta feature selection,
  `insertThinkingLevelMessages`, effort mapping, strict tool schema fallback on `isStrictToolLimitErrorMessage`),
  headers (`anthropic-version`, `anthropic-beta`, auth header choice for API key vs OAuth token vs
  `ANTHROPIC_AUTH_TOKEN`, identity headers, the SDK's `X-Stainless-*` and `User-Agent` values reproduced
  verbatim), workload identity federation token exchange, message threads (session affinity hashing and the
  thread request/reply protocol), SSE event mapping to `AssistantMessageEvent` through `ToolCallArgumentsStream`,
  stop reason mapping, usage and cost including `cacheWrite1h`, OpenRouter reported cost, diagnostics, error body
  normalization, retries through `retry_provider_request` with the same classification the SDK applies.
- `api/openai_completions.rs` (+ submodules): the same for chat completions: request building with compat flags
  (`OpenAICompletionsCompat`: every field honoured as in TS), prompt cache keys, reasoning details, thinking-as-text,
  tool choice, non-null assistant content, vLLM priority, cache control format, response model, raw stop reason,
  tool-result images, the SDK's headers reproduced verbatim.
- `providers/anthropic.rs`, `providers/openai.rs` (openai's `openai-responses` and `openai-decisions` APIs are C13a;
  in this slice the openai provider registers only the models it can serve through completions-compatible paths,
  and the report says which models are still unreachable), `providers/all.rs` with the registration list (only the
  two providers for now; the others come with their APIs).

## Acceptance

- Port case by case: `anthropic-auth-token`, `anthropic-cache-write-1h-cost`, `anthropic-eager-tool-input-compat`,
  `anthropic-empty-thinking-signature-compat`, `anthropic-error-tool-result-images`, `anthropic-federation`,
  `anthropic-federation-sdk` (the parts that are not SDK internals), `anthropic-force-adaptive-thinking`,
  `anthropic-message-threads`, `anthropic-mid-conversation-effort`, `anthropic-sse-parsing`,
  `anthropic-strict-tool-fallback`, `anthropic-strict-tool-schema`, `anthropic-temperature-compat`,
  `anthropic-thinking-disable`, `anthropic-tool-name-normalization`, `cache-retention`, `interleaved-thinking`,
  `openai-completions-*` (all 13), `openrouter-cost`, `fetch-option`, `abort`, `pre-generation-error`,
  `provider-error-body-passthrough`, `provider-error-body-regression`, `responseid`, `tool-call-without-result`,
  `providers` (for the two providers). Tests that need live keys stay `#[ignore]` with the env var named.
- Request goldens: `rust/fixtures/ai-requests/gen.mjs` runs the TS `stream()` of both APIs against a local
  capturing server for 20 scenarios (system prompt with sections, tools with every schema feature, images, thinking
  levels, cache retention short/long/none, tool results with errors, a mid-conversation system update, OAuth token
  auth, federation env) and records `{ scenario, url, headers, body }` with volatile headers masked; the Rust test
  builds the same requests and compares JSON bodies (via `JsonValue` equality AND byte equality after
  `to_js_string`) and the header subset `anthropic-version`, `anthropic-beta`, `content-type`, `authorization`
  shape, `x-api-key` presence, `user-agent`, `x-stainless-*`.

## Carry-over inputs

None.
