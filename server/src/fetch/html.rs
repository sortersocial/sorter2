//! Markup for entity import / “Fetch from Reddit” (`POST /ui`, SSE response).

use maud::{html, Markup};

use crate::{
    form_template::template_json_compact,
    html::sanitize::entity_body_html,
    nsfw::{node_is_nsfw, nsfw_entity_gate},
    path_types::ItemId,
    reddit::{is_children_fetchable, is_fetchable, is_ranked_fetchable},
    reducer::NodeState,
    render::reddit::is_reddit_post,
    ui_action::UI_RPC_FIELD,
};

/// CSS selector for Idiomorph / SSE updates of one entity block.
pub fn entity_section_selector(item: &ItemId) -> String {
    format!(r#"[data-entity-section="{}"]"#, item.as_str())
}

pub fn entity_panel(node: &NodeState, nsfw_ok: bool) -> Markup {
    // Fail closed before any entity renderer: fetch morph can land here after a
    // Reddit import marks the node NSFW, before a full page reload shows the
    // page-level gate. Always include the opt-in CTA when we hide content.
    if node_is_nsfw(node) && !nsfw_ok {
        return nsfw_entity_gate(&node.id.browse_href());
    }
    if let Some(markup) = crate::render::reddit::entity_markup(node, nsfw_ok) {
        return markup;
    }
    html! {
        @if let Some(data) = &node.data {
            div class="entity-card" {
                h2 { (data.title) }
                @if let Some(author) = &data.author {
                    p class="muted small" { "by " (author) }
                }
                @if node_is_nsfw(node) {
                    p class="muted small" {
                        span class="nsfw-badge" { "NSFW" }
                    }
                }
                @if let Some(body) = &data.body_html {
                    div class="entity-body" { (maud::PreEscaped(entity_body_html(body))) }
                }
            }
        }
    }
}

/// One `fetch_entity` form/button targeting `kind` ("self", "children", or "ranked").
fn fetch_button(item: &ItemId, kind: &str, label: &str, fetching: bool) -> Markup {
    let rpc = template_json_compact(&serde_json::json!({
        "action": "fetch_entity",
        "item": item.as_str(),
        "kind": kind,
    }))
    .expect("fetch_entity rpc template");
    html! {
        form method="post" action="/ui" class="fetch-entity-form" {
            input type="hidden" name=(UI_RPC_FIELD) value=(rpc);
            @if fetching {
                button type="submit" class="btn-secondary" disabled { (label) }
            } @else {
                button type="submit" class="btn-secondary" { (label) }
            }
        }
    }
}

/// Reddit/API import controls — `POST /ui` with `fetch_entity` returns an SSE
/// stream whose events are JS snippets to `eval`.
pub fn fetch_entity_panel(item: &ItemId, node: &NodeState, fetching: bool) -> Markup {
    let has_data = node.data.is_some();
    let self_ok = is_fetchable(item);
    let children_ok = is_children_fetchable(item);
    let ranked_ok = is_ranked_fetchable(item) && node.children.iter().any(is_reddit_post);
    if !self_ok && !children_ok && !ranked_ok {
        return html! {};
    }
    let self_label = if fetching {
        "Fetching…"
    } else if has_data {
        "Refresh this"
    } else {
        "Fetch from Reddit"
    };
    html! {
        div id="fetch-controls" class="fetch-controls" {
            @if self_ok {
                (fetch_button(item, "self", self_label, fetching))
            }
            @if children_ok {
                (fetch_button(item, "children", if fetching { "Fetching…" } else { "Fetch posts" }, fetching))
            }
            @if ranked_ok {
                (fetch_button(
                    item,
                    "ranked",
                    if fetching { "Fetching…" } else { "Refresh ranking" },
                    fetching
                ))
            }
        }
    }
}

/// Entity card + fetch control (morph target [`entity_section_selector`]).
pub fn entity_section(item: &ItemId, node: &NodeState, fetching: bool, nsfw_ok: bool) -> Markup {
    let gated = node_is_nsfw(node) && !nsfw_ok;
    html! {
        section class="entity-section demo-panel" data-entity-section=(item.as_str()) {
            (entity_panel(node, nsfw_ok))
            @if !gated {
                (fetch_entity_panel(item, node, fetching))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::EntityData;

    #[test]
    fn nsfw_subreddit_soft_gate_offers_opt_in() {
        let node = NodeState {
            id: ItemId::from_url("https://reddit.com/r/nsfw").unwrap(),
            data: Some(EntityData {
                title: "nsfw".into(),
                author: None,
                body_html: None,
                over_18: true,
                thumb_url: None,
                image_url: None,
                link_url: None,
            }),
            ..Default::default()
        };
        let html = entity_section(&node.id, &node, false, false).into_string();
        assert!(html.contains("NSFW content is hidden until you opt in."));
        assert!(
            html.contains("Yes, I am 18+"),
            "soft gate must offer opt-in: {html}"
        );
        assert!(html.contains("action=\"/nsfw/enter\""));
        assert!(html.contains("/~/https://reddit.com/r/nsfw"));
        assert!(
            !html.contains("Fetch from Reddit") && !html.contains("Fetch posts"),
            "gated entity should not show fetch controls: {html}"
        );
    }
}
