//! Apply event-log records to the durable projection as precise point updates.
//!
//! Each batch of records lowers to reified durable writes (edge merges, child
//! links, voted-pair flags, recent-vote pushes) plus a cursor advance, all
//! committed in one atomic `DisableWal` batch. The cursor moving in the same
//! batch as the (non-idempotent) edge merges guarantees exactly-once application
//! across replay.

use crate::{
    event_log::EventLogError,
    events::{Event, EventRecord},
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
            } => {
                let vote = VoteData::from_recorded(*ts, a, b, *ratio_left, *ratio_right)
                    .ok_or_else(|| EventLogError::Apply(format!("invalid vote event: {a} vs {b}")))?;
                let parent = parent_from_event_scope(scope);
                vote_writes(
                    &mut batch,
                    &parent,
                    vote.a.as_str(),
                    vote.b.as_str(),
                    *ratio_left,
                    *ratio_right,
                    *ts,
                )
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
