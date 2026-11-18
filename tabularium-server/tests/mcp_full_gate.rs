//! MCP `mcp.full` tool registration (safe vs trusted full surface).

#![cfg(feature = "mcp")]

use std::sync::Arc;
use std::time::Duration;

use bma_ts::Monotonic;
use tabularium::SqliteDatabase;
use tabularium_server::mcp::TabulariumMcp;
use tabularium_server::web::AppState;

async fn test_app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("mcp_gate.db");
    let idx_path = dir.path().join("mcp_gate.idx");
    let uri = format!("sqlite://{}", db_path.display());
    let db = Arc::new(
        SqliteDatabase::init(&uri, &idx_path, 8)
            .await
            .expect("db init"),
    );
    let app = AppState {
        db,
        wait_timeout: Duration::from_secs(3),
        process_started_at: Monotonic::now(),
        authenticate_api: false,
        authenticate_mcp: false,
        oidc: None,
        anonymous: None,
    };
    (app, dir)
}

#[tokio::test]
async fn mcp_standard_registers_document_operations_but_not_admin() {
    let (app, _dir) = test_app().await;
    let mcp = TabulariumMcp::new(app, Arc::from(""), false);
    for name in [
        "delete_document",
        "delete_directory",
        "move_document",
        "move_directory",
        "rename_document",
        "rename_directory",
        "create_snapshot",
        "list_snapshots",
        "delete_snapshot",
        "rename_snapshot",
        "rollback_snapshot",
    ] {
        assert!(mcp.has_mcp_tool(name), "{name}");
    }
    assert!(!mcp.has_mcp_tool("reindex_job_start"));
    assert!(mcp.has_mcp_tool("get_document"));
    assert!(mcp.has_mcp_tool("list_directory"));
    assert!(mcp.has_mcp_tool("append_if_not_contains"));
    assert!(!mcp.has_mcp_tool("whoami"));
    assert!(!mcp.has_mcp_tool("acl_list"));
    assert!(!mcp.has_mcp_tool("psk_create"));
}

#[tokio::test]
async fn mcp_full_true_registers_destructive_tools() {
    let (app, _dir) = test_app().await;
    let mcp = TabulariumMcp::new(app, Arc::from(""), true);
    assert!(mcp.has_mcp_tool("delete_document"));
    assert!(mcp.has_mcp_tool("delete_directory"));
    assert!(mcp.has_mcp_tool("rename_document"));
    assert!(mcp.has_mcp_tool("rename_directory"));
    assert!(mcp.has_mcp_tool("move_document"));
    assert!(mcp.has_mcp_tool("move_directory"));
    assert!(mcp.has_mcp_tool("reindex_job_start"));
    assert!(mcp.has_mcp_tool("whoami"));
    assert!(mcp.has_mcp_tool("acl_list"));
    assert!(mcp.has_mcp_tool("acl_destroy"));
    assert!(mcp.has_mcp_tool("psk_destroy"));
}

#[tokio::test]
async fn standard_mcp_snapshot_lifecycle_and_search() {
    use serde_json::{Value, json};
    let (app, _dir) = test_app().await;
    let mcp = TabulariumMcp::new(app, Arc::from(""), false);
    mcp.invoke_rpc_for_test(
        "put_document",
        json!({"path":"/note","content":"historicalword"}),
    )
    .await
    .unwrap();
    mcp.invoke_rpc_for_test("create_snapshot", json!({"path":"/note","name":"first"}))
        .await
        .unwrap();
    mcp.invoke_rpc_for_test(
        "put_document",
        json!({"path":"/note","content":"newword","force":true}),
    )
    .await
    .unwrap();
    let body = mcp
        .invoke_rpc_for_test(
            "search",
            json!({"query":"historicalword","kind":"lexical","include_snapshots":true}),
        )
        .await
        .unwrap();
    assert!(body.contains("/note@first"), "{body}");
    let body = mcp
        .invoke_rpc_for_test("search", json!({"query":"historicalword","kind":"lexical"}))
        .await
        .unwrap();
    assert!(!body.contains("/note@first"), "{body}");
    mcp.invoke_rpc_for_test(
        "rename_snapshot",
        json!({"path":"/note","name":"first","new_name":"renamed"}),
    )
    .await
    .unwrap();
    mcp.invoke_rpc_for_test(
        "rollback_snapshot",
        json!({"path":"/note","name":"renamed"}),
    )
    .await
    .unwrap();
    let body = mcp
        .invoke_rpc_for_test("get_document", json!({"path":"/note"}))
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["content"], "historicalword");
    assert!(
        mcp.invoke_rpc_for_test("delete_document", json!({"path":"/note"}))
            .await
            .is_err()
    );
    mcp.invoke_rpc_for_test("delete_document", json!({"path":"/note","recursive":true}))
        .await
        .unwrap();
    assert!(
        mcp.invoke_rpc_for_test("get_document", json!({"path":"/note@renamed"}))
            .await
            .is_err()
    );
}
