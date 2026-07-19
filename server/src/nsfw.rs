//! NSFW content dimension: cookie opt-in and strict listing boundaries.
//!
//! Reddit `over_18` / `over18` is stored on ephemeral entity data. An item is
//! treated as NSFW if it or any ancestor in the loaded tree is marked `over_18`.
//! Listings, rankings, and vote pools omit NSFW items unless the browser has
//! opted in via the `sorter2_nsfw` cookie ("Yes, I am 18+").

use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

use crate::{
    auth::config,
    path_types::ItemId,
    reducer::{EntityData, GlobalTree},
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

pub fn entity_is_nsfw(data: &EntityData) -> bool {
    data.over_18
}

/// True if this item or any loaded ancestor is marked NSFW.
///
/// Posts imported from a listing carry their own `over_18` even when the
/// parent subreddit is still a data-less stub. When the parent *is* known
/// NSFW, children inherit that status here even if their own flag was lost
/// (e.g. after ephemeral eviction).
pub fn item_is_nsfw(tree: &GlobalTree, id: &ItemId) -> bool {
    let mut cur = Some(id.clone());
    while let Some(item) = cur {
        if tree
            .get(&item)
            .and_then(|n| n.data.as_ref())
            .is_some_and(entity_is_nsfw)
        {
            return true;
        }
        cur = item.parent();
    }
    false
}

/// Children visible in the current dimension (SFW-only unless `nsfw_ok`).
pub fn visible_children(tree: &GlobalTree, parent: &ItemId, nsfw_ok: bool) -> Vec<ItemId> {
    let Some(node) = tree.get(parent) else {
        return Vec::new();
    };
    let mut children: Vec<ItemId> = node
        .children
        .iter()
        .filter(|c| nsfw_ok || !item_is_nsfw(tree, c))
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
        let post =
            ItemId::from_url("https://reddit.com/r/nsfw/comments/abc/adult").unwrap();
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
        let post =
            ItemId::from_url("https://reddit.com/r/gonewild/comments/abc/x").unwrap();
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
