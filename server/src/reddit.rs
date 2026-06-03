//! Reddit API import via a single background worker (rate limits, dedup, backoff).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use reqwest::{header, Client, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::{
    entity_store::EntityStore, events::Event, fetch::now_ms, journal::JournalClient,
    path_types::ItemId, reducer::GlobalTree,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchJobResult {
    /// Number of entities written (1 for self, N for children).
    Imported(usize),
    NotFound,
    SkippedDuplicate,
    SkippedCached,
    RateLimited {
        reset_secs: u64,
    },
    Failed(String),
}

/// What to import for a node: the node's own entity, or its child listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchKind {
    SelfEntity,
    Children,
}

pub struct RedditCommand {
    pub id: ItemId,
    pub kind: FetchKind,
    /// User-initiated fetch bypasses the in-memory "recently fetched" cache.
    pub force: bool,
    pub done: Option<oneshot::Sender<FetchJobResult>>,
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
    /// Unauthenticated `.json` requests (public).
    pub api_base: String,
    /// `POST …/api/v1/access_token` (always www.reddit.com in production).
    pub oauth_token_base: String,
    /// Bearer-authenticated API (`GET` subreddit about, etc.).
    pub oauth_api_base: String,
    pub user_agent: String,
    creds: Option<RedditCredentials>,
}

struct OAuthToken {
    access_token: String,
    expires_at: Instant,
}

impl RedditBroker {
    pub fn spawn(journal: JournalClient, config: RedditApiConfig) -> Self {
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

        tracing::debug!(
            api_base = %config.api_base,
            oauth_token_base = %config.oauth_token_base,
            oauth_api_base = %config.oauth_api_base,
            oauth = config.creds.is_some(),
            "reddit worker started"
        );

        tokio::spawn(reddit_worker(rx, journal, client, config));

        Self { tx }
    }

    /// Queue a fetch; drops when the channel is full (backpressure).
    pub fn request_fetch(
        &self,
        id: ItemId,
        kind: FetchKind,
        force: bool,
        done: Option<oneshot::Sender<FetchJobResult>>,
    ) {
        match self.tx.try_send(RedditCommand {
            id: id.clone(),
            kind,
            force,
            done,
        }) {
            Ok(()) => tracing::debug!(item = %id, ?kind, force, "reddit fetch queued"),
            Err(_) => tracing::warn!(item = %id, "reddit fetch queue full, dropped"),
        }
    }
}

impl RedditApiConfig {
    pub fn from_env() -> Self {
        Self {
            api_base: reddit_api_base(),
            oauth_token_base: reddit_oauth_token_base(),
            oauth_api_base: reddit_oauth_api_base(),
            user_agent: default_user_agent(),
            creds: RedditCredentials::from_env(),
        }
    }
}

impl RedditCredentials {
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

/// Where to `POST /api/v1/access_token` (not the bearer API host).
pub fn reddit_oauth_token_base() -> String {
    std::env::var("REDDIT_OAUTH_TOKEN_BASE")
        .or_else(|_| std::env::var("REDDIT_OAUTH_BASE"))
        .unwrap_or_else(|_| "https://www.reddit.com".into())
}

/// Host for OAuth-authenticated API `GET`s (must not be www.reddit.com).
pub fn reddit_oauth_api_base() -> String {
    std::env::var("REDDIT_OAUTH_API_BASE").unwrap_or_else(|_| "https://oauth.reddit.com".into())
}

pub fn default_user_agent() -> String {
    std::env::var("REDDIT_USER_AGENT")
        .unwrap_or_else(|_| "web:sorter2.social:v0.0.1 (by /u/sorter2)".to_string())
}

/// Can the node's own entity be imported (subreddit `about` or a post)?
pub fn is_fetchable(id: &ItemId) -> bool {
    !map_item_to_reddit_api(id, "https://example.com").is_empty()
}

/// Can we import this node's children (currently: a subreddit's posts)?
pub fn is_children_fetchable(id: &ItemId) -> bool {
    !map_children_url(id, "https://example.com").is_empty()
}

pub fn entity_view_from_payload(
    id: &ItemId,
    payload: &Value,
) -> Option<crate::reducer::EntityData> {
    if id.as_str().starts_with("reddit.com") {
        return parse_reddit_view(id, payload);
    }
    None
}

pub fn apply_entity_import(
    tree: &mut GlobalTree,
    store: &EntityStore,
    id: &ItemId,
    payload: Value,
) -> Result<(), String> {
    let view = entity_view_from_payload(id, &payload);
    store.put(id, &payload).map_err(|e| e.to_string())?;
    tree.apply_entity(id, view);
    Ok(())
}

fn notify(done: Option<oneshot::Sender<FetchJobResult>>, result: FetchJobResult) {
    if let Some(tx) = done {
        let _ = tx.send(result);
    }
}

async fn reddit_worker(
    mut rx: mpsc::Receiver<RedditCommand>,
    journal: JournalClient,
    client: Client,
    config: RedditApiConfig,
) {
    let mut in_flight: HashSet<(ItemId, FetchKind)> = HashSet::new();
    let mut recently_fetched: HashMap<(ItemId, FetchKind), Instant> = HashMap::new();
    let mut current_delay = Duration::from_secs(1);
    let mut oauth: Option<OAuthToken> = None;
    let cache_ttl = Duration::from_secs(300);
    let creds = config.creds.clone();
    let api_base = config.api_base.clone();
    let oauth_token_base = config.oauth_token_base.clone();
    let oauth_api_base = config.oauth_api_base.clone();

    while let Some(cmd) = rx.recv().await {
        let now = Instant::now();
        recently_fetched.retain(|_, t| now.duration_since(*t) < cache_ttl);

        let kind = cmd.kind;
        let key = (cmd.id.clone(), kind);

        if in_flight.contains(&key) {
            tracing::debug!(item = %cmd.id, ?kind, "reddit fetch skipped: already in flight");
            notify(cmd.done, FetchJobResult::SkippedDuplicate);
            continue;
        }
        if !cmd.force && recently_fetched.contains_key(&key) {
            tracing::debug!(item = %cmd.id, ?kind, "reddit fetch skipped: recently fetched cache");
            notify(cmd.done, FetchJobResult::SkippedCached);
            continue;
        }

        in_flight.insert(key.clone());
        let fetch_id = cmd.id.clone();
        let done = cmd.done;

        tracing::debug!(
            item = %fetch_id,
            ?kind,
            delay_ms = current_delay.as_millis(),
            "reddit fetch starting after delay"
        );
        tokio::time::sleep(current_delay).await;

        if let Some(c) = &creds {
            oauth = ensure_oauth_token(&client, &oauth_token_base, c, oauth.take()).await;
        }

        let token = oauth.as_ref().map(|t| t.access_token.as_str());
        let fetch_base = if token.is_some() {
            tracing::debug!(
                item = %fetch_id,
                base = %oauth_api_base,
                "reddit fetch using OAuth bearer"
            );
            &oauth_api_base
        } else {
            &api_base
        };
        let url = match kind {
            FetchKind::SelfEntity => map_item_to_reddit_api(&fetch_id, fetch_base),
            FetchKind::Children => map_children_url(&fetch_id, fetch_base),
        };
        let outcome = do_fetch(&client, &url, &fetch_id, token).await;

        match outcome {
            Ok(FetchOutcome::Payload(payload)) => {
                let imports: Vec<(ItemId, Value)> = match kind {
                    FetchKind::SelfEntity => vec![(fetch_id.clone(), payload)],
                    FetchKind::Children => parse_children(&fetch_id, &payload),
                };
                tracing::debug!(
                    item = %fetch_id,
                    ?kind,
                    count = imports.len(),
                    "reddit fetch got payload, importing"
                );

                let mut write_err: Option<String> = None;
                let mut written = 0usize;
                for (child_id, child_payload) in imports {
                    let event = Event::EntityImported {
                        id: child_id.as_str().to_string(),
                        ts: now_ms(),
                        payload: child_payload,
                    };
                    if let Err(e) = journal.append(event).await {
                        tracing::warn!(item = %child_id, err = %e, "reddit import journal failed");
                        write_err = Some(e);
                        break;
                    }
                    written += 1;
                }

                match write_err {
                    Some(e) => notify(done, FetchJobResult::Failed(e)),
                    None => {
                        recently_fetched.insert(key.clone(), Instant::now());
                        current_delay = Duration::from_millis(600);
                        tracing::info!(item = %fetch_id, ?kind, written, "reddit import complete");
                        notify(done, FetchJobResult::Imported(written));
                    }
                }
            }
            Ok(FetchOutcome::NotFound) => {
                tracing::debug!(item = %fetch_id, "reddit fetch: not found (no event written)");
                recently_fetched.insert(key.clone(), Instant::now());
                notify(done, FetchJobResult::NotFound);
            }
            Ok(FetchOutcome::RateLimited { reset_secs }) => {
                tracing::warn!(
                    item = %fetch_id,
                    reset_secs,
                    "reddit rate limited"
                );
                tokio::time::sleep(Duration::from_secs(reset_secs.max(1))).await;
                current_delay = (current_delay * 2).min(Duration::from_secs(60));
                notify(done, FetchJobResult::RateLimited { reset_secs });
            }
            Err(e) => {
                tracing::warn!(item = %fetch_id, err = %e, "reddit fetch failed");
                current_delay = (current_delay * 2).min(Duration::from_secs(60));
                notify(done, FetchJobResult::Failed(e));
            }
        }

        in_flight.remove(&key);
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
            tracing::debug!("reddit OAuth token still valid");
            return Some(t);
        }
    }

    let url = format!("{}/api/v1/access_token", oauth_base.trim_end_matches('/'));
    tracing::debug!(%url, "reddit OAuth token request");

    let resp = client
        .post(&url)
        .basic_auth(&creds.client_id, Some(&creds.client_secret))
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("reddit OAuth token request failed: {e}");
            return None;
        }
    };

    if !resp.status().is_success() {
        tracing::warn!("reddit OAuth token HTTP {}", resp.status());
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
            tracing::warn!("reddit OAuth token parse failed: {e}");
            return None;
        }
    };

    tracing::debug!(expires_in = body.expires_in, "reddit OAuth token acquired");
    Some(OAuthToken {
        access_token: body.access_token,
        expires_at: Instant::now() + Duration::from_secs(body.expires_in),
    })
}

async fn do_fetch(
    client: &Client,
    url: &str,
    id: &ItemId,
    bearer: Option<&str>,
) -> Result<FetchOutcome, String> {
    if url.is_empty() {
        tracing::debug!(item = %id, "reddit do_fetch: no API URL for item");
        return Ok(FetchOutcome::NotFound);
    }

    tracing::debug!(item = %id, %url, bearer = bearer.is_some(), "reddit HTTP GET");

    let mut req = client.get(url);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }

    let resp = req.send().await.map_err(|e| {
        tracing::debug!(item = %id, %url, err = %e, "reddit HTTP transport error");
        e.to_string()
    })?;

    let status = resp.status();
    tracing::debug!(
        item = %id,
        %url,
        %status,
        remaining = ?rate_limit_remaining(&resp),
        reset = ?rate_limit_reset_secs(&resp),
        "reddit HTTP response"
    );

    if status == StatusCode::TOO_MANY_REQUESTS {
        let reset = rate_limit_reset_secs(&resp);
        return Ok(FetchOutcome::RateLimited { reset_secs: reset });
    }

    if status == StatusCode::SERVICE_UNAVAILABLE {
        return Err("Reddit unavailable (503)".into());
    }

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::debug!(
            item = %id,
            %status,
            body_len = body.len(),
            body_prefix = %body.chars().take(240).collect::<String>(),
            "reddit non-success body"
        );
        if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
            return Err(format!("Reddit API {status}: {body}"));
        }
        return Ok(FetchOutcome::NotFound);
    }

    if rate_limit_remaining(&resp) == Some(0) {
        let reset = rate_limit_reset_secs(&resp);
        tracing::debug!(item = %id, reset_secs = reset, "reddit headers: rate limit exhausted");
        return Ok(FetchOutcome::RateLimited { reset_secs: reset });
    }

    let text = resp.text().await.map_err(|e| e.to_string())?;
    tracing::debug!(item = %id, bytes = text.len(), "reddit response body received");

    let payload: Value = serde_json::from_str(&text).map_err(|e| {
        tracing::debug!(
            item = %id,
            err = %e,
            body_prefix = %text.chars().take(240).collect::<String>(),
            "reddit JSON parse failed"
        );
        format!("invalid JSON: {e}")
    })?;

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

/// Listing URL for a node's children. Currently only subreddits
/// (`reddit.com/r/<sub>` → `/r/<sub>.json`) expose a child listing.
pub fn map_children_url(id: &ItemId, api_base: &str) -> String {
    let path = id.as_str();
    if !path.starts_with("reddit.com/") {
        return String::new();
    }
    let base = api_base.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() == 3 && segments[1] == "r" {
        return format!("{base}/r/{}.json?raw_json=1&limit=25", segments[2]);
    }
    String::new()
}

/// Parse a subreddit listing payload into `(child_id, child_payload)` entries.
/// Each child id is the post's permalink under `reddit.com/…`, and the payload
/// is the raw `{kind, data}` listing element (persisted per child).
fn parse_children(_parent: &ItemId, payload: &Value) -> Vec<(ItemId, Value)> {
    let mut out = Vec::new();
    let children = match payload.pointer("/data/children").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return out,
    };
    for child in children {
        let permalink = match child.pointer("/data/permalink").and_then(|p| p.as_str()) {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };
        let path = format!("reddit.com{}", permalink.trim_end_matches('/'));
        if let Some(id) = ItemId::from_storage(&path) {
            out.push((id, child.clone()));
        }
    }
    out
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
        image_url: None,
        link_url: None,
    })
}

fn parse_post_listing(v: &Value) -> Option<crate::reducer::EntityData> {
    // Two shapes: a comments-page array `[listing, comments]`, or a single
    // listing element `{kind, data}` (from a subreddit children import).
    let child = if let Some(arr) = v.as_array() {
        arr.first()?.pointer("/data/children/0/data")?
    } else {
        v.get("data")?
    };
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
    let image_url = reddit_post_image_url(child);
    let link_url = reddit_post_link_url(child);

    Some(crate::reducer::EntityData {
        title,
        author,
        body_html,
        thumb_url,
        image_url,
        link_url,
    })
}

fn reddit_post_link_url(data: &Value) -> Option<String> {
    for key in ["url_overridden_by_dest", "url"] {
        if let Some(u) = data.get(key).and_then(|v| v.as_str()) {
            if u.starts_with("http") {
                return Some(u.to_string());
            }
        }
    }
    None
}

/// Full-size still for post detail: direct image `url`, else Reddit preview source.
fn reddit_post_image_url(data: &Value) -> Option<String> {
    for key in ["url", "url_overridden_by_dest"] {
        if let Some(u) = data.get(key).and_then(|v| v.as_str()) {
            if reddit_direct_image_url(u) {
                return Some(u.to_string());
            }
        }
    }
    reddit_preview_source_url(data)
}

fn reddit_preview_source_url(data: &Value) -> Option<String> {
    data.pointer("/preview/images/0/source/url")
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("http"))
        .map(str::to_string)
}

fn reddit_direct_image_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    if u.contains("redgifs.com") {
        return false;
    }
    u.contains("i.redd.it")
        || u.contains("preview.redd.it")
        || u.contains("external-preview.redd.it")
        || u.ends_with(".jpg")
        || u.ends_with(".jpeg")
        || u.ends_with(".png")
        || u.ends_with(".gif")
        || u.ends_with(".webp")
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
            map_item_to_reddit_api(&id, "https://oauth.reddit.com"),
            "https://oauth.reddit.com/r/rust/about.json?raw_json=1"
        );
    }

    #[test]
    fn parse_subreddit_fixture() {
        let json = include_str!("../../test/fixtures/reddit/r_rust_about.json");
        let v: Value = serde_json::from_str(json).unwrap();
        let entity =
            entity_view_from_payload(&ItemId::parse("reddit.com/r/rust").unwrap(), &v).unwrap();
        assert_eq!(entity.title, "The Rust Programming Language");
    }

    #[test]
    fn parse_post_listing_extracts_thumb_and_full_preview() {
        let json = include_str!("../../test/fixtures/reddit/post_preview.json");
        let v: Value = serde_json::from_str(json).unwrap();
        let id = ItemId::parse("reddit.com/r/nsfw/comments/1tpy6a1/angel_eyes").unwrap();
        let entity = entity_view_from_payload(&id, &v).unwrap();
        assert_eq!(entity.title, "Angel Eyes");
        assert!(entity.thumb_url.as_ref().unwrap().contains("width=140"));
        assert!(entity.image_url.as_ref().unwrap().contains("auto=webp"));
        assert!(!entity.image_url.as_ref().unwrap().contains("redgifs"));
        assert_eq!(
            entity.link_url.as_deref(),
            Some("http://v3.redgifs.com/watch/impossibleprestigioushedgehog")
        );
    }
}
