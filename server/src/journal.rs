//! Single-writer pipeline: append to JSONL, then update durable projection.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::{
    event_log::EventLog,
    events::{event_timestamp, Event, EventRecord},
    projection_apply,
    projection_store::ProjectionStore,
};

pub struct JournalCommand {
    pub events: Vec<Event>,
    pub reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct JournalClient {
    tx: mpsc::Sender<JournalCommand>,
}

impl JournalClient {
    pub fn spawn(
        event_log: Arc<EventLog>,
        projection_store: ProjectionStore,
        next_seq: u64,
    ) -> Self {
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(journal_worker(
            rx,
            event_log,
            projection_store,
            next_seq,
        ));
        Self { tx }
    }

    /// Append one event to the log and apply it to all projections (sole write path).
    pub async fn append(&self, event: Event) -> Result<(), String> {
        self.append_many(vec![event]).await
    }

    /// Append multiple events as one journal command.
    pub async fn append_many(&self, events: Vec<Event>) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(JournalCommand { events, reply })
            .await
            .map_err(|_| "journal worker stopped".to_string())?;
        rx.await.map_err(|_| "journal worker stopped".to_string())?
    }
}

async fn journal_worker(
    mut rx: mpsc::Receiver<JournalCommand>,
    event_log: Arc<EventLog>,
    projection_store: ProjectionStore,
    mut next_seq: u64,
) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(more) = rx.try_recv() {
            batch.push(more);
        }

        let result = append_and_project_batch(
            &event_log,
            &projection_store,
            &mut next_seq,
            &batch,
        )
        .await;
        let is_projection_failure = result
            .as_ref()
            .err()
            .is_some_and(|e| e.starts_with("projection apply failed after durable append:"));
        for cmd in batch {
            let _ = cmd.reply.send(result.clone());
        }
        if is_projection_failure {
            tracing::error!(
                "projection apply failed after durable append; terminating for replay repair"
            );
            std::process::exit(1);
        }
    }
}

async fn append_and_project_batch(
    event_log: &EventLog,
    projection_store: &ProjectionStore,
    next_seq: &mut u64,
    commands: &[JournalCommand],
) -> Result<(), String> {
    let mut records = Vec::with_capacity(commands.len());
    let mut seq = *next_seq;
    for cmd in commands {
        for event in &cmd.events {
            records.push(EventRecord::new(seq, event_timestamp(event), event.clone()));
            seq += 1;
        }
    }

    event_log
        .append_batch(&records)
        .await
        .map_err(|e| e.to_string())?;
    *next_seq = seq;
    projection_apply::apply_records(projection_store, &records)
        .map_err(|e| format!("projection apply failed after durable append: {e}"))
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
        let projection_store = ProjectionStore::from_db(&db).unwrap();

        let journal = JournalClient::spawn(event_log, projection_store.clone(), 1);

        let j1 = journal.clone();
        let j2 = journal.clone();
        let (r1, r2) = tokio::join!(
            j1.append(Event::NodeEnsured {
                id: "https://reddit.com/r/rust".into(),
            }),
            j2.append(Event::NodeEnsured {
                id: "https://reddit.com/r/python".into(),
            }),
        );
        r1.unwrap();
        r2.unwrap();

        assert_eq!(projection_store.last_applied_event_count().unwrap(), 2);
        let tree = projection_store.load_tree().unwrap();
        assert!(tree
            .get(&ItemId::parse("https://reddit.com/r/rust").unwrap())
            .is_some());
        assert!(tree
            .get(&ItemId::parse("https://reddit.com/r/python").unwrap())
            .is_some());
    }

    #[tokio::test]
    async fn journal_allocates_sequences_from_log_tail_not_projection_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let event_log = Arc::new(EventLog::new(log_path));
        event_log
            .append(&EventRecord::new(
                1,
                1,
                Event::NodeEnsured {
                    id: "https://reddit.com/r/rust".into(),
                },
            ))
            .await
            .unwrap();

        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();
        projection_apply::apply_records(
            &projection_store,
            &[EventRecord::new(
                1,
                1,
                Event::NodeEnsured {
                    id: "https://reddit.com/r/rust".into(),
                },
            )],
        )
        .unwrap();
        let next_seq = event_log.last_sequence().await.unwrap() + 1;
        assert_eq!(next_seq, 2);

        let journal = JournalClient::spawn(
            event_log.clone(),
            projection_store.clone(),
            next_seq,
        );
        journal
            .append(Event::NodeEnsured {
                id: "https://reddit.com/r/python".into(),
            })
            .await
            .unwrap();

        let (records, _) = event_log.load_all().await.unwrap();
        let seqs: Vec<u64> = records.into_iter().map(|record| record.seq).collect();
        assert_eq!(seqs, vec![1, 2]);
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 2);
    }

    #[tokio::test]
    async fn append_many_assigns_contiguous_sequences_and_projects_once() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let event_log = Arc::new(EventLog::new(log_path));
        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();
        let journal =
            JournalClient::spawn(event_log.clone(), projection_store.clone(), 1);

        journal
            .append_many(vec![
                Event::NodeEnsured {
                    id: "https://reddit.com/r/rust".into(),
                },
                Event::NodeEnsured {
                    id: "https://reddit.com/r/python".into(),
                },
                Event::NodeEnsured {
                    id: "https://reddit.com/r/clojure".into(),
                },
            ])
            .await
            .unwrap();

        let (records, _) = event_log.load_all().await.unwrap();
        let seqs: Vec<u64> = records.into_iter().map(|record| record.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 3);
        let tree = projection_store.load_tree().unwrap();
        assert!(tree
            .get(&ItemId::parse("https://reddit.com/r/clojure").unwrap())
            .is_some());
    }
}
