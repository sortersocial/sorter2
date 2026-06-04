//! Composable URL normalization primitives.

use std::collections::HashMap;

use url::Url;

/// Mutable URL view used by rule combinators before serializing to a canonical string.
#[derive(Debug, Clone)]
pub struct ParsedUrl {
    pub scheme: String,
    pub host: String,
    pub path_segments: Vec<String>,
    pub query: HashMap<String, String>,
    pub fragment: Option<String>,
}

impl ParsedUrl {
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
            fragment: url.fragment().map(str::to_string),
            host,
        })
    }

    pub fn with_path_segments(&self, segments: &[String]) -> Self {
        let mut u = self.clone();
        u.path_segments = segments.to_vec();
        u
    }

    pub fn to_url(&self) -> Option<Url> {
        let mut url = if self.path_segments.is_empty() {
            Url::parse(&format!("{}://{}", self.scheme, self.host)).ok()?
        } else {
            let path = format!("/{}", self.path_segments.join("/"));
            Url::parse(&format!("{}://{}{}", self.scheme, self.host, path)).ok()?
        };
        if !self.query.is_empty() {
            let mut pairs: Vec<_> = self.query.iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(b.0));
            url.query_pairs_mut().clear();
            for (k, v) in pairs {
                url.query_pairs_mut().append_pair(k, v);
            }
        }
        if let Some(ref frag) = self.fragment {
            url.set_fragment(Some(frag));
        }
        Some(url)
    }

    pub fn canonical_string(&self) -> Option<String> {
        let url = self.to_url()?;
        let mut s = url.to_string();
        if self.path_segments.is_empty() {
            s = s.trim_end_matches('/').to_string();
        }
        Some(s)
    }
}

pub fn force_https(u: &mut ParsedUrl) {
    if u.scheme == "http" {
        u.scheme = "https".to_string();
    }
}

pub fn drop_fragment(u: &mut ParsedUrl) {
    u.fragment = None;
}

pub fn strip_www(u: &mut ParsedUrl) {
    if u.host.starts_with("www.") {
        u.host = u.host[4..].to_string();
    }
}

pub fn lowercase_host(u: &mut ParsedUrl) {
    u.host = u.host.to_ascii_lowercase();
}

pub fn lowercase_path(u: &mut ParsedUrl) {
    for seg in &mut u.path_segments {
        *seg = seg.to_ascii_lowercase();
    }
}

pub fn clear_query(u: &mut ParsedUrl) {
    u.query.clear();
}

pub fn keep_only_query(u: &mut ParsedUrl, keys: &[&str]) {
    u.query
        .retain(|k, _| keys.iter().any(|want| want == &k.as_str()));
}

pub fn strip_tracking_params(u: &mut ParsedUrl) {
    u.query.retain(|k, _| {
        let lower = k.to_ascii_lowercase();
        !(lower.starts_with("utm_")
            || matches!(
                lower.as_str(),
                "fbclid" | "gclid" | "ref" | "ref_src" | "ref_source" | "mc_cid" | "mc_eid"
            ))
    });
}

pub fn truncate_after_segment(u: &mut ParsedUrl, name: &str, keep: usize) {
    if let Some(i) = u.path_segments.iter().position(|s| s == name) {
        let end = (i + 1 + keep).min(u.path_segments.len());
        u.path_segments.truncate(end);
    }
}

pub fn drop_listing_suffix(u: &mut ParsedUrl, suffixes: &[&str]) {
    if u.path_segments.len() >= 3 && u.path_segments.first().map(String::as_str) == Some("r") {
        if let Some(last) = u.path_segments.last() {
            if suffixes.iter().any(|s| *s == last.as_str()) {
                u.path_segments.pop();
            }
        }
    }
}

pub fn normalize_reddit_host(u: &mut ParsedUrl) {
    if matches!(
        u.host.as_str(),
        "old.reddit.com" | "new.reddit.com" | "www.reddit.com"
    ) {
        u.host = "reddit.com".to_string();
    }
}

pub fn rewrite_youtu_be(u: &mut ParsedUrl) {
    if u.host == "youtu.be" && u.path_segments.len() == 1 {
        let id = u.path_segments[0].clone();
        u.host = "youtube.com".to_string();
        u.path_segments = vec!["watch".to_string()];
        u.query.insert("v".to_string(), id);
    }
}

pub fn rewrite_youtube_shorts(u: &mut ParsedUrl) {
    if u.host == "youtube.com" && u.path_segments.first().map(String::as_str) == Some("shorts") {
        if let Some(id) = u.path_segments.get(1).cloned() {
            u.path_segments = vec!["watch".to_string()];
            u.query.insert("v".to_string(), id);
        }
    }
}

pub fn normalize_youtube_host(u: &mut ParsedUrl) {
    if matches!(u.host.as_str(), "m.youtube.com" | "www.youtube.com") {
        u.host = "youtube.com".to_string();
    }
}
