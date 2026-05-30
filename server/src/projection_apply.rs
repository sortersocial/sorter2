use crate::{
    entity_store::EntityStore, event_log::EventLogError, event_reducer, events::Event,
    projection_store::ProjectionStore, reducer::GlobalTree, views::ViewStore,
};

pub fn apply_event(
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    view_store: &ViewStore,
    event_count: u64,
    ev: &Event,
) -> Result<(), EventLogError> {
    if let Event::ViewRecorded { path, .. } = ev {
        view_store
            .record_view(path.clone())
            .map_err(|e| EventLogError::Apply(e.to_string()))?;
        projection_store
            .persist_event(&GlobalTree::new(), event_count, ev)
            .map_err(|e| EventLogError::Apply(e.to_string()))?;
        return Ok(());
    }

    let mut tree = GlobalTree::new();
    projection_store
        .hydrate_event(&mut tree, ev)
        .map_err(|e| EventLogError::Apply(e.to_string()))?;
    event_reducer::apply_event(ev.clone(), &mut tree, entity_store)?;
    projection_store
        .persist_event(&tree, event_count, ev)
        .map_err(|e| EventLogError::Apply(e.to_string()))
}

pub fn apply_next_event(
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    view_store: &ViewStore,
    ev: &Event,
) -> Result<u64, EventLogError> {
    let event_count = projection_store
        .last_applied_event_count()
        .map_err(|e| EventLogError::Apply(e.to_string()))?
        + 1;
    apply_event(projection_store, entity_store, view_store, event_count, ev)?;
    Ok(event_count)
}
