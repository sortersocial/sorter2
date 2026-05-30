//! Reddit post cards: thumbnail in child lists, full image on the post page.

use maud::{html, Markup};

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GlobalTree, NodeState},
};

pub fn is_reddit_post(id: &ItemId) -> bool {
    id.as_str().starts_with("reddit.com/") && id.as_str().contains("/comments/")
}

/// Post detail card (`#entity-panel`).
pub fn entity_markup(node: &NodeState) -> Option<Markup> {
    if !is_reddit_post(&node.id) {
        return None;
    }
    let data = node.data.as_ref()?;
    Some(post_entity_card(data))
}

/// One row in a parent ranking list (thumbnail + title).
pub fn child_row_markup(tree: &GlobalTree, id: &ItemId, href: &str) -> Option<Markup> {
    if !is_reddit_post(id) {
        return None;
    }
    let data = tree.get(id)?.data.as_ref()?;
    Some(html! {
        @if let Some(thumb) = &data.thumb_url {
            a class="reddit-post-thumb-link" href=(href) {
                img class="reddit-post-thumb" src=(thumb) alt="" loading="lazy";
            }
        }
        a href=(href) {
            strong { (data.title) }
        }
    })
}

fn post_entity_card(data: &EntityData) -> Markup {
    let image = data.image_url.as_ref().or(data.thumb_url.as_ref());
    html! {
        div id="entity-panel" class="entity-card reddit-post" {
            h2 { (data.title) }
            @if let Some(author) = &data.author {
                p class="muted small" { "by " (author) }
            }
            @if let Some(url) = &data.link_url {
                p class="reddit-post-url muted small" {
                    a href=(url) rel="noopener noreferrer" { (url) }
                }
            }
            @if let Some(src) = image {
                figure class="reddit-post-figure" {
                    img class="reddit-post-image" src=(src) alt="" loading="lazy";
                }
            }
            @if let Some(body) = &data.body_html {
                div class="entity-body" { (maud::PreEscaped(body)) }
            }
        }
    }
}
