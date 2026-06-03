//! Single-writer pipeline: append to JSONL, then update durable projection.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::{
    entity_store::EntityStore, event_log::EventLog, events::{event_timestamp, Event, EventRecord},
    projection_apply, projection_store::ProjectionStore,
};

pub struct JournalCommand {
    pub event: Event,
    pub reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct JournalClient {
    tx: mpsc::Sender<JournalCommand>,
}

impl JournalClient {
    pub fn spawn(
        event_log: Arc<EventLog>,
        entity_store: EntityStore,
        projection_store: ProjectionStore,
    ) -> Self {
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(journal_worker(
            rx,
            event_log,
            entity_store,
            projection_store,
        ));
        Self { tx }
    }

    /// Append one event to the log and apply it to all projections (sole write path).
    pub async fn append(&self, event: Event) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(JournalCommand { event, reply })
            .await
            .map_err(|_| "journal worker stopped".to_string())?;
        rx.await.map_err(|_| "journal worker stopped".to_string())?
    }
}

async fn journal_worker(
    mut rx: mpsc::Receiver<JournalCommand>,
    event_log: Arc<EventLog>,
    entity_store: EntityStore,
    projection_store: ProjectionStore,
) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }

        for cmd in batch {
            let result = append_and_project(
                &event_log,
                &projection_store,
                &entity_store,
                &cmd.event,
            )
            .await;
            let _ = cmd.reply.send(result);
        }
    }
}

async fn append_and_project(
    event_log: &EventLog,
    projection_store: &ProjectionStore,
    entity_store: &EntityStore,
    event: &Event,
) -> Result<(), String> {
    let seq = projection_store
        .last_applied_event_count()
        .map_err(|e| e.to_string())?
        + 1;
    let record = EventRecord::new(seq, event_timestamp(event), event.clone());
    event_log.append(&record).await.map_err(|e| e.to_string())?;
    projection_apply::apply_event(projection_store, entity_store, seq, event)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{events::Event, path_types::ItemId, projection_store::ProjectionStore};

    #[tokio::test]
    async fn journal_serializes_concurrent_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let event_log = Arc::new(EventLog::new(log_path));
        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let entity_store = EntityStore::from_db(&db).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();

        let journal = JournalClient::spawn(
            event_log,
            entity_store,
            projection_store.clone(),
        );

        let j1 = journal.clone();
        let j2 = journal.clone();
        let (r1, r2) = tokio::join!(
            j1.append(Event::NodeEnsured {
                id: "reddit.com/r/rust".into(),
            }),
            j2.append(Event::NodeEnsured {
                id: "reddit.com/r/python".into(),
            }),
        );
        r1.unwrap();
        r2.unwrap();

        assert_eq!(projection_store.last_applied_event_count().unwrap(), 2);
        let tree = projection_store.load_tree().unwrap();
        assert!(tree.get(&ItemId::parse("reddit.com/r/rust").unwrap()).is_some());
        assert!(tree
            .get(&ItemId::parse("reddit.com/r/python").unwrap())
            .is_some());
    }
}
