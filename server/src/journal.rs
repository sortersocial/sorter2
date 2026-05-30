use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::{
    entity_store::EntityStore, event_log::EventLog, events::Event, projection_apply,
    projection_store::ProjectionStore,
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
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(journal_worker(
            rx,
            event_log,
            entity_store,
            projection_store,
        ));
        Self { tx }
    }

    pub async fn record_vote(&self, event: Event) -> Result<(), String> {
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

        let mut disk_err: Option<String> = None;
        for cmd in &batch {
            if let Err(e) = event_log.append(&cmd.event).await {
                disk_err = Some(e.to_string());
                break;
            }
        }

        if let Some(err) = disk_err {
            for cmd in batch {
                let _ = cmd.reply.send(Err(err.clone()));
            }
            continue;
        }

        for cmd in &batch {
            if let Err(e) =
                projection_apply::apply_next_event(&projection_store, &entity_store, &cmd.event)
            {
                tracing::warn!(err = %e, "projection update failed after vote append");
            }
        }

        for cmd in batch {
            let _ = cmd.reply.send(Ok(()));
        }
    }
}
