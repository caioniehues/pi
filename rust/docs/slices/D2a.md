# Slice D2a: Durable Session I: transactions, the writer (group commit), cached storage, keys, tails, forks

Read first: `rust/docs/conventions.md`, `rust/docs/slices.md` (every Carry-over entry under D2a; "Decided
divergences"; "Resolved points"), `pi-durable` (D1: types, `Storage`, `MemoryStorage`, the conformance suite),
`pi-chord` (context, delta tracker and snapshot, watch). TS references, read in full:
`packages/durable/docs/spec.md` sections 4 (transactions and storage ownership, including 4.1), 3.3-3.7
(document access, mutation ownership, bases and checkpoints, versions and migrations, forks), 10 (storage
contract, for what the Session may assume), 12 (footguns); `packages/durable/docs/group-commit.md` (all);
`packages/durable/src/session/{session,transaction,writer,cached-storage,keys,conversation-tails,forks}.ts`;
`packages/durable/src/documents.ts`; `packages/durable/test/{session-support,session-group-commit,session-cached-storage,session-conversation-tails,session-forks,session-tables,session-checkpoints-migrations,session-definitions,session-adoption}.test.ts`
and `test/session-support.ts`.

## Goal

The Session's mutation line: one job at a time, commits admitted logically and stored by a writer off the line as
group commits, keyed waits so no job observes an unpublished write, transactions that append entries and change
documents atomically, and the bookkeeping around them (keys, conversation tails, forks, cached storage).

## Deliverables (`pi-durable/src/session/`)

- `keys.rs`: the key vocabulary (`task_key`, `conversation_key`, `entry_key`, `append_key`, `submission_key`,
  `request_key`, `document_key`, `incarnation_key`, `scope_key`, `owned_key`, `ROOTS_KEY`, `INTENT_KEY`,
  `ALL_KEYS`, `table_keys`) and `GatedStorage` (the storage wrapper that records the keys a job touches and waits
  for unpublished writes on them: port its gate semantics exactly, including "a job waiting holds the line").
- `writer.rs`: `Writer` with `WriterHost`, `PartSpec`/`Part`, `PublishFailure`: the off-line writer that sends every
  admitted commit as one `Storage::commit` call with per-batch seqs, publishes in line order, the prefix semantics
  on refusal (the first refused batch fails the Session; earlier batches are durable), outcome fences
  (group-commit.md section 5), and the bounds of section 7.
- `transaction.rs`: `Transaction` implementing the `Tx` interface of spec section 2.2 (entries, documents with
  `doc`/`docFamily`/`tryDoc`, `createDoc`, `retire`, task creation with `TaskCreationOptions`, submissions, the
  read-after-write rule `ReadAfterWrite`, `TransactionScope`, `LoadedDocument`/`HeldDocument`/`AdoptedDocuments`,
  `storeHeld`), over Chord trackers: a document read on the line yields its tracker's current snapshot; a change is
  a tracker `Change` prepared at settlement; `checkpoint_when` decides base vs delta content; migrations on load
  per section 3.6.
- `cached_storage.rs`: `CachedStorage` (the read cache in front of `Storage`, invalidation on publish, `scope_of`).
- `conversation_tails.rs`: `ConversationTails` (newest entries per conversation, head markers).
- `forks.rs`: `ForkDocumentCopy` and the fork rules of section 3.7 (`current`, `initial`, `asOf`).
- `session/mod.rs` (the `Session` implementation is split: this slice builds `line.rs` (the job queue, logical
  admission, keyed waits, line reads, `contained`, `guard_observer`, `ENDED`, `close_ends`, `close_end_of`,
  `observation`) and the commit path of `SessionImpl` (`commit` with `CommitOptions` persistence immediate only;
  deferred commits are D2b), `create_session` with `documentRetentionMs`/`setTimer` options, `close`, `read_only`,
  the `SessionFailed` propagation); document retention, eviction, watches, observers and deferred commits are D2b.

Concurrency: the line is one tokio task; jobs are futures run to completion one at a time on it; the writer is a
second task; cancellation comes through `pi_chord::Context`; timers through the `SetTimer` abstraction of D1.
Document in `line.rs` how "a job waiting holds the line" is realized without blocking the executor.

## Acceptance

- The listed `session-*` tests ported case by case (names equal the TS cases); `test/session-support.ts` ported as
  `pi_durable::testing::session_support` (feature `testing`).
- The group-commit tests of `group-commit.md` section 8 ported where they target the Session (the Harness ones are
  D6a); the benchmark of section 9 ported as an ignored test printing runs per second, with the number in the report.
- A cross-storage fixture: a Session over `MemoryStorage` runs a scripted sequence of 50 commits (entries,
  documents with base and delta content, a retire, a fork) and the resulting storage state (scanned through the
  `Storage` API) equals the state the TS Session produces for the same script (node script under
  `rust/fixtures/durable/session/`, committed with its output); compare after `to_js_string` normalization.

## Carry-over inputs

Whatever D1 lists under D2a.
