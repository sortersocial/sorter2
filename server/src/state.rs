use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{event_log::EventLog, events::Event, views::ViewStore};

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
    pub demo_counter: Arc<RwLock<u64>>,
}

impl AppState {
    pub async fn new(cfg: AppConfig) -> Self {
        let event_log = Arc::new(EventLog::new(cfg.event_log_path.clone()));
        let views_path = format!("{}/views.json", cfg.data_dir);
        let views = ViewStore::new(&views_path);

        let mut demo_counter: u64 = 0;
        if let Ok((events, _)) = event_log.load_all().await {
            for ev in events {
                if let Event::DemoCounterBumped { value, .. } = ev {
                    demo_counter = demo_counter.max(value);
                }
            }
        }

        Self {
            cfg: Arc::new(cfg),
            event_log,
            views,
            demo_counter: Arc::new(RwLock::new(demo_counter)),
        }
    }

    pub async fn bump_demo_counter(&self) -> u64 {
        let mut guard = self.demo_counter.write().await;
        *guard += 1;
        let value = *guard;
        drop(guard);

        let ts = crate::html::now_ms();
        let _ = self
            .event_log
            .append(&Event::DemoCounterBumped { ts, value })
            .await;

        value
    }
}
