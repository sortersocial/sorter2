//! NSFW content dimension: cookie opt-in and strict listing boundaries.
//!
//! Reddit `over_18` / `over18` is stored on ephemeral entity data. An item is
//! treated as NSFW if it or any ancestor in the loaded tree is marked `over_18`.
//! Listings, rankings, and vote pools omit NSFW items unless the browser has
//! opted in via the `sorter2_nsfw` cookie ("Yes, I am 18+").

use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use maud::{html, Markup};

use crate::{
    auth::config,
    path_types::ItemId,
    reducer::{EntityData, GlobalTree, NodeState},
};

pub const NSFW_COOKIE: &str = "sorter2_nsfw";

/// Whether the browser has opted into the NSFW dimension.
pub fn nsfw_allowed(jar: &CookieJar) -> bool {
    jar.get(NSFW_COOKIE)
        .map(|c| matches!(c.value(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub fn nsfw_enter_cookie() -> Cookie<'static> {
    let mut builder = Cookie::build((NSFW_COOKIE, "1".to_string()))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/");
    if config::cookies_secure() {
        builder = builder.secure(true);
    }
    builder.build()
}

pub fn nsfw_leave_cookie() -> Cookie<'static> {
    let mut builder = Cookie::build((NSFW_COOKIE, ""))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .removal();
    if config::cookies_secure() {
        builder = builder.secure(true);
    }
    builder.build()
}

/// Opt-in form posting to `/nsfw/enter` (sets the dimension cookie, then redirects).
pub fn nsfw_enter_form(return_to: &str) -> Markup {
    html! {
        form method="post" action="/nsfw/enter" data-navigate="full" {
            input type="hidden" name="return_to" value=(return_to);
            button type="submit" class="btn-primary" data-testid="nsfw-enter" {
                "Yes, I am 18+"
            }
        }
    }
}

/// Full-page age gate when the current browse/vote URL is in the NSFW dimension.
pub fn nsfw_enter_panel(return_to: &str) -> Markup {
    html! {
        div class="nsfw-gate-panel demo-panel" data-testid="nsfw-enter-panel" {
            h2 { "NSFW dimension" }
            p {
                "This page contains adult content. Nothing NSFW is listed or shown until you confirm you are 18 or older."
            }
            (nsfw_enter_form(return_to))
        }
    }
}

/// Inline failsafe when an entity card would otherwise render NSFW payloads.
/// Always includes the same opt-in CTA as the page gate.
pub fn nsfw_entity_gate(return_to: &str) -> Markup {
    html! {
        div class="nsfw-gate" data-testid="nsfw-gate" {
            span class="nsfw-badge" { "NSFW" }
            p { "NSFW content is hidden until you opt in." }
            (nsfw_enter_form(return_to))
        }
    }
}

pub fn entity_is_nsfw(data: &EntityData) -> bool {
    data.over_18
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsfwStatus {
    Safe,
    Nsfw,
    /// Reddit entity metadata has not been fetched/classified yet.
    Unknown,
}

pub fn node_nsfw_status(node: &NodeState) -> NsfwStatus {
    if node.nsfw_classification == Some(true) || node.data.as_ref().is_some_and(entity_is_nsfw) {
        NsfwStatus::Nsfw
    } else if node.nsfw_classification == Some(false) || node.data.is_some() {
        NsfwStatus::Safe
    } else {
        NsfwStatus::Unknown
    }
}

pub fn node_is_nsfw(node: &NodeState) -> bool {
    node_nsfw_status(node) == NsfwStatus::Nsfw
}

fn needs_reddit_classification(id: &ItemId) -> bool {
    crate::reddit::is_fetchable(id)
}

/// Classification inherited from this Reddit entity and its concrete Reddit
/// ancestors. Unknown metadata fails closed at listing boundaries.
pub fn item_nsfw_status(tree: &GlobalTree, id: &ItemId) -> NsfwStatus {
    if !needs_reddit_classification(id) {
        return NsfwStatus::Safe;
    }

    let mut unknown = false;
    let mut cur = Some(id.clone());
    while let Some(item) = cur {
        if needs_reddit_classification(&item) {
            match tree.get(&item).map(node_nsfw_status) {
                Some(NsfwStatus::Nsfw) => return NsfwStatus::Nsfw,
                Some(NsfwStatus::Safe) => {}
                Some(NsfwStatus::Unknown) | None => unknown = true,
            }
        }
        cur = item.parent();
    }
    if unknown {
        NsfwStatus::Unknown
    } else {
        NsfwStatus::Safe
    }
}

pub fn item_is_nsfw(tree: &GlobalTree, id: &ItemId) -> bool {
    item_nsfw_status(tree, id) == NsfwStatus::Nsfw
}

/// Lightweight NSFW check that walks ancestors via `load_node` instead of
/// materializing a full scope tree (used on the vote write path).
pub fn item_nsfw_status_in_store(
    store: &crate::projection_store::ProjectionStore,
    id: &ItemId,
) -> NsfwStatus {
    if !needs_reddit_classification(id) {
        return NsfwStatus::Safe;
    }

    let mut unknown = false;
    let mut cur = Some(id.clone());
    while let Some(item) = cur {
        if needs_reddit_classification(&item) {
            match store.load_node(&item) {
                Ok(Some(node)) => match node_nsfw_status(&node) {
                    NsfwStatus::Nsfw => return NsfwStatus::Nsfw,
                    NsfwStatus::Safe => {}
                    NsfwStatus::Unknown => unknown = true,
                },
                Ok(None) | Err(_) => unknown = true,
            }
        }
        cur = item.parent();
    }
    if unknown {
        NsfwStatus::Unknown
    } else {
        NsfwStatus::Safe
    }
}

pub fn item_is_nsfw_in_store(
    store: &crate::projection_store::ProjectionStore,
    id: &ItemId,
) -> bool {
    item_nsfw_status_in_store(store, id) == NsfwStatus::Nsfw
}

pub fn item_is_visible(status: NsfwStatus, nsfw_ok: bool) -> bool {
    match status {
        NsfwStatus::Safe => true,
        NsfwStatus::Nsfw => nsfw_ok,
        NsfwStatus::Unknown => false,
    }
}

/// Children visible in the current dimension (SFW-only unless `nsfw_ok`).
pub fn visible_children(tree: &GlobalTree, parent: &ItemId, nsfw_ok: bool) -> Vec<ItemId> {
    let Some(node) = tree.get(parent) else {
        return Vec::new();
    };
    let mut children: Vec<ItemId> = node
        .children
        .iter()
        .filter(|c| item_is_visible(item_nsfw_status(tree, c), nsfw_ok))
        .cloned()
        .collect();
    children.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    children
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::{EntityData, GlobalTree, NodeState};

    fn nsfw_data(title: &str) -> EntityData {
        EntityData {
            title: title.into(),
            author: None,
            body_html: None,
            over_18: true,
            thumb_url: None,
            image_url: None,
            link_url: None,
        }
    }

    fn sfw_data(title: &str) -> EntityData {
        EntityData {
            title: title.into(),
            author: None,
            body_html: None,
            over_18: false,
            thumb_url: None,
            image_url: None,
            link_url: None,
        }
    }

    #[test]
    fn post_over_18_is_nsfw_even_when_parent_is_stub() {
        let parent = ItemId::from_url("https://reddit.com/r/nsfw").unwrap();
        let post = ItemId::from_url("https://reddit.com/r/nsfw/comments/abc/adult").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                data: None, // stub — about never fetched
                children: [post.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            post.clone(),
            NodeState {
                id: post.clone(),
                data: Some(nsfw_data("adult")),
                ..Default::default()
            },
        );
        assert!(item_is_nsfw(&tree, &post));
        assert!(!item_is_nsfw(&tree, &parent));
    }

    #[test]
    fn child_inherits_nsfw_from_fetched_parent() {
        let parent = ItemId::from_url("https://reddit.com/r/gonewild").unwrap();
        let post = ItemId::from_url("https://reddit.com/r/gonewild/comments/abc/x").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                data: Some(nsfw_data("gonewild")),
                children: [post.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            post.clone(),
            NodeState {
                id: post.clone(),
                data: Some(sfw_data("missing flag")), // flag lost / absent
                ..Default::default()
            },
        );
        assert!(item_is_nsfw(&tree, &post));
    }

    #[test]
    fn child_inherits_durable_nsfw_after_parent_content_is_evicted() {
        let parent = ItemId::from_url("https://reddit.com/r/gonewild").unwrap();
        let post = ItemId::from_url("https://reddit.com/r/gonewild/comments/abc/x").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                data: None,
                nsfw_classification: Some(true),
                children: [post.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            post.clone(),
            NodeState {
                id: post.clone(),
                data: None,
                nsfw_classification: Some(false),
                ..Default::default()
            },
        );

        assert_eq!(item_nsfw_status(&tree, &post), NsfwStatus::Nsfw);
        assert!(visible_children(&tree, &parent, false).is_empty());
        assert_eq!(visible_children(&tree, &parent, true), vec![post]);
    }

    #[test]
    fn unknown_reddit_children_fail_closed_even_with_opt_in() {
        let parent = ItemId::from_url("https://reddit.com/r/mixed").unwrap();
        let post = ItemId::from_url("https://reddit.com/r/mixed/comments/abc/x").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                nsfw_classification: Some(false),
                children: [post.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            post.clone(),
            NodeState {
                id: post.clone(),
                ..Default::default()
            },
        );

        assert_eq!(item_nsfw_status(&tree, &post), NsfwStatus::Unknown);
        assert!(visible_children(&tree, &parent, false).is_empty());
        assert!(visible_children(&tree, &parent, true).is_empty());
    }

    #[test]
    fn visible_children_hides_nsfw_unless_opted_in() {
        let parent = ItemId::from_url("https://reddit.com/r/mixed").unwrap();
        let sfw = ItemId::from_url("https://reddit.com/r/mixed/comments/1/a").unwrap();
        let nsfw = ItemId::from_url("https://reddit.com/r/mixed/comments/2/b").unwrap();
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                data: Some(sfw_data("mixed")),
                children: [sfw.clone(), nsfw.clone()].into_iter().collect(),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            sfw.clone(),
            NodeState {
                id: sfw.clone(),
                data: Some(sfw_data("safe")),
                ..Default::default()
            },
        );
        tree.nodes.insert(
            nsfw.clone(),
            NodeState {
                id: nsfw.clone(),
                data: Some(nsfw_data("adult")),
                ..Default::default()
            },
        );
        let hidden = visible_children(&tree, &parent, false);
        assert_eq!(hidden, vec![sfw.clone()]);
        let shown = visible_children(&tree, &parent, true);
        assert_eq!(shown.len(), 2);
        assert!(shown.contains(&nsfw));
    }
}
