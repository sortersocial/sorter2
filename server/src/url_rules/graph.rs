//! Semantic URL graph: DFA traversal on host + path, query in context, generic fallback.

use std::collections::HashMap;
use std::sync::OnceLock;

use url::Url;

use super::graph_builder::GraphBuilder;
use super::parse::{normalize_match_host, strip_tracking_query, UrlParts};

#[derive(Debug, Clone, Default)]
pub struct Context {
    pub vars: HashMap<String, String>,
    pub query: HashMap<String, String>,
}

pub type CanonicalFn = fn(&Context) -> Option<String>;

#[derive(Clone, Copy)]
pub enum EdgePattern {
    Literal(&'static str),
    Variable(&'static str),
    /// Absorb any trailing segment without leaving this node (e.g. post title slug).
    AbsorbAny,
    /// Absorb segment when `cond(seg)` (e.g. subreddit listing suffix).
    AbsorbIf(fn(&str) -> bool),
}

pub struct Edge {
    pub pattern: EdgePattern,
    pub target: &'static str,
}

pub struct Node {
    pub edges: Vec<Edge>,
    pub canonical: CanonicalFn,
    pub parent: Option<&'static str>,
}

impl Node {
    pub(crate) fn empty() -> Self {
        Self {
            edges: Vec::new(),
            canonical: |_| None,
            parent: None,
        }
    }
}

pub struct Graph {
    pub nodes: HashMap<&'static str, Node>,
}

static GRAPH: OnceLock<Graph> = OnceLock::new();

pub fn graph() -> &'static Graph {
    GRAPH.get_or_init(build_graph)
}

impl Graph {
    pub fn resolve_canonical(&self, parts: &UrlParts) -> Option<String> {
        let mut query = parts.query.clone();
        strip_tracking_query(&mut query);
        let mut ctx = Context {
            vars: HashMap::new(),
            query,
        };

        if let Some(node_id) = self.traverse(parts, &mut ctx) {
            if let Some(canon) = (self.nodes.get(node_id)?.canonical)(&ctx) {
                return Some(canon);
            }
        }
        Some(generic_canonical(parts))
    }

    pub fn breadcrumbs(&self, parts: &UrlParts) -> Vec<String> {
        let mut query = parts.query.clone();
        strip_tracking_query(&mut query);
        let mut ctx = Context {
            vars: HashMap::new(),
            query,
        };

        if let Some(mut node_id) = self.traverse(parts, &mut ctx) {
            let mut paths = Vec::new();
            loop {
                let node = match self.nodes.get(node_id) {
                    Some(n) => n,
                    None => break,
                };
                if let Some(url) = (node.canonical)(&ctx) {
                    if paths.last() != Some(&url) {
                        paths.push(url);
                    }
                }
                match node.parent {
                    Some(p) => node_id = p,
                    None => break,
                }
            }
            paths.reverse();
            if !paths.is_empty() {
                return paths;
            }
        }
        generic_breadcrumbs(parts)
    }

    fn traverse(&self, parts: &UrlParts, ctx: &mut Context) -> Option<&'static str> {
        let host = parts.match_host();
        let mut node_id = match host.as_str() {
            "reddit.com" => "reddit_root",
            "youtube.com" => "youtube_root",
            "youtu.be" => "youtu_be_entry",
            _ => return None,
        };

        let segs: Vec<&str> = parts.path_segments.iter().map(String::as_str).collect();
        let mut i = 0;
        while i < segs.len() {
            let seg = segs[i];
            match self.follow_edge(node_id, seg, ctx) {
                Ok(next) => {
                    node_id = next;
                    i += 1;
                }
                Err(()) => {
                    if self.try_absorb(node_id, seg) {
                        i += 1;
                        continue;
                    }
                    return None;
                }
            }
        }
        Some(node_id)
    }

    fn follow_edge(
        &self,
        node_id: &'static str,
        seg: &str,
        ctx: &mut Context,
    ) -> Result<&'static str, ()> {
        let node = self.nodes.get(node_id).ok_or(())?;
        for edge in &node.edges {
            match edge.pattern {
                EdgePattern::Literal(lit) if lit == seg => return Ok(edge.target),
                EdgePattern::Variable(name) => {
                    ctx.vars.insert(name.to_string(), seg.to_string());
                    return Ok(edge.target);
                }
                EdgePattern::AbsorbAny
                | EdgePattern::AbsorbIf(_)
                | EdgePattern::Literal(_)
                | EdgePattern::Variable(_) => {}
            }
        }
        Err(())
    }

    fn try_absorb(&self, node_id: &'static str, seg: &str) -> bool {
        let node = match self.nodes.get(node_id) {
            Some(n) => n,
            None => return false,
        };
        for edge in &node.edges {
            match edge.pattern {
                EdgePattern::AbsorbAny => return true,
                EdgePattern::AbsorbIf(cond) if cond(seg) => return true,
                EdgePattern::AbsorbIf(_) | EdgePattern::Literal(_) | EdgePattern::Variable(_) => {}
            }
        }
        false
    }

    /// Test hook: terminal graph node and captured context after traversal.
    #[cfg(test)]
    pub fn traverse_terminal(&self, parts: &UrlParts) -> Option<(&'static str, Context)> {
        let mut query = parts.query.clone();
        strip_tracking_query(&mut query);
        let mut ctx = Context {
            vars: HashMap::new(),
            query,
        };
        let node = self.traverse(parts, &mut ctx)?;
        Some((node, ctx))
    }
}

fn is_reddit_listing_suffix(seg: &str) -> bool {
    matches!(seg, "hot" | "top" | "new" | "rising" | "controversial")
}

/// Percent-encode a path or query fragment so `&`, `?`, etc. cannot break URL structure.
fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

// --- Canonical formatters ---

fn canon_reddit_root(_: &Context) -> Option<String> {
    Some("https://reddit.com".to_string())
}

fn canon_reddit_r_hub(_: &Context) -> Option<String> {
    Some("https://reddit.com/r".to_string())
}

fn canon_reddit_subreddit(ctx: &Context) -> Option<String> {
    let sub = ctx.vars.get("subreddit")?;
    Some(format!(
        "https://reddit.com/r/{}",
        enc(&sub.to_ascii_lowercase())
    ))
}

fn canon_reddit_post(ctx: &Context) -> Option<String> {
    let sub = ctx.vars.get("subreddit")?.to_ascii_lowercase();
    let id = ctx.vars.get("post_id")?;
    Some(format!(
        "https://reddit.com/r/{}/comments/{}",
        enc(&sub),
        enc(id)
    ))
}

fn canon_youtube_root(_: &Context) -> Option<String> {
    Some("https://youtube.com".to_string())
}

fn canon_youtube_watch(ctx: &Context) -> Option<String> {
    let v = ctx
        .query
        .get("v")
        .or_else(|| ctx.vars.get("video_id"))?;
    Some(format!("https://youtube.com/watch?v={}", enc(v)))
}

fn canon_youtu_be(ctx: &Context) -> Option<String> {
    let v = ctx.vars.get("vid_id")?;
    Some(format!("https://youtube.com/watch?v={}", enc(v)))
}

pub fn build_graph() -> Graph {
    GraphBuilder::new()
        .node("reddit_root")
        .canonical(canon_reddit_root)
        .edge(EdgePattern::Literal("r"), "reddit_r_hub")
        .node("reddit_r_hub")
        .parent("reddit_root")
        .canonical(canon_reddit_r_hub)
        .edge(EdgePattern::Variable("subreddit"), "reddit_subreddit")
        .node("reddit_subreddit")
        .parent("reddit_r_hub")
        .canonical(canon_reddit_subreddit)
        .edge(
            EdgePattern::AbsorbIf(is_reddit_listing_suffix),
            "reddit_subreddit",
        )
        .edge(EdgePattern::Literal("comments"), "reddit_comments_gate")
        .node("reddit_comments_gate")
        .parent("reddit_subreddit")
        .canonical(canon_reddit_subreddit)
        .edge(EdgePattern::Variable("post_id"), "reddit_post")
        .node("reddit_post")
        .parent("reddit_subreddit")
        .canonical(canon_reddit_post)
        .edge(EdgePattern::AbsorbAny, "reddit_post")
        .node("youtube_root")
        .canonical(canon_youtube_root)
        .edge(EdgePattern::Literal("watch"), "youtube_watch")
        .edge(EdgePattern::Literal("shorts"), "youtube_shorts_gate")
        .node("youtube_watch")
        .parent("youtube_root")
        .canonical(canon_youtube_watch)
        .node("youtube_shorts_gate")
        .parent("youtube_root")
        .canonical(canon_youtube_root)
        .edge(EdgePattern::Variable("video_id"), "youtube_watch")
        .node("youtu_be_entry")
        .canonical(canon_youtube_root)
        .edge(EdgePattern::Variable("vid_id"), "youtu_be_video")
        .node("youtu_be_video")
        .parent("youtube_root")
        .canonical(canon_youtu_be)
        .build()
}

// --- Generic internet fallback ---

pub fn generic_canonical(parts: &UrlParts) -> String {
    let host = normalize_match_host(&parts.host);
    let path_segments: Vec<String> = parts.path_segments.clone();
    let mut query = parts.query.clone();
    strip_tracking_query(&mut query);

    let mut url = if path_segments.is_empty() {
        Url::parse(&format!("https://{host}"))
            .unwrap_or_else(|_| Url::parse("https://invalid").unwrap())
    } else {
        let path = format!("/{}", path_segments.join("/"));
        Url::parse(&format!("https://{host}{path}"))
            .unwrap_or_else(|_| Url::parse("https://invalid").unwrap())
    };

    if !query.is_empty() {
        let mut pairs: Vec<_> = query.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        url.query_pairs_mut().clear();
        for (k, v) in pairs {
            url.query_pairs_mut().append_pair(k, v);
        }
    }

    let mut s = url.to_string();
    if path_segments.is_empty() {
        s = s.trim_end_matches('/').to_string();
    }
    s
}

pub fn generic_breadcrumbs(parts: &UrlParts) -> Vec<String> {
    let host = normalize_match_host(&parts.host);
    let n = parts.path_segments.len();
    let mut out = Vec::new();

    let base = generic_canonical(&UrlParts {
        scheme: "https".to_string(),
        host: host.clone(),
        path_segments: vec![],
        query: HashMap::new(),
    });
    out.push(base);

    for i in 0..n {
        let segs: Vec<String> = parts.path_segments[..=i].to_vec();
        let url = generic_canonical(&UrlParts {
            scheme: "https".to_string(),
            host: host.clone(),
            path_segments: segs,
            query: HashMap::new(),
        });
        if out.last() != Some(&url) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url_rules::parse::test_parts;

    fn g() -> &'static Graph {
        graph()
    }

    fn canon(parts: &UrlParts) -> String {
        g().resolve_canonical(parts).unwrap()
    }

    fn crumbs(parts: &UrlParts) -> Vec<String> {
        g().breadcrumbs(parts)
    }

    fn terminal(parts: &UrlParts) -> Option<&'static str> {
        g().traverse_terminal(parts).map(|(n, _)| n)
    }

    fn vars(parts: &UrlParts) -> HashMap<String, String> {
        g().traverse_terminal(parts)
            .map(|(_, c)| c.vars)
            .unwrap_or_default()
    }

    #[test]
    fn youtu_be_malicious_segment_encoded_not_injected() {
        let p = test_parts("youtu.be", &["abc&t=1"], &[]);
        assert_eq!(canon(&p), "https://youtube.com/watch?v=abc%26t%3D1");
        assert!(!canon(&p).contains("abc&t=1"));
    }

    #[test]
    fn youtube_query_v_encoded() {
        let p = test_parts("youtube.com", &["watch"], &[("v", "a&b=c")]);
        assert_eq!(canon(&p), "https://youtube.com/watch?v=a%26b%3Dc");
    }

    #[test]
    fn absorb_patterns_live_on_edges_not_in_engine() {
        let g = build_graph();
        let sub = g.nodes.get("reddit_subreddit").unwrap();
        assert!(sub
            .edges
            .iter()
            .any(|e| matches!(e.pattern, EdgePattern::AbsorbIf(_))));
        let post = g.nodes.get("reddit_post").unwrap();
        assert!(post
            .edges
            .iter()
            .any(|e| matches!(e.pattern, EdgePattern::AbsorbAny)));
    }

    #[test]
    fn builder_rejects_missing_parent() {
        let result = std::panic::catch_unwind(|| {
            GraphBuilder::new()
                .node("orphan")
                .parent("nonexistent_parent")
                .build();
        });
        assert!(result.is_err());
    }

    #[test]
    fn generic_canonical_forces_https_and_strips_www() {
        let p = test_parts("www.example.com", &["blog", "post"], &[]);
        assert_eq!(canon(&p), "https://example.com/blog/post");
    }

    #[test]
    fn generic_canonical_sorts_query_keys() {
        let p = test_parts("example.com", &["search"], &[("q", "rust"), ("page", "2")]);
        assert_eq!(canon(&p), "https://example.com/search?page=2&q=rust");
    }

    #[test]
    fn generic_canonical_strips_tracking_from_query() {
        let p = test_parts(
            "news.ycombinator.com",
            &["item"],
            &[("id", "1"), ("utm_medium", "social")],
        );
        assert_eq!(canon(&p), "https://news.ycombinator.com/item?id=1");
    }

    #[test]
    fn generic_breadcrumbs_cumulative_path() {
        let p = test_parts("paulgraham.com", &["articles", "lisp.html"], &[]);
        assert_eq!(
            crumbs(&p),
            vec![
                "https://paulgraham.com",
                "https://paulgraham.com/articles",
                "https://paulgraham.com/articles/lisp.html"
            ]
        );
    }

    #[test]
    fn generic_breadcrumbs_domain_only() {
        let p = test_parts("example.com", &[], &[]);
        assert_eq!(crumbs(&p), vec!["https://example.com"]);
    }

    #[test]
    fn unknown_host_uses_generic_not_graph() {
        let p = test_parts("hackernews.com", &["item", "123"], &[]);
        assert_eq!(terminal(&p), None);
        assert_eq!(canon(&p), "https://hackernews.com/item/123");
    }

    #[test]
    fn traverse_captures_subreddit_variable() {
        let p = test_parts("reddit.com", &["r", "Rust"], &[]);
        assert_eq!(terminal(&p), Some("reddit_subreddit"));
        assert_eq!(vars(&p).get("subreddit").map(String::as_str), Some("Rust"));
    }

    #[test]
    fn traverse_captures_post_id() {
        let p = test_parts("reddit.com", &["r", "aww", "comments", "abc123"], &[]);
        assert_eq!(terminal(&p), Some("reddit_post"));
        assert_eq!(vars(&p).get("post_id").map(String::as_str), Some("abc123"));
    }

    #[test]
    fn traverse_absorbs_listing_suffix_stays_on_subreddit() {
        let p = test_parts("reddit.com", &["r", "rust", "hot"], &[]);
        assert_eq!(terminal(&p), Some("reddit_subreddit"));
        assert_eq!(canon(&p), "https://reddit.com/r/rust");
    }

    #[test]
    fn traverse_absorbs_all_listing_suffixes() {
        for suffix in ["hot", "top", "new", "rising", "controversial"] {
            let p = test_parts("reddit.com", &["r", "test", suffix], &[]);
            assert_eq!(terminal(&p), Some("reddit_subreddit"), "suffix {suffix}");
            assert_eq!(canon(&p), "https://reddit.com/r/test", "suffix {suffix}");
        }
    }

    #[test]
    fn traverse_absorbs_post_title_slug() {
        let p = test_parts(
            "reddit.com",
            &["r", "rust", "comments", "aaa", "my_great_post_title"],
            &[],
        );
        assert_eq!(terminal(&p), Some("reddit_post"));
        assert_eq!(canon(&p), "https://reddit.com/r/rust/comments/aaa");
    }

    #[test]
    fn traverse_unknown_segment_falls_back_to_generic() {
        let p = test_parts("reddit.com", &["r", "rust", "wiki", "faq"], &[]);
        assert_eq!(terminal(&p), None);
        assert_eq!(canon(&p), "https://reddit.com/r/rust/wiki/faq");
    }

    #[test]
    fn traverse_youtube_watch_requires_v_in_query() {
        let p = test_parts("youtube.com", &["watch"], &[("v", "xyz")]);
        assert_eq!(terminal(&p), Some("youtube_watch"));
    }

    #[test]
    fn traverse_youtu_be_captures_vid_id() {
        let p = test_parts("youtu.be", &["dQw4w9WgXcQ"], &[]);
        assert_eq!(terminal(&p), Some("youtu_be_video"));
        assert_eq!(vars(&p).get("vid_id").map(String::as_str), Some("dQw4w9WgXcQ"));
    }

    #[test]
    fn traverse_shorts_sets_video_id_var() {
        let p = test_parts("youtube.com", &["shorts", "abc99"], &[]);
        assert_eq!(terminal(&p), Some("youtube_watch"));
        assert_eq!(vars(&p).get("video_id").map(String::as_str), Some("abc99"));
    }

    #[test]
    fn reddit_domain_canonical() {
        let p = test_parts("reddit.com", &[], &[]);
        assert_eq!(canon(&p), "https://reddit.com");
    }

    #[test]
    fn reddit_r_hub_canonical() {
        let p = test_parts("reddit.com", &["r"], &[]);
        assert_eq!(terminal(&p), Some("reddit_r_hub"));
        assert_eq!(canon(&p), "https://reddit.com/r");
    }

    #[test]
    fn reddit_subreddit_lowercases_name() {
        let p = test_parts("reddit.com", &["r", "AmITheAsshole"], &[]);
        assert_eq!(canon(&p), "https://reddit.com/r/amitheasshole");
    }

    #[test]
    fn reddit_host_aliases_old_new_www() {
        for host in ["old.reddit.com", "new.reddit.com", "www.reddit.com"] {
            let p = test_parts(host, &["r", "rust"], &[]);
            assert_eq!(canon(&p), "https://reddit.com/r/rust", "host {host}");
        }
    }

    #[test]
    fn reddit_post_strips_slug_and_query() {
        let p = test_parts(
            "old.reddit.com",
            &["r", "Rust", "comments", "1abc", "title_slug_here"],
            &[("sort", "new")],
        );
        assert_eq!(canon(&p), "https://reddit.com/r/rust/comments/1abc");
    }

    #[test]
    fn reddit_post_multiple_slugs_absorbed() {
        let p = test_parts(
            "reddit.com",
            &["r", "x", "comments", "id1", "slug1", "extra"],
            &[],
        );
        assert_eq!(canon(&p), "https://reddit.com/r/x/comments/id1");
    }

    #[test]
    fn reddit_listing_with_query_only() {
        let p = test_parts("www.reddit.com", &["r", "programming"], &[("sort", "top")]);
        assert_eq!(canon(&p), "https://reddit.com/r/programming");
    }

    #[test]
    fn reddit_subreddit_breadcrumbs_include_r_hub() {
        let p = test_parts("reddit.com", &["r", "movies"], &[]);
        assert_eq!(
            crumbs(&p),
            vec![
                "https://reddit.com",
                "https://reddit.com/r",
                "https://reddit.com/r/movies"
            ]
        );
    }

    #[test]
    fn reddit_post_breadcrumbs_skip_comments_node() {
        let p = test_parts("reddit.com", &["r", "aww", "comments", "1trnvdl"], &[]);
        let c = crumbs(&p);
        assert!(!c.iter().any(|u| u.ends_with("/comments")));
        assert_eq!(
            c.last().map(String::as_str),
            Some("https://reddit.com/r/aww/comments/1trnvdl")
        );
        assert!(c.contains(&"https://reddit.com/r/aww".to_string()));
    }

    #[test]
    fn reddit_post_parent_is_subreddit_not_comments() {
        let p = test_parts("reddit.com", &["r", "aww", "comments", "1trnvdl"], &[]);
        let c = crumbs(&p);
        let parent = c.get(c.len() - 2).unwrap();
        assert_eq!(parent, "https://reddit.com/r/aww");
    }

    #[test]
    fn reddit_domain_parent_is_none_in_breadcrumb_chain() {
        let p = test_parts("reddit.com", &[], &[]);
        assert_eq!(crumbs(&p), vec!["https://reddit.com"]);
    }

    #[test]
    fn youtube_watch_canonical_uses_v_only() {
        let p = test_parts("youtube.com", &["watch"], &[("v", "abc"), ("t", "99")]);
        assert_eq!(canon(&p), "https://youtube.com/watch?v=abc");
    }

    #[test]
    fn youtube_query_order_independent() {
        let a = test_parts("youtube.com", &["watch"], &[("v", "abc"), ("t", "4")]);
        let b = test_parts("youtube.com", &["watch"], &[("t", "4"), ("v", "abc")]);
        assert_eq!(canon(&a), canon(&b));
    }

    #[test]
    fn youtube_host_aliases() {
        for host in ["www.youtube.com", "m.youtube.com"] {
            let p = test_parts(host, &["watch"], &[("v", "x")]);
            assert_eq!(canon(&p), "https://youtube.com/watch?v=x", "host {host}");
        }
    }

    #[test]
    fn youtube_shorts_canonical_matches_watch() {
        let shorts = test_parts("youtube.com", &["shorts", "vid123"], &[]);
        let watch = test_parts("youtube.com", &["watch"], &[("v", "vid123")]);
        assert_eq!(canon(&shorts), canon(&watch));
        assert_eq!(canon(&shorts), "https://youtube.com/watch?v=vid123");
    }

    #[test]
    fn youtu_be_matches_youtube_watch() {
        let be = test_parts("youtu.be", &["dQw4w9WgXcQ"], &[]);
        let watch = test_parts("youtube.com", &["watch"], &[("v", "dQw4w9WgXcQ")]);
        assert_eq!(canon(&be), canon(&watch));
    }

    #[test]
    fn youtube_breadcrumbs_domain_then_watch() {
        let p = test_parts("youtube.com", &["watch"], &[("v", "abc")]);
        assert_eq!(
            crumbs(&p),
            vec!["https://youtube.com", "https://youtube.com/watch?v=abc"]
        );
    }

    #[test]
    fn youtu_be_breadcrumbs_include_youtube_domain() {
        let p = test_parts("youtu.be", &["abc"], &[]);
        let c = crumbs(&p);
        assert_eq!(c.first().map(String::as_str), Some("https://youtube.com"));
        assert_eq!(
            c.last().map(String::as_str),
            Some("https://youtube.com/watch?v=abc")
        );
    }

    #[test]
    fn parsed_urls_match_hand_built_parts() {
        let raw = "https://www.reddit.com/r/rust/comments/aaa/title/?utm=x";
        let parsed = UrlParts::parse(raw).unwrap();
        let hand = test_parts(
            "www.reddit.com",
            &["r", "rust", "comments", "aaa", "title"],
            &[("utm", "x")],
        );
        assert_eq!(canon(&parsed), canon(&hand));
    }

    #[test]
    fn equivalence_cluster_youtube_formats() {
        let urls = [
            "https://youtu.be/abc123",
            "https://www.youtube.com/watch?v=abc123",
            "https://youtube.com/watch?v=abc123&t=1",
            "https://m.youtube.com/watch?t=1&v=abc123",
        ];
        let canonical: Vec<_> = urls
            .iter()
            .map(|u| canon(&UrlParts::parse(u).unwrap()))
            .collect();
        assert!(canonical.iter().all(|c| *c == "https://youtube.com/watch?v=abc123"));
    }

    #[test]
    fn equivalence_cluster_reddit_post_formats() {
        let urls = [
            "https://old.reddit.com/r/Rust/comments/aaa/slug/",
            "reddit.com/r/rust/comments/aaa/other_slug",
            "https://reddit.com/r/RUST/comments/aaa",
        ];
        let canonical: Vec<_> = urls
            .iter()
            .map(|u| canon(&UrlParts::parse(u).unwrap()))
            .collect();
        assert!(
            canonical
                .iter()
                .all(|c| *c == "https://reddit.com/r/rust/comments/aaa")
        );
    }

    #[test]
    fn graph_nodes_all_have_valid_parent_links() {
        let g = build_graph();
        for (id, node) in &g.nodes {
            if let Some(parent) = node.parent {
                assert!(g.nodes.contains_key(parent), "node {id} parent {parent}");
            }
        }
    }

    #[test]
    fn graph_terminal_canonical_always_succeeds_for_reddit_paths() {
        let cases: &[(&[&str], &str)] = &[
            (&["r", "rust"], "https://reddit.com/r/rust"),
            (
                &["r", "rust", "comments", "x"],
                "https://reddit.com/r/rust/comments/x",
            ),
        ];
        for (segs, want) in cases {
            let p = test_parts("reddit.com", segs, &[]);
            assert_eq!(canon(&p), *want);
        }
    }

    #[test]
    fn breadcrumb_parent_walk_matches_parent_url_semantics() {
        let p = test_parts("reddit.com", &["r", "aww", "comments", "id1"], &[]);
        let c = crumbs(&p);
        assert_eq!(c.len(), 4);
        assert_eq!(
            c.get(c.len() - 2).map(String::as_str),
            Some("https://reddit.com/r/aww")
        );
    }

    #[test]
    fn youtube_watch_without_v_falls_back_to_generic() {
        let p = test_parts("youtube.com", &["watch"], &[]);
        assert_eq!(terminal(&p), Some("youtube_watch"));
        assert_eq!(canon(&p), "https://youtube.com/watch");
    }

    #[test]
    fn reddit_only_comments_path_stops_at_gate() {
        let p = test_parts("reddit.com", &["r", "rust", "comments"], &[]);
        assert_eq!(terminal(&p), Some("reddit_comments_gate"));
        assert_eq!(canon(&p), "https://reddit.com/r/rust");
    }

    #[test]
    fn generic_deep_path_many_segments() {
        let segs: Vec<&str> = (0..10)
            .map(|i| match i {
                0 => "a",
                1 => "b",
                2 => "c",
                3 => "d",
                4 => "e",
                5 => "f",
                6 => "g",
                7 => "h",
                8 => "i",
                _ => "j",
            })
            .collect();
        let p = test_parts("site.com", &segs, &[]);
        assert_eq!(crumbs(&p).len(), 11);
    }

    #[test]
    fn traverse_literal_r_required_for_subreddit() {
        let p = test_parts("reddit.com", &["rust"], &[]);
        assert_eq!(terminal(&p), None);
    }

    #[test]
    fn http_scheme_upgraded_via_generic_fallback_host() {
        let parsed = UrlParts::parse("http://example.com/page").unwrap();
        assert_eq!(canon(&parsed), "https://example.com/page");
    }

    #[test]
    fn each_graph_node_canonical_is_invokable() {
        let g = build_graph();
        let empty = Context::default();
        for (id, node) in &g.nodes {
            let _ = (node.canonical)(&empty);
            let _ = id;
        }
    }

    #[test]
    fn reddit_double_listing_suffix_both_absorbed() {
        let p = test_parts("reddit.com", &["r", "rust", "hot", "new"], &[]);
        assert_eq!(terminal(&p), Some("reddit_subreddit"));
        assert_eq!(canon(&p), "https://reddit.com/r/rust");
    }

    #[test]
    fn youtu_be_empty_path_stays_at_entry() {
        let p = test_parts("youtu.be", &[], &[]);
        assert_eq!(terminal(&p), Some("youtu_be_entry"));
    }
}
