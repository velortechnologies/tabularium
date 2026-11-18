mod common;
use serde_json::{Value, json};
async fn rpc(base: &str, method: &str, params: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{base}/rpc"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
#[tokio::test]
async fn explain_start_disconnect_and_rest_job_control() {
    let server = common::spawn_test_server().await;
    let base = &server.base_url;
    let created = rpc(
        base,
        "create_document",
        json!({"path":"/file","content":"indexed body"}),
    )
    .await;
    assert!(created.get("error").is_none(), "{created}");
    let state = rpc(base, "index_state", json!({"path":"/file"})).await;
    assert_eq!(state["result"]["lexical"], "ready");
    assert_eq!(state["result"]["semantic"], "disabled");
    let explained = rpc(
        base,
        "reindex_job_explain",
        json!({"path":"/","non_indexed_only":true}),
    )
    .await;
    assert_eq!(explained["result"], json!([]));
    let explained = rpc(base, "reindex_job_explain", json!({"path":"/"})).await;
    assert_eq!(explained["result"][0]["path"], "/file");
    let started = rpc(base, "reindex_job_start", json!({"path":"/"})).await;
    let id = started["result"]["id"].as_str().expect("UUID returned");
    assert_eq!(id.len(), 36);
    // rpc() dropped its HTTP client; job ownership is independent.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let list = rpc(base, "reindex_job_list", json!({})).await;
            if list["result"] == json!([]) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        rpc(base, "reindex_job_status", json!({"id":id}))
            .await
            .get("error")
            .is_some()
    );
    let response: Value = reqwest::Client::new()
        .post(format!("{base}/api/reindex"))
        .json(&json!({"action":"explain","path":"/","non_indexed_only":true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response, json!([]));
    assert!(
        rpc(
            base,
            "reindex_job_start",
            json!({"path":"/","indexer_kind":"semantic"})
        )
        .await
        .get("error")
        .is_some()
    );
    assert!(
        rpc(
            base,
            "reindex_job_start",
            json!({"path":"/","surprise":true})
        )
        .await
        .get("error")
        .is_some()
    );
}
#[tokio::test]
async fn job_operations_require_admin_when_authenticated() {
    let server =
        common::spawn_test_server_with_wait_timeout(std::time::Duration::from_secs(2), true).await;
    for method in [
        "reindex_job_start",
        "reindex_job_explain",
        "reindex_job_list",
        "reindex_job_status",
        "reindex_job_stop",
    ] {
        assert!(
            rpc(&server.base_url, method, json!({"path":"/","id":"test"}))
                .await
                .get("error")
                .is_some()
        );
    }
}
