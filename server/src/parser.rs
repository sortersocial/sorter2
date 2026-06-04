//! Extract a canonical [`crate::path_types::ItemId`] from a pasted Reddit URL or path.

use crate::path_types::ItemId;

pub fn parse_reddit_url(query: &str) -> Result<ItemId, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("Paste a Reddit URL or r/subreddit path".into());
    }

    if let Some(id) = ItemId::from_url(q) {
        return Ok(id);
    }

    Err("Could not parse that Reddit URL".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_path() {
        assert_eq!(
            parse_reddit_url("r/rust").unwrap().as_str(),
            "https://reddit.com/r/rust"
        );
    }

    #[test]
    fn parses_full_url() {
        assert_eq!(
            parse_reddit_url("https://www.reddit.com/r/programming/hot")
                .unwrap()
                .as_str(),
            "https://reddit.com/r/programming"
        );
    }

    #[test]
    fn parses_post_url() {
        let id = parse_reddit_url(
            "https://old.reddit.com/r/AmItheAsshole/comments/1trnvdl/aita_for_cancelling/",
        )
        .unwrap();
        assert_eq!(
            id.as_str(),
            "https://reddit.com/r/amitheasshole/comments/1trnvdl"
        );
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_reddit_url("").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_reddit_url("hello world").is_err());
    }
}
