//! Reddit post cards: thumbnail in child lists, full image on the post page.

use maud::{html, Markup};

use crate::{
    html::sanitize::entity_body_html,
    path_types::ItemId,
    reducer::{EntityData, GlobalTree, NodeState},
};

pub fn is_reddit_post(id: &ItemId) -> bool {
    id.as_str().contains("reddit.com/") && id.as_str().contains("/comments/")
}

/// Post detail card (inside [`crate::fetch::html::entity_panel`]).
pub fn entity_markup(node: &NodeState, reveal_nsfw: bool) -> Option<Markup> {
    if !is_reddit_post(&node.id) {
        return None;
    }
    let data = node.data.as_ref()?;
    Some(post_entity_card(&node.id, data, reveal_nsfw))
}

/// One row in a parent ranking list (thumbnail + title).
pub fn child_row_markup(tree: &GlobalTree, id: &ItemId, href: &str) -> Option<Markup> {
    if !is_reddit_post(id) {
        return None;
    }
    let data = tree.get(id)?.data.as_ref()?;
    Some(html! {
        @if data.over_18 {
            span class="nsfw-badge" { "NSFW" }
        } @else if let Some(thumb) = &data.thumb_url {
            a class="reddit-post-thumb-link" href=(href) {
                img class="reddit-post-thumb" src=(thumb) alt="" loading="lazy";
            }
        }
        a href=(href) {
            strong { (data.title) }
        }
    })
}

fn post_entity_card(id: &ItemId, data: &EntityData, reveal_nsfw: bool) -> Markup {
    let image = data.image_url.as_ref().or(data.thumb_url.as_ref());
    let gated = data.over_18 && !reveal_nsfw;
    let reveal_href = format!("{}?reveal_nsfw=1", id.browse_href());
    html! {
        div class="entity-card reddit-post" {
            h2 { (data.title) }
            @if let Some(author) = &data.author {
                p class="muted small" { "by " (author) }
            }
            @if data.over_18 {
                p class="muted small" {
                    span class="nsfw-badge" { "NSFW" }
                    " adult Reddit content"
                }
            }
            @if gated {
                div class="nsfw-gate" {
                    p { "Media, body text, and outbound links are hidden until you choose to reveal this NSFW post." }
                    a class="btn-secondary" href=(reveal_href) { "Reveal NSFW post" }
                }
            } @else {
                @if data.over_18 {
                    p class="muted small" { "NSFW content revealed for this page view." }
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
                    div class="entity-body" { (maud::PreEscaped(entity_body_html(body))) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nsfw_node() -> NodeState {
        NodeState {
            id: ItemId::from_url("https://reddit.com/r/nsfw/comments/abc/adult").unwrap(),
            data: Some(EntityData {
                title: "adult post".into(),
                author: Some("alice".into()),
                body_html: Some("<p>adult body</p>".into()),
                over_18: true,
                thumb_url: Some("https://example.com/thumb.jpg".into()),
                image_url: Some("https://example.com/image.jpg".into()),
                link_url: Some("https://example.com/out".into()),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn nsfw_entity_card_hides_media_until_revealed() {
        let html = entity_markup(&nsfw_node(), false).unwrap().into_string();
        assert!(html.contains("NSFW"));
        assert!(html.contains("Reveal NSFW post"));
        assert!(!html.contains("adult body"));
        assert!(!html.contains("https://example.com/image.jpg"));
        assert!(!html.contains("https://example.com/out"));
    }

    #[test]
    fn nsfw_entity_card_renders_media_when_revealed() {
        let html = entity_markup(&nsfw_node(), true).unwrap().into_string();
        assert!(html.contains("NSFW content revealed"));
        assert!(html.contains("adult body"));
        assert!(html.contains("https://example.com/image.jpg"));
        assert!(html.contains("https://example.com/out"));
    }
}
