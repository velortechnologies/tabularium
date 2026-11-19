//! Contract tests using an actual HTTP mock and persisted source/sidecar databases.
use super::*;
use crate::{AuthContext, SearchKind, parse_acl_json};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

async fn embeddings(
    State(offline): State<Arc<AtomicBool>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if headers
        .get("x-embedding-test")
        .and_then(|h| h.to_str().ok())
        != Some("test-value")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if offline.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let rows: Vec<Value> = body["input"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let s = text.as_str().unwrap();
            let v = if s.contains("backup") || s.contains("recover") {
                vec![1.0, 0.0, 0.0]
            } else if s.contains("cake") {
                vec![0.0, 1.0, 0.0]
            } else {
                vec![0.0, 0.0, 1.0]
            };
            json!({"index":index,"embedding":v})
        })
        .rev()
        .collect(); // Client must honor indices, not response order.
    Json(json!({"data":rows})).into_response()
}
async fn mock() -> (String, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let offline = Arc::new(AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/embeddings", post(embeddings))
        .with_state(offline.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, offline, task)
}
fn config(url: &str) -> SemanticConfig {
    serde_json::from_value(json!({"base_url":url,"model":"test-embedding","headers":{"x-embedding-test":"test-value"},"chunk_bytes":128,"overlap_bytes":16})).unwrap()
}
async fn open(dir: &std::path::Path, url: &str) -> SqliteDatabase {
    SqliteDatabase::init(
        format!("sqlite://{}", dir.join("source.db").display()),
        dir.join("lexical"),
        0,
    )
    .await
    .unwrap()
    .with_semantic(config(url), &dir.join("semantic"))
    .await
    .unwrap()
}

#[tokio::test]
async fn hybrid_headers_quotes_acl_and_failed_api() {
    let temp = tempfile::tempdir().unwrap();
    let (url, offline, task) = mock().await;
    let db = open(temp.path(), &url).await;
    db.create_directory("/public", None, false).await.unwrap();
    db.create_directory("/private", None, false).await.unwrap();
    let id = db
        .create_file_in_directory("/public", "recovery", "Title\nUse the backup to restart.\n")
        .await
        .unwrap();
    db.create_file_in_directory("/private", "secret", "private backup instruction")
        .await
        .unwrap();
    db.create_file_in_directory("/public", "food", "apple cake recipe")
        .await
        .unwrap();
    let before = db
        .search_report("recover", None, 10, None, SearchKind::Hybrid, None)
        .await
        .unwrap();
    assert_eq!(before.semantic_status(), "partial");
    assert!(
        db.search_report("recover", None, 10, None, SearchKind::Semantic, None)
            .await
            .is_ok()
    );
    db.reindex_kind(None, SearchKind::Hybrid).await.unwrap();
    while db.flush_semantic_pending().await.unwrap() > 0 {}
    let acl = AuthContext::new(
        "public".into(),
        parse_acl_json(r#"{"admin":false,"allow":{"read":["/public/*"],"write":[]},"deny":{"read":["/private/*"],"write":[]}}"#).unwrap(),
    );
    let r = db
        .search_report("recover", None, 1, None, SearchKind::Hybrid, Some(&acl))
        .await
        .unwrap();
    assert_eq!(r.hits().len(), 1);
    let h = &r.hits()[0];
    assert_eq!(h.document_id(), id);
    assert_eq!(h.path(), "/public/recovery");
    assert_eq!(h.snippet(), "Title\nUse the backup to restart.\n");
    assert_eq!(h.line_number(), Some(1));
    assert_eq!(h.line_end(), Some(2));
    assert!(h.revision().is_some());
    offline.store(true, Ordering::SeqCst);
    let r = db
        .search_report(
            "backup",
            Some("/public"),
            10,
            None,
            SearchKind::Hybrid,
            None,
        )
        .await
        .unwrap();
    assert_eq!(r.semantic_status(), "unavailable");
    assert_eq!(r.hits()[0].document_id(), id);
    assert!(
        db.search_report("recover", None, 10, None, SearchKind::Semantic, None)
            .await
            .is_err()
    );
    task.abort();
}

#[tokio::test]
async fn stale_revision_retry_delete_move_and_persisted_state() {
    let temp = tempfile::tempdir().unwrap();
    let (url, offline, task) = mock().await;
    let db = open(temp.path(), &url).await;
    db.create_directory("/old", None, false).await.unwrap();
    db.create_directory("/new", None, false).await.unwrap();
    let id = db
        .create_file_in_directory("/old", "doc", "backup version one")
        .await
        .unwrap();
    db.reindex_kind(None, SearchKind::Semantic).await.unwrap();
    db.flush_semantic_pending().await.unwrap();
    offline.store(true, Ordering::SeqCst);
    db.update_document(id, "cake version two").await.unwrap();
    assert!(db.flush_semantic_pending().await.is_err());
    offline.store(false, Ordering::SeqCst);
    let r = db
        .search_report("recover", None, 10, None, SearchKind::Semantic, None)
        .await
        .unwrap();
    assert!(
        r.hits().is_empty(),
        "stale revision must not produce a quote"
    );
    drop(db);
    let db = open(temp.path(), &url).await;
    assert!(db.flush_semantic_pending().await.unwrap() > 0);
    let r = db
        .search_report("cake", None, 10, None, SearchKind::Semantic, None)
        .await
        .unwrap();
    assert_eq!(r.hits()[0].snippet(), "cake version two");
    db.move_document_to_directory(id, "/new", "doc")
        .await
        .unwrap();
    db.flush_semantic_pending().await.unwrap();
    assert!(
        db.search_report("cake", Some("/old"), 10, None, SearchKind::Semantic, None)
            .await
            .unwrap()
            .hits()
            .is_empty()
    );
    assert_eq!(
        db.search_report("cake", Some("/new"), 10, None, SearchKind::Semantic, None)
            .await
            .unwrap()
            .hits()[0]
            .path(),
        "/new/doc"
    );
    db.delete_document(id).await.unwrap();
    assert!(
        db.search_report("cake", None, 10, None, SearchKind::Semantic, None)
            .await
            .unwrap()
            .hits()
            .is_empty()
    );
    db.flush_semantic_pending().await.unwrap();
    task.abort();
}

#[tokio::test]
async fn model_fingerprint_invalidates_old_index() {
    let temp = tempfile::tempdir().unwrap();
    let (url, _, task) = mock().await;
    let db = open(temp.path(), &url).await;
    db.create_file_in_directory("/", "doc", "backup")
        .await
        .unwrap();
    db.reindex_kind(None, SearchKind::Semantic).await.unwrap();
    drop(db);
    let mut cfg = config(&url);
    cfg.model_revision = "different".into();
    let db = SqliteDatabase::init(
        format!("sqlite://{}", temp.path().join("source.db").display()),
        temp.path().join("lexical"),
        0,
    )
    .await
    .unwrap()
    .with_semantic(cfg, &temp.path().join("semantic"))
    .await
    .unwrap();
    assert_eq!(
        db.search_report("recover", None, 10, None, SearchKind::Hybrid, None)
            .await
            .unwrap()
            .semantic_status(),
        "incompatible"
    );
    assert!(
        db.reindex_kind(Some("/x"), SearchKind::Semantic)
            .await
            .is_err()
    );
    db.reindex_kind(None, SearchKind::Semantic).await.unwrap();
    assert!(
        !db.search_report("recover", None, 10, None, SearchKind::Semantic, None)
            .await
            .unwrap()
            .hits()
            .is_empty()
    );
    task.abort();
}

#[tokio::test]
async fn failed_rebuild_preserves_active_index_and_scoped_reindex_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let (url, offline, task) = mock().await;
    let db = open(temp.path(), &url).await;
    for path in ["/a", "/ab"] {
        db.create_directory(path, None, false).await.unwrap();
    }
    db.create_file_in_directory("/a", "doc", "backup alpha")
        .await
        .unwrap();
    db.create_file_in_directory("/ab", "doc", "cake beta")
        .await
        .unwrap();
    db.reindex_kind(None, SearchKind::Semantic).await.unwrap();
    while db.flush_semantic_pending().await.unwrap() > 0 {}
    offline.store(true, Ordering::SeqCst);
    assert!(db.reindex_kind(None, SearchKind::Semantic).await.is_err());
    offline.store(false, Ordering::SeqCst);
    let r = db
        .search_report("recover", Some("/a"), 10, None, SearchKind::Semantic, None)
        .await
        .unwrap();
    assert_eq!(r.hits().len(), 1);
    assert_eq!(r.hits()[0].path(), "/a/doc");
    db.reindex_kind(Some("/a"), SearchKind::Semantic)
        .await
        .unwrap();
    let r = db
        .search_report("cake", Some("/ab"), 10, None, SearchKind::Semantic, None)
        .await
        .unwrap();
    assert_eq!(r.hits()[0].path(), "/ab/doc");
    let index = db.semantic.as_ref().unwrap();
    index.mark_pending(999).await.unwrap();
    let generation = index
        .pending(16)
        .await
        .unwrap()
        .into_iter()
        .find(|(id, _)| *id == 999)
        .unwrap()
        .1;
    index.mark_pending(999).await.unwrap();
    index.acknowledge(999, generation).await.unwrap();
    assert!(
        index
            .pending(16)
            .await
            .unwrap()
            .iter()
            .any(|(id, _)| *id == 999)
    );
    task.abort();
}

/// Synthetic meaning fixture: retrieval contract, not a benchmark of a real model.
#[tokio::test]
async fn search_time_order_filters_ties_and_semantic_excerpts() {
    use crate::{SearchOptions, SearchSort};
    let temp = tempfile::tempdir().unwrap();
    let (url, _offline, task) = mock().await;
    let db = open(temp.path(), &url).await;
    db.create_directory("/public", None, false).await.unwrap();
    db.create_directory("/private", None, false).await.unwrap();
    let source = "Title\nUse the backup to restart.\n";
    // Creation/ID order deliberately differs from canonical path and modification order.
    let mut ids = Vec::new();
    for (path, name) in [
        ("/public", "z"),
        ("/public", "b"),
        ("/public", "a"),
        ("/private", "hidden"),
    ] {
        ids.push(
            db.create_file_in_directory(path, name, source)
                .await
                .unwrap(),
        );
    }
    let dessert = db
        .create_file_in_directory("/public", "dessert", "apple cake recipe")
        .await
        .unwrap();
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite://{}",
        temp.path().join("source.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("UPDATE entries SET modified_at = 50 WHERE id = ?")
        .bind(dessert.raw())
        .execute(&pool)
        .await
        .unwrap();
    for (id, ns) in ids.iter().zip([100_i64, 200, 200, 300]) {
        sqlx::query("UPDATE entries SET modified_at = ? WHERE id = ?")
            .bind(ns)
            .bind(id.raw())
            .execute(&pool)
            .await
            .unwrap();
    }
    db.reindex_kind(None, SearchKind::Hybrid).await.unwrap();
    while db.flush_semantic_pending().await.unwrap() > 0 {}
    let acl = AuthContext::new("public".into(),
        parse_acl_json(r#"{"admin":false,"allow":{"read":["/public/*"],"write":[]},"deny":{"read":["/private/*"],"write":[]}}"#).unwrap());
    // Both engines must filter before top-k; newest private hit must not affect truncation.
    for kind in [
        SearchKind::Lexical,
        SearchKind::Semantic,
        SearchKind::Hybrid,
    ] {
        let query = if kind == SearchKind::Lexical {
            "backup"
        } else {
            "recover"
        };
        let options = SearchOptions {
            sort: SearchSort::ModifiedDesc,
            ..Default::default()
        };
        let report = db
            .search_report_with_options(query, None, 1, None, kind, Some(&acl), options)
            .await
            .unwrap();
        assert_eq!(report.hits()[0].path(), "/public/a");
        assert!(report.has_more());
        let bounded = SearchOptions {
            modified_after: Some(99),
            modified_before: Some(200),
            ..options
        };
        let report = db
            .search_report_with_options(query, None, 1, None, kind, Some(&acl), bounded)
            .await
            .unwrap();
        assert_eq!(report.hits()[0].path(), "/public/z");
        assert!(!report.has_more());
        let empty = SearchOptions {
            modified_after: Some(200),
            modified_before: Some(200),
            ..options
        };
        assert!(
            db.search_report_with_options(query, None, 1, None, kind, Some(&acl), empty)
                .await
                .unwrap()
                .hits()
                .is_empty()
        );
        let report = db
            .search_report_with_options(
                query,
                Some("/public"),
                10,
                Some(ids[1]),
                kind,
                Some(&acl),
                options,
            )
            .await
            .unwrap();
        assert_eq!(report.hits().len(), 1);
        assert_eq!(report.hits()[0].document_id(), ids[1]);
    }
    let lexical = db
        .search_report(
            "recover",
            Some("/public"),
            10,
            None,
            SearchKind::Lexical,
            Some(&acl),
        )
        .await
        .unwrap();
    assert!(lexical.hits().is_empty());
    let semantic = db
        .search_report(
            "recover",
            Some("/public"),
            10,
            None,
            SearchKind::Semantic,
            Some(&acl),
        )
        .await
        .unwrap();
    // Semantic search ranks all candidates, without an absolute relevance cutoff.
    // Expected sources rank above the unrelated negative; a large limit still returns it.
    assert_eq!(semantic.hits().len(), 4);
    assert_eq!(semantic.hits()[3].path(), "/public/dessert");
    assert!(semantic.hits()[0].score() > semantic.hits()[3].score());
    assert_eq!(semantic.hits()[0].path(), "/public/a"); // equal scores use path, not creation order
    for hit in &semantic.hits()[..3] {
        assert_eq!(hit.snippet(), source);
        let (text, revision) = db
            .document_slice_versioned(
                hit.document_id(),
                u32::try_from(hit.start_line().unwrap()).unwrap(),
                u32::try_from(hit.line_end().unwrap()).unwrap(),
                hit.revision(),
            )
            .await
            .unwrap();
        assert_eq!(text, source.trim_end_matches('\n'));
        assert_eq!(Some(revision.as_str()), hit.revision());
    }
    let invalid = SearchOptions {
        modified_after: Some(201),
        modified_before: Some(200),
        ..Default::default()
    };
    assert!(
        db.search_report_with_options(
            "recover",
            None,
            1,
            None,
            SearchKind::Semantic,
            None,
            invalid
        )
        .await
        .is_err()
    );
    pool.close().await;
    task.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the cohesive operation/scenario together.
async fn snapshots_semantic_filter_move_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let (url, _, task) = mock().await;
    let db = open(temp.path(), &url).await;
    let id = db
        .create_file_in_directory("/", "note", "backup history")
        .await
        .unwrap();
    let snapshot = db.create_snapshot("/note", "old", false).await.unwrap()[0].id();
    db.update_document(id, "cake current").await.unwrap();
    db.reindex_kind(None, SearchKind::Hybrid).await.unwrap();
    while db.flush_semantic_pending().await.unwrap() > 0 {}
    for kind in [
        SearchKind::Lexical,
        SearchKind::Semantic,
        SearchKind::Hybrid,
    ] {
        let normal = db
            .search_report("backup", None, 100, None, kind, None)
            .await
            .unwrap();
        assert!(normal.hits().iter().all(|h| h.document_id() != snapshot));
        let history = db
            .search_report_with_options(
                "backup",
                None,
                100,
                None,
                kind,
                None,
                crate::SearchOptions {
                    include_snapshots: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            history
                .hits()
                .iter()
                .any(|h| h.document_id() == snapshot && h.path() == "/note@old")
        );
    }
    for kind in [
        SearchKind::Lexical,
        SearchKind::Semantic,
        SearchKind::Hybrid,
    ] {
        let scoped = db
            .search_report_with_options(
                "backup",
                None,
                100,
                Some(id),
                kind,
                None,
                crate::SearchOptions {
                    include_snapshots: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(scoped.hits().iter().any(|h| h.document_id() == snapshot));
        assert!(
            scoped
                .hits()
                .iter()
                .all(|h| h.document_id() == snapshot || h.document_id() == id)
        );
    }
    db.rename_document(id, "renamed").await.unwrap();
    while db.flush_semantic_pending().await.unwrap() > 0 {}
    let history = db
        .search_report_with_options(
            "backup",
            None,
            100,
            None,
            SearchKind::Semantic,
            None,
            crate::SearchOptions {
                include_snapshots: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(history.hits().iter().any(|h| h.path() == "/renamed@old"));
    db.delete_document_recursive(id).await.unwrap();
    while db.flush_semantic_pending().await.unwrap() > 0 {}
    assert!(
        !db.semantic
            .as_ref()
            .unwrap()
            .indexed_ids()
            .await
            .unwrap()
            .contains(&snapshot.raw())
    );
    db.reindex_kind(None, SearchKind::Hybrid).await.unwrap();
    let history = db
        .search_report_with_options(
            "backup",
            None,
            100,
            None,
            SearchKind::Hybrid,
            None,
            crate::SearchOptions {
                include_snapshots: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(history.hits().is_empty());
    task.abort();
}

async fn wait_jobs(db: &SqliteDatabase) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !db.reindex_jobs().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("job completion");
}
fn missing_options() -> ReindexOptions {
    ReindexOptions {
        path: "/".into(),
        non_indexed_only: true,
        indexer_kind: SearchKind::Hybrid,
    }
}

/// Owner's mandatory practical scenario, exercising actual HTTP endpoint failure.
#[tokio::test]
async fn outage_create_update_metadata_then_missing_job_repairs_six_documents() {
    let temp = tempfile::tempdir().unwrap();
    let (url, offline, task) = mock().await;
    let db = Arc::new(open(temp.path(), &url).await);
    db.create_directory("/outage", None, false).await.unwrap();
    let mut ids = Vec::new();
    for name in ["body1", "body2", "meta1", "meta2"] {
        ids.push(
            db.create_document_at_path(format!("/outage/{name}"), "old cake", false, None)
                .await
                .unwrap(),
        );
    }
    let control = db
        .create_document_at_path("/outage/control", "unchanged cake", false, None)
        .await
        .unwrap();
    assert_eq!(
        db.document_index_state(control).await.unwrap().semantic,
        "ready"
    );
    offline.store(true, Ordering::SeqCst);
    for id in &ids[..2] {
        db.update_document(*id, "backup changed").await.unwrap();
    }
    for name in ["meta1", "meta2"] {
        db.set_document_metadata(
            &format!("/outage/{name}"),
            &json!({"note":"newmetadata"}),
            None,
        )
        .await
        .unwrap();
    }
    for name in ["new1", "new2"] {
        ids.push(
            db.create_document_at_path(format!("/outage/{name}"), "backup new", false, None)
                .await
                .unwrap(),
        );
    }
    for id in &ids {
        let state = db.document_index_state(*id).await.unwrap();
        assert_eq!(state.lexical, "ready");
        assert_eq!(state.semantic, "pending");
        assert_ne!(db.get_document(*id).await.unwrap(), "");
    }
    assert_eq!(
        db.reindex_explain(&missing_options()).await.unwrap().len(),
        6
    );
    assert_eq!(
        db.document_index_state(control).await.unwrap().semantic,
        "ready"
    );
    // Outage is an engine error: a job must terminate, not stay in a retry loop.
    db.reindex_job_start(missing_options()).await.unwrap();
    wait_jobs(&db).await;
    assert_eq!(
        db.reindex_explain(&missing_options()).await.unwrap().len(),
        6
    );
    assert_eq!(db.indexing.active(), 0);
    offline.store(false, Ordering::SeqCst);
    db.reindex_job_start(missing_options()).await.unwrap();
    wait_jobs(&db).await;
    assert!(
        db.reindex_explain(&missing_options())
            .await
            .unwrap()
            .is_empty()
    );
    for id in ids {
        assert_eq!(db.document_index_state(id).await.unwrap().semantic, "ready");
    }
    assert_eq!(
        db.search_report("newmetadata", None, 20, None, SearchKind::Lexical, None)
            .await
            .unwrap()
            .hits()
            .len(),
        2
    );
    assert_eq!(
        db.search_report("backup", None, 20, None, SearchKind::Semantic, None)
            .await
            .unwrap()
            .hits()
            .len(),
        7
    );
    assert_eq!(db.indexing.active(), 0);
    assert!(db.reindex_jobs().is_empty());
    task.abort();
}

#[tokio::test]
async fn llm_timeout_releases_inline_operation_and_can_recover() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let hung = Arc::new(AtomicBool::new(true));
    let h = hung.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/embeddings",
                post(move || {
                    let h = h.clone();
                    async move {
                        if h.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                        Json(json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(&url);
    cfg.timeout = 1;
    let db = SqliteDatabase::init(
        format!("sqlite://{}", temp.path().join("source.db").display()),
        temp.path().join("lex"),
        0,
    )
    .await
    .unwrap()
    .with_semantic(cfg, &temp.path().join("sem"))
    .await
    .unwrap();
    let id = tokio::time::timeout(
        Duration::from_secs(5),
        db.create_document_at_path("/hung", "backup", false, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        db.document_index_state(id).await.unwrap().semantic,
        "pending"
    );
    assert_eq!(db.indexing.active(), 0);
    assert!(db.indexing.mutex.try_lock().is_ok());
    hung.store(false, Ordering::SeqCst);
    db.reindex_selected("/", SearchKind::Semantic, true)
        .await
        .unwrap();
    assert_eq!(db.document_index_state(id).await.unwrap().semantic, "ready");
    assert_eq!(db.indexing.active(), 0);
    task.abort();
}

#[tokio::test]
async fn document_failure_skips_but_model_failure_stops_before_next_document() {
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let status = Arc::new(std::sync::atomic::AtomicUsize::new(413));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let c = counter.clone();
    let s = status.clone();
    let app = Router::new().route(
        "/v1/embeddings",
        post(move |Json(body): Json<Value>| {
            let c = c.clone();
            let s = s.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                if body["input"][0].as_str().unwrap().contains("bad") {
                    StatusCode::from_u16(u16::try_from(s.load(Ordering::SeqCst)).unwrap())
                        .unwrap()
                        .into_response()
                } else {
                    Json(json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]})).into_response()
                }
            }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(open(temp.path(), &url).await);
    let bad = db.storage.create_file("/bad", "bad input").await.unwrap();
    let good = db
        .storage
        .create_file("/good", "valid input")
        .await
        .unwrap();
    db.reindex_job_start(missing_options()).await.unwrap();
    wait_jobs(&db).await;
    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert_eq!(
        db.document_index_state(bad).await.unwrap().semantic,
        "pending"
    );
    assert_eq!(
        db.document_index_state(good).await.unwrap().semantic,
        "ready"
    );
    status.store(503, Ordering::SeqCst);
    counter.store(0, Ordering::SeqCst);
    db.storage.update_file(good, "changed").await.unwrap();
    db.reindex_job_start(missing_options()).await.unwrap();
    wait_jobs(&db).await;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "engine outage must stop the pass"
    );
    assert_eq!(
        db.document_index_state(good).await.unwrap().semantic,
        "pending"
    );
    assert_eq!(db.indexing.active(), 0);
    task.abort();
}

#[tokio::test]
async fn stop_waits_for_inflight_commit_and_old_revision_stays_pending() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let e = entered.clone();
    let r = release.clone();
    let app = Router::new().route(
        "/v1/embeddings",
        post(move || {
            let e = e.clone();
            let r = r.clone();
            async move {
                e.notify_one();
                r.notified().await;
                Json(json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}))
            }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(open(temp.path(), &url).await);
    let id = db.storage.create_file("/race", "old").await.unwrap();
    let job = db.reindex_job_start(missing_options()).await.unwrap();
    entered.notified().await;
    db.storage.update_file(id, "new").await.unwrap();
    let d = db.clone();
    let j = job.clone();
    let stop = tokio::spawn(async move { d.reindex_job_stop(&j).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!stop.is_finished());
    release.notify_one();
    stop.await.unwrap().unwrap();
    assert!(db.reindex_job_status(&job).is_err());
    assert_eq!(
        db.document_index_state(id).await.unwrap().semantic,
        "pending"
    );
    assert_eq!(db.indexing.active(), 0);
    task.abort();
}

#[tokio::test]
async fn time_slice_allows_api_write_to_pass_large_job() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let e = entered.clone();
    let app = Router::new().route(
        "/v1/embeddings",
        post(move || {
            let e = e.clone();
            async move {
                e.notify_one();
                tokio::time::sleep(Duration::from_millis(120)).await;
                Json(json!({"data":[{"index":0,"embedding":[1.0,0.0,0.0]}]}))
            }
        }),
    );
    let endpoint = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let db = Arc::new(open(temp.path(), &url).await);
    for i in 0..8 {
        db.storage
            .create_file(&format!("/large{i}"), "text")
            .await
            .unwrap();
    }
    let job = db.reindex_job_start(missing_options()).await.unwrap();
    entered.notified().await;
    let other = db.clone();
    let write = tokio::spawn(async move {
        other
            .create_document_at_path("/small", "text", false, None)
            .await
    });
    let id = tokio::time::timeout(Duration::from_secs(5), write)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(db.document_index_state(id).await.unwrap().semantic, "ready");
    assert!(
        db.reindex_job_status(&job).is_ok(),
        "API must pass before large job finishes"
    );
    db.reindex_job_stop(&job).await.unwrap();
    assert_eq!(db.indexing.active(), 0);
    endpoint.abort();
}

#[tokio::test]
async fn failed_forced_reindex_clears_persistent_semantic_flag() {
    let (url, offline, task) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path(), &url).await;
    let id = db.storage.create_file("/doc", "backup").await.unwrap();
    db.inline_index(id).await;
    assert_eq!(db.document_index_state(id).await.unwrap().semantic, "ready");
    offline.store(true, Ordering::SeqCst);
    assert!(
        db.reindex_selected("/", SearchKind::Semantic, false)
            .await
            .is_err()
    );
    let state = db.document_index_state(id).await.unwrap();
    assert_eq!(state.lexical, "ready");
    assert_eq!(state.semantic, "pending");
    offline.store(false, Ordering::SeqCst);
    db.reindex_selected("/", SearchKind::Semantic, true)
        .await
        .unwrap();
    assert_eq!(db.document_index_state(id).await.unwrap().semantic, "ready");
    task.abort();
}

#[tokio::test]
async fn semantic_metadata_reads_scale_with_documents_not_chunks() {
    let temp = tempfile::tempdir().unwrap();
    let (url, _offline, task) = mock().await;
    let db = open(temp.path(), &url).await;
    db.create_directory("/manuals", None, false).await.unwrap();
    let body = "Use the backup to recover the server.\n".repeat(80);
    let id = db
        .create_file_in_directory("/manuals", "recovery", &body)
        .await
        .unwrap();
    db.reindex_kind(None, SearchKind::Semantic).await.unwrap();
    let candidates = db
        .semantic
        .as_ref()
        .unwrap()
        .candidates("recover", Some("/manuals"), None)
        .await
        .unwrap();
    assert!(candidates.len() > 20, "fixture must exercise many chunks");
    assert!(candidates.iter().all(|c| c.id == id.raw()));

    db.storage.metadata_reads.store(0, Ordering::Relaxed);
    let report = db
        .search_report(
            "recover",
            Some("/manuals"),
            1,
            None,
            SearchKind::Semantic,
            None,
        )
        .await
        .unwrap();
    assert_eq!(report.hits().len(), 1);
    assert_eq!(report.hits()[0].document_id(), id);
    assert!(body.contains(report.hits()[0].snippet()));
    // One candidate metadata read per document, plus three fresh source
    // validation reads: before/after the body and immediately before emission.
    assert_eq!(db.storage.metadata_reads.load(Ordering::Relaxed), 4);

    // A new request must not reuse metadata from the previous request.
    db.storage
        .update_file(id, "apple cake recipe")
        .await
        .unwrap();
    db.storage.metadata_reads.store(0, Ordering::Relaxed);
    let stale = db
        .search_report(
            "recover",
            Some("/manuals"),
            1,
            None,
            SearchKind::Semantic,
            None,
        )
        .await
        .unwrap();
    assert!(stale.hits().is_empty(), "stale revision must not escape");
    assert_eq!(db.storage.metadata_reads.load(Ordering::Relaxed), 1);

    // Missing source documents are cached too, without returning orphan chunks.
    db.storage.delete_file(id).await.unwrap();
    db.storage.metadata_reads.store(0, Ordering::Relaxed);
    let deleted = db
        .search_report(
            "recover",
            Some("/manuals"),
            1,
            None,
            SearchKind::Semantic,
            None,
        )
        .await
        .unwrap();
    assert!(deleted.hits().is_empty());
    assert_eq!(db.storage.metadata_reads.load(Ordering::Relaxed), 1);
    task.abort();
}
