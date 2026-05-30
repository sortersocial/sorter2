//! Extract a subreddit name from a pasted Reddit URL or path.

pub fn parse_reddit_url(query: &str) -> Result<String, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("Paste a Reddit URL or r/subreddit path".into());
    }

    if let Some(sub) = subreddit_after_prefix(q, "r/") {
        return Ok(sub);
    }

    if let Some(sub) = subreddit_from_path_segment(q, "/r/") {
        return Ok(sub);
    }

    Err("Could not find a subreddit in that URL".into())
}

fn subreddit_after_prefix(text: &str, prefix: &str) -> Option<String> {
    let rest = text.strip_prefix(prefix)?;
    let sub = rest.split(['/', '?', '#']).next()?.trim();
    valid_subreddit(sub)
}

fn subreddit_from_path_segment(text: &str, needle: &str) -> Option<String> {
    let idx = text.find(needle)?;
    let rest = &text[idx + needle.len()..];
    let sub = rest.split(['/', '?', '#']).next()?.trim();
    valid_subreddit(sub)
}

fn valid_subreddit(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        Some(name.to_ascii_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_path() {
        assert_eq!(parse_reddit_url("r/rust").unwrap(), "rust");
    }

    #[test]
    fn parses_path_with_trailing_slash() {
        assert_eq!(parse_reddit_url("r/rust/").unwrap(), "rust");
    }

    #[test]
    fn parses_full_url() {
        assert_eq!(
            parse_reddit_url("https://www.reddit.com/r/programming/hot").unwrap(),
            "programming"
        );
    }

    #[test]
    fn parses_url_without_scheme() {
        assert_eq!(
            parse_reddit_url("reddit.com/r/AskReddit").unwrap(),
            "askreddit"
        );
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_reddit_url("").is_err());
        assert!(parse_reddit_url("   ").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_reddit_url("hello world").is_err());
    }
}
