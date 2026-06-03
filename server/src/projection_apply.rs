use crate::{
    entity_store::EntityStore, event_log::EventLogError, event_reducer, events::Event,
    projection_store::ProjectionStore, reducer::GlobalTree,
};

pub fn apply_event(
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    event_seq: u64,
    ev: &Event,
) -> Result<(), EventLogError> {
    let mut tree = GlobalTree::new();
    projection_store
        .hydrate_event(&mut tree, ev)
        .map_err(|e| EventLogError::Apply(e.to_string()))?;
    event_reducer::apply_event(ev.clone(), &mut tree, entity_store)?;
    projection_store
        .persist_event(&tree, event_seq, ev)
        .map_err(|e| EventLogError::Apply(e.to_string()))
}

pub fn apply_next_event(
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    ev: &Event,
) -> Result<u64, EventLogError> {
    let event_seq = projection_store
        .last_applied_event_count()
        .map_err(|e| EventLogError::Apply(e.to_string()))?
        + 1;
    apply_event(projection_store, entity_store, event_seq, ev)?;
    Ok(event_seq)
}
