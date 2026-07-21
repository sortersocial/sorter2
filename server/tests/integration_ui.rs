use std::collections::HashMap;
use std::net::SocketAddr;

use axum::Router;
use sorter2_server::{
    auth::session::SESSION_COOKIE, create_app, create_app_state, nsfw::NSFW_COOKIE,
    path_types::ItemId, reducer::EntityData, state::AppConfig, ui_action::UI_RPC_FIELD,
};
use tempfile::TempDir;
use tokio::net::TcpListener;

async fn start_test_server() -> (SocketAddr, TempDir, String) {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().to_string_lossy().into_owned();
    let cfg = AppConfig {
        data_dir: data.clone(),
        event_log_path: format!("{data}/events.jsonl"),
        views_log_path: format!("{data}/views.jsonl"),
        port: 0,
    };
    let state = create_app_state(cfg).await;
    let session_id = state.create_default_session().unwrap();
    let session_cookie = format!("{SESSION_COOKIE}={session_id}");
    let app: Router = create_app(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, tmp, session_cookie)
}

#[tokio::test]
async fn post_ui_vote_compare_morphs_edge_history() {
    let (addr, _tmp, session_cookie) = start_test_server().await;
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
        .header("Cookie", &session_cookie)
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
    let (addr, tmp, session_cookie) = start_test_server().await;
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
        .header("Cookie", &session_cookie)
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
    let ranked = sorter2_server::ranking::ranked_items(&root.votes);
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].item.as_str(), "alpha");
}

#[tokio::test]
async fn browse_url_renders_subreddit_page() {
    let (addr, _tmp, _session_cookie) = start_test_server().await;
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
    let (addr, _tmp, session_cookie) = start_test_server().await;
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
        .header("Cookie", &session_cookie)
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
    assert!(html.contains("vote-hud"));
    assert!(html.contains("vote-ratio-display"));
    assert!(html.contains(">1:1<"));
    assert!(!html.contains("vote-slider-left-label"));
    assert!(html.contains("--rank-bg: oklch("));
    assert!(html.contains("--rank-fg: #"));
    assert!(html.contains("data-rank-item=\"alpha\""));
    assert!(html.contains("is-compared"));
}

#[tokio::test]
async fn nsfw_items_hidden_until_opt_in_and_leave_returns() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().to_string_lossy().into_owned();
    let cfg = AppConfig {
        data_dir: data.clone(),
        event_log_path: format!("{data}/events.jsonl"),
        views_log_path: format!("{data}/views.jsonl"),
        port: 0,
    };
    let state = create_app_state(cfg).await;
    let parent = ItemId::from_url("https://reddit.com/r/mixed").unwrap();
    let sfw = ItemId::from_url("https://reddit.com/r/mixed/comments/aaa/safe").unwrap();
    let nsfw = ItemId::from_url("https://reddit.com/r/mixed/comments/bbb/adult").unwrap();
    state.ensure_node(&parent).await.unwrap();
    state.ensure_node(&sfw).await.unwrap();
    state.ensure_node(&nsfw).await.unwrap();
    state
        .projection_store
        .put_ephemeral_content(
            &parent,
            &EntityData {
                title: "mixed".into(),
                author: None,
                body_html: None,
                over_18: false,
                thumb_url: None,
                image_url: None,
                link_url: None,
            },
            1,
        )
        .unwrap();
    state
        .projection_store
        .put_ephemeral_content(
            &sfw,
            &EntityData {
                title: "safe post".into(),
                author: None,
                body_html: None,
                over_18: false,
                thumb_url: None,
                image_url: None,
                link_url: None,
            },
            1,
        )
        .unwrap();
    state
        .projection_store
        .put_ephemeral_content(
            &nsfw,
            &EntityData {
                title: "adult post".into(),
                author: None,
                body_html: Some("<p>secret</p>".into()),
                over_18: true,
                thumb_url: Some("https://example.com/nsfw.jpg".into()),
                image_url: Some("https://example.com/nsfw-full.jpg".into()),
                link_url: Some("https://example.com/out".into()),
            },
            1,
        )
        .unwrap();

    let app: Router = create_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let parent_url = format!("http://{addr}/~/https://reddit.com/r/mixed");

    let html = client
        .get(&parent_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("safe post"), "SFW should list: {html}");
    assert!(
        !html.contains("adult post"),
        "NSFW must not list without opt-in: {html}"
    );

    let nsfw_page = client
        .get(format!(
            "http://{addr}/~/https://reddit.com/r/mixed/comments/bbb"
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        nsfw_page.contains("Yes, I am 18+"),
        "direct NSFW URL should gate: {nsfw_page}"
    );
    assert!(
        !nsfw_page.contains("https://example.com/nsfw-full.jpg"),
        "gated page must hide media: {nsfw_page}"
    );

    let enter = client
        .post(format!("http://{addr}/nsfw/enter"))
        .form(&[("return_to", "/~/https://reddit.com/r/mixed")])
        .send()
        .await
        .unwrap();
    assert_eq!(enter.status(), reqwest::StatusCode::SEE_OTHER);
    let set_cookie = enter
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect::<Vec<_>>()
        .join(";");
    assert!(set_cookie.contains(NSFW_COOKIE));

    let opted = client
        .get(&parent_url)
        .header("Cookie", format!("{NSFW_COOKIE}=1"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        opted.contains("adult post"),
        "opted-in should list NSFW: {opted}"
    );
    assert!(
        opted.contains("Exit NSFW"),
        "opted-in nav should offer leave: {opted}"
    );

    let leave = client
        .post(format!("http://{addr}/nsfw/leave"))
        .header("Cookie", format!("{NSFW_COOKIE}=1"))
        .form(&[("return_to", "/~/https://reddit.com/r/mixed/comments/bbb")])
        .send()
        .await
        .unwrap();
    assert_eq!(leave.status(), reqwest::StatusCode::SEE_OTHER);
    let loc = leave
        .headers()
        .get(reqwest::header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(loc, "/~/https://reddit.com/r/mixed");
}

#[tokio::test]
async fn post_ui_parse_query_redirects_to_subreddit() {
    let (addr, _tmp, _session_cookie) = start_test_server().await;
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

#[tokio::test]
async fn anonymous_record_vote_redirects_to_login_with_return_to() {
    let (addr, _tmp, _session_cookie) = start_test_server().await;
    let rpc = serde_json::json!({
        "action": "record_vote",
        "a": "alpha",
        "b": "beta",
        "ratio_left": 2,
        "ratio_right": 1,
        "scope": "https://reddit.com/r/rust",
        "vote_compare": true,
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

    assert!(
        body.contains("/login?return_to="),
        "anonymous vote should send browser to login with return_to, got: {body}"
    );
    assert!(
        body.contains("encodeURIComponent(window.location.pathname+window.location.search)"),
        "return_to must capture the current pair URL path+query, got: {body}"
    );
}

#[tokio::test]
async fn login_page_preserves_pair_return_to_in_oauth_link() {
    let (addr, _tmp, _session_cookie) = start_test_server().await;
    let pair_return = "/vote?parent=https%3A%2F%2Freddit.com%2Fr%2Frust&left=https%3A%2F%2Freddit.com%2Fr%2Frust%2Fcomments%2Faaa&right=https%3A%2F%2Freddit.com%2Fr%2Frust%2Fcomments%2Fbbb";
    let enc = urlencoding::encode(pair_return);

    // OAuth provider buttons only render when credentials are configured.
    std::env::set_var("GITHUB_CLIENT_ID", "test-client");
    std::env::set_var("GITHUB_CLIENT_SECRET", "test-secret");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let html = client
        .get(format!("http://{addr}/login?return_to={enc}"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(
        html.contains(&format!("/auth/github?return_to={enc}")),
        "login page should pass pair return_to into GitHub OAuth start, got: {html}"
    );
}

#[tokio::test]
async fn claim_pseudonym_redirects_to_pair_return_to() {
    let tmp = TempDir::new().unwrap();
    let data = tmp.path().to_string_lossy().into_owned();
    let cfg = AppConfig {
        data_dir: data.clone(),
        event_log_path: format!("{data}/events.jsonl"),
        views_log_path: format!("{data}/views.jsonl"),
        port: 0,
    };
    let state = create_app_state(cfg).await;
    let session_id = state
        .create_session(sorter2_server::identity::DEFAULT_ACTOR_UUID, "")
        .unwrap();
    let session_cookie = format!("{SESSION_COOKIE}={session_id}");
    let app: Router = create_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let pair_return = "/vote?parent=https%3A%2F%2Freddit.com%2Fr%2Frust&left=https%3A%2F%2Freddit.com%2Fr%2Frust%2Fcomments%2Faaa&right=https%3A%2F%2Freddit.com%2Fr%2Frust%2Fcomments%2Fbbb";
    let rpc = serde_json::json!({
        "action": "claim_pseudonym",
        "pseudonym": {"$form": "pseudonym"},
        "return_to": pair_return,
    })
    .to_string();
    let mut form = HashMap::new();
    form.insert(UI_RPC_FIELD.to_string(), rpc);
    form.insert("pseudonym".into(), "fresh-alias".into());

    let client = reqwest::Client::new();
    let body = client
        .post(format!("http://{addr}/ui"))
        .header("Cookie", &session_cookie)
        .form(&form)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let expected = serde_json::to_string(pair_return).unwrap();
    assert!(
        body.contains("window.location.href="),
        "claim should redirect, got: {body}"
    );
    assert!(
        body.contains(&expected),
        "claim redirect must send user back to the shared pair, got: {body}"
    );
}
