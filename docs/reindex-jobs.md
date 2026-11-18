# Reindex jobs

The old blocking public reindex operation is replaced by server-owned jobs.
Jobs and job history are never persisted. Finished, failed and stopped jobs
leave the active registry immediately; results are recorded in server logs.

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
Lexical revision/content stamps live in derived index-state.db; semantic
revision/path stamps live in the existing sidecar. Source schema is unchanged.

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
