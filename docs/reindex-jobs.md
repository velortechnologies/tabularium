# Reindex jobs

The old blocking public reindex operation is replaced by server-owned jobs.
Jobs and job history are never persisted. Finished, failed and stopped jobs
leave the active registry immediately; results are recorded in server logs.

## Upgrade: one-time reindex for persistent flags

When upgrading from a build without persistent indexing flags (including the
initial 0.2.0 build), all existing documents initially show `pending` for each
applicable indexer. **This is expected, not index corruption or lost documents.**
The new flags are initialized conservatively; existing search indexes are kept
and remain available. Binary documents are not semantic work, and disabled
indexers do not count as pending work.

After upgrading, perform one root reindex to populate the flags for all enabled
indexers (administrator access required):

```sh
tb reindex job start / --non-indexed-only
```

The first missing-only pass therefore covers the existing corpus. The command
returns a job UUID; processing continues on the server. Monitor with
`tb reindex job list` / `tb reindex job status UUID` and server logs. Disappearance
from the active list alone does not prove success: failed jobs also disappear.
After completion, verify that `tb reindex job explain / --non-indexed-only`
returns no pending documents. If a job fails, resolve the cause and rerun the
same missing-only command; successfully indexed documents need not be repeated.

This is a **one-time upgrade step**, not a requirement after every restart.
Flags persist afterward. Startup itself does not launch this rebuild; if
`auto_reindex` is configured, its scheduled root missing-only job can perform
this pass instead. Do not start a duplicate while that job is running.

## CLI

    tb reindex job start / --non-indexed-only --indexer-kind hybrid
    tb reindex job explain / --non-indexed-only --indexer-kind hybrid
    tb reindex job list
    tb reindex job status UUID
    tb reindex job stop UUID

Path defaults to /. Indexer kind is lexical, semantic, or hybrid (default:
all configured engines). Disabled semantic is not missing work. Explain
returns paths and per-engine state without modifying indexes. It is an
inspection, not a reservation.

RPC methods: reindex_job_start, reindex_job_explain, reindex_job_list,
reindex_job_status, reindex_job_stop. Start/explain accept path,
non_indexed_only (false), indexer_kind (hybrid). Start returns {"id":"UUID"}.
Status/stop accept {"id":"UUID"}. List/status expose processed, skipped,
total (null during selection). Job methods require administrator rights
when authentication is enabled. Trusted-full MCP exposes the same methods.

REST POST /api/reindex accepts action (start, explain, list, status, stop)
and the same selection/id fields. RPC index_state {"path":"/document"}
is read-authorized and exposes lexical/semantic ready, pending, disabled,
or not_applicable states. Indexing failures are logged; failed revisions
remain pending.

## Writes and concurrency

Successful storage writes are indexed inline in the same API call. Index
failures do not turn a stored write into an API failure; storage failures
still propagate. Metadata-only changes advance the source revision.
The source SQLite database stores `entry_index_state`: separate persistent
`lexical_indexed` / `semantic_indexed` flags and a generation counter. Triggers
invalidate both flags in the same transaction as document/metadata/description,
snapshot, or path/subtree changes. Completion marks only the captured generation;
a concurrent mutation cannot be blessed by old index work. A forced rebuild
clears its engine flag before attempting work, so failures remain pending.

`explain` and job selection use one metadata-only SQL query (plus path resolution),
with partial indexes for pending lexical/semantic rows. They never load document
bodies, recalculate hashes, or issue per-document state queries. Snapshots are
selected without joining snapshot content. Disabled semantic and binary documents
are excluded from semantic missing work.

Flags survive restarts. Engine instance identity and semantic configuration
fingerprints invalidate the affected engine flags when the derived index or model
changes. Introducing flags into an older database conservatively marks existing
documents pending; it does not run a reindex automatically. The old derived
`index-state.db` is no longer used or deleted automatically.

All indexing operations share a mutex and own a semaphore permit throughout
waiting/working/sleeping. Capacity is Tokio Semaphore::MAX_PERMITS, not an
administrative concurrency limit. At safe document boundaries after 200 ms,
jobs release the mutex and, if another operation is registered, sleep 1 ms
before reacquiring. This is cooperative fairness, not a hard latency guarantee.
One document/embedding request may take longer. Stop waits for an in-flight
commit; after it returns the stopped job cannot mutate indexes.

Document-specific errors (including HTTP 413 input too large) are logged and
skipped. Transport/model/service errors or lexical engine failures stop the
job. Stale index results cannot mark a concurrent newer source revision ready.

## Configuration

At TOML root, before any sections:

    auto_reindex = 900

Omission disables it; zero is rejected. A Tokio interval starts a root
missing-only hybrid job and asynchronously awaits completion. An already
active equivalent job suppresses the automatic start atomically. Long runs
do not accumulate catch-up bursts. Manual jobs may still run in parallel.

In the existing semantic section:

    llm_timeout = 30

Seconds per embedding HTTP request/batch. The old timeout key remains an
alias. This is separate from search_timeout. No background semantic dirty
worker is launched; repair is explicit or via auto_reindex.

## Tests

Disposable DBs and local HTTP fixtures cover the required outage scenario:
two document creates, two body changes, two metadata-only changes, then
missing-only repair and an unchanged control. Additional tests cover
document versus engine failures, timeout, revision races, stopping while
waiting/committing, permit abort/unwind, auto repair, duplicate suppression,
HTTP client independence and administrator authorization.

## Persistent flags correction verification (2026-11-18, uncommitted)

- Workspace: 377 tests passed; Clippy `--workspace --all-targets -- -D warnings`,
  db-only/core-only checks, formatting and diff whitespace checks passed.
- Regression coverage includes transactional invalidation/rollback, stale
  completion, metadata-only writes during endpoint outage, persisted restart
  state, index identity changes, subtree/snapshot mutations, binary eligibility,
  unchanged snapshots, and failed forced semantic rebuild followed by recovery.
- The actual missing-only query plan uses both pending partial indexes.
- Disposable v1 fixture: 7000 documents with 1 KiB bodies; metadata-only full
  selection 67.9 ms, missing-only selection with two pending records 6.4 ms.
  These are single-run storage-selection measurements, not production RPC timings.
- No production deployment, commit, push, release or full production reindex.
