use governor::{Quota, RateLimiter, Jitter};
use nonzero_ext::nonzero;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use anyhow::{Result, Context, anyhow};

/// Reddit API client with built-in rate limiting
pub struct RedditClient {
    client: Client,
    limiter: Arc<governor::DefaultDirectRateLimiter>,
    user_agent: String,
}

impl RedditClient {
    /// Create a new Reddit client with rate limiting
    /// Reddit API allows 60 requests per minute for OAuth2 authenticated apps
    /// We'll be conservative and use 50 requests per minute
    pub fn new() -> Self {
        // Create rate limiter: 50 requests per minute
        let quota = Quota::per_minute(nonzero!(50u32));
        let limiter = Arc::new(RateLimiter::direct(quota));
        
        // Create HTTP client with timeout
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");
        
        // Reddit requires a unique user agent
        let user_agent = format!(
            "rust:sorter:v{} (by /u/hourLong_arnould)",
            env!("CARGO_PKG_VERSION")
        );
        
        Self {
            client,
            limiter,
            user_agent,
        }
    }
    
    /// Fetch hot posts from a subreddit
    pub async fn get_subreddit_hot(&self, subreddit: &str, limit: usize) -> Result<RedditListingResponse> {
        // Wait for rate limiter
        self.limiter.until_ready_with_jitter(Jitter::up_to(Duration::from_millis(100))).await;
        
        let url = format!("https://www.reddit.com/r/{}/hot.json?limit={}", subreddit, limit);
        
        let response = self.client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .context("Failed to send request to Reddit")?;
        
        self.handle_response(response).await
    }
    
    /// Fetch top posts from a subreddit
    pub async fn get_subreddit_top(&self, subreddit: &str, limit: usize, time_period: &str) -> Result<RedditListingResponse> {
        self.limiter.until_ready_with_jitter(Jitter::up_to(Duration::from_millis(100))).await;
        
        let url = format!(
            "https://www.reddit.com/r/{}/top.json?limit={}&t={}",
            subreddit, limit, time_period
        );
        
        let response = self.client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .context("Failed to send request to Reddit")?;
        
        self.handle_response(response).await
    }
    
    /// Fetch new posts from a subreddit
    pub async fn get_subreddit_new(&self, subreddit: &str, limit: usize) -> Result<RedditListingResponse> {
        self.limiter.until_ready_with_jitter(Jitter::up_to(Duration::from_millis(100))).await;
        
        let url = format!("https://www.reddit.com/r/{}/new.json?limit={}", subreddit, limit);
        
        let response = self.client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .context("Failed to send request to Reddit")?;
        
        self.handle_response(response).await
    }
    
    /// Fetch a specific post and its comments
    pub async fn get_post(&self, subreddit: &str, post_id: &str) -> Result<Vec<RedditListingResponse>> {
        self.limiter.until_ready_with_jitter(Jitter::up_to(Duration::from_millis(100))).await;
        
        let url = format!(
            "https://www.reddit.com/r/{}/comments/{}.json",
            subreddit, post_id
        );
        
        let response = self.client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .context("Failed to send request to Reddit")?;
        
        match response.status() {
            StatusCode::OK => {
                let listings: Vec<RedditListingResponse> = response
                    .json()
                    .await
                    .context("Failed to parse Reddit response")?;
                Ok(listings)
            }
            StatusCode::TOO_MANY_REQUESTS => {
                Err(anyhow!("Reddit rate limit exceeded. Please try again later."))
            }
            StatusCode::NOT_FOUND => {
                Err(anyhow!("Post not found: r/{}/comments/{}", subreddit, post_id))
            }
            status => {
                Err(anyhow!("Reddit API error: {}", status))
            }
        }
    }
    
    /// Fetch user profile (posts and comments)
    pub async fn get_user_profile(&self, username: &str, content_type: &str, limit: usize) -> Result<RedditListingResponse> {
        self.limiter.until_ready_with_jitter(Jitter::up_to(Duration::from_millis(100))).await;
        
        let url = format!(
            "https://www.reddit.com/user/{}/{}.json?limit={}",
            username, content_type, limit
        );
        
        let response = self.client
            .get(&url)
            .header("User-Agent", &self.user_agent)
            .send()
            .await
            .context("Failed to send request to Reddit")?;
        
        self.handle_response(response).await
    }
    
    /// Handle Reddit API response with proper error checking
    async fn handle_response(&self, response: reqwest::Response) -> Result<RedditListingResponse> {
        match response.status() {
            StatusCode::OK => {
                let listing: RedditListingResponse = response
                    .json()
                    .await
                    .context("Failed to parse Reddit response")?;
                Ok(listing)
            }
            StatusCode::TOO_MANY_REQUESTS => {
                Err(anyhow!("Reddit rate limit exceeded. Please try again later."))
            }
            StatusCode::NOT_FOUND => {
                Err(anyhow!("Reddit resource not found"))
            }
            StatusCode::FORBIDDEN => {
                Err(anyhow!("Access forbidden. The subreddit may be private."))
            }
            status => {
                Err(anyhow!("Reddit API error: {}", status))
            }
        }
    }
}

// Reddit API response types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditListingResponse {
    pub kind: String,
    pub data: RedditListingData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditListingData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    pub children: Vec<RedditThing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modhash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditThing {
    pub kind: String,
    pub data: RedditThingData,
}

/// Reddit's "Thing" data can be either a post, comment, or other types
/// We use untagged enum to handle different data structures
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RedditThingData {
    Post(RedditPost),
    Comment(RedditComment),
    // For things we don't care about yet (like "more" comments)
    Other(serde_json::Value),
}

/// Reddit post data with serde handling all the parsing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditPost {
    pub id: String,
    pub name: String, // Full name (t3_xxx)
    #[serde(default = "default_untitled")]
    pub title: String,
    #[serde(default = "default_deleted_user")]
    pub author: String,
    pub subreddit: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub permalink: String,
    #[serde(default, deserialize_with = "deserialize_selftext")]
    pub selftext: Option<String>,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub num_comments: i64,
    #[serde(default)]
    pub created_utc: f64,
    #[serde(default, deserialize_with = "deserialize_thumbnail")]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub is_video: bool,
    #[serde(default)]
    pub is_self: bool,
    // Additional useful fields
    #[serde(default)]
    pub ups: i64,
    #[serde(default)]
    pub downs: i64,
    #[serde(default)]
    pub upvote_ratio: f64,
    #[serde(default)]
    pub over_18: bool,
    #[serde(default)]
    pub spoiler: bool,
    #[serde(default)]
    pub stickied: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub distinguished: Option<String>,
}

/// Reddit comment data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedditComment {
    pub id: String,
    pub name: String, // Full name (t1_xxx)
    #[serde(default = "default_deleted_user")]
    pub author: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub score: i64,
    #[serde(default)]
    pub created_utc: f64,
    #[serde(default)]
    pub parent_id: String,
    #[serde(default)]
    pub permalink: String,
    #[serde(default)]
    pub depth: i32,
    // Additional useful fields
    #[serde(default)]
    pub ups: i64,
    #[serde(default)]
    pub downs: i64,
    #[serde(default)]
    pub edited: RedditEditStatus,
    #[serde(default)]
    pub stickied: bool,
    #[serde(default)]
    pub distinguished: Option<String>,
    #[serde(default)]
    pub is_submitter: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub controversiality: i32,
}

/// Reddit's edit status - can be false or a timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RedditEditStatus {
    NotEdited(bool),
    EditedAt(f64),
}

impl Default for RedditEditStatus {
    fn default() -> Self {
        RedditEditStatus::NotEdited(false)
    }
}

// Helper functions for serde defaults
fn default_untitled() -> String {
    "Untitled".to_string()
}

fn default_deleted_user() -> String {
    "[deleted]".to_string()
}

// Custom deserializer for selftext (empty strings should be None)
fn deserialize_selftext<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    Ok(s.filter(|text| !text.is_empty()))
}

// Custom deserializer for thumbnail (filter out "self", "default", empty)
fn deserialize_thumbnail<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    Ok(s.filter(|thumb| {
        !thumb.is_empty() && thumb != "self" && thumb != "default" && thumb != "nsfw" && thumb != "spoiler"
    }))
}

// Helper methods for extracting typed data from Things
impl RedditThing {
    /// Try to get this Thing as a Post
    pub fn as_post(&self) -> Option<&RedditPost> {
        if self.kind != "t3" {
            return None;
        }
        match &self.data {
            RedditThingData::Post(post) => Some(post),
            _ => None,
        }
    }
    
    /// Try to get this Thing as a Comment
    pub fn as_comment(&self) -> Option<&RedditComment> {
        if self.kind != "t1" {
            return None;
        }
        match &self.data {
            RedditThingData::Comment(comment) => Some(comment),
            _ => None,
        }
    }
    
    /// Check if this is a "more comments" indicator
    pub fn is_more_comments(&self) -> bool {
        self.kind == "more"
    }
}

// Helper methods for working with listing responses
impl RedditListingResponse {
    /// Extract all posts from this listing
    pub fn get_posts(&self) -> Vec<&RedditPost> {
        self.data.children.iter()
            .filter_map(|thing| thing.as_post())
            .collect()
    }
    
    /// Extract all comments from this listing
    pub fn get_comments(&self) -> Vec<&RedditComment> {
        self.data.children.iter()
            .filter_map(|thing| thing.as_comment())
            .collect()
    }
    
    /// Extract posts as owned values
    pub fn into_posts(self) -> Vec<RedditPost> {
        self.data.children.into_iter()
            .filter_map(|thing| {
                if thing.kind == "t3" {
                    match thing.data {
                        RedditThingData::Post(post) => Some(post),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect()
    }
    
    /// Extract comments as owned values
    pub fn into_comments(self) -> Vec<RedditComment> {
        self.data.children.into_iter()
            .filter_map(|thing| {
                if thing.kind == "t1" {
                    match thing.data {
                        RedditThingData::Comment(comment) => Some(comment),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect()
    }
}



