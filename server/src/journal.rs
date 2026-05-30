use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{
    event_log::EventLog,
    events::Event,
    path_types::ItemId,
    reducer::{GlobalTree, VoteData},
};

pub struct JournalCommand {
    pub parent: ItemId,
    pub vote: VoteData,
    pub event: Event,
    pub reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct JournalClient {
    tx: mpsc::Sender<JournalCommand>,
}

impl JournalClient {
    pub fn spawn(tree: Arc<RwLock<GlobalTree>>, event_log: Arc<EventLog>) -> Self {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(journal_worker(rx, tree, event_log));
        Self { tx }
    }

    pub async fn record_vote(
        &self,
        parent: ItemId,
        vote: VoteData,
        event: Event,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(JournalCommand {
                parent,
                vote,
                event,
                reply,
            })
            .await
            .map_err(|_| "journal worker stopped".to_string())?;
        rx.await
            .map_err(|_| "journal worker stopped".to_string())?
    }
}

async fn journal_worker(
    mut rx: mpsc::Receiver<JournalCommand>,
    tree: Arc<RwLock<GlobalTree>>,
    event_log: Arc<EventLog>,
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

        {
            let mut w = tree.write().await;
            for cmd in &batch {
                w.apply_vote(&cmd.parent, cmd.vote.clone());
            }
        }

        for cmd in batch {
            let _ = cmd.reply.send(Ok(()));
        }
    }
}
