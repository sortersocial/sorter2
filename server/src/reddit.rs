//! Reddit API import via a single background worker (rate limits, dedup, backoff).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::{header, Client, StatusCode};
use serde::Deserialize;
use tokio::sync::{mpsc, RwLock};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GlobalTree},
};

/// Bootstrap blank nodes along a URL path so breadcrumbs and voting work before fetch.
pub fn ensure_partial_tree(tree: &mut GlobalTree, id: &ItemId) {
    tree.ensure_path(id);
}

pub struct RedditCommand {
    pub id: ItemId,
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

struct OAuthToken {
    access_token: String,
    expires_at: Instant,
}

impl RedditBroker {
    pub fn spawn(tree: Arc<RwLock<GlobalTree>>, user_agent: &str) -> Self {
        let (tx, rx) = mpsc::channel(100);

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(user_agent).expect("valid user agent"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");

        let creds = RedditCredentials::from_env();
        tokio::spawn(reddit_worker(rx, tree, client, creds));

        Self { tx }
    }

    /// Fire-and-forget: queue a fetch; worker updates the tree when done.
    pub fn request_fetch(&self, id: ItemId) {
        let _ = self.tx.try_send(RedditCommand { id });
    }
}

impl RedditCredentials {
    fn from_env() -> Option<Self> {
        let client_id = std::env::var("REDDIT_CLIENT_ID").ok()?;
        let client_secret = std::env::var("REDDIT_CLIENT_SECRET").ok()?;
        if client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        Some(Self {
            client_id,
            client_secret,
        })
    }
}

pub fn default_user_agent() -> String {
    std::env::var("REDDIT_USER_AGENT").unwrap_or_else(|_| {
        "web:sorter2.social:v0.0.1 (by /u/sorter2)".to_string()
    })
}

async fn reddit_worker(
    mut rx: mpsc::Receiver<RedditCommand>,
    tree: Arc<RwLock<GlobalTree>>,
    client: Client,
    creds: Option<RedditCredentials>,
) {
    let mut in_flight = HashSet::new();
    let mut recently_fetched: HashMap<ItemId, Instant> = HashMap::new();
    let mut current_delay = Duration::from_secs(1);
    let mut oauth: Option<OAuthToken> = None;
    let cache_ttl = Duration::from_secs(300);

    while let Some(cmd) = rx.recv().await {
        let now = Instant::now();
        recently_fetched.retain(|_, t| now.duration_since(*t) < cache_ttl);

        if in_flight.contains(&cmd.id) || recently_fetched.contains_key(&cmd.id) {
            continue;
        }

        in_flight.insert(cmd.id.clone());
        let fetch_id = cmd.id.clone();

        tokio::time::sleep(current_delay).await;

        if let Some(c) = &creds {
            oauth = ensure_oauth_token(&client, c, oauth.take()).await;
        }

        let token = oauth.as_ref().map(|t| t.access_token.as_str());
        let use_oauth = token.is_some();

        match do_fetch(&client, &fetch_id, use_oauth, token).await {
            Ok(FetchOutcome::Entity(data)) => {
                let mut w = tree.write().await;
                w.set_entity_data(&fetch_id, data);
                recently_fetched.insert(fetch_id.clone(), Instant::now());
                current_delay = Duration::from_millis(600);
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
    Entity(EntityData),
    NotFound,
    RateLimited { reset_secs: u64 },
}

async fn ensure_oauth_token(
    client: &Client,
    creds: &RedditCredentials,
    existing: Option<OAuthToken>,
) -> Option<OAuthToken> {
    if let Some(t) = existing {
        if Instant::now() < t.expires_at - Duration::from_secs(60) {
            return Some(t);
        }
    }

    let resp = client
        .post("https://www.reddit.com/api/v1/access_token")
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
    id: &ItemId,
    use_oauth: bool,
    bearer: Option<&str>,
) -> Result<FetchOutcome, String> {
    let url = map_item_to_reddit_api(id, use_oauth);
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

    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(parse_reddit_json(id, &bytes)
        .map(FetchOutcome::Entity)
        .unwrap_or(FetchOutcome::NotFound))
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

/// Map canonical item id to Reddit JSON API URL.
pub fn map_item_to_reddit_api(id: &ItemId, oauth: bool) -> String {
    let path = id.as_str();
    if !path.starts_with("reddit.com/") && path != "reddit.com" {
        return String::new();
    }

    let base = if oauth {
        "https://oauth.reddit.com"
    } else {
        "https://www.reddit.com"
    };

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

fn parse_reddit_json(id: &ItemId, bytes: &[u8]) -> Option<EntityData> {
    let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let segments: Vec<&str> = id.as_str().split('/').collect();

    if segments.iter().any(|&p| p == "comments") {
        parse_post_listing(&v)
    } else {
        parse_subreddit_about(&v)
    }
}

fn parse_subreddit_about(v: &serde_json::Value) -> Option<EntityData> {
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

    Some(EntityData {
        title,
        author: None,
        body_html,
        thumb_url,
    })
}

fn parse_post_listing(v: &serde_json::Value) -> Option<EntityData> {
    let listing = v.as_array()?.first()?;
    let child = listing
        .pointer("/data/children/0/data")?;
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

    Some(EntityData {
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
            map_item_to_reddit_api(&id, false),
            "https://www.reddit.com/r/rust/about.json?raw_json=1"
        );
        assert_eq!(
            map_item_to_reddit_api(&id, true),
            "https://oauth.reddit.com/r/rust/about.json?raw_json=1"
        );
    }

    #[test]
    fn map_post_url() {
        let id =
            ItemId::parse("reddit.com/r/amitheasshole/comments/1trnvdl").unwrap();
        assert_eq!(
            map_item_to_reddit_api(&id, false),
            "https://www.reddit.com/r/amitheasshole/comments/1trnvdl.json?raw_json=1"
        );
    }

    #[test]
    fn map_non_reddit_empty() {
        let id = ItemId::opaque("example.com/foo");
        assert!(map_item_to_reddit_api(&id, false).is_empty());
    }

    #[test]
    fn parse_subreddit_fixture() {
        let json = r#"{"kind":"t5","data":{"title":"Rust","display_name":"rust","public_description":"systems"}}"#;
        let entity = parse_reddit_json(
            &ItemId::parse("reddit.com/r/rust").unwrap(),
            json.as_bytes(),
        )
        .unwrap();
        assert_eq!(entity.title, "Rust");
    }

    #[test]
    fn parse_post_fixture() {
        let json = r#"[{"kind":"Listing","data":{"children":[{"kind":"t3","data":{"title":"AITA","author":"op","selftext_html":"&lt;p&gt;hi&lt;/p&gt;","thumbnail":"https://b.thumbs.redditmedia.com/x.jpg"}}]}}]"#;
        let entity = parse_reddit_json(
            &ItemId::parse("reddit.com/r/x/comments/abc").unwrap(),
            json.as_bytes(),
        )
        .unwrap();
        assert_eq!(entity.title, "AITA");
        assert_eq!(entity.author.as_deref(), Some("op"));
    }
}
