pub const AUTH_RETURN_COOKIE: &str = "sorter2_auth_return";

pub fn sanitize_return_to(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() || !s.starts_with('/') || s.starts_with("//") {
        return "/".to_string();
    }
    s.to_string()
}
