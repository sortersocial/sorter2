use crate::{
    event_log::EventLogError,
    events::Event,
    path_types::ItemId,
    reddit::entity_view_from_payload,
    reducer::{GlobalTree, VoteData},
};
use serde_json::Value;

/// Legacy-compatible scope parsing for persisted vote events.
pub fn parent_from_event_scope(scope: &str) -> ItemId {
    if scope.contains('/') {
        ItemId::parse(scope).unwrap_or_else(|| ItemId::from_legacy_scope(scope))
    } else {
        ItemId::from_legacy_scope(scope)
    }
}

#[derive(Debug, Default)]
pub struct ProjectionEffects {
    pub entity_payloads: Vec<(ItemId, Value)>,
}

pub fn apply_event(ev: &Event, tree: &mut GlobalTree) -> Result<ProjectionEffects, EventLogError> {
    let mut effects = ProjectionEffects::default();
    match ev {
        Event::VoteRecorded {
            ts,
            a,
            b,
            ratio_left,
            ratio_right,
            scope,
        } => {
            if let Some(vote) = VoteData::from_recorded(*ts, a, b, *ratio_left, *ratio_right) {
                let parent = parent_from_event_scope(&scope);
                tree.apply_vote(&parent, vote);
            } else {
                return Err(EventLogError::Apply(format!(
                    "invalid vote event: {a} vs {b}"
                )));
            }
        }
        Event::NodeEnsured { id } => {
            if let Some(parsed) = ItemId::parse(id).or_else(|| ItemId::from_url(id)) {
                tree.ensure_path(&parsed);
            } else {
                return Err(EventLogError::Apply(format!("invalid node id: {id}")));
            }
        }
        Event::EntityImported { id, payload, .. } => {
            let parsed = ItemId::parse(id)
                .or_else(|| ItemId::from_url(id))
                .ok_or_else(|| EventLogError::Apply(format!("invalid entity id: {id}")))?;
            let view = entity_view_from_payload(&parsed, payload);
            tree.apply_entity(&parsed, view);
            effects.entity_payloads.push((parsed, payload.clone()));
        }
    }
    Ok(effects)
}
