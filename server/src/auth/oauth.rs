//! OAuth providers (GitHub + Reddit). Provider accounts only *link* to a UUID;
//! the UUID is the canonical identity. Which providers are linked is private.

use reqwest::Client;
use serde::Deserialize;

use crate::reddit::{
    default_user_agent, reddit_oauth_api_base, reddit_oauth_token_base,
};

// ── GitHub ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GitHubConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub oauth_base: String,
    pub api_base: String,
}

pub fn github_oauth_base() -> String {
    std::env::var("GITHUB_OAUTH_BASE").unwrap_or_else(|_| "https://github.com".into())
}

pub fn github_api_base() -> String {
    std::env::var("GITHUB_API_BASE").unwrap_or_else(|_| "https://api.github.com".into())
}

impl GitHubConfig {
    pub fn from_env(base_url: &str) -> Option<Self> {
        let client_id = std::env::var("GITHUB_CLIENT_ID").ok()?;
        let client_secret = std::env::var("GITHUB_CLIENT_SECRET").ok()?;
        if client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        let oauth_base = github_oauth_base();
        let base = base_url.trim_end_matches('/');
        Some(Self {
            client_id,
            client_secret,
            redirect_uri: format!("{base}/auth/github/callback"),
            oauth_base,
            api_base: github_api_base(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GitHubTokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHubUser {
    pub id: u64,
    pub login: String,
}

pub fn github_authorize_url(cfg: &GitHubConfig, state: &str, mock_user: Option<&str>) -> String {
    let mut url = format!(
        "{}/login/oauth/authorize?client_id={}&redirect_uri={}&scope=read:user&state={}",
        cfg.oauth_base.trim_end_matches('/'),
        urlencoding::encode(&cfg.client_id),
        urlencoding::encode(&cfg.redirect_uri),
        urlencoding::encode(state),
    );
    if let Some(user) = mock_user {
        url.push_str("&mock_user=");
        url.push_str(&urlencoding::encode(user));
    }
    url
}

pub async fn github_exchange_code(
    client: &Client,
    cfg: &GitHubConfig,
    code: &str,
) -> Result<String, String> {
    let resp = client
        .post(format!(
            "{}/login/oauth/access_token",
            cfg.oauth_base.trim_end_matches('/')
        ))
        .header("Accept", "application/json")
        .form(&[
            ("client_id", cfg.client_id.as_str()),
            ("client_secret", cfg.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", cfg.redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("github token request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("github token HTTP {}", resp.status()));
    }

    let body: GitHubTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("github token parse failed: {e}"))?;
    Ok(body.access_token)
}

pub async fn github_fetch_user(
    client: &Client,
    api_base: &str,
    access_token: &str,
) -> Result<GitHubUser, String> {
    let resp = client
        .get(format!("{}/user", api_base.trim_end_matches('/')))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "sorter2")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("github user request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("github user HTTP {}", resp.status()));
    }

    resp.json()
        .await
        .map_err(|e| format!("github user parse failed: {e}"))
}

pub fn github_provider_id(user: &GitHubUser) -> String {
    user.id.to_string()
}

// ── Reddit ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RedditConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    /// Host for `/api/v1/authorize` (www.reddit.com in production).
    pub authorize_base: String,
    /// Host for `POST /api/v1/access_token`.
    pub token_base: String,
    /// Host for bearer `GET /api/v1/me` (oauth.reddit.com).
    pub api_base: String,
    pub user_agent: String,
}

/// Authorize page base; defaults to the same host as token POSTs.
pub fn reddit_authorize_base() -> String {
    std::env::var("REDDIT_OAUTH_AUTHORIZE_BASE")
        .or_else(|_| std::env::var("REDDIT_OAUTH_BASE"))
        .unwrap_or_else(|_| "https://www.reddit.com".into())
}

impl RedditConfig {
    pub fn from_env(base_url: &str) -> Option<Self> {
        let client_id = std::env::var("REDDIT_CLIENT_ID")
            .or_else(|_| std::env::var("REDDIT_APP_ID"))
            .ok()?;
        let client_secret = std::env::var("REDDIT_CLIENT_SECRET")
            .or_else(|_| std::env::var("REDDIT_APP_SECRET"))
            .ok()?;
        if client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        let base = base_url.trim_end_matches('/');
        Some(Self {
            client_id,
            client_secret,
            redirect_uri: format!("{base}/auth/reddit/callback"),
            authorize_base: reddit_authorize_base(),
            token_base: reddit_oauth_token_base(),
            api_base: reddit_oauth_api_base(),
            user_agent: default_user_agent(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct RedditTokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct RedditUser {
    /// Stable id (`t2_…`); never use `name` as identity.
    pub id: String,
    pub name: String,
}

pub fn reddit_authorize_url(cfg: &RedditConfig, state: &str, mock_user: Option<&str>) -> String {
    let mut url = format!(
        "{}/api/v1/authorize?client_id={}&response_type=code&state={}&redirect_uri={}&duration=temporary&scope=identity",
        cfg.authorize_base.trim_end_matches('/'),
        urlencoding::encode(&cfg.client_id),
        urlencoding::encode(state),
        urlencoding::encode(&cfg.redirect_uri),
    );
    if let Some(user) = mock_user {
        url.push_str("&mock_user=");
        url.push_str(&urlencoding::encode(user));
    }
    url
}

pub async fn reddit_exchange_code(
    client: &Client,
    cfg: &RedditConfig,
    code: &str,
) -> Result<String, String> {
    let resp = client
        .post(format!(
            "{}/api/v1/access_token",
            cfg.token_base.trim_end_matches('/')
        ))
        .header("User-Agent", &cfg.user_agent)
        .basic_auth(&cfg.client_id, Some(&cfg.client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", cfg.redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("reddit token request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("reddit token HTTP {status}: {body}"));
    }

    let body: RedditTokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("reddit token parse failed: {e}"))?;
    Ok(body.access_token)
}

pub async fn reddit_fetch_user(
    client: &Client,
    cfg: &RedditConfig,
    access_token: &str,
) -> Result<RedditUser, String> {
    let resp = client
        .get(format!(
            "{}/api/v1/me",
            cfg.api_base.trim_end_matches('/')
        ))
        .header("User-Agent", &cfg.user_agent)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("reddit user request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("reddit user HTTP {}", resp.status()));
    }

    resp.json()
        .await
        .map_err(|e| format!("reddit user parse failed: {e}"))
}

pub fn reddit_provider_id(user: &RedditUser) -> String {
    user.id.clone()
}

// ── Shared helpers ──────────────────────────────────────────────────────────

pub fn validate_pseudonym(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("enter a name");
    }
    if trimmed.len() > 64 {
        return Err("too long");
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("letters, numbers, _ and - only");
    }
    Ok(trimmed.to_string())
}

pub fn sanitize_pseudonym(login: &str) -> String {
    validate_pseudonym(login).unwrap_or_else(|_| "user".to_string())
}

/// Display name for a provider key (`github` → `GitHub`). Never show provider ids.
pub fn provider_label(provider: &str) -> &'static str {
    match provider {
        "github" => "GitHub",
        "reddit" => "Reddit",
        _ => "OAuth",
    }
}
