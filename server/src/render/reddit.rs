//! Reddit post cards: thumbnail in child lists, full image on the post page.

use maud::{html, Markup};

use crate::{
    html::sanitize::entity_body_html,
    nsfw::{item_nsfw_status, node_is_nsfw, nsfw_entity_gate, NsfwStatus},
    path_types::ItemId,
    reducer::{EntityData, GlobalTree, NodeState},
};

pub fn is_reddit_post(id: &ItemId) -> bool {
    id.as_str().contains("reddit.com/") && id.as_str().contains("/comments/")
}

/// Post detail card (inside [`crate::fetch::html::entity_panel`]).
///
/// When `nsfw_ok` is false and the post is NSFW, callers should show the age
/// gate instead — this function still refuses to emit media/body/links and
/// includes the opt-in CTA so "until you opt in" is never a dead end.
pub fn entity_markup(node: &NodeState, nsfw_ok: bool) -> Option<Markup> {
    if !is_reddit_post(&node.id) {
        return None;
    }
    let data = node.data.as_ref()?;
    Some(post_entity_card(
        data,
        node_is_nsfw(node),
        nsfw_ok,
        &node.id.browse_href(),
    ))
}

/// One row in a parent ranking list (thumbnail + title).
/// NSFW items are expected to already be filtered from the list when not opted in;
/// when `nsfw_ok` is true, NSFW rows show the same thumbnail treatment as SFW.
pub fn child_row_markup(tree: &GlobalTree, id: &ItemId, href: &str, nsfw_ok: bool) -> Option<Markup> {
    if !is_reddit_post(id) {
        return None;
    }
    let data = tree.get(id)?.data.as_ref()?;
    let status = item_nsfw_status(tree, id);
    let show_thumb = match status {
        NsfwStatus::Unknown => false,
        NsfwStatus::Nsfw => nsfw_ok,
        NsfwStatus::Safe => true,
    };
    Some(html! {
        @if status == NsfwStatus::Unknown {
            span class="muted small" { "Unclassified Reddit item" }
        } @else {
            @if status == NsfwStatus::Nsfw {
                span class="nsfw-badge" { "NSFW" }
            }
            @if show_thumb {
                @if let Some(thumb) = &data.thumb_url {
                    a class="reddit-post-thumb-link" href=(href) {
                        img class="reddit-post-thumb" src=(thumb) alt="" loading="lazy";
                    }
                }
            }
            a href=(href) {
                strong { (data.title) }
            }
        }
    })
}

fn post_entity_card(data: &EntityData, is_nsfw: bool, nsfw_ok: bool, return_to: &str) -> Markup {
    let image = data.image_url.as_ref().or(data.thumb_url.as_ref());
    let gated = is_nsfw && !nsfw_ok;
    if gated {
        return nsfw_entity_gate(return_to);
    }
    html! {
        div class="entity-card reddit-post" {
            h2 { (data.title) }
            @if let Some(author) = &data.author {
                p class="muted small" { "by " (author) }
            }
            @if is_nsfw {
                p class="muted small" {
                    span class="nsfw-badge" { "NSFW" }
                    " adult Reddit content"
                }
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
    fn nsfw_entity_card_hides_media_until_opted_in() {
        let html = entity_markup(&nsfw_node(), false).unwrap().into_string();
        assert!(html.contains("NSFW"));
        assert!(html.contains("nsfw-gate"));
        assert!(
            html.contains("Yes, I am 18+"),
            "soft gate must offer opt-in: {html}"
        );
        assert!(html.contains("action=\"/nsfw/enter\""));
        assert!(!html.contains("adult post"));
        assert!(!html.contains("adult body"));
        assert!(!html.contains("https://example.com/image.jpg"));
        assert!(!html.contains("https://example.com/out"));
    }

    #[test]
    fn nsfw_entity_card_renders_media_when_opted_in() {
        let html = entity_markup(&nsfw_node(), true).unwrap().into_string();
        assert!(html.contains("adult body"));
        assert!(html.contains("https://example.com/image.jpg"));
        assert!(html.contains("https://example.com/out"));
        assert!(!html.contains("nsfw-gate"));
    }

    #[test]
    fn inherited_nsfw_child_never_renders_a_thumbnail() {
        let parent = ItemId::from_url("https://reddit.com/r/nsfw").unwrap();
        let post = ItemId::from_url("https://reddit.com/r/nsfw/comments/abc/adult").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                nsfw_classification: Some(true),
                children: [post.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            post.clone(),
            NodeState {
                id: post.clone(),
                nsfw_classification: Some(false),
                data: Some(EntityData {
                    title: "inherited adult post".into(),
                    author: None,
                    body_html: None,
                    over_18: false,
                    thumb_url: Some("https://example.com/should-not-load.jpg".into()),
                    image_url: None,
                    link_url: None,
                }),
                ..Default::default()
            },
        );

        let html = child_row_markup(&tree, &post, "/post", false)
            .unwrap()
            .into_string();
        assert!(html.contains("NSFW"));
        assert!(html.contains("inherited adult post"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("should-not-load.jpg"));
    }

    #[test]
    fn nsfw_child_shows_thumbnail_when_opted_in() {
        let parent = ItemId::from_url("https://reddit.com/r/nsfw").unwrap();
        let post = ItemId::from_url("https://reddit.com/r/nsfw/comments/abc/adult").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                nsfw_classification: Some(true),
                children: [post.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            post.clone(),
            NodeState {
                id: post.clone(),
                nsfw_classification: Some(true),
                data: Some(EntityData {
                    title: "adult post".into(),
                    author: None,
                    body_html: None,
                    over_18: true,
                    thumb_url: Some("https://example.com/thumb.jpg".into()),
                    image_url: None,
                    link_url: None,
                }),
                ..Default::default()
            },
        );

        let html = child_row_markup(&tree, &post, "/post", true)
            .unwrap()
            .into_string();
        assert!(html.contains("NSFW"));
        assert!(html.contains("adult post"));
        assert!(html.contains("https://example.com/thumb.jpg"));
        assert!(html.contains("href=\"/post\""));
    }
}
