# Rust port: conventions

Binding for every crate under `rust/`, when writing and when reviewing. The model is `packages/env/daemon/` (the
existing Rust crate) and the Chord v2 code style in `AGENTS.md`, carried over to Rust.

## Files and modules

- One concept per file, named after it; no one-helper files, no grab-bag files. The first line of every file is a
  `//!` comment saying what the file is for. Order: imports, exported types, then the public entry point first and
  its helpers below it in the order they are used; constants right above their first user.
- Split files by concept, never by size. A long file for one concept is fine (about 1,000 lines); a second concept in
  a file is the reason to split.
- A module tree mirrors the TS package it ports where that helps a reviewer find the reference (`pi-core/src/session_manager.rs`
  ports `core/session-manager.ts`). Rust names are snake_case; the TS name is cited in the file comment.

## Functions and types

- Every `pub` item has a one-line doc comment saying what it does or guarantees. Inline comments explain a non-obvious
  why (cost, race, decided rule, TS quirk being preserved), never what the next line does.
- Short functions with one job (about 40 lines needs a reason; one `match` over an enum is fine). No boolean flag
  parameters that switch behaviour: two functions or an enum.
- Enums (tagged unions) for every state and result. `Option` only for absence, never for failure. Errors are
  `thiserror` enums in libraries and `anyhow` in binaries and tests.
- No `unwrap`/`expect` in library code except with a `// invariant:` comment saying why it cannot fail. `#![forbid(unsafe_code)]`
  in every crate; exceptions are decided per crate in the slice spec, and each `unsafe` block carries `// SAFETY:`.
- Context last: required arguments, then the cancellation/telemetry context, then optional arguments.
- Same-host values are plain data. No defensive checks of values whose types already guarantee the shape; validate at
  trust boundaries only (wire input, files on disk, schema-checked arguments) and name the boundary in a comment.

## Compatibility rules

- The TS implementation is the reference. Port behaviour, not structure; where the TS does something odd, keep it and
  comment `// TS parity:` with the reason, unless the slice spec says otherwise.
- Every byte that reaches a file or a wire goes through `pi_json::to_js_string` (JSON) or `pi_json::iso_ms`
  (timestamps), never through `serde_json::to_string` directly.
- Fixtures generated from TS are generated at the pinned commit in `rust/tools/pi-difftest/TS_PIN` (until that file
  exists: the commit `rust-port` branched from, `085c84e98`). The generator script is committed next to the fixture.

## Dependencies

- Declared once in `[workspace.dependencies]` with exact versions (`=x.y.z`); crates use `workspace = true`.
  `cargo run -p xtask -- check-pins` enforces it. Adding a dependency is a reviewed decision: say why in the slice
  report. Prefer pure-Rust crates; no OpenSSL.
- `cargo deny check` must pass (advisories, license allowlist, duplicate versions warned).

## Tests

- One test module or file per source file, named after it; tests that port a TS test keep the TS test's name;
  regression tests are `issue_<number>_<slug>`. Conformance suites (JSON Schema, storage, env, telemetry) are reusable
  functions parameterised by the implementation under test.
- Goldens use `insta` with `INSTA_UPDATE=no` in `check.sh`; a changed snapshot is reviewed like code.
- No network in tests. Servers are in-process (`axum`) or node fixture scripts from the TS tests.

## Before reporting a slice

1. Re-read every changed file top to bottom as a stranger would; each file must be understandable without jumping.
2. `rust/check.sh` green.
3. Carry-over entries added to `rust/docs/slices.md` for anything deferred, in the same change.
4. The report names: what was built, what was verified and how, what was deferred and why, and every new dependency.
