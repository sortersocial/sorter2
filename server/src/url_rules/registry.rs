//! Per-domain canonicalization and hierarchy rules.

use std::collections::HashSet;

use super::engine::{
    clear_query, drop_fragment, drop_listing_suffix, force_https, keep_only_query, lowercase_host,
    lowercase_path, normalize_reddit_host, normalize_youtube_host, rewrite_youtu_be,
    rewrite_youtube_shorts, strip_tracking_params, strip_www, truncate_after_segment, ParsedUrl,
};

/// Result of canonicalizing a raw URL string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalResult {
    pub canonical: String,
    /// When the input normalizes to a different string, the original is an alias.
    pub alias_of: Option<String>,
}

fn apply_global(u: &mut ParsedUrl) {
    force_https(u);
    drop_fragment(u);
    strip_www(u);
    lowercase_host(u);
    strip_tracking_params(u);
}

fn normalize_reddit(u: &mut ParsedUrl) {
    normalize_reddit_host(u);
    lowercase_path(u);
    truncate_after_segment(u, "comments", 1);
    drop_listing_suffix(u, &["hot", "top", "new", "rising", "controversial"]);
    clear_query(u);
}

fn normalize_youtube(u: &mut ParsedUrl) {
    rewrite_youtu_be(u);
    normalize_youtube_host(u);
    rewrite_youtube_shorts(u);
    keep_only_query(u, &["v", "list"]);
}

fn normalize_default(_u: &mut ParsedUrl) {
    // Global rules only.
}

fn domain_key(host: &str) -> &'static str {
    if host == "reddit.com" || host.ends_with(".reddit.com") {
        "reddit.com"
    } else if host == "youtube.com" || host == "youtu.be" {
        "youtube.com"
    } else {
        "default"
    }
}

fn normalize_for_host(u: &mut ParsedUrl) {
    apply_global(u);
    match domain_key(&u.host) {
        "reddit.com" => normalize_reddit(u),
        "youtube.com" => normalize_youtube(u),
        _ => normalize_default(u),
    }
}

/// Structural path segments that must not become standalone tree nodes when more path follows.
fn structural_trailing(host: &str) -> &'static [&'static str] {
    match domain_key(host) {
        "reddit.com" => &["comments"],
        _ => &[],
    }
}

/// Canonicalize a raw URL. Returns `None` if the input is not URL-like.
pub fn canonicalize_raw(raw: &str) -> Option<CanonicalResult> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut u = ParsedUrl::parse(trimmed)?;
    let input_snapshot = u.canonical_string()?;
    normalize_for_host(&mut u);
    let canonical = u.canonical_string()?;
    let alias_of = if input_snapshot != canonical {
        Some(trimmed.to_string())
    } else {
        None
    };
    Some(CanonicalResult {
        canonical,
        alias_of,
    })
}

/// Resolve a stored or event id string to its canonical URL identity.
pub fn resolve_id(raw: &str) -> Option<String> {
    canonicalize_raw(raw).map(|r| r.canonical)
}

/// Navigable ancestor URLs from domain root up to and including `canonical` (full URLs).
pub fn navigable_breadcrumbs(canonical: &str) -> Vec<String> {
    let Some(u) = ParsedUrl::parse(canonical) else {
        return vec![canonical.to_string()];
    };
    let structural: HashSet<&str> = structural_trailing(&u.host).iter().copied().collect();
    let n = u.path_segments.len();
    let mut out = Vec::new();

    // Domain root (no path segments).
    if let Some(base) = u.with_path_segments(&[]).canonical_string() {
        out.push(base);
    }

    for i in 0..n {
        let segs: Vec<String> = u.path_segments[..=i].to_vec();
        let is_last = i == n - 1;
        let seg = u.path_segments[i].as_str();
        if structural.contains(seg) && !is_last {
            continue;
        }
        if let Some(url) = u.with_path_segments(&segs).canonical_string() {
            if out.last() != Some(&url) {
                out.push(url);
            }
        }
    }
    out
}

/// Immediate parent scope URL, or `None` for tree root / opaque single-segment ids.
pub fn parent_url(canonical: &str) -> Option<String> {
    let crumbs = navigable_breadcrumbs(canonical);
    if crumbs.len() <= 1 {
        None
    } else {
        crumbs.get(crumbs.len() - 2).cloned()
    }
}

/// True when `raw` looks like a URL (has scheme or host-like shape).
pub fn looks_like_url(raw: &str) -> bool {
    let t = raw.trim();
    t.contains("://")
        || t.starts_with("r/")
        || t.starts_with("/r/")
        || (t.contains('.') && t.contains('/'))
        || t.starts_with("reddit.com")
        || t.starts_with("www.")
        || t.starts_with("youtu.be/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reddit_post_drops_slug_and_normalizes_host() {
        let r = canonicalize_raw(
            "https://old.reddit.com/r/AmItheAsshole/comments/1trnvdl/aita_for_cancelling/",
        )
        .unwrap();
        assert_eq!(
            r.canonical,
            "https://reddit.com/r/amitheasshole/comments/1trnvdl"
        );
    }

    #[test]
    fn reddit_strips_query_and_listing() {
        assert_eq!(
            canonicalize_raw("https://www.reddit.com/r/rust/?sort=top")
                .unwrap()
                .canonical,
            "https://reddit.com/r/rust"
        );
        assert_eq!(
            canonicalize_raw("https://www.reddit.com/r/programming/hot")
                .unwrap()
                .canonical,
            "https://reddit.com/r/programming"
        );
    }

    #[test]
    fn reddit_short_path() {
        assert_eq!(
            canonicalize_raw("r/rust").unwrap().canonical,
            "https://reddit.com/r/rust"
        );
    }

    #[test]
    fn reddit_breadcrumbs_skip_phantom_comments() {
        let post = "https://reddit.com/r/aww/comments/1trnvdl";
        let crumbs = navigable_breadcrumbs(post);
        assert!(!crumbs.iter().any(|c| c.ends_with("/comments")));
        assert_eq!(
            crumbs.last().map(String::as_str),
            Some(post)
        );
        assert!(crumbs.contains(&"https://reddit.com/r/aww".to_string()));
    }

    #[test]
    fn reddit_parent_of_post_is_subreddit() {
        assert_eq!(
            parent_url("https://reddit.com/r/aww/comments/1trnvdl").as_deref(),
            Some("https://reddit.com/r/aww")
        );
    }

    #[test]
    fn youtube_youtu_be_and_watch_same_canonical() {
        let a = canonicalize_raw("https://youtu.be/dQw4w9WgXcQ").unwrap().canonical;
        let b = canonicalize_raw("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10").unwrap();
        assert_eq!(a, b.canonical);
        assert_eq!(a, "https://youtube.com/watch?v=dQw4w9WgXcQ");
    }

    #[test]
    fn legacy_schemeless_upgrades() {
        assert_eq!(
            canonicalize_raw("reddit.com/r/rust/comments/aaa/announcing_rust_199")
                .unwrap()
                .canonical,
            "https://reddit.com/r/rust/comments/aaa"
        );
    }

    #[test]
    fn alias_recorded_when_input_differs() {
        let r = canonicalize_raw("https://youtu.be/abc123").unwrap();
        assert_eq!(r.canonical, "https://youtube.com/watch?v=abc123");
        assert!(r.alias_of.is_some());
    }
}
