# Slice D0b: `pi-chord` tracker, diff, pending ops, watch, output, sqlite driver

Read first: D0a's spec and crate, `rust/docs/conventions.md`. TS references, read in full:
`packages/chord/src/delta/README.md`, `packages/chord/src/delta/{tracker,draft,diff,pending}.ts`,
`packages/chord/src/{watch,output}.ts`, `packages/chord/src/sqlite/{index,database,migrations,schema,node}.ts`,
`packages/chord/test/{delta,delta-append-text,delta-apply-immutable,delta-clone,delta-diff,delta-op-text-retention,delta-pending,output,output-skip,watch}.test.ts`,
`packages/chord/test/delta-tracker/*.test.ts`, `packages/chord/test/sqlite/*.test.ts`, and how Durable uses them:
`grep -rn "track(\|beginChange\|diffRevisions\|appendText\|PendingOps\|new Watch\|boundOutput\|OutputBuffer\|Progress\b" packages/durable/src`.

## Goal

The rest of what Durable needs from Chord: a way to edit a revision and get the ops, the diff of two revisions,
folding of pending ops for slow consumers, the watch lifecycle, bounded output, and the SQLite driver interface.

## Decided rule for the tracker

The TS tracker is a `Proxy` over the draft with a piece table for arrays. Rust has no proxies, and Chord's README
states that op shape is not contractual: "Only the resulting value is contractual." So the Rust tracker exposes an
explicit `Draft` API and must satisfy: (1) `apply(base, ops) == value` for the TS applier (verified through the
D0a corpus machinery: a node script applies Rust-produced ops with the TS `apply` and compares), (2) the ops are
valid under `assert_valid_op`, (3) the same editing script produces the same **value** as the TS draft, (4) ops
are reasonably minimal: an `append_text` produces one `a` op, a single `set` one `s` op, a `push` one `p` op; no
whole-root replacement unless `prepare_replace` was used. Byte-identical op batches are NOT required. Document
this decision in the file comment and in `rust/docs/slices.md` under D0b as a resolved point.

## Deliverables

- `delta/tracker.rs`: `Tracker` (`track(initial)`, `value()` as a `Snapshot`, `revision()`, `begin_change() -> Change`,
  `prepare_replace(value)`, `adopt(prepared)`, abort and stale semantics: a second `begin_change` while one is
  open, adopting a prepared change built on an older base, using a draft after `prepare`/`abort`: each returns
  the error the TS throws for), `Change` with `state() -> &mut Draft` and `prepare() -> Prepared { base, value, ops }`,
  `Draft`: `get(path)`, `set(path, value)`, `delete(path)`, `append_text(path, text)` (with `overlap` detection
  of already-present prefixes as `appendText` does), arrays: `push`, `pop`, `shift`, `unshift`, `splice`,
  `reverse`, `sort_by`, `fill`, `copy_within`, `set_index`, `truncate_len`; `undefined` rules become
  `Option`: `set(path, None)` deletes a key and is an error inside arrays. Placed values are owned (no aliasing
  concerns in Rust); validation of strict JSON is unnecessary because `JsonValue` is strict by construction.
- `delta/diff.rs`: `diff_revisions(before, after) -> Vec<Op>` with the same heuristics as TS where they decide
  op minimality (text append detection, array LCS/splice runs, permutation detection); acceptance is by result
  equality plus op-count bounds taken from the TS tests.
- `delta/pending.rs`: `PendingOps` (`push`, `take -> PendingTake`), the folding rules of the README (leaf folds,
  structural ops end folding, `r` drops, the 10,000-op replace fallback); byte-identical to TS folding because
  the TS tests assert op shapes here (port them as they are).
- `watch.rs`: `Watch`, `NativeWatch`, `WatchHandle`, `WatchEnd`, `WatchCall`, `Watchable`, `NativeWatchable`
  with the lifecycle rules of `watch.ts` (batching, reentrancy, end reasons), over `Snapshot` values.
- `output.rs`: `sanitize_output`, `bound_output`, `character_end`, `OutputBuffer`, `OutputSkip`, `utf8_byte_length`,
  `Progress` with `ProgressClock`, `PROGRESS_BYTES_PER_SECOND`.
- `sqlite/mod.rs`: `SqliteValue`, `SqliteExecutor` and `SqliteDatabase` traits (async, `Send`), `SqliteMigration`,
  `apply_sqlite_migrations`, `validate_sqlite_schema_prefix`; `sqlite/rusqlite.rs`: the Node driver's equivalent
  over `rusqlite` (bundled) with the same pragmas, modes (`read-write`, `read-only`), busy timeout, WAL settings
  and transaction semantics as `node.ts` (read the pragmas it sets and copy them).

## Acceptance

- `delta-tracker/*.test.ts`, `delta.test.ts`, `delta-append-text.test.ts`, `delta-clone.test.ts`,
  `delta-op-text-retention.test.ts` ported case by case against the decided rule (value equality; op shapes only
  where the TS test's intent is a minimality bound); `delta-diff.test.ts`, `delta-pending.test.ts`, `output*.test.ts`,
  `watch.test.ts`, `sqlite/*.test.ts` ported case by case.
- Cross-apply corpus: `rust/fixtures/chord-delta/tracker-scripts.jsonl` (200 editing scripts over seeded bases);
  the Rust tracker runs each and writes `{ base, ops, value }` to `rust/fixtures/chord-delta/tracker-out.jsonl`
  in a test; a node script `check-tracker-out.mjs` applies the ops with the TS `apply` and `applyImmutable` and
  asserts equality with `value`; the Rust test spawns node to run it when `node` is available (skipped with a
  message otherwise) and the generated file is not committed.
- Structural sharing of `Tracker::value()` revisions across adopts (pointer test as in D0a).

## Carry-over inputs

Whatever D0a lists under D0b.
