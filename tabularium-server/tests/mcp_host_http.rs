//! Trusted-network MCP compatibility: arbitrary Host/Origin and large requests.

#![cfg(feature = "mcp")]

use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use bma_ts::Monotonic;
use tabularium::SqliteDatabase;
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
async fn trusted_network_transport_accepts_hosts_origins_and_large_requests() {
    let (app, _dir) = test_app().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let cancel = CancellationToken::new();
    let shutdown = cancel.clone();
    let task = tokio::spawn(async move {
        tabularium_server::mcp::serve(&addr.to_string(), app, String::new(), false, shutdown)
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{addr}/mcp");
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let init = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-03-26","capabilities":{},
        "clientInfo":{"name":"host-regression","version":"1"}}});
    let arbitrary_host = client
        .post(&url)
        .header("Host", "other.review.invalid")
        .header("Origin", "https://external-client.example")
        .header("Accept", "application/json, text/event-stream")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(arbitrary_host.status(), reqwest::StatusCode::OK);
    assert!(
        arbitrary_host
            .text()
            .await
            .unwrap()
            .contains("protocolVersion")
    );
    let allowed = client
        .post(&url)
        .header("Host", "tb.review.invalid")
        .header("Accept", "application/json, text/event-stream")
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), reqwest::StatusCode::OK);
    let body = allowed.text().await.unwrap();
    assert!(body.contains("protocolVersion"), "{body}");
    // rmcp 3.x defaults to a 4 MiB request cap; preserve the previous transport policy.
    let mut large_init = init.clone();
    large_init["params"]["clientInfo"]["name"] =
        serde_json::Value::String("x".repeat(4 * 1024 * 1024 + 1));
    let large = client
        .post(&url)
        .header("Host", "large.review.invalid")
        .header("Origin", "https://another-client.example")
        .header("Accept", "application/json, text/event-stream")
        .json(&large_init)
        .send()
        .await
        .unwrap();
    assert_eq!(large.status(), reqwest::StatusCode::OK);
    let body = large.text().await.unwrap();
    assert!(body.contains("protocolVersion"), "{body}");
    cancel.cancel();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}
