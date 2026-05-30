//! Markup for entity import / “Fetch from Reddit” (`POST /ui`, SSE response).

use maud::{html, Markup};

use crate::{
    form_template::template_json_compact,
    path_types::ItemId,
    reddit::is_fetchable,
    reducer::NodeState,
    ui_action::UI_RPC_FIELD,
};

fn entity_panel(node: &NodeState) -> Markup {
    html! {
        @if let Some(data) = &node.data {
            div id="entity-panel" class="entity-card" {
                h2 { (data.title) }
                @if let Some(author) = &data.author {
                    p class="muted small" { "by " (author) }
                }
                @if let Some(body) = &data.body_html {
                    div class="entity-body" { (maud::PreEscaped(body)) }
                }
            }
        }
    }
}

/// Reddit/API import — `POST /ui` with `fetch_entity` returns an SSE stream.
pub fn fetch_entity_panel(item: &ItemId, has_data: bool, fetching: bool) -> Markup {
    if !is_fetchable(item) {
        return html! {};
    }
    let label = if fetching {
        "Fetching…"
    } else if has_data {
        "Fetch more"
    } else {
        "Fetch from Reddit"
    };
    let rpc = template_json_compact(&serde_json::json!({
        "action": "fetch_entity",
        "item": item.as_str(),
    }))
    .expect("fetch_entity rpc template");
    html! {
        form method="post" action="/ui" id="fetch-entity-form" class="fetch-entity-form" {
            input type="hidden" name=(UI_RPC_FIELD) value=(rpc);
            @if fetching {
                button type="submit" class="btn-secondary" disabled { (label) }
            } @else {
                button type="submit" class="btn-secondary" { (label) }
            }
        }
    }
}

/// Entity card + fetch control (target `#entity-section` for Idiomorph / SSE).
pub fn entity_section(item: &ItemId, node: &NodeState, fetching: bool) -> Markup {
    let has_data = node.data.is_some();
    html! {
        section id="entity-section" class="demo-panel" {
            (entity_panel(node))
            (fetch_entity_panel(item, has_data, fetching))
        }
    }
}
