//! Apply event-log records to the durable projection as precise point updates.
//!
//! Each batch of records lowers to reified durable writes (uuid vote upserts,
//! child links, recent-vote appends) plus a cursor advance, all committed in
//! one atomic `DisableWal` batch.

use crate::{
    event_log::EventLogError,
    events::{Event, EventRecord},
    identity::resolve_actor_uuid,
    path_types::ItemId,
    projection_store::ProjectionStore,
    reducer::VoteData,
    storage_schema::{ensure_path_writes, vote_writes},
};

fn parse_event_id(id: &str) -> Result<ItemId, EventLogError> {
    ItemId::from_storage(id)
        .or_else(|| ItemId::parse(id))
        .ok_or_else(|| EventLogError::Apply(format!("invalid id: {id}")))
}

/// Scope key from a vote event (canonicalized at apply time).
fn parent_from_event_scope(scope: &str) -> ItemId {
    let s = scope.trim();
    if s.is_empty() {
        return ItemId::root();
    }
    ItemId::from_storage(s)
        .or_else(|| ItemId::parse(s))
        .unwrap_or_else(|| ItemId::from_legacy_scope(s))
}

pub fn apply_records(
    projection_store: &ProjectionStore,
    records: &[EventRecord],
) -> Result<(), EventLogError> {
    if records.is_empty() {
        return Ok(());
    }

    let db = projection_store.db();
    let mut batch = db.batch();
    let mut last_seq = 0u64;

    for record in records {
        match &record.event {
            Event::VoteRecorded {
                ts,
                a,
                b,
                ratio_left,
                ratio_right,
                scope,
                pseudonym,
                trust_weight,
            } => {
                let left = (*ratio_left).max(0);
                let right = (*ratio_right).max(0);
                if left == 0 && right == 0 {
                    return Err(EventLogError::Apply(format!(
                        "invalid vote event: zero weights ({a} vs {b})"
                    )));
                }
                let vote = VoteData::from_event(
                    *ts,
                    a,
                    b,
                    left,
                    right,
                    pseudonym.clone(),
                    *trust_weight,
                )
                .ok_or_else(|| EventLogError::Apply(format!("invalid vote event: {a} vs {b}")))?;
                let actor_uuid = resolve_actor_uuid(db, pseudonym)
                    .map_err(|e| EventLogError::Apply(e))?;
                let parent = parent_from_event_scope(scope);
                vote_writes(&mut batch, &parent, &vote, &actor_uuid)
                    .map_err(|e| EventLogError::Apply(e.to_string()))?;
            }
            Event::NodeEnsured { id } => {
                let parsed = parse_event_id(id)?;
                ensure_path_writes(&mut batch, &parsed);
            }
        }
        last_seq = record.seq;
    }

    batch.write(projection_store.cursor_write(last_seq));
    batch
        .commit_with(durable::Durability::DisableWal)
        .map_err(|e| EventLogError::Apply(e.to_string()))?;

    Ok(())
}
