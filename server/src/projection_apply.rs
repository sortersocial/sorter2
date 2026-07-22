//! Apply event-log records to the durable projection as precise point updates.
//!
//! Each batch of records lowers to reified durable writes (uuid vote upserts,
//! child links, recent-vote appends) plus a cursor advance, all committed in
//! one atomic `DisableWal` batch.

use std::collections::HashMap;

use crate::{
    auth::identity::{trust_weight_after_link, BASE_TRUST_WEIGHT},
    event_log::EventLogError,
    events::{Event, EventRecord},
    path_types::ItemId,
    projection_store::ProjectionStore,
    reducer::VoteData,
    storage_schema::{
        ensure_path_writes, nsfw_classification_writes, oauth_link_key, pseudonym_owner,
        skip_item_write, unskip_item_write, vote_writes, Store, StoreFields,
    },
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
    // Weight reads must see earlier writes in this same batch.
    let mut pending_weights: HashMap<String, f64> = HashMap::new();

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
                let vote =
                    VoteData::from_event(*ts, a, b, left, right, pseudonym.clone(), *trust_weight)
                        .ok_or_else(|| {
                            EventLogError::Apply(format!("invalid vote event: {a} vs {b}"))
                        })?;
                let actor_uuid = crate::identity::resolve_actor_uuid(db, pseudonym)
                    .map_err(|e| EventLogError::Apply(e))?;
                let parent = parent_from_event_scope(scope);
                vote_writes(&mut batch, &parent, &vote, &actor_uuid)
                    .map_err(|e| EventLogError::Apply(e.to_string()))?;
            }
            Event::NodeEnsured { id } => {
                let parsed = parse_event_id(id)?;
                ensure_path_writes(&mut batch, &parsed);
            }
            Event::NsfwClassified { id, over_18 } => {
                let parsed = parse_event_id(id)?;
                nsfw_classification_writes(&mut batch, &parsed, *over_18);
            }
            Event::PrincipalCreated { uuid, .. } => {
                pending_weights.insert(uuid.clone(), BASE_TRUST_WEIGHT);
                batch.write(
                    Store::root()
                        .user_weights()
                        .key(&uuid.clone())
                        .set(&BASE_TRUST_WEIGHT),
                );
            }
            Event::OauthLinked {
                uuid,
                provider,
                provider_id,
                ..
            } => {
                let link_key = oauth_link_key(provider, provider_id);
                if let Some(existing) = Store::root()
                    .oauth_links()
                    .key(&link_key)
                    .get(db)
                    .map_err(|e| EventLogError::Apply(e.to_string()))?
                {
                    if existing != *uuid {
                        return Err(EventLogError::Apply(format!(
                            "oauth link {link_key} already owned by {existing}"
                        )));
                    }
                } else {
                    batch.write(Store::root().oauth_links().key(&link_key).set(uuid));
                    let current = pending_weights
                        .get(uuid)
                        .copied()
                        .or_else(|| {
                            Store::root()
                                .user_weights()
                                .key(&uuid.clone())
                                .get(db)
                                .ok()
                                .flatten()
                        })
                        .unwrap_or(BASE_TRUST_WEIGHT);
                    let next = trust_weight_after_link(current);
                    pending_weights.insert(uuid.clone(), next);
                    batch.write(Store::root().user_weights().key(&uuid.clone()).set(&next));
                }
            }
            Event::PseudonymClaimed {
                uuid, pseudonym, ..
            } => {
                if let Some(owner) = pseudonym_owner(db, pseudonym)
                    .map_err(|e| EventLogError::Apply(e.to_string()))?
                {
                    if owner != *uuid {
                        return Err(EventLogError::Apply(format!(
                            "pseudonym {pseudonym} already claimed by {owner}"
                        )));
                    }
                } else {
                    batch.write(Store::root().pseudonyms().key(&pseudonym.clone()).set(uuid));
                    batch
                        .push(
                            &Store::root().user_pseudonyms().key(&uuid.clone()),
                            &pseudonym.clone(),
                        )
                        .map_err(|e| EventLogError::Apply(e.to_string()))?;
                }
            }
            Event::ItemSkipped { uuid, item, .. } => {
                let item = parse_event_id(item)?;
                skip_item_write(&mut batch, uuid, &item);
            }
            Event::ItemUnskipped { uuid, item, .. } => {
                let item = parse_event_id(item)?;
                unskip_item_write(&mut batch, uuid, &item);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::EventRecord,
        identity::resolve_actor_uuid,
        projection_store::ProjectionStore,
        storage_schema::{load_user_skips, oauth_link_owner, user_trust_weight, StoreFields},
    };

    fn record(seq: u64, event: Event) -> EventRecord {
        EventRecord::new(seq, crate::events::event_timestamp(&event), event)
    }

    #[test]
    fn identity_events_project_pseudonym_and_oauth_link() {
        let dir = tempfile::tempdir().unwrap();
        let db = durable::Db::open(dir.path()).unwrap();
        let store = ProjectionStore::from_db(&db).unwrap();
        let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let ts = 1;

        apply_records(
            &store,
            &[
                record(
                    1,
                    Event::PrincipalCreated {
                        uuid: uuid.into(),
                        ts,
                    },
                ),
                record(
                    2,
                    Event::OauthLinked {
                        uuid: uuid.into(),
                        provider: "github".into(),
                        provider_id: "42".into(),
                        ts,
                    },
                ),
                record(
                    3,
                    Event::OauthLinked {
                        uuid: uuid.into(),
                        provider: "reddit".into(),
                        provider_id: "t2_abc".into(),
                        ts,
                    },
                ),
                record(
                    4,
                    Event::PseudonymClaimed {
                        uuid: uuid.into(),
                        pseudonym: "octocat".into(),
                        ts,
                    },
                ),
            ],
        )
        .unwrap();

        assert_eq!(
            oauth_link_owner(store.db(), "github", "42").unwrap(),
            Some(uuid.to_string())
        );
        assert_eq!(
            crate::storage_schema::linked_providers_for_uuid(store.db(), uuid).unwrap(),
            vec!["github".to_string(), "reddit".to_string()]
        );
        assert_eq!(resolve_actor_uuid(store.db(), "octocat").unwrap(), uuid);
        assert_eq!(user_trust_weight(store.db(), uuid).unwrap(), 2.0);
        let aliases = Store::root()
            .user_pseudonyms()
            .key(&uuid.to_string())
            .iter(store.db())
            .unwrap();
        assert_eq!(aliases, vec!["octocat".to_string()]);
    }

    #[test]
    fn pseudonym_claim_rejects_second_owner() {
        let dir = tempfile::tempdir().unwrap();
        let db = durable::Db::open(dir.path()).unwrap();
        let store = ProjectionStore::from_db(&db).unwrap();

        apply_records(
            &store,
            &[record(
                1,
                Event::PseudonymClaimed {
                    uuid: "uuid-a".into(),
                    pseudonym: "taken".into(),
                    ts: 1,
                },
            )],
        )
        .unwrap();

        let err = apply_records(
            &store,
            &[record(
                2,
                Event::PseudonymClaimed {
                    uuid: "uuid-b".into(),
                    pseudonym: "taken".into(),
                    ts: 2,
                },
            )],
        )
        .unwrap_err();
        assert!(err.to_string().contains("already claimed"));
    }

    #[test]
    fn skip_events_are_isolated_by_user_and_reversible() {
        let dir = tempfile::tempdir().unwrap();
        let db = durable::Db::open(dir.path()).unwrap();
        let store = ProjectionStore::from_db(&db).unwrap();

        apply_records(
            &store,
            &[
                record(
                    1,
                    Event::ItemSkipped {
                        uuid: "user-a".into(),
                        item: "alpha".into(),
                        ts: 1,
                    },
                ),
                record(
                    2,
                    Event::ItemSkipped {
                        uuid: "user-b".into(),
                        item: "beta".into(),
                        ts: 2,
                    },
                ),
                record(
                    3,
                    Event::ItemUnskipped {
                        uuid: "user-a".into(),
                        item: "alpha".into(),
                        ts: 3,
                    },
                ),
            ],
        )
        .unwrap();

        assert!(load_user_skips(store.db(), "user-a").unwrap().is_empty());
        assert_eq!(
            load_user_skips(store.db(), "user-b").unwrap(),
            [ItemId::opaque("beta")].into_iter().collect()
        );
    }
}
