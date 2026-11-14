# Tantivy 0.22.1 → 0.27.0

The application uses upstream Tantivy 0.27, without a local vendor patch.
The search schema and SQLite document storage are unchanged.

## Compatibility evidence

`db::search_index::legacy_index_tests::reads_and_updates_tantivy_022_index`
opens a persisted synthetic index produced with Tantivy 0.22.1. It verifies:

- searching stored documents and filtering directory subtrees;
- absence of deleted documents;
- updating and deleting existing documents;
- reopening after new-version writes;
- a complete index rebuild.

The fixture and regeneration instructions are under
`tabularium/tests/fixtures/tantivy-0.22.1/`. This tests our schema and fixture,
not every historical index or a production-data migration.

Tantivy 0.27 writes format 7; 0.22 wrote format 6. Passing the forward test
is not proof that an old binary can read newly written segments. For rollback,
retain a matching pre-upgrade index backup or rebuild the search sidecar from
SQLite with the chosen application version. No production index is altered by
the tests.
