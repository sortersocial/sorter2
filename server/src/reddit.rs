//! Reddit API import (async, decoupled from UI request path).

use crate::{
    path_types::ItemId,
    reducer::{EntityData, GlobalTree},
};

/// Bootstrap blank nodes along a URL path so breadcrumbs and voting work before fetch.
pub fn ensure_partial_tree(tree: &mut GlobalTree, id: &ItemId) {
    tree.ensure_path(id);
}

/// Placeholder for Reddit JSON import. Returns entity data when implemented.
pub async fn fetch_reddit_entity(_id: &ItemId) -> Option<EntityData> {
    None
}

/// Apply fetched entity data to a node (called from async worker).
pub fn apply_entity(tree: &mut GlobalTree, id: &ItemId, data: EntityData) {
    tree.set_entity_data(id, data);
}
