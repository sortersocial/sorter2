use std::collections::BTreeSet;

use serde_json::Value;

use crate::{
    entity_store::EntityStore,
    event_log::EventLogError,
    event_reducer,
    events::{Event, EventRecord},
    path_types::ItemId,
    projection_store::{self, ProjectionStore},
    reducer::GlobalTree,
};

pub fn apply_event(
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    event_seq: u64,
    ev: &Event,
) -> Result<(), EventLogError> {
    let record = EventRecord::new(event_seq, crate::events::event_timestamp(ev), ev.clone());
    apply_records(projection_store, entity_store, &[record])
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

pub fn apply_records(
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    records: &[EventRecord],
) -> Result<(), EventLogError> {
    if records.is_empty() {
        return Ok(());
    }

    let mut tree = GlobalTree::default();
    let mut affected = BTreeSet::<ItemId>::new();
    let mut entity_payloads = Vec::<(ItemId, Value)>::new();
    let mut last_seq = 0;

    for record in records {
        projection_store
            .hydrate_event(&mut tree, &record.event)
            .map_err(|e| EventLogError::Apply(e.to_string()))?;
        let effects = event_reducer::apply_event(&record.event, &mut tree)?;
        affected.extend(projection_store::affected_nodes(&record.event));
        entity_payloads.extend(effects.entity_payloads);
        last_seq = record.seq;
    }

    projection_store
        .persist_batch(
            &tree,
            last_seq,
            affected,
            &entity_payloads,
            Some(entity_store),
        )
        .map_err(|e| EventLogError::Apply(e.to_string()))
}
