//! Per-user item skips used by comparison pools and ranking views.

use std::collections::HashSet;

use axum_extra::extract::cookie::CookieJar;

use crate::{
    auth::session::{load_valid_session, session_id_from_jar},
    nsfw::visible_children,
    path_types::ItemId,
    projection_store::ProjectionStore,
    reducer::GlobalTree,
};

/// Load the active user's skipset. Signed-out and expired sessions have none.
pub fn for_jar(store: &ProjectionStore, jar: &CookieJar) -> HashSet<ItemId> {
    let Some(session_id) = session_id_from_jar(jar) else {
        return HashSet::new();
    };
    let Some(session) = load_valid_session(store.db(), &session_id) else {
        return HashSet::new();
    };
    store.user_skips(&session.uuid).unwrap_or_default()
}

/// Children visible in the current content dimension and not skipped by this user.
pub fn visible_unskipped_children(
    tree: &GlobalTree,
    parent: &ItemId,
    nsfw_ok: bool,
    skipped: &HashSet<ItemId>,
) -> Vec<ItemId> {
    visible_children(tree, parent, nsfw_ok)
        .into_iter()
        .filter(|item| !skipped.contains(item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::{GlobalTree, NodeState};

    #[test]
    fn skipped_children_are_removed_from_visible_pool() {
        let parent = ItemId::opaque("parent");
        let alpha = ItemId::opaque("alpha");
        let beta = ItemId::opaque("beta");
        let mut tree = GlobalTree::new();
        tree.nodes.insert(
            parent.clone(),
            NodeState {
                id: parent.clone(),
                children: [alpha.clone(), beta.clone()].into_iter().collect(),
                ..Default::default()
            },
        );

        let visible =
            visible_unskipped_children(&tree, &parent, false, &[alpha].into_iter().collect());
        assert_eq!(visible, vec![beta]);
    }
}
