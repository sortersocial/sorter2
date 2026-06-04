//! End-to-end tests for the public registry API (`canonicalize_raw`, breadcrumbs, parent).

use super::registry::{
    canonicalize_raw, looks_like_url, navigable_breadcrumbs, parent_url, resolve_id,
};

fn canon(raw: &str) -> String {
    canonicalize_raw(raw).unwrap().canonical
}

#[test]
fn looks_like_url_positive_cases() {
    for raw in [
        "https://reddit.com/r/rust",
        "r/rust",
        "/r/aww",
        "reddit.com/r/x",
        "www.example.com/path",
        "youtu.be/abc",
    ] {
        assert!(looks_like_url(raw), "{raw}");
    }
}

#[test]
fn looks_like_url_negative_cases() {
    for raw in ["alpha", "beta", "", "hello world", "no-dots"] {
        assert!(!looks_like_url(raw), "{raw}");
    }
}

#[test]
fn resolve_id_matches_canonicalize_raw() {
    let raw = "https://youtu.be/xyz";
    assert_eq!(
        resolve_id(raw).as_deref(),
        Some(canon(raw).as_str())
    );
}

#[test]
fn canonicalize_empty_returns_none() {
    assert!(canonicalize_raw("").is_none());
}

#[test]
fn alias_when_slug_stripped() {
    let r = canonicalize_raw(
        "https://reddit.com/r/rust/comments/aaa/very_long_title_slug",
    )
    .unwrap();
    assert_eq!(r.canonical, "https://reddit.com/r/rust/comments/aaa");
    assert!(r.alias_of.is_some());
}

#[test]
fn alias_none_when_already_canonical() {
    let raw = "https://reddit.com/r/rust";
    let r = canonicalize_raw(raw).unwrap();
    assert_eq!(r.canonical, raw);
    assert!(r.alias_of.is_none());
}

#[test]
fn parent_url_subreddit_under_r_hub() {
    assert_eq!(
        parent_url("https://reddit.com/r/movies").as_deref(),
        Some("https://reddit.com/r")
    );
}

#[test]
fn parent_url_domain_has_none() {
    assert_eq!(parent_url("https://reddit.com").as_deref(), None);
}

#[test]
fn parent_url_generic_site() {
    assert_eq!(
        parent_url("https://example.com/a/b").as_deref(),
        Some("https://example.com/a")
    );
}

#[test]
fn breadcrumbs_from_canonical_string_roundtrip() {
    let id = "https://reddit.com/r/golang/comments/abc123";
    let crumbs = navigable_breadcrumbs(id);
    assert_eq!(crumbs.last().map(String::as_str), Some(id));
}

// --- Table: Reddit raw URLs → canonical ---

#[test]
fn reddit_canonical_matrix() {
    let cases: &[(&str, &str)] = &[
        ("r/rust", "https://reddit.com/r/rust"),
        ("/r/aww", "https://reddit.com/r/aww"),
        ("https://reddit.com/r/rust", "https://reddit.com/r/rust"),
        (
            "https://www.reddit.com/r/programming/new",
            "https://reddit.com/r/programming",
        ),
        (
            "https://old.reddit.com/r/test/comments/xyz/slug/",
            "https://reddit.com/r/test/comments/xyz",
        ),
        (
            "reddit.com/r/Movies/comments/abc/Title_Case_Slug",
            "https://reddit.com/r/movies/comments/abc",
        ),
    ];
    for (raw, want) in cases {
        assert_eq!(canon(raw), *want, "raw={raw}");
    }
}

// --- Table: YouTube raw URLs → canonical ---

#[test]
fn youtube_canonical_matrix() {
    let cases: &[(&str, &str)] = &[
        (
            "https://youtube.com/watch?v=abc",
            "https://youtube.com/watch?v=abc",
        ),
        (
            "https://www.youtube.com/watch?v=abc&t=1&feature=share",
            "https://youtube.com/watch?v=abc",
        ),
        ("https://youtu.be/abc", "https://youtube.com/watch?v=abc"),
        (
            "https://youtube.com/shorts/abc",
            "https://youtube.com/watch?v=abc",
        ),
    ];
    for (raw, want) in cases {
        assert_eq!(canon(raw), *want, "raw={raw}");
    }
}

// --- Table: generic sites ---

#[test]
fn generic_canonical_matrix() {
    let cases: &[(&str, &str)] = &[
        (
            "https://news.ycombinator.com/item?id=38472",
            "https://news.ycombinator.com/item?id=38472",
        ),
        (
            "https://www.github.com/rust-lang/rust/issues/1?utm_source=x",
            "https://github.com/rust-lang/rust/issues/1",
        ),
        ("https://example.com", "https://example.com"),
    ];
    for (raw, want) in cases {
        assert_eq!(canon(raw), *want, "raw={raw}");
    }
}

// --- Phantom /comments/ regression (sorter2-specific) ---

#[test]
fn phantom_comments_not_in_breadcrumbs_for_post() {
    let crumbs = navigable_breadcrumbs("https://reddit.com/r/rust/comments/aaa");
    assert!(!crumbs.iter().any(|c| c.ends_with("/comments")));
}

#[test]
fn phantom_comments_not_sibling_of_subreddit_in_breadcrumb_chain() {
    let crumbs = navigable_breadcrumbs("https://reddit.com/r/rust/comments/aaa");
    let subs: Vec<_> = crumbs
        .iter()
        .filter(|c| c.contains("/r/rust") && !c.contains("/comments/"))
        .collect();
    assert_eq!(subs, vec!["https://reddit.com/r/rust"]);
}

// --- Distinct items must stay distinct ---

#[test]
fn different_posts_different_canonical() {
    let a = canon("https://reddit.com/r/rust/comments/aaa");
    let b = canon("https://reddit.com/r/rust/comments/bbb");
    assert_ne!(a, b);
}

#[test]
fn different_subreddits_different_canonical() {
    assert_ne!(
        canon("https://reddit.com/r/rust"),
        canon("https://reddit.com/r/golang")
    );
}
