use serde::{Deserialize, Serialize};
use std::fmt;

use crate::url_rules::{looks_like_url, navigable_breadcrumbs, parent_url, resolve_canonical};

/// Canonical identity: a real URL (with scheme) or an opaque non-URL key.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct ItemId(String);

impl ItemId {
    /// Parse an already-canonical id (no normalization). Empty string is invalid; use [`Self::root`].
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        Some(Self(t.to_string()))
    }

    /// Build an opaque item key (demo votes, non-URL items).
    pub fn opaque(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Root of the internet tree.
    pub fn root() -> Self {
        Self(String::new())
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Canonical URL from a raw pasted or fetched URL.
    pub fn from_url(raw_url: &str) -> Option<Self> {
        resolve_canonical(raw_url).map(Self)
    }

    /// Normalize strings from forms, events, and imports into canonical identity.
    pub fn from_storage(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        if looks_like_url(t) {
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
        if looks_like_url(s) || s.contains('/') {
            Self::from_storage(s).unwrap_or_else(|| Self::opaque(s))
        } else {
            Self(format!("https://reddit.com/r/{s}"))
        }
    }

    /// Immediate parent scope in the tree.
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        if looks_like_url(self.0.as_str()) {
            return parent_url(self.0.as_str()).map(Self);
        }
        let parts: Vec<&str> = self.0.trim_end_matches('/').split('/').collect();
        if parts.len() <= 1 {
            return None;
        }
        Some(Self(parts[..parts.len() - 1].join("/")))
    }

    pub fn segments(&self) -> Vec<&str> {
        if self.is_root() {
            return vec![];
        }
        if let Some(rest) = self.0.strip_prefix("https://") {
            return rest.split('/').filter(|s| !s.is_empty()).collect();
        }
        if let Some(rest) = self.0.strip_prefix("http://") {
            return rest.split('/').filter(|s| !s.is_empty()).collect();
        }
        self.0.split('/').filter(|s| !s.is_empty()).collect()
    }

    /// Cumulative navigable paths for breadcrumbs and tree wiring (includes self).
    pub fn breadcrumb_paths(&self) -> Vec<ItemId> {
        if self.is_root() {
            return vec![];
        }
        if looks_like_url(self.0.as_str()) {
            return navigable_breadcrumbs(self.0.as_str())
                .into_iter()
                .map(ItemId)
                .collect();
        }
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
        if self.0.contains("://") {
            return self.0.clone();
        }
        if self.segments().first().is_some_and(|s| s.contains('.')) {
            format!("https://{}", self.0)
        } else {
            self.0.clone()
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
}

fn normalize_browse_tail(tail: &str) -> String {
    let t = tail.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.starts_with("https:/") && !t.starts_with("https://") {
        return format!("https://{}", &t[7..]);
    }
    if t.starts_with("http:/") && !t.starts_with("http://") {
        return format!("http://{}", &t[6..]);
    }
    t.to_string()
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
        assert_eq!(
            id.as_str(),
            "https://reddit.com/r/amitheasshole/comments/1trnvdl"
        );
    }

    #[test]
    fn from_url_strips_query() {
        let id = ItemId::from_url("https://www.reddit.com/r/rust/?sort=top").unwrap();
        assert_eq!(id.as_str(), "https://reddit.com/r/rust");
    }

    #[test]
    fn from_url_short_path() {
        assert_eq!(
            ItemId::from_url("r/rust").unwrap().as_str(),
            "https://reddit.com/r/rust"
        );
    }

    #[test]
    fn parent_of_post_is_subreddit() {
        let id = ItemId::from_url("https://reddit.com/r/aww/comments/1trnvdl").unwrap();
        assert_eq!(id.parent().unwrap().as_str(), "https://reddit.com/r/aww");
    }

    #[test]
    fn parent_of_subreddit_is_r_segment() {
        let id = ItemId::from_url("https://reddit.com/r/movies").unwrap();
        assert_eq!(id.parent().unwrap().as_str(), "https://reddit.com/r");
    }

    #[test]
    fn breadcrumb_paths_skip_phantom_comments() {
        let id = ItemId::from_url("https://reddit.com/r/aww/comments/1trnvdl").unwrap();
        let crumbs = id.breadcrumb_paths();
        let paths: Vec<_> = crumbs.iter().map(|p| p.as_str()).collect();
        assert!(!paths.iter().any(|p| p.ends_with("/comments")));
        assert!(paths.contains(&"https://reddit.com/r/aww"));
    }

    #[test]
    fn breadcrumb_paths_subreddit() {
        let id = ItemId::from_url("https://reddit.com/r/movies").unwrap();
        let crumbs = id.breadcrumb_paths();
        let paths: Vec<_> = crumbs.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "https://reddit.com",
                "https://reddit.com/r",
                "https://reddit.com/r/movies"
            ]
        );
    }

    #[test]
    fn legacy_scope_maps_to_reddit_sub() {
        assert_eq!(
            ItemId::from_legacy_scope("rust").as_str(),
            "https://reddit.com/r/rust"
        );
        assert!(ItemId::from_legacy_scope("").is_root());
    }

    #[test]
    fn browse_href_wraps_canonical_url() {
        let id = ItemId::from_url("https://reddit.com/r/rust").unwrap();
        assert_eq!(id.browse_href(), "/~/https://reddit.com/r/rust");
    }

    #[test]
    fn from_browse_tail_parses_full_url() {
        let id = ItemId::from_browse_tail("https://reddit.com/r/AmITheAsshole");
        assert_eq!(id.as_str(), "https://reddit.com/r/amitheasshole");
    }

    #[test]
    fn from_storage_strips_post_title_slug() {
        let id =
            ItemId::from_storage("reddit.com/r/rust/comments/aaa/announcing_rust_199").unwrap();
        assert_eq!(id.as_str(), "https://reddit.com/r/rust/comments/aaa");
    }

    #[test]
    fn from_browse_uri_strips_prefix() {
        let id = ItemId::from_browse_uri("/~/https://reddit.com/r/rust").unwrap();
        assert_eq!(id.as_str(), "https://reddit.com/r/rust");
    }
}
