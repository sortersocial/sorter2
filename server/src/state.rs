use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    event_log::EventLog,
    events::Event,
    reducer::VoteData,
    settlement::{GroupMap, SettlementClient},
    views::ViewStore,
};

/// Normalize a raw ranking subject into a scope key: strip an optional `r/`
/// prefix, keep only `[a-z0-9_]`, lowercase, and cap the length. Empty string
/// is the default/global scope.
pub fn normalize_scope(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix("r/")
        .or_else(|| s.strip_prefix("R/"))
        .unwrap_or(s);
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .flat_map(|c| c.to_lowercase())
        .take(64)
        .collect()
}

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
    pub groups: Arc<RwLock<GroupMap>>,
    settlement: SettlementClient,
}

impl AppState {
    pub async fn new(cfg: AppConfig) -> Self {
        let event_log = Arc::new(EventLog::new(cfg.event_log_path.clone()));
        let views_path = format!("{}/views.json", cfg.data_dir);
        let views = ViewStore::new(&views_path);

        let mut groups: GroupMap = HashMap::new();
        if let Ok((events, _)) = event_log.load_all().await {
            for ev in events {
                match ev {
                    Event::VoteRecorded {
                        ts,
                        a,
                        b,
                        ratio_left,
                        ratio_right,
                        scope,
                    } => {
                        if let Some(vote) =
                            VoteData::from_recorded(ts, &a, &b, ratio_left, ratio_right)
                        {
                            groups.entry(scope).or_default().apply_vote(vote);
                        }
                    }
                    Event::ViewRecorded { .. } => {}
                }
            }
        }

        let groups = Arc::new(RwLock::new(groups));
        let settlement = SettlementClient::spawn(groups.clone(), event_log.clone());

        Self {
            cfg: Arc::new(cfg),
            event_log,
            views,
            groups,
            settlement,
        }
    }

    pub async fn record_vote(
        &self,
        scope: &str,
        a: &str,
        b: &str,
        ratio_left: i32,
        ratio_right: i32,
    ) -> Result<(), String> {
        let ts = crate::html::now_ms();
        let vote = VoteData::from_recorded(ts, a, b, ratio_left, ratio_right)
            .ok_or_else(|| "invalid vote: need two distinct non-empty items".to_string())?;

        let scope = normalize_scope(scope);
        let event = Event::VoteRecorded {
            ts,
            a: vote.a.as_str().to_string(),
            b: vote.b.as_str().to_string(),
            ratio_left: vote.ratio_left,
            ratio_right: vote.ratio_right,
            scope: scope.clone(),
        };

        self.settlement.record_vote(scope, vote, event).await
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_scope;

    #[test]
    fn normalize_scope_strips_prefix_and_lowercases() {
        assert_eq!(normalize_scope("r/AmITheAsshole"), "amitheasshole");
        assert_eq!(normalize_scope("  rust  "), "rust");
        assert_eq!(normalize_scope("r/web_dev!!"), "web_dev");
        assert_eq!(normalize_scope(""), "");
    }
}
