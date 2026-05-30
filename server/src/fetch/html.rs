//! Markup for entity import / “Fetch from Reddit” (`POST /ui`, SSE response).

use maud::{html, Markup};

use crate::{
    form_template::template_json_compact,
    path_types::ItemId,
    reddit::{is_children_fetchable, is_fetchable},
    reducer::NodeState,
    ui_action::UI_RPC_FIELD,
};

fn entity_panel(node: &NodeState) -> Markup {
    if let Some(markup) = crate::render::reddit::entity_markup(node) {
        return markup;
    }
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

/// One `fetch_entity` form/button targeting `kind` ("self" or "children").
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
pub fn fetch_entity_panel(item: &ItemId, has_data: bool, fetching: bool) -> Markup {
    let self_ok = is_fetchable(item);
    let children_ok = is_children_fetchable(item);
    if !self_ok && !children_ok {
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
