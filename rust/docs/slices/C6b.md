# Slice C6b: `pi-sdk` theme and UI surface, `pi-tui` component contract and colors

Read first: C6a's spec and crate, `rust/docs/conventions.md`. TS references, read in full:
`packages/coding-agent/src/modes/interactive/theme/{theme,theme-tokens,theme-schema,theme-controller,system-theme}.ts`,
`theme/dark.json`, `theme/light.json`, `packages/coding-agent/docs/themes.md`, `packages/coding-agent/schemas/theme.schema.json`,
`packages/tui/src/{tui,colors,oklab,terminal-colors,editor-component,autocomplete,keybindings}.ts` (the parts that
define `Component`, `Focusable`, `OverlayOptions`, `OverlayHandle`, `TUI`'s public surface, `AutocompleteProvider`,
`EditorComponent`, `KeybindingsManager`), `packages/tui/test/colors.test.ts`, `packages/tui/test/oklab*.test.ts`,
`packages/coding-agent/src/core/extensions/types.ts` lines 116-330 and 480-720 and 1550-1615,
`packages/coding-agent/src/core/keybindings.ts` (`DEFAULT_APP_KEYBINDINGS`, `AppKeybinding`),
`packages/coding-agent/src/core/footer-data-provider.ts` (`ReadonlyFooterDataProvider`).

## Goal

The parts of the SDK that touch the terminal: the theme (tokens, JSON format, color resolution), the component
contract extensions render into, and the UI context extensions drive dialogs, widgets, footer, header, editor
and custom components through.

## Deliverables in `pi-tui` (the crate's first real code; C15 builds the rest on it)

- `colors.rs` and `oklab.rs`: ports of `colors.ts` and `oklab.ts` (indexed, sRGB, OKLCH, OKHSL parsing; mixing
  in oklch or srgb; truecolor and 256-color output; the exact ANSI sequences); `terminal_colors.rs` types only.
- `component.rs`: `Component` trait (`render(&mut self, width: usize) -> Vec<String>`, `handle_input(&mut self, data: &str)`,
  `handle_mouse`, `captures_viewport_input`, `invalidate`, `dispose`), `Focusable`, the cursor marker constants
  (`CURSOR_MARKER`, fake cursor markers), `MouseEvent` types, `OverlayOptions`, `OverlayHandle` trait,
  `TuiHandle` trait (what components and extensions may call on the running TUI: `request_render`,
  `show_overlay`, `terminal_size`, `set_title`; extend in C15b), `EditorComponent` trait, `AutocompleteProvider`
  trait and `AutocompleteItem`, `KeybindingsManager` trait surface (`get_keys`, `matches`) with the TUI and app
  keybinding id tables (`TUI_KEYBINDINGS` from `tui/src/keybindings.ts`, `DEFAULT_APP_KEYBINDINGS` from
  `core/keybindings.ts`) as data.

## Deliverables in `pi-sdk`

- `theme/tokens.rs`: `THEME_TOKENS` table (name, description, fallback) copied from `theme-tokens.ts`; `ThemeToken`
  enum; `theme/json.rs`: `ThemeJson` serde struct (`name`, `appearance`, `vars`, `colors`, `export`) validated with
  `pi-schema` against a schema equal to `schemas/theme.schema.json`; `theme/theme.rs`: `Theme` (resolve vars and
  fallbacks, `fg(token, text)`, `bg`, `bold`, `italic`, `underline`, `strikethrough`, `dim`, `get_color`,
  `get_markdown_theme`, `get_editor_theme`, `get_select_list_theme`, the derived `appearance`, the `name`),
  built-in `dark` and `light` themes embedded from the JSON files, `system_theme` detection types (the OSC
  query logic itself runs in the TUI; the SDK holds `ColorScheme` and the `light/dark` setting resolution).
- `ui.rs`: `UiContext` trait (object-safe, async where TS is a Promise): `select`, `confirm`, `input`, `editor`
  with `DialogOptions { cancel, timeout }`; `notify`; `on_terminal_input`; `set_status`, `set_working_message`,
  `set_working_visible`, `set_working_indicator`, `set_hidden_thinking_label`; `set_widget(key, WidgetContent::Lines | ::Component(factory), options)`;
  `set_footer(factory)`, `set_header(factory)`, `set_title`; `custom(factory, CustomOptions)` where the factory
  receives `&dyn TuiHandle`, `&Theme`, `&dyn KeybindingsManager` and a `Done<T>` sender; `paste_to_editor`,
  `set_editor_text`, `get_editor_text`, `add_autocomplete_provider`, `set_editor_component`,
  `get_editor_component`; `theme()`, `get_all_themes`, `get_theme`, `set_theme`; `get_tools_expanded`,
  `set_tools_expanded`. Plus `NoopUiContext` (print mode) in `pi_sdk::testing`.
- `render.rs`: `MessageRenderer`, `EntryRenderer`, `MarkdownTransformer` with `MarkdownMessageType`,
  `ToolRendererResolver`, `ToolRenderContext`, `ToolRenderResultOptions`, `MessageRenderOptions`,
  `EntryRenderOptions`, `KeybindingHints` trait, `ReadonlyFooterDataProvider` trait; `Tool::render_call` and
  `Tool::render_result` default methods added to the C6a trait.
- `docs/sdk.md`: the UI, theme and rendering sections.

## Acceptance

- `colors.test.ts` and the oklab tests ported case by case; the exact escape sequences asserted.
- `dark.json` and `light.json` load, validate, and resolve every token; a theme with an unknown token or a bad
  color fails validation with the same message the TS schema produces for the same input (test three cases).
- A compiled example `examples/widget.rs`: an extension that sets a widget, a status, a custom footer and shows a
  custom component through `FakeApi` and a recording `UiContext` in `pi_sdk::testing`.

## Carry-over inputs

None.
