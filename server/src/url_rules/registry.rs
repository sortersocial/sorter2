//! Public API: canonical identity and hierarchy via the URL graph.

use super::graph::graph;
use super::parse::UrlParts;

/// Result of canonicalizing a raw URL string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalResult {
    pub canonical: String,
    /// When the input normalizes to a different string, the original is an alias.
    pub alias_of: Option<String>,
}

/// Canonicalize a raw URL. Returns `None` if the input is not URL-like.
pub fn canonicalize_raw(raw: &str) -> Option<CanonicalResult> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts = UrlParts::parse(trimmed)?;
    let g = graph();
    let canonical = g.resolve_canonical(&parts)?;
    let alias_of = if trimmed != canonical {
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
    let parts = match UrlParts::parse(canonical) {
        Some(p) => p,
        None => return vec![canonical.to_string()],
    };
    graph().breadcrumbs(&parts)
}

/// Immediate parent scope URL, or `None` for tree root.
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

