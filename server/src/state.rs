use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    event_log::EventLog,
    events::Event,
    journal::JournalClient,
    path_types::ItemId,
    reddit::{apply_entity_import, RedditApiConfig, RedditBroker},
    reducer::{GlobalTree, VoteData},
    views::ViewStore,
};

/// Parse `?item=` query value into a canonical node id.
pub fn parse_item_param(raw: &str) -> ItemId {
    let s = raw.trim();
    if s.is_empty() {
        return ItemId::root();
    }
    ItemId::from_url(s).or_else(|| ItemId::parse(s)).unwrap_or_else(|| ItemId::opaque(s))
}

/// Legacy: normalize raw ranking subject into a scope key for old event replay.
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

fn parent_from_event_scope(scope: &str) -> ItemId {
    if scope.contains('/') {
        ItemId::parse(scope).unwrap_or_else(|| ItemId::from_legacy_scope(scope))
    } else {
        ItemId::from_legacy_scope(scope)
    }
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
    pub tree: Arc<RwLock<GlobalTree>>,
    journal: JournalClient,
    pub reddit: RedditBroker,
}

impl AppState {
    pub async fn new(cfg: AppConfig) -> Self {
        let event_log = Arc::new(EventLog::new(cfg.event_log_path.clone()));
        let views_path = format!("{}/views.json", cfg.data_dir);
        let views = ViewStore::new(&views_path);

        let mut tree = GlobalTree::new();
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
                            let parent = parent_from_event_scope(&scope);
                            tree.apply_vote(&parent, vote);
                        }
                    }
                    Event::ViewRecorded { .. } => {}
                    Event::NodeEnsured { id } => {
                        if let Some(parsed) = ItemId::parse(&id).or_else(|| ItemId::from_url(&id)) {
                            tree.ensure_path(&parsed);
                        }
                    }
                    Event::EntityImported { id, payload, .. } => {
                        if let Some(parsed) = ItemId::parse(&id).or_else(|| ItemId::from_url(&id)) {
                            apply_entity_import(&mut tree, &parsed, payload);
                        }
                    }
                }
            }
        }

        let tree = Arc::new(RwLock::new(tree));
        let journal = JournalClient::spawn(tree.clone(), event_log.clone());
        let reddit = RedditBroker::spawn(tree.clone(), event_log.clone(), RedditApiConfig::from_env());

        Self {
            cfg: Arc::new(cfg),
            event_log,
            views,
            tree,
            journal,
            reddit,
        }
    }

    pub async fn ensure_node(&self, id: &ItemId) -> Result<(), String> {
        let event = Event::NodeEnsured {
            id: id.as_str().to_string(),
        };
        self.event_log.append(&event).await.map_err(|e| e.to_string())?;
        {
            let mut w = self.tree.write().await;
            w.ensure_path(id);
        }
        Ok(())
    }

    /// User-initiated Reddit/API import (SSE / fetch module only).
    pub fn queue_entity_fetch(&self, id: ItemId, done: Option<tokio::sync::oneshot::Sender<crate::reddit::FetchJobResult>>) {
        self.reddit.request_fetch(id, true, done);
    }

    pub async fn record_vote(
        &self,
        parent: &ItemId,
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
            scope: parent.as_str().to_string(),
        };

        self.journal
            .record_vote(parent.clone(), vote, event)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_scope, parse_item_param};
    use crate::{
        event_log::EventLog,
        events::Event,
        path_types::ItemId,
        reddit::apply_entity_import,
        reducer::GlobalTree,
    };
    use serde_json::json;

    #[tokio::test]
    async fn replay_entity_imported_restores_view() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let log = EventLog::new(log_path.to_string_lossy().into_owned());
        let payload = json!({"kind":"t5","data":{"title":"Rust","display_name":"rust"}});
        log.append(&Event::EntityImported {
            id: "reddit.com/r/rust".into(),
            ts: 1,
            payload: payload.clone(),
        })
        .await
        .unwrap();

        let mut tree = GlobalTree::new();
        let (events, _) = log.load_all().await.unwrap();
        for ev in events {
            if let Event::EntityImported { id, payload, .. } = ev {
                let parsed = ItemId::parse(&id).unwrap();
                apply_entity_import(&mut tree, &parsed, payload);
            }
        }
        let node = tree.get(&ItemId::parse("reddit.com/r/rust").unwrap()).unwrap();
        assert_eq!(node.data.as_ref().unwrap().title, "Rust");
        assert_eq!(
            node.entity_raw.as_ref().unwrap()["data"]["display_name"],
            "rust"
        );
    }

    #[test]
    fn normalize_scope_strips_prefix_and_lowercases() {
        assert_eq!(normalize_scope("r/AmITheAsshole"), "amitheasshole");
        assert_eq!(normalize_scope("  rust  "), "rust");
        assert_eq!(normalize_scope("r/web_dev!!"), "web_dev");
        assert_eq!(normalize_scope(""), "");
    }

    #[test]
    fn parse_item_param_from_url() {
        let id = parse_item_param("https://reddit.com/r/rust");
        assert_eq!(id.as_str(), "reddit.com/r/rust");
    }

    #[test]
    fn parse_item_param_empty_is_root() {
        assert!(parse_item_param("").is_root());
    }
}
