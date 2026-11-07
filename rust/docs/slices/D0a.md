# Slice D0a: `pi-chord` context, json, delta ops

Read first: `rust/docs/conventions.md`, `rust/docs/slices.md`, `rust/crates/pi-json/src/*.rs`,
`rust/crates/pi-telemetry/src/context.rs` (C1).
TS references, read in full: `packages/chord/src/context/index.ts`, `packages/chord/src/json.ts`,
`packages/chord/src/delta/README.md`, `packages/chord/src/delta/index.ts`,
`packages/chord/src/delta/apply-immutable-trusted.ts`, `packages/chord/src/delta/revision-validator.ts`,
`packages/chord/src/delta/pending.ts`, the delta tests under `packages/chord/test/` (find them with
`grep -rl "delta" packages/chord/test --include=*.test.ts`), and `packages/chord/docs/design.md` section 9
(documents) for how ops are used. `tracker.ts`, `diff.ts` and `draft.ts` are D0b; skim them only to keep the op
types compatible with what they will need.

## Goal

The Chord subset Durable's documents are built on: an explicit context, strict JSON, and the delta op vocabulary with
its validator, appliers, and wire encoder, byte-compatible with the TypeScript implementation so stored deltas and
replicas can be mixed.

## Deliverables

Crate `pi-chord`, modules `context`, `json`, `delta` (files `delta/op.rs`, `delta/validate.rs`, `delta/apply.rs`,
`delta/snapshot.rs`, `delta/wire.rs`, `delta/pending.rs`, `delta/revision_validator.rs`).

- `context`: `Context` = immutable, cheaply clonable value carrying an optional cancellation signal
  (`tokio_util::sync::CancellationToken`), a telemetry span (`Arc<dyn pi_telemetry::TelemetrySpan>`, NOOP by
  default), and typed values (`ContextKey<T>` with a `with_value` chain); `BACKGROUND_CONTEXT()`, `with_abort_signal`,
  `without_abort_signal`, `with_telemetry`, `await_with_context` (a future that resolves with
  `ContextError::Aborted` when the signal fires first), `Display` matching `toString()`.
- `json`: `JsonValue`/`JsonObject` re-exported from `pi-json`; `copy_json` is unnecessary (values are owned) and is
  not ported; `is_strict_json` and `canonical_equal` only if the delta tests need them.
- `delta/op.rs`: `Seg = String | u64` (`enum Seg { Key(String), Index(u64) }`), `Path`, `Op` enum with variants
  `Replace(JsonValue)`, `Set(NonEmptyPath, JsonValue)`, `Delete(NonEmptyPath)`, `Append(NonEmptyPath, String)`,
  `Truncate(NonEmptyPath, u64)`, `Splice(Path, u64, u64, Vec<JsonValue>)`, `Move(Path, Vec<u64>)`; `WireOp` with
  the short forms, id references and the `["#", id, path]` definition. Custom `Serialize`/`Deserialize` produce and
  accept exactly the TS tuple JSON (`["s", ["a", 0], 1]`); numbers in paths are JSON integers.
  `is_replace`, `is_base`, `overlap(a, b, scan, probe, max_candidates)` with the same results as TS for the same
  inputs (the corpus checks it), `RESERVED_SEGMENTS`.
- `delta/validate.rs`: `assert_valid_op`, `assert_valid_wire_op`, `assert_safe_path`, `assert_permutation`, index
  range rule (an index may address an existing element or append one past the end), with error enums whose
  messages equal the TS `TypeError` messages (tests compare them).
- `delta/apply.rs`: `apply(&mut JsonValue, &[Op])` in place, with the same `PathError` and `UnsafePathError`
  semantics; `apply_batches`.
- `delta/snapshot.rs`: `Snapshot`, a persistent JSON tree (`Arc`-shared containers: arrays and objects behind `Arc`,
  strings behind `Arc<str>`, insertion-ordered objects) with `Snapshot::from(&JsonValue)`, `to_value()`, and
  `apply_immutable(&Snapshot, &[Op]) -> Result<Snapshot, _>` that copies only the containers along each op's path
  and shares everything else (a test applies one op to a document with 10,000 siblings and asserts the untouched
  subtree is the same `Arc` pointer before and after); `apply_immutable_trusted` as in TS (no validation; the
  caller validated). This is what Durable's documents and watchers will hold.
- `delta/wire.rs`: `encode(&[Op]) -> Vec<WireOp>` (path dictionary with ids defined on a path's second use, short
  forms reusing the previous op's path, arity disambiguation) and `decode(&[WireOp]) -> Result<Vec<Op>, _>`,
  byte-compatible with TS `encode`/`decode` (same ids, same short forms, same order).
- `delta/pending.rs`: `PendingOps` for a slow consumer (`take`, coalescing rules as in TS).
- `delta/revision_validator.rs`: port.

## Fixture corpus (generated from TS)

`rust/fixtures/chord-delta/gen.mjs`, run with the worktree's `node` directly against
`packages/chord/src/delta/index.ts` (Node 26 strips types; the delta module has no package dependencies; verify by
running it, and if some import needs `node_modules`, run `npm ci --ignore-scripts` in your worktree first and say so
in the report). It writes:

- `apply-cases.jsonl`: 2,000 records `{ base, ops, expected }` or `{ base, ops, error: "<message>" }`, covering every
  verb, root and nested paths, numeric and string segments, appends on existing and missing strings, splices at the
  end, permutations, reserved segments, out-of-range indices and every validator error branch; seeded, reproducible.
- `wire-cases.jsonl`: 1,000 records `{ ops, wire }` with ops sequences that exercise the path dictionary (second use,
  many ids), short forms and arity collisions; and `{ wire, error }` for invalid wire ops.
- `overlap-cases.jsonl`: 500 records `{ a, b, scan, probe, max, n }`.

Commit the generator and the generated files. Rust tests assert: `apply` matches `expected` or the error message;
`apply_immutable` gives the same value as `apply`; `encode(ops) == wire` byte-for-byte after `to_js_string` and
`decode(wire) == ops`; `overlap` equals `n`.

## Acceptance

- All three corpora pass; the structural-sharing test passes; the TS delta unit tests that cover ops, validation,
  apply, encoding and pending are ported case by case (list which TS test file each Rust test came from).
- `pi-chord` depends on `pi-json`, `pi-telemetry`, `serde`, `serde_json`, `tokio-util` and nothing else.

## Carry-over inputs

None. Anything the tracker will need from the op layer (for example an `append` detection helper) is D0b's; note
it under D0b in `slices.md` if you discover it.
