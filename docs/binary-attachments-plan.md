# Binary attachments implementation plan

Status: implemented 2026-11-11 from main 1369ba3; deployed to citests
10.90.1.71 with migrated tb-test data. Validation details: binary-attachments-validation.md.

## Agreed design

A binary is a first-class TB entry, e.g. /_memory/kira/photos/photo1.jpg,
with ordinary identity, path, ACL (path-based, as today), metadata and
revision. Bytes live outside SQLite, owned by the entry and addressed by its
entry ID. No sentinel text or user-editable metadata controls content type.

Out of scope by decision: compatibility with old clients, backups, upload
size limits (a front server handles them if one is installed), a strict
kind/MIME consistency constraint (revisit after practice).

## 1. Schema

- New column `content_kind` on `entries`: 0 text (default), 1 binary.
  `EntryKind` (dir/file) stays as is. Add by the existing "ensure column"
  migration; existing rows become text. Migration is repeatable.
- Reuse existing `mime_type` and `size` columns (size = byte length). Add
  `sha256`. Private `blob_revision` stores the content generation; the location derives
  from entry ID and blob_revision (see clarification below).
- The table CHECK requires `content IS NOT NULL` for files and SQLite cannot
  alter a CHECK: binary rows store `content = ''`. No table rebuild.
- One guard in the database layer: text reads and writes (`get_file_content`,
  update, append and everything built on them: head, tail, slice, grep, wc,
  stat, say, append_if_not_contains, ws_doc, search excerpts, body cache)
  return a typed "binary entry" error for content_kind = 1. Binary writes
  refuse text entries unless the request is an explicit replacement of kind.
- `PUT /api/doc/...` must stop turning arbitrary bytes into text over a
  binary entry (today: from_utf8_lossy + force=true).

## 2. Blob storage and lifecycle

- Configurable private blob root (default beside the database file).
- Layout by entry ID, zero-padded decimal, 1000 entries per directory:
  `<id / 1_000_000>/<id / 1000 % 1000>/<id>.<revision>`
  e.g. entry 1234567, revision R -> `001/234/1234567.R`.
  Entry IDs are AUTOINCREMENT and never reused. Paths are server-derived only.
- Lifecycle is bound to the entry, nothing old is kept:
  - create/replace: stream to a temp file inside the blob root (same
    filesystem), hash while writing, fsync file, rename to the final name,
    fsync directory, commit the DB row (kind, mime, size, sha256, revision),
    then unlink the previous revision file.
  - delete entry / recursive directory delete: collect binary IDs and
    revisions in the same transaction as the row deletes, commit, unlink.
  - move/rename: DB only; the blob does not move.
  - cp: physical copy into the destination entry ID, plus metadata.
- Revision in the filename is what keeps "replace immediately" safe: a failed
  replacement leaves the old file referenced, and a reader that already
  opened the old file finishes its stream after the unlink.
- Crash windows leave only unreferenced files (ID missing in DB, or revision
  not current). A startup or on-demand sweep may remove them; a DB row
  without its file must never result from a normal write.
- Optimistic `only_if_revision` is checked inside the DB transaction after
  the upload; the loser's file is removed.

## 3. Sessions (browser auth for media)

`<img>` and `<video>` cannot send `X-Auth-Key`, so the UI gets a cookie
session.

- Login endpoint: valid `X-Auth-Key` -> server session row (random token,
  stored hashed, bound to the PSK name) and cookie. Logout deletes the row.
- No server-side expiry: the session lives until logout or until its PSK or
  ACL is removed. Browsers cap cookie lifetime (about 400 days), so the
  cookie is re-issued with the maximum lifetime on use.
- Cookie: HttpOnly, SameSite=Strict, Secure when served over TLS.
- Cookie auth is accepted for GET/HEAD; mutating requests via cookie must
  also pass an Origin check (CSRF). `X-Auth-Key` and the OIDC assertion
  header keep working unchanged and take precedence.

## 4. REST

Route `/api/content/<path>`; `/api/doc` stays the JSON/text route.

- PUT: raw streamed body, no body limit on this route, optional `If-Match`
  (= only_if_revision). Received length must equal Content-Length when given;
  a short or aborted upload publishes nothing.
- MIME resolution: request Content-Type (including explicit application/octet-stream); if absent, by file extension; with no extension the entry
  is text/markdown. A resolved text type creates a text entry (body must be
  valid UTF-8, otherwise 400); anything else creates a binary entry.
- GET/HEAD: raw bytes, Content-Type, Content-Length, strong ETag = revision.
  Range with 206/416 and `If-Range`; a mismatched If-Range returns the full
  current body. ACL is checked on every request before any byte.
- Inline only for an allowlist (jpeg, png, gif, webp, mp4, webm, mp3, ogg,
  wav, pdf to be decided); everything else `Content-Disposition: attachment`.
  Always `X-Content-Type-Options: nosniff`, `Content-Security-Policy:
  sandbox`, `Cache-Control: private`. The UI shares the origin, so HTML/SVG
  never render inline.
- `GET /api/doc/<path>` for a binary: same JSON with `content_kind`,
  `mime_type`, `size_bytes`, `sha256`, `content: null`.
- Metadata via the existing metadata API after upload.

## 5. RPC/MCP and CLI

- `put_binary` / `get_binary`: Base64 in JSON with path, MIME, revision.
  These buffer in memory; large files belong to REST. Open point: `/rpc`
  keeps or drops its present 32 MB body limit.
- get_document/search/list never return Base64. MCP returns metadata and the
  content route, never credentials.
- CLI export writes raw bytes for binaries; import uploads non-UTF-8 files
  and known binary extensions through the content route (streamed), text as
  today. Metadata sidecars work for both. The CLI needs a REST streaming
  path besides RPC.

## 6. Search

- Index name, description and metadata of binaries; body is empty.
- Hits show attachment info, no excerpt. Reindex and the semantic queue skip
  binary bodies.
- STT/captions/OCR as separate linked text documents, outside this work.

## 7. UI

- Session login/logout replaces the sessionStorage key for media requests.
- Preview by content kind and MIME: image and video from the allowlist in a
  bounded viewport (max-width/max-height 100%, object-fit: contain, fixed
  container height); video with controls, no autoplay, preload=metadata.
- Other types: bold "Binary attachment", centered, with download.
- Decode failure falls back to download. Text and metadata views unchanged.

## 8. Delivery order and tests

1. Schema, DB guard, blob store with the write/replace/delete sequence.
2. Sessions.
3. REST content route with Range.
4. RPC/MCP, CLI import/export.
5. Search and UI.

Tests: repeatable migration; unchanged text behaviour; empty, non-UTF-8 and
large round-trips with sha256; text operations refused on binaries and old
PUT unable to corrupt them; replace removes the old file and a concurrent
reader completes; interrupted upload publishes nothing; delete and recursive
delete remove files; cp duplicates bytes; move keeps them; revision
preconditions; ACL denial on GET/HEAD/Range by key and by cookie; session
survives restart, dies on logout and PSK removal; CSRF refusal; MIME
fallback chain; inline allowlist; export/import round-trip with sidecars;
video seeking; tall/wide previews.

Follow AGENTS.md for implementation checks. UI integration uses 10.90.1.71,
not localhost.

## First consumer

Import the preserved 2026-11-11 Telegram ZIP and individual media. Link
messages/transcripts to stable TB paths; preserve Telegram IDs, timestamps,
original relative paths and hashes. Verify counts/checksums before calling
the migration complete. Do not remove current local originals.

## Implementation clarification: blob generation

Entry revision changes on metadata edits, move/rename and timestamp updates,
not only content replacement. Store a private `blob_revision` generation
separately and derive the disk filename from ID + blob_revision. Entry revision
remains the public optimistic-concurrency token / ETag. Metadata changes and
moves must never lose or copy the blob. No physical locator is exposed by APIs.
