//! Snapshot selectors must not bypass descendant ACLs.
use bma_ts::Monotonic;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tabularium::SqliteDatabase;
use tabularium_server::web::{AppState, router};

#[tokio::test]
async fn recursive_snapshots_respect_denied_children_in_all_mutation_routes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        SqliteDatabase::init(
            format!("sqlite://{}", dir.path().join("db").display()),
            dir.path().join("idx"),
            0,
        )
        .await
        .unwrap(),
    );
    db.create_directory("/docs", None, false).await.unwrap();
    db.create_directory("/copy", None, false).await.unwrap();
    db.put_document_by_path("/docs/public", "visible", false, None)
        .await
        .unwrap();
    db.put_document_by_path("/docs/private", "secret", false, None)
        .await
        .unwrap();
    db.create_snapshot("/docs", "old", true).await.unwrap();
    db.acl_upsert_validated("limited",r#"{"admin":false,"allow":{"read":["/docs","/copy"],"write":["/docs","/copy"]},"deny":{"read":["/docs/private"],"write":["/docs/private"]}}"#).await.unwrap();
    db.psk_insert("tester", "limited", "snapshot-test-key-abcdefghijklmnop")
        .await
        .unwrap();
    let app = router(AppState {
        db: db.clone(),
        wait_timeout: Duration::from_secs(3),
        process_started_at: Monotonic::now(),
        authenticate_api: true,
        authenticate_mcp: false,
        oidc: None,
        anonymous: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    for (method, params) in [
        (
            "create_snapshot",
            json!({"path":"/docs","name":"new","recursive":true}),
        ),
        (
            "rollback_snapshot",
            json!({"path":"/docs","name":"old","recursive":true}),
        ),
        (
            "delete_snapshot",
            json!({"path":"/docs","name":"old","recursive":true}),
        ),
        (
            "delete_document",
            json!({"path":"/docs@old","recursive":true}),
        ),
        (
            "move_document",
            json!({"path":"/docs@old","new_path":"/docs@renamed","recursive":true}),
        ),
        (
            "copy_entries",
            json!({"src":"/docs@old","dst":"/copy","recursive":true}),
        ),
    ] {
        let value: Value = client
            .post(format!("{base}/rpc"))
            .header("X-Auth-Key", "snapshot-test-key-abcdefghijklmnop")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(value.get("error").is_some(), "{method}: {value}");
        assert_eq!(
            db.list_snapshots("/docs", None, true).await.unwrap().len(),
            2
        );
    }
    let response = client
        .delete(format!("{base}/api/doc/docs@old?recursive=true"))
        .header("X-Auth-Key", "snapshot-test-key-abcdefghijklmnop")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(db.list_directory("/copy").await.unwrap().is_empty());
    task.abort();
}
