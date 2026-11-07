# Slice C6a: `pi-sdk` core: extension, events, tools, contexts, api, settings

Read first: `rust/docs/conventions.md`, `rust/docs/slices.md`, `pi-types`, `pi-ai` (C3a) and `pi-agent` (C5) docs.
TS references, read in full: `packages/coding-agent/docs/extensions.md` (all 3,367 lines), `docs/settings.md`,
`docs/mcp.md`, `docs/virtual-models.md`, `docs/keybindings.md`, `packages/coding-agent/src/core/extensions/types.ts`
(all), `core/extensions/{runner,wrapper,loader}.ts` (to see how handlers compose and what the runtime needs from
the API), `core/{settings-schema,settings-defaults,key-id-schema,event-bus,exec,virtual-models,mcp-servers,source-info,slash-commands}.ts`
(types only; implementations stay in pi-core), `core/tools/{tool-definition-wrapper,bash}.ts` (the `BashOperations`
and `BashResult` types), `core/system-prompt.ts` (`BuildSystemPromptOptions`), `core/compaction/compaction.ts`
(`CompactionResult`, `CompactionPreparation`), `packages/mcp/src/config.ts` (`McpServerConfig`),
`packages/coding-agent/examples/extensions/*.ts` (skim ten varied ones to see what the API must make easy).

## Goal

The Rust extension API. Built-in extensions (mcp, codemode, tool_search) and user extensions are written only
against this crate, so it must cover the whole TypeScript `ExtensionAPI`, `ExtensionContext`, `ExtensionUIContext`
(C6b), `ToolDefinition`, every event with its result type, and the types those carry. It depends on `pi-types`,
`pi-ai`, `pi-agent`, `pi-schema`, `pi-json`, `pi-tui` (C6b's part) and never on `pi-core`.

## Decided shape

- `Extension` trait: `fn name(&self) -> &str` (used for `builtin:<name>` and diagnostics), `async fn setup(&self, api: Arc<dyn ExtensionApi>) -> Result<(), ExtensionError>`
  (the factory; may store the `Arc` for later calls), plus `fn replaceable(&self) -> bool { false }`,
  `fn hidden(&self) -> bool { false }`, `fn builtin(&self) -> bool { false }` mirroring `InlineExtension`.
  Built-ins export `pub fn extension() -> Box<dyn Extension>`.
- `EventHandlers` trait (object-safe, `async_trait`, every method has a default no-op implementation returning
  the event's "no change" value): one method per TS event, named after it (`project_trust`, `resources_discover`,
  `mcp_servers_change`, `session_start`, ..., `tool_call`, `tool_result`, `user_bash`, `input`), taking
  `&self, event: &mut <Event>, ctx: &ExtensionContext` (or `&ExtensionCommandContext` where TS passes it) and
  returning that event's result type (`Option<...>` where TS returns `void | R`). Events that TS lets handlers
  mutate in place (`tool_call.input`, `before_provider_headers.headers`, `before_agent_start.systemPromptOptions`)
  are `&mut`. `api.on(Arc<dyn EventHandlers>)` registers a handler object; an extension may register several.
  Composition rules (first result wins, merge, cancel-wins, and so on) are documented per event in `docs/sdk.md`
  from `runner.ts`, and implemented in C10a.
- Event and result types: one file per TS section (`events/session.rs`, `events/agent.rs`, `events/provider.rs`,
  `events/tool.rs`, `events/input.rs`, `events/ui.rs`, `events/resources.rs`), structs named after the TS
  interfaces. `ToolCallEvent` carries `tool_name`, `tool_call_id`, `parent_tool_call_id`, `input: JsonObject`
  (mutable) and `fn input_as<T: DeserializeOwned>(&self)`; typed accessors for the built-in tools
  (`BashToolInput` etc.) live in `tools/builtin_inputs.rs` as serde structs matching the TS event types.
- `Tool` trait (object-safe, `Send + Sync`): `fn spec(&self) -> &ToolSpec` (name, label, description,
  prompt_snippet, prompt_guidelines, parameters: JsonValue schema, output_schema, constrained_sampling,
  render_shell, exposure, namespace, annotations, default_active, execution_mode),
  `async fn execute(&self, call: ToolCallRequest) -> Result<AgentToolResult, ToolError>` where `ToolCallRequest`
  bundles `tool_call_id`, `params: JsonValue` (validated and coerced by the runtime), `cancel: CancellationToken`,
  `on_update: Option<ToolUpdateSink>`, `ctx: ExtensionToolContext`; optional `prepare_arguments`, `reaches_tools`,
  `prepare_loadout`; `render_call`/`render_result` are C6b (default `None`). A `define_tool!`-free helper
  `ToolSpec::new(name, label, description, parameters)` with builder setters keeps definitions short.
  `ToolLoadout`, `ToolLoadoutChanges`, `ToolExposure`, `ToolAnnotations`, `ToolNamespace`, `ToolInfo`.
- Contexts: `ExtensionContext` (a cheap-to-clone handle over `Arc<dyn ContextActions>`; fields and methods as in
  TS: `ui`, `mode`, `has_ui`, `cwd`, `session: Arc<dyn ReadonlySession>`, `models: Arc<dyn ModelRegistry>`,
  `model`, `scoped_models`, `thinking_level`, `is_idle`, `is_project_trusted`, `cancel`, `abort`,
  `has_pending_messages`, `shutdown`, `get_context_usage`, `compact`, `get_system_prompt`),
  `ExtensionToolContext` (adds `tools`, `execute_tool`), `ExtensionCommandContext` (adds
  `get_system_prompt_options`, `wait_for_idle`, `new_session`, `fork`, `navigate_tree`, `switch_session`,
  `reload`), `ReplacedSessionContext`. The backing traits `ContextActions`, `CommandContextActions` mirror
  `ExtensionContextActions`/`ExtensionCommandContextActions` so `pi-core` implements them.
- `ReadonlySession` trait: the `ReadonlySessionManager` pick from `session-manager.ts` (`get_cwd`,
  `get_session_dir`, `get_session_id`, `get_session_file`, `get_leaf_id`, `get_leaf_entry`, `get_entry`,
  `get_label`, `get_branch`, `build_context_entries`, `build_session_projection`, `get_header`, `get_entries`,
  `get_tree`); `ModelRegistry` trait: the sync facade of `core/model-registry.ts` that extensions use
  (`get_available`, `get_model`, `get_api_key`/`get_auth`, `stream_simple` through `pi-ai`).
- `ExtensionApi` trait (object-safe): `on`, `register_tool`, `register_command(name, CommandSpec)`,
  `register_shortcut(KeyId, ShortcutSpec)`, `register_flag(name, FlagSpec)`, `get_flag`,
  `register_message_renderer`, `register_markdown_transformer`, `register_entry_renderer`, `register_tool_renderer`
  (renderer types from C6b; in C6a they are declared in `render.rs` as traits with no TUI dependency beyond
  `pi_tui::Component`), `send_message`, `send_user_message`, `append_entry`, `set_session_name`,
  `get_session_name`, `set_label`, `exec`, `get_active_tools`, `get_all_tools`, `get_settings`, `set_active_tools`,
  `get_commands`, `set_model`, `get_thinking_level`, `set_thinking_level`, `register_provider(ProviderConfig)`,
  `register_native_provider(Arc<dyn pi_ai::Provider>)`, `unregister_provider`, `register_mcp_server`,
  `unregister_mcp_server`, `get_mcp_servers`, `register_virtual_model`, `unregister_virtual_model`, `events()`.
  Calls made during `setup` before the runtime binds are queued by the runtime (C10a), not by the SDK.
- Types ported as serde structs: `Settings` (every key of `settings-schema.ts`, with defaults from
  `settings-defaults.ts` and the same JSON names), `KeyId` (validated newtype per `key-id-schema.ts`),
  `SourceInfo`, `SlashCommandInfo`, `ExecOptions`/`ExecResult`, `BashOperations` (as a trait) and `BashResult`,
  `McpServerConfig` and `RegisteredMcpServer`, `ProviderConfig` and `ProviderModelConfig` (with the `apiKey`
  `$ENV`/`!command` string kept verbatim; resolution is pi-core's), `ExtensionVirtualModel`, `ModelRouteRequest`,
  `ModelRoute`, `BuildSystemPromptOptions`, `CompactionResult`, `CompactionPreparation`, `ContextUsage`,
  `CompactOptions`, `EventBus` (port of `event-bus.ts`), `ExtensionError`.
- `docs/sdk.md`: the Rust extension author's guide: how to write an extension, the event list with composition
  rules, tools, commands, UI (C6b fills that section), providers, MCP servers, with a complete `hello` example.

## Acceptance

- `rust/crates/pi-sdk/examples/hello.rs` (a compiled example): an extension registering a tool with a parameter
  schema built with `pi-schema` builders and typed params via serde, a command, a shortcut and a flag, handling
  `tool_call` (blocks `rm -rf`) and `session_start` (notifies). It links against a tiny in-crate test runtime
  (`pi_sdk::testing::FakeApi`) that records registrations and dispatches events, so the example has a test.
- Unit tests for `KeyId` validation (port `key-id-schema` tests), `Settings` defaults and round trip against
  `packages/coding-agent/schemas/settings.schema.json` (every property name present, no extras), `EventBus` tests
  ported, `McpServerConfig` validation cases ported from `packages/mcp/test/config*.test.ts`.
- `docs/sdk.md` reviewed by the coordinator before C10a starts; the builder's report lists every TS API member and
  the Rust item it maps to, and every member deliberately not ported with the reason.

## Carry-over inputs

None.
