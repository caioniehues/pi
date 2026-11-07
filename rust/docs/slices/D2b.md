# Slice D2b: Durable Session II: documents in memory, retention, watches, observers, deferred commits

Read first: D2a's spec and crate, `rust/docs/conventions.md`, `rust/docs/slices.md` (every Carry-over entry
under D2b: op payload sharing, string append cost, container-width cost; "Decided divergences"). TS references,
read in full: `packages/durable/docs/spec.md` sections 4 (retention and eviction paragraphs), 4.1 (deferred
commits), 9.1-9.2 (watches, `watchDoc`), and the observer parts of 2.2 (`DocumentObserver`, `Observer`,
`CommitObserver`, `Observation`, `snapshot`, `snapshotAsOf`); the rest of `packages/durable/src/session/session.ts`
(`SessionImpl`: documents, retention timers, eviction, watches, observers, deferred commits, `ObservedCommit`,
`setDefaultDocumentRetention`, `documentRetention`, `persistWithin`); `packages/durable/src/documents.ts`;
`packages/durable/test/{session-documents,session-document-retention,session-watches,session-observers,session-deferred-commits}.test.ts`,
`test/setup-document-retention.ts`, `test/session-document-cache-bench.ts`, `test/document-append-bench.ts`.

## Goal

The rest of the Session: documents held as Chord trackers with retention and eviction on the line, watches over
committed revisions (`watchDoc`, `snapshot`, `snapshotAsOf`), commit observers, and deferred commits held in
memory and stored within their deadline.

## Deliverables (`pi-durable/src/session/`)

- `documents.rs`: the in-memory document table: load from storage (base plus deltas replayed with
  `apply_immutable_batches`), trackers per incarnation, migration on load, retention (`documentRetentionMs`,
  the granule rule, "an observed document never leaves"), eviction after commits and on the retention timer
  (through `SetTimer`; hosts that decline background timers evict only on commits), task documents retiring with
  their task (hook for D6a), `snapshot`, `snapshot_as_of` (rewindable history through stored bases and deltas).
- `watches.rs`: `watch_doc` over `pi_chord::watch` (the acquisition revision captured in O(1), later committed
  revisions delivered with their op batches and the `retired` flag, `WatchEnd` reasons including `failed` when the
  Session fails), `DocumentWatch`; `observers.rs`: `Observer`, `CommitObserver`, `Observation`, `ObservedCommit`,
  guarded observers (`guard_observer`), listener error reporting through `onReport`-style callbacks (the Harness
  wires `onReport` in D6a; here a `report` callback on the Session options).
- `deferred.rs`: deferred commits per section 4.1: the hold rule (only changes to existing `latest`/`initial`
  conversation documents at their stored version), adoption in memory, the `withinMs` deadline and the other
  triggers that flush (another commit touching the document, close, an immediate commit, the limit), `persistWithin`,
  and the memory bound.
- Decide and record (Carry-over entries under D2b): whether ops need `Snapshot` payloads (measure an op backlog of
  2,000 streamed appends against a 1 MB document held by three watches: memory with `JsonValue` payloads vs a
  prototype with shared payloads); whether `Snapshot` needs a chunked string (measure 2,000 appends to a 1 MB
  string through the full commit path); and whether the container-width cost needs an index (measure 2,000 appends
  on a 10k-element array through the full commit path). Each decision with its numbers goes into
  "Resolved points" or "Decided divergences", and the chosen fix, if any, into this slice.

## Acceptance

- The listed `session-*` tests ported case by case; the two benches ported as ignored tests printing their numbers
  (and quoted in the report).
- `watch_doc` end-to-end: a document watched by two consumers receives every committed revision in order with ops
  that replay to the value (checked with `apply_immutable`), `retired` on retirement, and the right `WatchEnd` on
  stop, close and Session failure.
- Deferred commits: a held commit is visible immediately, stored within `withinMs` (fake clock), flushed early by
  each trigger, and lost on a crash before the deadline exactly as the TS semantics describe (test with a storage
  that records when writes arrive).

## Carry-over inputs

Whatever D1 and D2a list under D2b.
