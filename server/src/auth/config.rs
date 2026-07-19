pub const AUTH_RETURN_COOKIE: &str = "sorter2_auth_return";

/// Allow `mock_user` on `/auth/github` (test harness only).
pub fn mock_oauth_allowed() -> bool {
    matches!(
        std::env::var("SORTER2_ALLOW_MOCK_OAUTH").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

/// Set the Secure flag on auth cookies when serving over HTTPS.
pub fn cookies_secure() -> bool {
    std::env::var("SORTER2_BASE_URL")
        .map(|u| u.starts_with("https://"))
        .unwrap_or(false)
}

pub fn sanitize_return_to(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() || !s.starts_with('/') || s.starts_with("//") || s.starts_with("/\\") {
        return "/".to_string();
    }
    if s.contains('\\') {
        return "/".to_string();
    }
    // Browse paths embed a canonical URL after `/~/`
    // (`/~/https://reddit.com/r/rust`). That is still a same-origin path, not
    // an open redirect — only reject bare scheme URLs / smuggling forms.
    if s.contains("://") && !s.starts_with("/~/") {
        return "/".to_string();
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_return_to_blocks_open_redirects() {
        assert_eq!(sanitize_return_to(""), "/");
        assert_eq!(sanitize_return_to("//evil.com"), "/");
        assert_eq!(sanitize_return_to("/\\evil.com"), "/");
        assert_eq!(sanitize_return_to("https://evil.com"), "/");
        assert_eq!(sanitize_return_to("/vote?parent=x"), "/vote?parent=x");
        assert_eq!(
            sanitize_return_to("/~/https://reddit.com/r/rust"),
            "/~/https://reddit.com/r/rust"
        );
    }
}
