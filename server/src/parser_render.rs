use maud::{html, Markup};

use crate::{
    form_template::template_json_compact,
    ui_action::UI_RPC_FIELD,
};

fn parse_query_rpc_template() -> String {
    template_json_compact(&serde_json::json!({
        "action": "parse_query",
        "query": {"$form": "query"},
    }))
    .expect("parse_query rpc template")
}

/// Navigate panel: paste a Reddit URL and click Go.
pub fn navigate_panel(query: &str, error: Option<&str>) -> Markup {
    html! {
        section id="parser-panel" class="demo-panel" {
            h2 { "Navigate" }
            p class="muted small" {
                "Paste a Reddit URL or "
                code { "r/subreddit" }
                " path, then click Go to rank that subreddit."
            }
            form method="post" action="/ui" id="parser-form" {
                textarea
                    name="query"
                    id="parser-input"
                    rows="3"
                    placeholder="https://reddit.com/r/rust or r/rust"
                    autocomplete="off"
                    spellcheck="false" {
                    (query)
                }
                input type="hidden" name=(UI_RPC_FIELD) value=(parse_query_rpc_template());
                button type="submit" class="btn-primary" { "Go" }
            }
            @if let Some(msg) = error {
                p class="parser-error muted" { (msg) }
            }
        }
    }
}
