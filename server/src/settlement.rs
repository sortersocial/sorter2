use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, RwLock};

use crate::{
    event_log::EventLog,
    events::Event,
    ranking::compute_scores_from_edges,
    reducer::{GroupState, VoteData},
};

const MAX_ITERS: usize = 10_000;
const TOL: f64 = 1e-8;

pub struct SettlementCommand {
    pub vote: VoteData,
    pub event: Event,
    pub reply: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct SettlementClient {
    tx: mpsc::Sender<SettlementCommand>,
}

impl SettlementClient {
    pub fn spawn(group: Arc<RwLock<GroupState>>, event_log: Arc<EventLog>) -> Self {
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(settlement_worker(rx, group, event_log));
        Self { tx }
    }

    pub async fn record_vote(&self, vote: VoteData, event: Event) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(SettlementCommand {
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
    group: Arc<RwLock<GroupState>>,
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

        let (edges, n) = {
            let mut w = group.write().await;
            for cmd in &batch {
                w.apply_vote(cmd.vote.clone());
            }
            (w.edges.clone(), w.idx_to_item.len())
        };

        let new_scores = compute_scores_from_edges(
            n,
            edges.iter().map(|(&k, &v)| (k, v)),
            MAX_ITERS,
            TOL,
        );

        {
            let mut w = group.write().await;
            w.cached_scores = new_scores;
            w.dirty = false;
        }

        for cmd in batch {
            let _ = cmd.reply.send(Ok(()));
        }
    }
}

/// Compute ranking cache from current in-memory edges (startup replay only).
pub fn warm_ranking_cache(group: &mut GroupState) {
    if !group.dirty {
        return;
    }
    let n = group.idx_to_item.len();
    group.cached_scores = compute_scores_from_edges(
        n,
        group.edges.iter().map(|(&k, &v)| (k, v)),
        MAX_ITERS,
        TOL,
    );
    group.dirty = false;
}
