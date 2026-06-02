use serde::{Deserialize, Serialize};
use std::fmt;

/// Canonical hierarchical identity for any URL/path in the fractal tree.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct ItemId(String);

impl ItemId {
    /// Parse an already-canonical path (no URL normalization). Empty string is invalid here;
    /// use [`Self::root`] for the tree root.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        Some(Self(t.to_string()))
    }

    /// Build an opaque item key (legacy demo votes, non-URL items).
    pub fn opaque(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Root of the internet tree (empty path).
    pub fn root() -> Self {
        Self(String::new())
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Creates a canonical ID from a raw URL or path. Normalizes domains and
    /// trims tracking query params.
    pub fn from_url(raw_url: &str) -> Option<Self> {
        Self::canonicalize(raw_url).map(Self)
    }

    /// Normalize strings from forms, events, and Reddit imports into the same
    /// stored id shape (e.g. drop post title slug after comment id).
    pub fn from_storage(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        if t.contains("://") || t.starts_with("r/") {
            return Self::from_url(t).or_else(|| Self::parse(t));
        }
        if t.starts_with("reddit.com/") && t.contains("/comments/") {
            return Self::from_url(t).or_else(|| Self::parse(t));
        }
        Self::parse(t).or_else(|| Self::from_url(t))
    }

    /// Map legacy scope keys (`""`, `"rust"`) to fractal parent nodes.
    pub fn from_legacy_scope(raw: &str) -> Self {
        let s = raw.trim();
        if s.is_empty() {
            return Self::root();
        }
        Self(format!("reddit.com/r/{s}"))
    }

    /// Extract the parent, e.g. `reddit.com/r/aww/comments/1trnvdl` →
    /// `reddit.com/r/aww`.
    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }

        let parts: Vec<&str> = self.0.trim_end_matches('/').split('/').collect();
        if parts.len() <= 1 {
            return None;
        }

        if self.0.contains("/comments/") {
            return Some(Self(parts[..parts.len().saturating_sub(2)].join("/")));
        }

        Some(Self(parts[..parts.len() - 1].join("/")))
    }

    pub fn segments(&self) -> Vec<&str> {
        self.0.split('/').filter(|s| !s.is_empty()).collect()
    }

    /// Cumulative paths for breadcrumb rendering, e.g.
    /// `reddit.com/r/movies` → `["reddit.com", "reddit.com/r", "reddit.com/r/movies"]`.
    pub fn breadcrumb_paths(&self) -> Vec<ItemId> {
        let segs = self.segments();
        let mut paths = Vec::with_capacity(segs.len());
        let mut current = String::new();
        for seg in segs {
            if current.is_empty() {
                current = seg.to_string();
            } else {
                current.push('/');
                current.push_str(seg);
            }
            paths.push(ItemId(current.clone()));
        }
        paths
    }

    /// Full URL for the browser location bar after `/~/`.
    pub fn to_browse_url(&self) -> String {
        if self.is_root() {
            return String::new();
        }
        if self.as_str().contains("://") {
            return self.as_str().to_string();
        }
        if self.segments().first().is_some_and(|s| s.contains('.')) {
            format!("https://{}", self.as_str())
        } else {
            self.as_str().to_string()
        }
    }

    /// App route, e.g. `/~/https://reddit.com/r/rust`.
    pub fn browse_href(&self) -> String {
        if self.is_root() {
            "/".to_string()
        } else {
            format!("/~/{}", self.to_browse_url())
        }
    }

    /// Parse the tail after `/~/` in a request path.
    pub fn from_browse_tail(tail: &str) -> ItemId {
        let raw = normalize_browse_tail(tail);
        if raw.is_empty() {
            return ItemId::root();
        }
        ItemId::from_url(&raw)
            .or_else(|| ItemId::parse(&raw))
            .unwrap_or_else(|| ItemId::opaque(raw))
    }

    pub fn from_browse_uri(path: &str) -> Option<ItemId> {
        path.strip_prefix("/~/").map(ItemId::from_browse_tail)
    }

    fn canonicalize(raw: &str) -> Option<String> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }

        let owned = if let Some(rest) = s.strip_prefix("r/") {
            format!("reddit.com/r/{rest}")
        } else if let Some(rest) = s.strip_prefix("/r/") {
            format!("reddit.com/r/{rest}")
        } else {
            s.to_string()
        };

        let (host_path, _query) = split_query(&owned);
        let host_path = host_path.trim_end_matches('/');

        let path = if host_path.contains("://") {
            parse_url_host_path(host_path)?
        } else if host_path.starts_with("reddit.com") || host_path.starts_with("www.reddit.com") {
            normalize_reddit_host_path(host_path)
        } else if host_path.contains('/') {
            host_path.to_string()
        } else {
            return None;
        };

        Some(normalize_reddit_path(&path))
    }
}

fn split_query(s: &str) -> (&str, Option<&str>) {
    if let Some((path, q)) = s.split_once('?') {
        (path, Some(q))
    } else {
        (s, None)
    }
}

fn parse_url_host_path(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = normalize_host(host);
    if path.is_empty() {
        Some(host)
    } else {
        Some(format!("{host}/{path}"))
    }
}

fn normalize_host(host: &str) -> String {
    let h = host
        .strip_prefix("www.")
        .unwrap_or(host)
        .to_ascii_lowercase();
    if h == "old.reddit.com" || h == "new.reddit.com" || h == "reddit.com" {
        "reddit.com".to_string()
    } else {
        h
    }
}

fn normalize_browse_tail(tail: &str) -> String {
    let t = tail.trim();
    if t.is_empty() {
        return String::new();
    }
    // Some HTTP stacks collapse `https://` → `https:/` inside a path segment.
    if t.starts_with("https:/") && !t.starts_with("https://") {
        return format!("https://{}", &t[7..]);
    }
    if t.starts_with("http:/") && !t.starts_with("http://") {
        return format!("http://{}", &t[6..]);
    }
    t.to_string()
}

fn normalize_reddit_host_path(s: &str) -> String {
    let (host, path) = s.split_once('/').unwrap_or((s, ""));
    let host = normalize_host(host);
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

/// Lowercase subreddit segment, drop listing suffixes, drop title slug after post id.
fn normalize_reddit_path(path: &str) -> String {
    let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
    if parts.len() >= 3 && parts[1] == "r" {
        parts[2] = parts[2].to_ascii_lowercase();
    }
    if let Some(i) = parts.iter().position(|p| p == "comments") {
        if parts.len() > i + 2 {
            parts.truncate(i + 2);
        }
    } else if parts.len() > 3 && parts.get(1).map(|s| s.as_str()) == Some("r") {
        // reddit.com/r/{sub}/hot → reddit.com/r/{sub}
        parts.truncate(3);
    }
    parts.join("/")
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_url_normalizes_reddit_domains() {
        let id = ItemId::from_url(
            "https://old.reddit.com/r/AmItheAsshole/comments/1trnvdl/aita_for_cancelling/",
        )
        .unwrap();
        assert_eq!(id.as_str(), "reddit.com/r/amitheasshole/comments/1trnvdl");
    }

    #[test]
    fn from_url_strips_query() {
        let id = ItemId::from_url("https://www.reddit.com/r/rust/?sort=top").unwrap();
        assert_eq!(id.as_str(), "reddit.com/r/rust");
    }

    #[test]
    fn from_url_short_path() {
        assert_eq!(
            ItemId::from_url("r/rust").unwrap().as_str(),
            "reddit.com/r/rust"
        );
    }

    #[test]
    fn parent_of_post_is_subreddit() {
        let id = ItemId::parse("reddit.com/r/aww/comments/1trnvdl").unwrap();
        assert_eq!(id.parent().unwrap().as_str(), "reddit.com/r/aww");
    }

    #[test]
    fn parent_of_subreddit_is_r_segment() {
        let id = ItemId::parse("reddit.com/r/movies").unwrap();
        assert_eq!(id.parent().unwrap().as_str(), "reddit.com/r");
    }

    #[test]
    fn breadcrumb_paths() {
        let id = ItemId::parse("reddit.com/r/movies").unwrap();
        let crumbs = id.breadcrumb_paths();
        let paths: Vec<_> = crumbs.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            paths,
            vec!["reddit.com", "reddit.com/r", "reddit.com/r/movies"]
        );
    }

    #[test]
    fn legacy_scope_maps_to_reddit_sub() {
        assert_eq!(
            ItemId::from_legacy_scope("rust").as_str(),
            "reddit.com/r/rust"
        );
        assert!(ItemId::from_legacy_scope("").is_root());
    }

    #[test]
    fn browse_href_wraps_canonical_path() {
        let id = ItemId::parse("reddit.com/r/rust").unwrap();
        assert_eq!(id.browse_href(), "/~/https://reddit.com/r/rust");
    }

    #[test]
    fn from_browse_tail_parses_full_url() {
        let id = ItemId::from_browse_tail("https://reddit.com/r/AmITheAsshole");
        assert_eq!(id.as_str(), "reddit.com/r/amitheasshole");
    }

    #[test]
    fn from_storage_strips_post_title_slug() {
        let id =
            ItemId::from_storage("reddit.com/r/rust/comments/aaa/announcing_rust_199").unwrap();
        assert_eq!(id.as_str(), "reddit.com/r/rust/comments/aaa");
    }

    #[test]
    fn from_browse_uri_strips_prefix() {
        let id = ItemId::from_browse_uri("/~/https://reddit.com/r/rust").unwrap();
        assert_eq!(id.as_str(), "reddit.com/r/rust");
    }
}
