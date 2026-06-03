use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use sorter2_server::{
    create_app, create_app_state, path_types::ItemId, state::AppConfig, ui_action::UI_RPC_FIELD,
};
use tempfile::TempDir;
use tokio::net::TcpListener;

async fn start_test_server() -> (SocketAddr, TempDir) {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().to_string_lossy().into_owned();
    let cfg = AppConfig {
        data_dir: data.clone(),
        event_log_path: format!("{data}/events.jsonl"),
        views_log_path: format!("{data}/views.jsonl"),
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
async fn post_ui_vote_compare_morphs_edge_history() {
    let (addr, _tmp) = start_test_server().await;
    let parent = "reddit.com/r/rust";
    let a = "reddit.com/r/rust/comments/aaa/announcing_rust_199";
    let b = "reddit.com/r/rust/comments/bbb/what_are_you_working_on";

    let rpc = serde_json::json!({
        "action": "record_vote",
        "a": a,
        "b": b,
        "ratio_left": {"$form:i32": "ratio_left"},
        "ratio_right": {"$form:i32": "ratio_right"},
        "scope": parent,
        "vote_compare": true,
    })
    .to_string();
    let mut form = HashMap::new();
    form.insert(UI_RPC_FIELD.to_string(), rpc);
    form.insert("ratio_left".into(), "70".into());
    form.insert("ratio_right".into(), "30".into());

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

    assert!(
        body.contains("vote-edge-history"),
        "expected edge history morph, got: {body}"
    );
    assert!(
        body.contains("vote-ranking-panel"),
        "expected ranking sidebar morph, got: {body}"
    );
    assert!(
        body.contains("sorter2MorphWithFlip"),
        "expected animated ranking morph, got: {body}"
    );
    assert!(
        body.contains("70:30"),
        "expected recorded ratio in morph, got: {body}"
    );
    assert!(
        !body.contains("no votes on this pair yet"),
        "should not show empty edge history after vote, got: {body}"
    );
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

    // Replay in a fresh data dir (RocksDB locks entity_db while the server runs).
    let replay_tmp = TempDir::new().unwrap();
    std::fs::copy(
        tmp.path().join("events.jsonl"),
        replay_tmp.path().join("events.jsonl"),
    )
    .unwrap();
    let replay_data = replay_tmp.path().to_string_lossy().into_owned();
    let cfg = AppConfig {
        data_dir: replay_data.clone(),
        event_log_path: format!("{replay_data}/events.jsonl"),
        views_log_path: format!("{replay_data}/views.jsonl"),
        port: 0,
    };
    let state = create_app_state(cfg).await;
    let tree = state.scope_tree(&ItemId::root()).unwrap();
    let root = tree.get(&ItemId::root()).expect("root node after replay");
    let ranked = sorter2_server::ranking::ranked_items(&root.local_ranking);
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item.as_str(), "alpha");
}

#[tokio::test]
async fn browse_url_renders_subreddit_page() {
    let (addr, _tmp) = start_test_server().await;
    let client = reqwest::Client::new();
    let html = client
        .get(format!("http://{addr}/~/https://reddit.com/r/rust"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("ranking-panel"));
    assert!(html.contains("/~/https://reddit.com/r/rust"));
}

#[tokio::test]
async fn vote_page_renders_live_ranking_sidebar() {
    let (addr, _tmp) = start_test_server().await;
    let client = reqwest::Client::new();
    let seed_rpc = serde_json::json!({
        "action": "record_vote",
        "a": "alpha",
        "b": "beta",
        "ratio_left": 2,
        "ratio_right": 1
    })
    .to_string();
    let mut form = HashMap::new();
    form.insert(UI_RPC_FIELD.to_string(), seed_rpc);
    client
        .post(format!("http://{addr}/ui"))
        .form(&form)
        .send()
        .await
        .unwrap();

    let html = client
        .get(format!("http://{addr}/vote?parent="))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("vote-ranking-panel"));
    assert!(html.contains("live ranking"));
    assert!(html.contains("scope-theme"));
    assert!(html.contains("--accent: oklch("));
    assert!(html.contains("--bg: oklch("));
    assert!(html.contains("vote-ratio-display"));
    assert!(html.contains(">1:1<"));
    assert!(html.contains("--rank-bg: oklch("));
    assert!(html.contains("--rank-fg: #"));
    assert!(html.contains("data-rank-item=\"alpha\""));
    assert!(html.contains("is-compared"));
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
    assert!(body.contains("/~/https://reddit.com/r/rust"));
}
