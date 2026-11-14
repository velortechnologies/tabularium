# Tantivy 0.22.1 compatibility fixture

Synthetic Tabularium schema, generated with Tantivy 0.22.1 before the 0.27 upgrade.
No user data. Two live documents (IDs 1 and 2), one deleted document (ID 3).
Includes persisted segments and the managed-file manifest; lock files are empty.

The regression test copies this directory to temporary storage before opening it.
It verifies search, directory filtering, deleted documents, updates, deletes,
reopening mixed old/new segments, and rebuilding the index.

Regenerate into a NEW empty directory from the repository root:

```sh
cargo run --release --manifest-path scripts/tantivy-legacy-fixture/Cargo.toml -- /tmp/tantivy-022-fixture
```

The legacy dependency is test-data tooling only, not part of the application workspace.
