use crate::{
    entity_store::EntityStore,
    event_log::EventLogError,
    events::Event,
    path_types::ItemId,
    reddit::apply_entity_import,
    reducer::{GlobalTree, VoteData},
};

/// Legacy-compatible scope parsing for persisted vote events.
pub fn parent_from_event_scope(scope: &str) -> ItemId {
    if scope.contains('/') {
        ItemId::parse(scope).unwrap_or_else(|| ItemId::from_legacy_scope(scope))
    } else {
        ItemId::from_legacy_scope(scope)
    }
}

pub fn apply_event(
    ev: Event,
    tree: &mut GlobalTree,
    entity_store: &EntityStore,
) -> Result<(), EventLogError> {
    match ev {
        Event::VoteRecorded {
            ts,
            a,
            b,
            ratio_left,
            ratio_right,
            scope,
        } => {
            if let Some(vote) = VoteData::from_recorded(ts, &a, &b, ratio_left, ratio_right) {
                let parent = parent_from_event_scope(&scope);
                tree.apply_vote(&parent, vote);
            }
        }
        Event::NodeEnsured { id } => {
            if let Some(parsed) = ItemId::parse(&id).or_else(|| ItemId::from_url(&id)) {
                tree.ensure_path(&parsed);
            }
        }
        Event::EntityImported { id, payload, .. } => {
            if let Some(parsed) = ItemId::parse(&id).or_else(|| ItemId::from_url(&id)) {
                if let Err(e) = apply_entity_import(tree, entity_store, &parsed, payload) {
                    tracing::warn!(item = %id, err = %e, "entity replay failed");
                }
            }
        }
    }
    Ok(())
}
