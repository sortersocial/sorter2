//! Reddit API import via a single background worker (rate limits, dedup, backoff).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{header, Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};

use crate::{
    event_log::EventLog,
    events::Event,
    html::now_ms,
    path_types::ItemId,
    reducer::GlobalTree,
};

/// Bootstrap blank nodes along a URL path so breadcrumbs and voting work before fetch.
pub fn ensure_partial_tree(tree: &mut GlobalTree, id: &ItemId) {
    tree.ensure_path(id);
}

pub struct RedditCommand {
    pub id: ItemId,
    /// User-initiated fetch bypasses the in-memory "recently fetched" cache.
    pub force: bool,
}

#[derive(Clone)]
pub struct RedditBroker {
    tx: mpsc::Sender<RedditCommand>,
}

#[derive(Clone)]
struct RedditCredentials {
    client_id: String,
    client_secret: String,
}

#[derive(Clone)]
pub struct RedditApiConfig {
    pub api_base: String,
    pub oauth_base: String,
    pub user_agent: String,
    creds: Option<RedditCredentials>,
}

struct OAuthToken {
    access_token: String,
    expires_at: Instant,
}

impl RedditBroker {
    pub fn spawn(
        tree: Arc<RwLock<GlobalTree>>,
        event_log: Arc<EventLog>,
        config: RedditApiConfig,
    ) -> Self {
        let (tx, rx) = mpsc::channel(100);

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(&config.user_agent).expect("valid user agent"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");

        tokio::spawn(reddit_worker(rx, tree, event_log, client, config));

        Self { tx }
    }

    /// Queue a fetch; drops when the channel is full (backpressure).
    pub fn request_fetch(&self, id: ItemId, force: bool) {
        let _ = self.tx.try_send(RedditCommand { id, force });
    }
}

impl RedditApiConfig {
    pub fn from_env() -> Self {
        Self {
            api_base: reddit_api_base(),
            oauth_base: reddit_oauth_base(),
            user_agent: default_user_agent(),
            creds: RedditCredentials::from_env(),
        }
    }
}

impl RedditCredentials {
    /// Reddit's OAuth docs call these "client id" and "client secret"; the app
    /// registration UI often labels them "app id" / "app secret" — same values.
    fn from_env() -> Option<Self> {
        let client_id = std::env::var("REDDIT_CLIENT_ID")
            .or_else(|_| std::env::var("REDDIT_APP_ID"))
            .ok()?;
        let client_secret = std::env::var("REDDIT_CLIENT_SECRET")
            .or_else(|_| std::env::var("REDDIT_APP_SECRET"))
            .ok()?;
        if client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        Some(Self {
            client_id,
            client_secret,
        })
    }
}

pub fn reddit_api_base() -> String {
    std::env::var("REDDIT_API_BASE").unwrap_or_else(|_| "https://www.reddit.com".into())
}

pub fn reddit_oauth_base() -> String {
    std::env::var("REDDIT_OAUTH_BASE").unwrap_or_else(|_| "https://www.reddit.com".into())
}

pub fn default_user_agent() -> String {
    std::env::var("REDDIT_USER_AGENT").unwrap_or_else(|_| {
        "web:sorter2.social:v0.0.1 (by /u/sorter2)".to_string()
    })
}

/// True when this node can be loaded from the Reddit JSON API.
pub fn is_fetchable(id: &ItemId) -> bool {
    !map_item_to_reddit_api(id, "https://example.com").is_empty()
}

/// Derive UI-facing fields from a stored payload (Reddit-specific when under reddit.com).
pub fn entity_view_from_payload(id: &ItemId, payload: &Value) -> Option<crate::reducer::EntityData> {
    if id.as_str().starts_with("reddit.com") {
        return parse_reddit_view(id, payload);
    }
    None
}

/// Apply a full API payload to the in-memory tree (view derived for known domains).
pub fn apply_entity_import(tree: &mut GlobalTree, id: &ItemId, payload: Value) {
    let view = entity_view_from_payload(id, &payload);
    tree.apply_entity_raw(id, payload, view);
}

async fn reddit_worker(
    mut rx: mpsc::Receiver<RedditCommand>,
    tree: Arc<RwLock<GlobalTree>>,
    event_log: Arc<EventLog>,
    client: Client,
    config: RedditApiConfig,
) {
    let mut in_flight = HashSet::new();
    let mut recently_fetched: HashMap<ItemId, Instant> = HashMap::new();
    let mut current_delay = Duration::from_secs(1);
    let mut oauth: Option<OAuthToken> = None;
    let cache_ttl = Duration::from_secs(300);
    let creds = config.creds.clone();
    let api_base = config.api_base.clone();
    let oauth_base = config.oauth_base.clone();

    while let Some(cmd) = rx.recv().await {
        let now = Instant::now();
        recently_fetched.retain(|_, t| now.duration_since(*t) < cache_ttl);

        if in_flight.contains(&cmd.id) {
            continue;
        }
        if !cmd.force && recently_fetched.contains_key(&cmd.id) {
            continue;
        }

        in_flight.insert(cmd.id.clone());
        let fetch_id = cmd.id.clone();

        tokio::time::sleep(current_delay).await;

        if let Some(c) = &creds {
            oauth = ensure_oauth_token(&client, &oauth_base, c, oauth.take()).await;
        }

        let token = oauth.as_ref().map(|t| t.access_token.as_str());

        match do_fetch(&client, &api_base, &fetch_id, token).await {
            Ok(FetchOutcome::Payload(payload)) => {
                let ts = now_ms();
                let event = Event::EntityImported {
                    id: fetch_id.as_str().to_string(),
                    ts,
                    payload: payload.clone(),
                };
                if let Err(e) = event_log.append(&event).await {
                    tracing::warn!("event log append failed for {}: {}", fetch_id, e);
                } else {
                    apply_entity_import(&mut *tree.write().await, &fetch_id, payload);
                    recently_fetched.insert(fetch_id.clone(), Instant::now());
                    current_delay = Duration::from_millis(600);
                }
            }
            Ok(FetchOutcome::NotFound) => {
                recently_fetched.insert(fetch_id.clone(), Instant::now());
            }
            Ok(FetchOutcome::RateLimited { reset_secs }) => {
                let wait = Duration::from_secs(reset_secs.max(1));
                tracing::warn!(
                    "Reddit rate limit for {}; sleeping {}s",
                    fetch_id,
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
                current_delay = (current_delay * 2).min(Duration::from_secs(60));
            }
            Err(e) => {
                tracing::warn!("Reddit fetch failed for {}: {}", fetch_id, e);
                current_delay = (current_delay * 2).min(Duration::from_secs(60));
            }
        }

        in_flight.remove(&fetch_id);
    }
}

enum FetchOutcome {
    Payload(Value),
    NotFound,
    RateLimited { reset_secs: u64 },
}

async fn ensure_oauth_token(
    client: &Client,
    oauth_base: &str,
    creds: &RedditCredentials,
    existing: Option<OAuthToken>,
) -> Option<OAuthToken> {
    if let Some(t) = existing {
        if Instant::now() < t.expires_at - Duration::from_secs(60) {
            return Some(t);
        }
    }

    let url = format!(
        "{}/api/v1/access_token",
        oauth_base.trim_end_matches('/')
    );

    let resp = client
        .post(&url)
        .basic_auth(&creds.client_id, Some(&creds.client_secret))
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Reddit OAuth token request failed: {e}");
            return None;
        }
    };

    if !resp.status().is_success() {
        tracing::warn!("Reddit OAuth token HTTP {}", resp.status());
        return None;
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        expires_in: u64,
    }

    let body: TokenResponse = match resp.json().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("Reddit OAuth token parse failed: {e}");
            return None;
        }
    };

    Some(OAuthToken {
        access_token: body.access_token,
        expires_at: Instant::now() + Duration::from_secs(body.expires_in),
    })
}

async fn do_fetch(
    client: &Client,
    api_base: &str,
    id: &ItemId,
    bearer: Option<&str>,
) -> Result<FetchOutcome, String> {
    let url = map_item_to_reddit_api(id, api_base);
    if url.is_empty() {
        return Ok(FetchOutcome::NotFound);
    }

    let mut req = client.get(&url);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;

    if resp.status() == StatusCode::TOO_MANY_REQUESTS {
        let reset = rate_limit_reset_secs(&resp);
        return Ok(FetchOutcome::RateLimited { reset_secs: reset });
    }

    if resp.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err("Reddit unavailable (503)".to_string());
    }

    if !resp.status().is_success() {
        return Ok(FetchOutcome::NotFound);
    }

    if rate_limit_remaining(&resp) == Some(0) {
        let reset = rate_limit_reset_secs(&resp);
        return Ok(FetchOutcome::RateLimited { reset_secs: reset });
    }

    let payload: Value = resp.json().await.map_err(|e| e.to_string())?;
    Ok(FetchOutcome::Payload(payload))
}

fn rate_limit_remaining(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f.floor() as u64)
}

fn rate_limit_reset_secs(resp: &reqwest::Response) -> u64 {
    resp.headers()
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f.ceil() as u64)
        .unwrap_or(5)
}

/// Map canonical item id to a Reddit JSON API URL under `api_base`.
pub fn map_item_to_reddit_api(id: &ItemId, api_base: &str) -> String {
    let path = id.as_str();
    if !path.starts_with("reddit.com/") && path != "reddit.com" {
        return String::new();
    }

    let base = api_base.trim_end_matches('/');

    let segments: Vec<&str> = path.split('/').collect();

    if let Some(i) = segments.iter().position(|&p| p == "comments") {
        if segments.len() > i + 1 {
            let api_path = segments[1..=i + 1].join("/");
            return format!("{base}/{api_path}.json?raw_json=1");
        }
    }

    if segments.len() == 3 && segments[1] == "r" {
        return format!("{base}/r/{}/about.json?raw_json=1", segments[2]);
    }

    String::new()
}

fn parse_reddit_view(id: &ItemId, v: &Value) -> Option<crate::reducer::EntityData> {
    let segments: Vec<&str> = id.as_str().split('/').collect();

    if segments.iter().any(|&p| p == "comments") {
        parse_post_listing(v)
    } else {
        parse_subreddit_about(v)
    }
}

fn parse_subreddit_about(v: &Value) -> Option<crate::reducer::EntityData> {
    let data = v.get("data")?;
    let title = data
        .get("title")
        .or_else(|| data.get("display_name"))
        .and_then(|t| t.as_str())?
        .to_string();
    let body_html = data
        .get("public_description_html")
        .or_else(|| data.get("public_description"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let thumb_url = data
        .get("icon_img")
        .or_else(|| data.get("community_icon"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    Some(crate::reducer::EntityData {
        title,
        author: None,
        body_html,
        thumb_url,
    })
}

fn parse_post_listing(v: &Value) -> Option<crate::reducer::EntityData> {
    let listing = v.as_array()?.first()?;
    let child = listing.pointer("/data/children/0/data")?;
    let title = child.get("title")?.as_str()?.to_string();
    let author = child
        .get("author")
        .and_then(|a| a.as_str())
        .filter(|a| *a != "[deleted]")
        .map(|s| s.to_string());
    let body_html = child
        .get("selftext_html")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let thumb_url = child
        .get("thumbnail")
        .and_then(|t| t.as_str())
        .filter(|s| s.starts_with("http"))
        .map(|s| s.to_string());

    Some(crate::reducer::EntityData {
        title,
        author,
        body_html,
        thumb_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_subreddit_about_url() {
        let id = ItemId::parse("reddit.com/r/rust").unwrap();
        assert_eq!(
            map_item_to_reddit_api(&id, "https://www.reddit.com"),
            "https://www.reddit.com/r/rust/about.json?raw_json=1"
        );
        assert_eq!(
            map_item_to_reddit_api(&id, "http://127.0.0.1:9999"),
            "http://127.0.0.1:9999/r/rust/about.json?raw_json=1"
        );
    }

    #[test]
    fn map_post_url() {
        let id = ItemId::parse("reddit.com/r/amitheasshole/comments/1trnvdl").unwrap();
        assert_eq!(
            map_item_to_reddit_api(&id, "https://www.reddit.com"),
            "https://www.reddit.com/r/amitheasshole/comments/1trnvdl.json?raw_json=1"
        );
    }

    #[test]
    fn is_fetchable_reddit_sub() {
        let id = ItemId::parse("reddit.com/r/rust").unwrap();
        assert!(is_fetchable(&id));
        assert!(!is_fetchable(&ItemId::opaque("example.com/x")));
    }

    #[test]
    fn parse_subreddit_fixture() {
        let json = include_str!("../../test/fixtures/reddit/r_rust_about.json");
        let v: Value = serde_json::from_str(json).unwrap();
        let entity = entity_view_from_payload(
            &ItemId::parse("reddit.com/r/rust").unwrap(),
            &v,
        )
        .unwrap();
        assert_eq!(entity.title, "The Rust Programming Language");
        assert!(entity.body_html.as_ref().is_some_and(|b| b.contains("Rust")));
    }

    #[test]
    fn parse_post_fixture() {
        let json = r#"[{"kind":"Listing","data":{"children":[{"kind":"t3","data":{"title":"AITA","author":"op","selftext_html":"&lt;p&gt;hi&lt;/p&gt;","thumbnail":"https://b.thumbs.redditmedia.com/x.jpg"}}]}}]"#;
        let v: Value = serde_json::from_str(json).unwrap();
        let entity = entity_view_from_payload(
            &ItemId::parse("reddit.com/r/x/comments/abc").unwrap(),
            &v,
        )
        .unwrap();
        assert_eq!(entity.title, "AITA");
        assert_eq!(entity.author.as_deref(), Some("op"));
    }
}
