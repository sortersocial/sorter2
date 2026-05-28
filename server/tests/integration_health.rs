use std::net::SocketAddr;

use axum::Router;
use sorter2_server::{create_app, create_app_state, state::AppConfig};
use tempfile::TempDir;
use tokio::net::TcpListener;

async fn start_test_server() -> (SocketAddr, TempDir) {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().to_string_lossy().into_owned();
    let cfg = AppConfig {
        data_dir: data.clone(),
        event_log_path: format!("{data}/events.jsonl"),
        port: 0,
    };
    let state = create_app_state(cfg).await;
    let app: Router = create_app(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, tmp)
}

#[tokio::test]
async fn healthz_ok() {
    let (addr, _tmp) = start_test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn home_has_demo_panel() {
    let (addr, _tmp) = start_test_server().await;
    let client = reqwest::Client::new();
    let html = client
        .get(format!("http://{addr}/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("vote-panel"));
    assert!(html.contains("ranking-panel"));
    assert!(html.contains("parser-panel"));
    assert!(html.contains("__rpc__"));
}
