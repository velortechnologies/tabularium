# TODO

## Indexing state and parallel in-memory reindex jobs

Owner-approved plan, updated 2026-11-18. Implemented for 0.2.0; acceptance checks recorded in docs/reindex-jobs.md. Deployment is separate.
This replaces the previous durable-job/outbox and queued-reindex proposal.

### Commands and job lifecycle

- Replace the current `reindex <path>` command with:
  `reindex job start <path> [non_indexed_only] [indexer_kind]`.
  Starting a job returns its UUID promptly.
- `non_indexed_only` has NO trailing underscore. It selects documents missing
  a current index entry for the selected indexer(s).
- Add `reindex job explain <path> [non_indexed_only] [indexer_kind]`, using
  exactly the same parameters and document-selection rules as `job start`.
  Return the list of documents the job would index. This is inspection only:
  do not create a job or perform indexing. The list reflects the current state;
  a later start may select differently if documents/index state have changed.
- Provide `reindex job list` and `reindex job status <uuid>` for inspection,
  including progress for currently active jobs. Completed jobs are removed
  from memory immediately; completion/results/errors are recorded only in logs.
  Do not retain completed-job history. The auto-reindex scheduler must still
  be able to await task completion without relying on a retained status entry.
- `reindex job stop <uuid>` cancels execution and removes the job. Ensure the
  stopped task cannot continue mutating indexes after cancellation completes.
- Jobs run in the background on the SERVER, independently of the initiating
  API connection. Client disconnect or request timeout does not cancel a job.
- Jobs exist ONLY in memory: do not write jobs to disk, persist a job queue,
  or resume jobs after a server restart.
- Jobs are NOT a queue. Multiple jobs execute concurrently.
- Omitted `indexer_kind` selects all enabled indexers, for both manual and
  automatic jobs. Disabled/not-applicable indexers are not missing work.
- Distinguish document-specific failures from clear model/index-engine failures.
  On a document-specific indexing failure, log the failure, leave the affected
  index state non-indexed/stale, skip that document and continue the job.
- On a clear failure of the embedding/LLM model or lexical search engine,
  stop the entire job with an error recorded in the logs. Do not treat an
  engine/model outage as a sequence of skippable document failures. Remove
  the terminated job from memory under the same lifecycle rules as other
  completed jobs; do not mark unfinished index work as current.
- Test both branches: a document-specific failure allows later documents to
  be indexed; a clear model or lexical-engine failure stops the job, logs the
  error, and releases the mutex and RAII operation registration.

### Document writes and mutex sharing

- Creating or changing a document does NOT create a job. Index it immediately
  within the current API call. If storage succeeds but indexing fails, return
  successful write without an indexing error in the response; log the indexing
  failure and leave the affected index state pending/stale. Actual storage
  failures must still propagate. Test this distinction during endpoint outage.
- Use a shared mutex for conflicting index operations, including reindex jobs
  and immediate API indexing, without holding it for the full long-running job.
- A job holding the mutex attempts to release it every 200 ms in its processing
  loop until finished, then allows competing jobs and API calls to acquire it
  before reacquiring. Hardcode 200 ms for now.
- This is a cooperative processing time slice, not a 200 ms sleep while holding
  the mutex. Yield at safe operation boundaries; do not corrupt an in-flight
  index commit. Check cancellation between work units.
- Count ALL active indexing operations, both reindex jobs and immediate API
  indexing calls, including operations waiting for the mutex. Each operation
  registers before waiting and unregisters after finishing.
- At each 200 ms job time-slice boundary, release the mutex first. If more than
  one indexing operation is registered (including itself), sleep for 1 ms
  before reacquiring; otherwise continue without sleeping. Hardcode the
  200 ms slice and 1 ms sleep for now. This is a contention heuristic, not a
  strict fairness guarantee.
- Implement operation registration with a semaphore/RAII permit or equivalent
  resource holder with explicit Drop-based counter release. Do not rely on
  manual decrement on every exit path. Keep the holder alive across mutex
  releases and sleeps; do not register the same operation on every time slice.
- Do not impose a practical indexing concurrency limit for now. Starting
  reindex jobs is an administrator operation; the administrator controls how
  many to start. For a semaphore, initialize with an effectively unlimited
  capacity (owner's conceptual `u64::MAX`); use the semaphore implementation's
  supported maximum rather than blindly passing an out-of-range integer.
  No concurrency-limit config is required for now.
- Preserve accurate active-operation accounting for mutex contention/yield
  decisions, including jobs and API calls waiting for the mutex. The holder
  releases its permit/registration exactly once on Drop. With semaphore-based
  accounting, derive usage from the actual initial capacity, not u64::MAX.
- Add a separate configurable `llm_timeout` for embedding/LLM calls so a hung
  endpoint cannot wait indefinitely. Seconds per HTTP embedding batch; default 30 (existing timeout alias retained).
  Timeout leaves the affected revision non-indexed and is logged; test a hung
  endpoint, resource release and subsequent recovery. Whether embedding calls
  run outside the index mutex remains a proposal, not an approved requirement.
- Thoroughly test accounting and registration lifecycle: normal completion, errors,
  cancellation while waiting/working/sleeping, stop/removal, task abortion,
  and unwinding. Test mixed API/job contention, no leaked registrations,
  no underflow/double decrement, and return to zero
  after all operations settle.
- The old promise that writes never wait for the mutex is superseded by
  synchronous indexing in the current API call.

### Per-document index state and reconciliation

- Persist separate lexical_indexed and semantic_indexed flags in the source
  database, transactionally invalidated by document changes. Use generation
  checks for completion and partial SQL indexes for pending selection.
  Explain must not read bodies or recompute hashes.
- Document changes invalidate the corresponding indexed state. Mark a revision
  indexed only after successful index commit. Use revision/generation checks
  so an old job cannot mark a newer document revision indexed.
- Add optional general-config `auto_reindex`, expressed in seconds. When set
  to N, automatically start a root (`/`) reindex job with
  `non_indexed_only=true` every N seconds. When absent, automatic reindexing
  is disabled. Reject zero rather than passing it to Tokio's interval.
- Implement the scheduler as a server-owned future using
  `tokio::time::interval` and awaiting completion of the submitted job before
  proceeding. This is an async wait, not blocking a Tokio runtime thread.
- If an equivalent root/non-indexed-only job is already running, skip the
  automatic start. Check-and-start must be atomic to avoid duplicate jobs.
- Do not accumulate missed ticks or launch catch-up bursts after a long job;
  preserve the N-second periodic intent without overlapping equivalent jobs.
  Automatic jobs use the same in-memory lifecycle, cancellation and
  mutex-sharing rules as manually started jobs; no durable queue/outbox.
- Test disabled configuration, interval validation, periodic execution,
  existing-job skip, concurrent manual/automatic start, long-running jobs,
  failure/cancellation recovery and scheduler shutdown.
- Handle deletes, moves, renames, metadata/subtree changes and binary-document
  eligibility. Distinguish disabled/not-applicable engines from missing work.
- Keep the previous usable index available during rebuild where feasible;
  report partial results and failures honestly.
- Verify concurrent jobs and writes, mutex time slicing, cancellation/removal,
  API disconnect independence, indexer selection, non-indexed-only filtering,
  failed embeddings, revision races, and loss of in-memory jobs after restart.

Current source observations (2026-11-17): entries have a revision but no pair
of indexing flags. Semantic sidecar stores docs(id,path,revision) and
 dirty(id,generation), with startup reconciliation; lexical Tantivy schema
 does not store source revision. These are observations, not the new design.

### Mandatory practical regression: embedding/LLM endpoint outage

Explicit owner-required acceptance scenario, 2026-11-18; additional tests do
not replace this scenario.

1. Make the actual embedding/LLM endpoint unavailable to the test server.
2. During the outage, create two documents, change the bodies of two existing
   documents, and change ONLY metadata on two other existing documents.
3. Verify that all six mutations are stored and that every affected document
   is marked non-indexed/stale for each indexer whose current revision was
   not successfully indexed. Metadata-only changes MUST be covered, not
   silently treated as already indexed. Successful lexical indexing must not
   hide failed semantic indexing or falsely mark semantic state current.
4. Restore the endpoint. Start root reindex with `non_indexed_only=true`
   (`reindex job start / [non_indexed_only]`, the replacement for the old
   `reindex / [non_indexed_only]` form) and await its completion.
5. Verify that the affected documents are selected and reindexed, indexed
   revisions match their latest stored revisions, and searches reflect the
   new bodies and metadata. Include an unchanged/current control document
   to verify it is not unnecessarily reindexed.

Outage must not lose the document mutations. A failed inline indexing attempt
must leave an accurate non-indexed/stale state that non-indexed-only reindex
can repair. Run in a disposable test environment, not by taking production
embeddings offline.
