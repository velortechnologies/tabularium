//! Regression tests for transactional flags and metadata-only selection.
use super::*;
use crate::SearchKind;

async fn ready(db: &SqliteStorage, id: EntryId) -> i64 {
    let (generation, _, _) = db.index_generation(id).await.unwrap();
    db.mark_indexed(id, generation, false).await.unwrap();
    db.mark_indexed(id, generation, true).await.unwrap();
    generation
}

#[tokio::test]
async fn flags_commit_with_writes_rollback_and_reject_stale_completion() {
    let db = SqliteStorage::connect("sqlite::memory:").await.unwrap();
    let id = db.create_file("/a", "body").await.unwrap();
    let generation = ready(&db, id).await;
    db.touch(id).await.unwrap();
    assert_eq!(
        db.index_generation(id).await.unwrap(),
        (generation, true, true)
    );
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("UPDATE entries SET metadata_json='{}',revision='changed' WHERE id=?")
        .bind(id.raw())
        .execute(&mut *tx)
        .await
        .unwrap();
    let flags: (bool, bool) =
        sqlx::query_as("SELECT lexical_indexed,semantic_indexed FROM entry_index_state WHERE id=?")
            .bind(id.raw())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(flags, (false, false));
    tx.rollback().await.unwrap();
    assert_eq!(
        db.index_generation(id).await.unwrap(),
        (generation, true, true)
    );
    db.set_file_metadata(id, r#"{"new":true}"#, None)
        .await
        .unwrap();
    db.mark_indexed(id, generation, false).await.unwrap();
    db.mark_indexed(id, generation, true).await.unwrap();
    let (current, l, s) = db.index_generation(id).await.unwrap();
    assert!(current > generation);
    assert!(!l && !s);
    assert_eq!(
        db.index_candidates("/", SearchKind::Hybrid, true, true)
            .await
            .unwrap()
            .len(),
        1
    );
    ready(&db, id).await;
    assert!(
        db.index_candidates("/", SearchKind::Hybrid, true, true)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn flags_cover_subtree_snapshot_and_delete() {
    let db = SqliteStorage::connect("sqlite::memory:").await.unwrap();
    db.create_directory("/a", None, false).await.unwrap();
    db.create_directory("/a/nested", None, false).await.unwrap();
    let id = db.create_file("/a/nested/doc", "text").await.unwrap();
    let snapshot = db
        .create_snapshots("/a/nested/doc", "saved", false)
        .await
        .unwrap()[0];
    ready(&db, id).await;
    ready(&db, snapshot).await;
    db.rename_directory("/a", "/b").await.unwrap();
    let pending = db
        .index_candidates("/b", SearchKind::Hybrid, true, true)
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().any(|r| r.1 == "/b/nested/doc@saved"));
    assert!(pending.iter().all(|r| !r.3 && !r.4));
    db.delete_file_recursive(id).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM entry_index_state")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn flags_persist_and_engine_identity_invalidates_only_affected_engine() {
    let dir = tempfile::tempdir().unwrap();
    let uri = format!("sqlite://{}", dir.path().join("source.db").display());
    let db = SqliteStorage::connect(&uri).await.unwrap();
    db.sync_index_identity(false, "lexical-1").await.unwrap();
    db.sync_index_identity(true, "semantic-1").await.unwrap();
    let id = db.create_file("/doc", "body").await.unwrap();
    ready(&db, id).await;
    db.pool.close().await;
    let db = SqliteStorage::connect(&uri).await.unwrap();
    db.sync_index_identity(true, "semantic-1").await.unwrap();
    assert!(
        db.index_candidates("/", SearchKind::Hybrid, true, true)
            .await
            .unwrap()
            .is_empty()
    );
    db.sync_index_identity(true, "semantic-2").await.unwrap();
    let (_, l, s) = db.index_generation(id).await.unwrap();
    assert!(l && !s);
    assert!(
        db.index_candidates("/", SearchKind::Hybrid, true, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.index_candidates("/", SearchKind::Semantic, true, true)
            .await
            .unwrap()
            .len(),
        1
    );
    db.sync_index_identity(false, "lexical-2").await.unwrap();
    assert!(!db.index_generation(id).await.unwrap().1);
}

#[tokio::test]
async fn missing_flag_indexes_exist_and_large_bodies_are_not_selected() {
    let db = SqliteStorage::connect("sqlite::memory:").await.unwrap();
    let body = "x".repeat(4 * 1024 * 1024);
    let id = db.create_file("/big", &body).await.unwrap();
    let row = db
        .index_candidates("/", SearchKind::Hybrid, true, true)
        .await
        .unwrap();
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].1, "/big");
    for (flag, index) in [
        ("lexical_indexed", "idx_pending_lexical"),
        ("semantic_indexed", "idx_pending_semantic"),
    ] {
        let rows = sqlx::query(&format!(
            "EXPLAIN QUERY PLAN SELECT * FROM entry_index_state WHERE {flag}=0"
        ))
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(
            rows.iter()
                .any(|r| r.get::<String, _>("detail").contains(index))
        );
    }
    ready(&db, id).await;
    assert!(
        db.index_candidates("/", SearchKind::Hybrid, true, true)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn actual_selection_plan_uses_both_pending_indexes() {
    let db = SqliteStorage::connect("sqlite::memory:").await.unwrap();
    let sql = index_candidates_sql(SearchKind::Hybrid, true, true);
    let rows = sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
        .bind(1_i64)
        .bind("/")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    let plan = rows
        .iter()
        .map(|r| r.get::<String, _>("detail"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(plan.contains("idx_pending_lexical"), "{plan}");
    assert!(plan.contains("idx_pending_semantic"), "{plan}");
    assert!(!sql.contains("readable_entries") && !sql.contains("snapshot_contents"));
    sqlx::raw_sql("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<7000) INSERT INTO entries(parent_id,kind,name,content,size,created_at,modified_at,accessed_at,revision) SELECT 1,1,'doc-' || i,printf('%01024d',i),1024,0,0,0,'r-' || i FROM n;")
        .execute(&db.pool).await.unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        db.index_candidates("/", SearchKind::Hybrid, false, true)
            .await
            .unwrap()
            .len(),
        7000
    );
    eprintln!("7000-document metadata selection: {:?}", started.elapsed());
    sqlx::query("UPDATE entry_index_state SET lexical_indexed=1,semantic_indexed=1 WHERE id>3")
        .execute(&db.pool)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        db.index_candidates("/", SearchKind::Hybrid, true, true)
            .await
            .unwrap()
            .len(),
        2
    );
    eprintln!(
        "7000-document missing-only selection (2 pending): {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn binary_and_unchanged_snapshots_do_not_create_semantic_debt() {
    let db = SqliteStorage::connect("sqlite::memory:").await.unwrap();
    let id = db.create_file("/doc", "old body").await.unwrap();
    let snapshot = db.create_snapshots("/doc", "saved", false).await.unwrap()[0];
    ready(&db, snapshot).await;
    db.update_file(id, "new body").await.unwrap();
    let (_, l, s) = db.index_generation(snapshot).await.unwrap();
    assert!(l && s);
    sqlx::query("UPDATE entries SET content_kind=1 WHERE id=?")
        .bind(id.raw())
        .execute(&db.pool)
        .await
        .unwrap();
    let (generation, _, _) = db.index_generation(id).await.unwrap();
    db.mark_indexed(id, generation, false).await.unwrap();
    assert!(
        db.index_candidates("/", SearchKind::Hybrid, true, true)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        db.index_candidates("/", SearchKind::Semantic, true, true)
            .await
            .unwrap()
            .is_empty()
    );
}
