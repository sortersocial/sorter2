//! Parse raw strings into host, path segments, and query (order-independent).

use std::collections::HashMap;

use url::Url;

#[derive(Debug, Clone)]
pub struct UrlParts {
    pub scheme: String,
    pub host: String,
    pub path_segments: Vec<String>,
    pub query: HashMap<String, String>,
}

impl UrlParts {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let with_scheme = if trimmed.contains("://") {
            trimmed.to_string()
        } else if trimmed.starts_with("r/") || trimmed.starts_with("/r/") {
            let rest = trimmed.trim_start_matches('/').trim_start_matches("r/");
            format!("https://reddit.com/r/{rest}")
        } else if trimmed.contains('.') && !trimmed.starts_with('/') {
            format!("https://{trimmed}")
        } else {
            trimmed.to_string()
        };

        let url = Url::parse(&with_scheme).ok()?;
        let host = url.host_str()?.to_string();
        let path_segments: Vec<String> = url
            .path_segments()
            .map(|segs| segs.filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default();

        let mut query = HashMap::new();
        for (k, v) in url.query_pairs() {
            query.insert(k.into_owned(), v.into_owned());
        }

        Some(Self {
            scheme: url.scheme().to_string(),
            path_segments,
            query,
            host,
        })
    }

    /// Host normalized for graph entry matching (lowercase, aliases).
    pub fn match_host(&self) -> String {
        normalize_match_host(&self.host)
    }
}

pub fn normalize_match_host(host: &str) -> String {
    let h = host
        .strip_prefix("www.")
        .unwrap_or(host)
        .to_ascii_lowercase();
    match h.as_str() {
        "old.reddit.com" | "new.reddit.com" => "reddit.com".to_string(),
        "m.youtube.com" => "youtube.com".to_string(),
        _ => h,
    }
}

pub fn strip_tracking_query(query: &mut HashMap<String, String>) {
    query.retain(|k, _| {
        let lower = k.to_ascii_lowercase();
        !(lower.starts_with("utm_")
            || matches!(
                lower.as_str(),
                "fbclid" | "gclid" | "ref" | "ref_src" | "ref_source" | "mc_cid" | "mc_eid"
            ))
    });
}

#[cfg(test)]
pub(crate) fn test_parts(host: &str, segs: &[&str], query: &[(&str, &str)]) -> UrlParts {
    UrlParts {
        scheme: "https".to_string(),
        host: host.to_string(),
        path_segments: segs.iter().map(|s| (*s).to_string()).collect(),
        query: query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_url_splits_host_path_query() {
        let p = UrlParts::parse("https://www.youtube.com/watch?v=abc&t=4").unwrap();
        assert_eq!(p.host, "www.youtube.com");
        assert_eq!(p.path_segments, vec!["watch"]);
        assert_eq!(p.query.get("v").map(String::as_str), Some("abc"));
        assert_eq!(p.query.get("t").map(String::as_str), Some("4"));
    }

    #[test]
    fn parse_r_shortcut_expands_to_reddit() {
        let p = UrlParts::parse("r/rust").unwrap();
        assert_eq!(p.match_host(), "reddit.com");
        assert_eq!(p.path_segments, vec!["r", "rust"]);
    }

    #[test]
    fn parse_slash_r_shortcut() {
        let p = UrlParts::parse("/r/aww").unwrap();
        assert_eq!(p.path_segments, vec!["r", "aww"]);
    }

    #[test]
    fn parse_schemeless_host_path() {
        let p = UrlParts::parse("reddit.com/r/rust/comments/aaa/slug").unwrap();
        assert_eq!(p.match_host(), "reddit.com");
        assert_eq!(
            p.path_segments,
            vec!["r", "rust", "comments", "aaa", "slug"]
        );
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(UrlParts::parse("").is_none());
        assert!(UrlParts::parse("   ").is_none());
    }

    #[test]
    fn normalize_match_host_reddit_aliases() {
        assert_eq!(normalize_match_host("old.reddit.com"), "reddit.com");
        assert_eq!(normalize_match_host("NEW.reddit.com"), "reddit.com");
        assert_eq!(normalize_match_host("www.reddit.com"), "reddit.com");
    }

    #[test]
    fn normalize_match_host_youtube_aliases() {
        assert_eq!(normalize_match_host("m.youtube.com"), "youtube.com");
        assert_eq!(normalize_match_host("www.youtube.com"), "youtube.com");
    }

    #[test]
    fn strip_tracking_query_removes_known_params() {
        let mut q = HashMap::from([
            ("v".into(), "1".into()),
            ("utm_source".into(), "x".into()),
            ("fbclid".into(), "y".into()),
            ("ref".into(), "z".into()),
        ]);
        strip_tracking_query(&mut q);
        assert_eq!(q.len(), 1);
        assert_eq!(q.get("v").map(String::as_str), Some("1"));
    }

    #[test]
    fn strip_tracking_query_utm_prefix() {
        let mut q = HashMap::from([("utm_campaign".into(), "email".into())]);
        strip_tracking_query(&mut q);
        assert!(q.is_empty());
    }
}
