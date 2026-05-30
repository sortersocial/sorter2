use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    event_log::EventLog,
    events::Event,
    reducer::{GroupState, VoteData},
    settlement::SettlementClient,
    views::ViewStore,
};

#[derive(Clone)]
pub struct AppConfig {
    pub data_dir: String,
    pub event_log_path: String,
    pub port: u16,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let data_dir = std::env::var("SORTER2_DATA_DIR").unwrap_or_else(|_| "./data".into());
        let event_log_path = std::env::var("SORTER2_EVENT_LOG")
            .unwrap_or_else(|_| format!("{data_dir}/events.jsonl"));
        let port = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);
        Self {
            data_dir,
            event_log_path,
            port,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub event_log: Arc<EventLog>,
    pub views: ViewStore,
    pub group: Arc<RwLock<GroupState>>,
    settlement: SettlementClient,
}

impl AppState {
    pub async fn new(cfg: AppConfig) -> Self {
        let event_log = Arc::new(EventLog::new(cfg.event_log_path.clone()));
        let views_path = format!("{}/views.json", cfg.data_dir);
        let views = ViewStore::new(&views_path);

        let mut group = GroupState::new();
        if let Ok((events, _)) = event_log.load_all().await {
            for ev in events {
                match ev {
                    Event::VoteRecorded {
                        ts,
                        a,
                        b,
                        ratio_left,
                        ratio_right,
                    } => {
                        if let Some(vote) =
                            VoteData::from_recorded(ts, &a, &b, ratio_left, ratio_right)
                        {
                            group.apply_vote(vote);
                        }
                    }
                    Event::ViewRecorded { .. } => {}
                }
            }
        }

        let group = Arc::new(RwLock::new(group));
        let settlement = SettlementClient::spawn(group.clone(), event_log.clone());

        Self {
            cfg: Arc::new(cfg),
            event_log,
            views,
            group,
            settlement,
        }
    }

    pub async fn record_vote(
        &self,
        a: &str,
        b: &str,
        ratio_left: i32,
        ratio_right: i32,
    ) -> Result<(), String> {
        let ts = crate::html::now_ms();
        let vote = VoteData::from_recorded(ts, a, b, ratio_left, ratio_right)
            .ok_or_else(|| "invalid vote: need two distinct non-empty items".to_string())?;

        let event = Event::VoteRecorded {
            ts,
            a: vote.a.as_str().to_string(),
            b: vote.b.as_str().to_string(),
            ratio_left: vote.ratio_left,
            ratio_right: vote.ratio_right,
        };

        self.settlement.record_vote(vote, event).await
    }
}
