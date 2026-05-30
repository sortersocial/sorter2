use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{
    event_log::EventLog,
    events::Event,
    reducer::{GroupState, VoteData},
};

/// Per-scope ranking state, keyed by scope (e.g. subreddit; "" is the default scope).
pub type GroupMap = HashMap<String, GroupState>;

pub struct SettlementCommand {
    pub scope: String,
    pub vote: VoteData,
    pub event: Event,
    pub reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct SettlementClient {
    tx: mpsc::Sender<SettlementCommand>,
}

impl SettlementClient {
    pub fn spawn(groups: Arc<RwLock<GroupMap>>, event_log: Arc<EventLog>) -> Self {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(settlement_worker(rx, groups, event_log));
        Self { tx }
    }

    pub async fn record_vote(
        &self,
        scope: String,
        vote: VoteData,
        event: Event,
    ) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SettlementCommand {
                scope,
                vote,
                event,
                reply,
            })
            .await
            .map_err(|_| "settlement worker stopped".to_string())?;
        rx.await
            .map_err(|_| "settlement worker stopped".to_string())?
    }
}

async fn settlement_worker(
    mut rx: mpsc::Receiver<SettlementCommand>,
    groups: Arc<RwLock<GroupMap>>,
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
            let mut w = groups.write().await;
            for cmd in &batch {
                w.entry(cmd.scope.clone())
                    .or_default()
                    .apply_vote(cmd.vote.clone());
            }
        }

        for cmd in batch {
            let _ = cmd.reply.send(Ok(()));
        }
    }
}
