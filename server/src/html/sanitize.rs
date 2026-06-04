//! Whitelist sanitization for untrusted HTML fragments (e.g. Reddit `selftext_html`).

use std::sync::LazyLock;

use ammonia::Builder;

static ENTITY_BODY: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut b = Builder::default();
    b.strip_comments(true);
    b.link_rel(Some("noopener noreferrer"));
    b
});

/// Sanitize HTML safe for embedding in our pages via [`maud::PreEscaped`].
pub fn entity_body_html(raw: &str) -> String {
    ENTITY_BODY.clean(raw).to_string()
}

#[cfg(test)]
mod tests {
    use super::entity_body_html;

    #[test]
    fn keeps_benign_markup() {
        assert_eq!(
            entity_body_html("<p>release notes</p>"),
            "<p>release notes</p>"
        );
    }

    #[test]
    fn strips_scripts_and_event_handlers() {
        let raw = "<p>ok</p><script>alert(1)</script><img src=x onerror=alert(1)>";
        let clean = entity_body_html(raw);
        assert!(!clean.contains("<script"));
        assert!(!clean.contains("onerror"));
        assert!(clean.contains("<p>ok</p>"));
    }
}
