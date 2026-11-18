# Hybrid lexical and semantic search

`kind` is the same lowercase enum on CLI, REST, JSON-RPC and MCP:

- `lexical`: Tantivy full-text/name/description search.
- `semantic`: embedding similarity; requires configured and initialized semantic storage.
- `hybrid` (default): both engines concurrently, reciprocal-rank fusion (`k=60`), one result per document.

Without `[semantic]`, default hybrid remains lexical-only and works with old databases. Explicit semantic mode returns an actionable error when not configured, unindexed, incompatible or unavailable. Hybrid keeps successful results from the other engine and exposes status; a failed semantic branch never causes a silent claim of full coverage.

## Storage and upgrade

- `tabularium.db` remains authoritative; its schema is not changed by this feature.
- `tabularium.index` remains the Tantivy directory.
- A separate sibling `tabularium.semantic.index/semantic.db` stores derived vectors, exact byte/line coordinates, document revisions, model/preprocessing identity. It is disposable and reconstructible. Treat it as private data like the source database.
- Initial upgrade: add optional `[semantic]` configuration, restart, run `tb reindex job start / --indexer-kind semantic` once. Plain `tb reindex job start /` rebuilds both configured engines.
- Model ID/revision, prefixes, normalization or chunk settings changing require a full semantic rebuild, not a migration. Pin the provider model version with `model_revision`; alias-only model changes on a remote server cannot be reliably detected when dimensions stay equal.
- Moving the same inference model between CPU and GPU does not require a new client or database layout.

Reindex is a server-owned background job, independent of the API connection. Writes index inline and log indexing failures without failing a successful source write. See [reindex jobs](reindex-jobs.md) for job lifecycle, cancellation, state, configuration and recovery. Commits happen per document; partial progress is retained on failure.

Document writes index inline; failures are logged and leave stale revisions for explicit or automatic missing-only jobs. No startup dirty-queue worker runs. Missing/revision-mismatched source excerpts are never returned. Indexing does not touch document accessed_at.

## Embeddings service

The Rust server is an HTTP client, not a model runtime. `base_url` includes the provider's API prefix (usually `/v1`), and the client calls `POST .../embeddings` with `model`, `input` array and `encoding_format=float`. CPU/GPU runtime may be on another host. No generative answer endpoint or Codex agent is used.

See `config.toml.example` for `[semantic]` and arbitrary `[semantic.headers]`. HTTP header values are treated as sensitive in diagnostics; config Debug does not expose them, redirects are disabled, and upstream response bodies are not copied into errors. No key is required unless configured through headers.

Chunks use UTF-8-safe bounded byte windows, preferring newline boundaries, with overlap. `chunk_bytes` is not a token budget: choose it conservatively for the provider/model and include prefixes/special tokens in the input allowance. The server never silently truncates rejected model inputs. It validates returned indices, dimensions, finite values and nonzero norms and normalizes vectors locally.

The first implementation uses exact cosine scans in the SQLite sidecar, appropriate for the present corpus, without an external vector DB. Larger corpora may require a different vector backend. Similarity is a relevance ranking, not a guarantee that a quote answers the question. Quotes always come from the current validated source revision, never from generated model prose.

## Interfaces

```sh
tb search --kind semantic 'restore after failure'
tb search --kind lexical 'exact_identifier'
tb search 'restore after failure' # hybrid
tb reindex job start / --indexer-kind semantic
tb reindex job start /manual --indexer-kind lexical
tb reindex job start / # hybrid
```

REST:

- `GET /api/search?q=...&kind=hybrid&dir=/manual`
- `POST /api/search`: JSON/form/multipart fields `q`, optional `dir`, `kind`, `with_status`.
- `POST /api/reindex`: JSON `{ "action":"start", "indexer_kind":"semantic", "path":"/" }`; same admin requirement as RPC reindex.

REST search returns the existing hit array unless `with_status=true`. It always sets `X-Tabularium-Lexical-Status` and `X-Tabularium-Semantic-Status` response headers, including for empty results.

JSON-RPC:

```json
{"jsonrpc":"2.0","id":1,"method":"search","params":{"query":"restore after failure","kind":"hybrid","with_status":true}}
{"jsonrpc":"2.0","id":2,"method":"reindex_job_start","params":{"indexer_kind":"semantic","path":"/"}}
```

RPC search preserves the legacy hit array by default. With `with_status=true`, it returns `{hits, lexical_status, semantic_status}`. MCP search uses this rich envelope by default. Existing hit fields remain; `line_end` and `revision` describe validated semantic excerpts. Ordinary snippets without source revision keep their legacy meaning.

Statuses include `ready`, `disabled`, `not_requested`, `unindexed`, `incompatible`, `partial`, `unavailable`. Partial means refreshes remain; hybrid results can still include current semantic quotes and lexical hits. CLI prints a concise diagnostic for incomplete/unavailable engines and retains the normal result list.

ACL, directory/file scope and source revision checks apply before semantic candidates consume the result limit. A denied document is not returned through either engine. The inference service receives source fragments during indexing and queries during search, so it is part of the trusted deployment boundary.

## Web UI

The search bar has independent scope (Local/Global) and kind
(Hybrid/Semantic/Lexical, default Hybrid) selectors. Changing kind during a
search refetches the committed query without changing its directory scope.

## Modification-time filters and ordering

Search accepts `sort=relevance` (default) or `sort=modified_desc`.
Equal primary keys are ordered by canonical path ascending, then document ID.
`modified_after` and `modified_before` are **exclusive unsigned UNIX
nanoseconds**, the same unit as returned `modified_at`; equality excludes the
document. Equal bounds yield an empty interval; a reversed interval is invalid.
Missing bounds are unbounded. Modification time is a technical timestamp, not
an assertion of factual currency. No source database schema change is required.

Filters and ACL/path constraints apply before per-engine result limits.
For hybrid search, relevance retains reciprocal-rank fusion; modified_desc
orders the union of each engine's newest matching candidates. Scores remain
engine/fusion scores even when ordering by modification time.
Existing array responses remain arrays unless with_status=true, limit, or
max_output_bytes requests an envelope. sort/time filters alone do not change shape.

CLI example:
```sh
tb search --kind hybrid --sort modified_desc --modified-after 1792713600000000000 --limit 10 --json memory
```

REST GET uses query parameters, REST POST accepts JSON/form/multipart, RPC/MCP
use the same snake_case keys. The Web UI provides an order selector and local
date/time inputs, converting milliseconds to nanoseconds using BigInt strings.

CI includes deterministic equal-date fixtures and offline semantic retrieval
controls (literal miss, semantic hit, unrelated negative, exact quote/slice,
revision and ACL/path). These validate retrieval plumbing, not real-model
relevance. Real embeddings still need a deployment-specific evaluation set.

### Relevance acceptance caveat

Semantic mode is a ranker, not an absence detector: it currently has no absolute
similarity threshold. With a large limit even unrelated sources can appear at
the bottom. The offline negative control verifies ordering, not exclusion.
Do not interpret a nonempty semantic result as proof that a relevant fact exists.
Calibrate a real-model evaluation set before introducing a score cutoff.

### Small deployment check (2026-10-24)

A read-only check against the deployed BGE-M3 index used three private,
paraphrased RU/EN queries and an expected document already known from source.
The expected document ranked first twice and second once in semantic mode;
lexical search missed it for the cross-language query. Two semantic excerpts
contained the requested fact, while the third selected a nearby general passage.
Private queries, source paths and content are intentionally not included here.
This demonstrates both a cross-language gain and an excerpt-selection limitation,
not statistical recall/precision acceptance. No production documents were changed.
