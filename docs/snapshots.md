# Named snapshots

Snapshots belong to a document ID, not its path. Directories have no snapshots:
recursive operations select their current descendant documents. Snapshot content,
description and metadata are immutable; renaming within the same source is allowed.
A snapshot has its own distinct ID. Rollback retains the live ID and current path,
restores captured state, assigns a fresh revision, and retains **all** snapshots.
Unlike ZFS rollback, older snapshots can be restored without destroying newer ones.

## CLI

- `tb snapshot /doc@name`; `tb snapshot -r /dir@name`.
- `tb ls --snapshot /doc`; `tb ls --snapshot -R /dir`.
- `tb rollback /doc@name`; `tb rollback -r /dir@name`.
- `tb mv /doc@old /doc@new`; `-r` renames matching descendant snapshots atomically.
- `tb cp /doc@name /copy`; `tb cp -r /dir@name /destination` copies only matching documents.
- `tb rm /doc@name` deletes one snapshot. `tb rm -r /dir@name` deletes matching descendant snapshots.
- Live documents with snapshots require `rm -r`; this removes their complete history.
- Ordinary read commands accept snapshot paths. Content and metadata editing is forbidden.
- `tb search --include-snapshots QUERY` includes history; normal search excludes it.

Names match `[A-Za-z0-9._-]+`. Live entry and new ACL/PSK names cannot contain `@`.
The versioned schema migration replaces existing entry-name `@` with `_`, adding
underscores on collisions; existing ACL/PSK names are untouched.

## APIs

RPC and MCP: `create_snapshot`, `list_snapshots`, `delete_snapshot`,
`rollback_snapshot`, `rename_snapshot`. Parameters: live `path`, snapshot `name`,
optional `recursive` (false), and `new_name` for rename. Listing may omit `name`.
HTTP JSON bodies use `POST /api/snapshots/{create,list,delete,rollback,rename}`.
Ordinary document reads accept `/entry@name`. Search uses `include_snapshots`
for lexical, semantic and hybrid modes. Standard MCP includes document/directory
deletion, movement and rename; ACL authorization still applies. Full mode retains
administration/reindex gates.

## Storage and UI

Text is preserved lazily, once per captured state, in the same SQL transaction as
the first subsequent content change. Binary generations remain referenced until
no live document or snapshot uses them; no snapshot hard links or symlinks.
Committed blob deletions are queued transactionally and replayed on startup after
a crash, rechecking all live/snapshot references before removing files.
Back up SQLite consistently together with its blob directory.

The right-aligned preview selector has an empty current-document option.
Selected snapshots are read-only; `snapshot=name` in the URL survives refresh.
The search checkbox `+snaps` includes snapshots.

## Metadata editor

`tb edit --metadata /doc` and `tb meta /doc --edit` share a JSON-object editor,
including for binary documents. Editor selection: VISUAL, EDITOR, then vi.
On errors the same buffer survives: any key retries, Ctrl-C cancels (130), with
a retained recovery file. Without a controlling terminal, exit nonzero and retain
the file rather than hanging. Timeout/disconnect does not prove no server write.
