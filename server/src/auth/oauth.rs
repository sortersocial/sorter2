//! GitHub OAuth (raw reqwest, same style as reddit.rs).

use reqwest::Client;
use serde::Deserialize;

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
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct GitHubUser {
    pub id: u64,
    pub login: String,
}

pub fn authorize_url(cfg: &GitHubConfig, state: &str, mock_user: Option<&str>) -> String {
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

pub async fn exchange_code(
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

    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("github token parse failed: {e}"))?;
    Ok(body.access_token)
}

pub async fn fetch_user(
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

pub fn provider_id(user: &GitHubUser) -> String {
    user.id.to_string()
}

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
