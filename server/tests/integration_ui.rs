use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use sorter2_server::{create_app, create_app_state, state::AppConfig, ui_action::UI_RPC_FIELD};
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
async fn post_ui_record_vote_morphs_ranking_and_persists() {
    let (addr, tmp) = start_test_server().await;
    let rpc = serde_json::json!({
        "action": "record_vote",
        "a": "alpha",
        "b": "beta",
        "ratio_left": 2,
        "ratio_right": 1
    })
    .to_string();
    let mut form = HashMap::new();
    form.insert(UI_RPC_FIELD.to_string(), rpc);

    let client = reqwest::Client::new();
    let body = client
        .post(format!("http://{addr}/ui"))
        .form(&form)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("Idiomorph.morph"));
    assert!(body.contains("ranking-panel"));
    assert!(body.contains("alpha"));

    let log = std::fs::read_to_string(tmp.path().join("events.jsonl")).unwrap();
    assert!(log.contains("vote_recorded"));

    let cfg = AppConfig {
        data_dir: tmp.path().to_string_lossy().into_owned(),
        event_log_path: tmp.path().join("events.jsonl").to_string_lossy().into_owned(),
        port: 0,
    };
    let state = create_app_state(cfg).await;
    let groups = state.groups.read().await;
    let group = groups.get("").expect("default scope group after replay");
    let ranked = sorter2_server::ranking::ranked_items(group);
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item.as_str(), "alpha");
}

#[tokio::test]
async fn post_ui_parse_query_redirects_to_subreddit() {
    let (addr, _tmp) = start_test_server().await;
    let rpc = serde_json::json!({
        "action": "parse_query",
        "query": "r/rust"
    })
    .to_string();
    let mut form = HashMap::new();
    form.insert(UI_RPC_FIELD.to_string(), rpc);

    let client = reqwest::Client::new();
    let body = client
        .post(format!("http://{addr}/ui"))
        .form(&form)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("window.location.href"));
    assert!(body.contains("/?sub=rust"));
}
