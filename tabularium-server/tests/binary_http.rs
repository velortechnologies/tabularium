use serde_json::{Value, json};
mod common;

#[tokio::test]
async fn binary_http_rpc_range_copy_and_metadata() {
    let server = common::spawn_test_server().await;
    let c = reqwest::Client::new();
    let url = format!("{}/api/content/test.png", server.base_url);
    let data = vec![0, 255, 1, 128, 4];
    let r = c
        .put(&url)
        .header("Content-Type", "image/png")
        .body(data.clone())
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    let r = c.get(&url).send().await.unwrap();
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(r.bytes().await.unwrap().as_ref(), data.as_slice());
    let r = c
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 1-3/5");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &[255, 1, 128]);
    assert_eq!(
        c.get(&url)
            .header("Range", "bytes=99-")
            .send()
            .await
            .unwrap()
            .status(),
        416
    );
    assert_eq!(
        c.get(&url)
            .header("Range", "bytes=1-2")
            .header("If-Range", "\"stale\"")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let meta: Value = c
        .get(format!("{}/api/doc/test.png", server.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta["content_kind"], "binary_attachment");
    assert!(meta["content"].is_null());
    assert_eq!(
        c.put(format!("{}/api/doc/test.png", server.base_url))
            .body("corrupt")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        c.put(&url)
            .header("Content-Type", "image/png")
            .header("If-Match", "\"stale\"")
            .body("bad")
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let rpc = |method: &str, params: Value| {
        c.post(format!("{}/rpc", server.base_url))
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
    };
    let copied: Value = rpc("copy_entries", json!({"src":"/test.png","dst":"/copy.png"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(copied.get("error").is_none(), "{copied}");
    let body: Value = rpc("get_binary", json!({"path":"/copy.png"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["result"]["content_base64"], "AP8BgAQ=");
    let changed: Value = rpc(
        "put_binary",
        json!({"path":"/copy.png","content_base64":"AAE=","mime_type":"image/png"}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(changed.get("error").is_none(), "{changed}");
    let metadata: Value = rpc(
        "set_document_metadata",
        json!({"path":"/copy.png","metadata":{"tag":"binaryneedle"}}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(metadata.get("error").is_none(), "{metadata}");
    assert_eq!(
        c.get(format!("{}/api/content/copy.png", server.base_url))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        &[0, 1]
    );
}

#[tokio::test]
async fn cookie_sessions_acl_csrf_logout_and_revocation() {
    use std::{sync::Arc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        tabularium::SqliteDatabase::init(
            format!("sqlite://{}", dir.path().join("session.db").display()),
            dir.path().join("idx"),
            0,
        )
        .await
        .unwrap(),
    );
    db.acl_upsert_validated(
        "admin",
        r#"{"admin":true,"allow":{"read":[],"write":[]},"deny":{"read":[],"write":[]}}"#,
    )
    .await
    .unwrap();
    db.psk_insert("test", "admin", "test-secret-not-production")
        .await
        .unwrap();
    let app = tabularium_server::web::router(tabularium_server::web::AppState {
        db: db.clone(),
        wait_timeout: Duration::from_secs(1),
        process_started_at: bma_ts::Monotonic::now(),
        authenticate_api: true,
        authenticate_mcp: true,
        oidc: None,
        anonymous: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let c = reqwest::Client::new();
    let login = c
        .post(format!("{base}/api/session/login"))
        .header("X-Auth-Key", "test-secret-not-production")
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let set_cookie = login.headers()["set-cookie"].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));
    assert!(!set_cookie.contains("test-secret"));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    let url = format!("{base}/api/content/image.png");
    assert_eq!(
        c.put(&url)
            .header("Cookie", &cookie)
            .header("Content-Type", "image/png")
            .body(vec![0, 255])
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        c.put(&url)
            .header("Cookie", &cookie)
            .header("Origin", "https://attacker.invalid")
            .header("Content-Type", "image/png")
            .body(vec![0, 255])
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        c.put(&url)
            .header("Cookie", &cookie)
            .header("Origin", &base)
            .header("Content-Type", "image/png")
            .body(vec![0, 255])
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(c.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        c.head(&url)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c.post(format!("{base}/api/session/logout"))
            .header("Cookie", &cookie)
            .header("Origin", &base)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c.get(&url)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let token = db
        .create_browser_session("test-secret-not-production")
        .await
        .unwrap();
    assert!(db.resolve_browser_session(&token).await.unwrap().is_some());
    db.psk_delete_named("test").await.unwrap();
    assert!(db.resolve_browser_session(&token).await.unwrap().is_none());
    task.abort();
}
