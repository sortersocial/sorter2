use std::sync::Arc;

use crate::{
    entity_store::EntityStore,
    event_log::EventLog,
    events::Event,
    journal::JournalClient,
    path_types::ItemId,
    projection_apply,
    projection_store::ProjectionStore,
    reddit::{RedditApiConfig, RedditBroker},
    reducer::{GlobalTree, VoteData},
    view_log::ViewLog,
    views::ViewStore,
};

/// Parse `?item=` query value into a canonical node id.
pub fn parse_item_param(raw: &str) -> ItemId {
    let s = raw.trim();
    if s.is_empty() {
        return ItemId::root();
    }
    ItemId::from_storage(s).unwrap_or_else(|| ItemId::opaque(s))
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

async fn catch_up_projection(
    event_log: &EventLog,
    entity_store: &EntityStore,
    projection_store: &ProjectionStore,
) -> Result<(), crate::event_log::EventLogError> {
    let after_seq = projection_store
        .last_applied_event_count()
        .map_err(|e| crate::event_log::EventLogError::Apply(e.to_string()))?;

    event_log
        .replay_from(after_seq, |record| {
            projection_apply::apply_event(
                projection_store,
                entity_store,
                record.seq,
                &record.event,
            )
        })
        .await?;

    Ok(())
}

#[derive(Clone)]
pub struct AppConfig {
    pub data_dir: String,
    pub event_log_path: String,
    pub views_log_path: String,
    pub port: u16,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let data_dir = std::env::var("SORTER2_DATA_DIR").unwrap_or_else(|_| "./data".into());
        let event_log_path = std::env::var("SORTER2_EVENT_LOG")
            .unwrap_or_else(|_| format!("{data_dir}/events.jsonl"));
        let views_log_path = std::env::var("SORTER2_VIEWS_LOG")
            .unwrap_or_else(|_| format!("{data_dir}/views.jsonl"));
        let port = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);
        Self {
            data_dir,
            event_log_path,
            views_log_path,
            port,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub event_log: Arc<EventLog>,
    pub view_log: Arc<ViewLog>,
    pub entity_store: EntityStore,
    pub projection_store: ProjectionStore,
    pub views: ViewStore,
    journal: JournalClient,
    pub reddit: RedditBroker,
}

impl AppState {
    pub async fn new(cfg: AppConfig) -> Self {
        let event_log = Arc::new(EventLog::new(cfg.event_log_path.clone()));
        let view_log = Arc::new(ViewLog::new(cfg.views_log_path.clone()));
        let store_path = format!("{}/store", cfg.data_dir);
        let db = durable::Db::open(std::path::Path::new(&store_path)).expect("store db");
        let entity_store = EntityStore::from_db(&db).expect("entity store");
        let projection_store = ProjectionStore::from_db(&db).expect("projection store");
        let views = ViewStore::from_db(&db).expect("view store");

        if let Err(e) = views.catch_up(&view_log).await {
            tracing::warn!(err = %e, "view log replay failed");
        }
        if let Err(e) = views.flush() {
            tracing::warn!(err = %e, "view store flush after catch-up failed");
        }
        views.spawn_worker(view_log.clone());

        if let Err(e) = catch_up_projection(&event_log, &entity_store, &projection_store).await {
            tracing::warn!(err = %e, "event log replay failed");
        }

        let journal = JournalClient::spawn(
            event_log.clone(),
            entity_store.clone(),
            projection_store.clone(),
        );
        let reddit = RedditBroker::spawn(journal.clone(), RedditApiConfig::from_env());

        Self {
            cfg: Arc::new(cfg),
            event_log,
            view_log,
            entity_store,
            projection_store,
            views,
            journal,
            reddit,
        }
    }

    pub async fn ensure_node(&self, id: &ItemId) -> Result<(), String> {
        self.journal
            .append(Event::NodeEnsured {
                id: id.as_str().to_string(),
            })
            .await
    }

    pub fn scope_tree(&self, id: &ItemId) -> Result<GlobalTree, String> {
        self.projection_store
            .scope_tree(id)
            .map_err(|e| e.to_string())
    }

    /// User-initiated Reddit/API import (SSE / fetch module only).
    pub fn queue_entity_fetch(
        &self,
        id: ItemId,
        kind: crate::reddit::FetchKind,
        done: Option<tokio::sync::oneshot::Sender<crate::reddit::FetchJobResult>>,
    ) {
        self.reddit.request_fetch(id, kind, true, done);
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

        self.journal.append(event).await
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_scope, parse_item_param, AppConfig, AppState};
    use crate::{
        entity_store::EntityStore, event_log::EventLog, event_reducer, events::Event,
        path_types::ItemId, projection_store::ProjectionStore, reducer::GlobalTree,
    };
    use serde_json::json;

    fn event_record(seq: u64, event: Event) -> crate::events::EventRecord {
        crate::events::EventRecord::new(seq, crate::events::event_timestamp(&event), event)
    }

    #[tokio::test]
    async fn replay_entity_imported_restores_view() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let log = EventLog::new(log_path.to_string_lossy().into_owned());
        let entity_store = EntityStore::open(&tmp.path().join("entity_db")).unwrap();
        let payload = json!({"kind":"t5","data":{"title":"Rust","display_name":"rust"}});
        let event = Event::EntityImported {
            id: "reddit.com/r/rust".into(),
            ts: 1,
            payload: payload.clone(),
        };
        log.append(&event_record(1, event))
            .await
            .unwrap();

        let mut tree = GlobalTree::new();
        log.replay(|record| event_reducer::apply_event(record.event, &mut tree, &entity_store))
            .await
            .unwrap();
        let node = tree
            .get(&ItemId::parse("reddit.com/r/rust").unwrap())
            .unwrap();
        assert_eq!(node.data.as_ref().unwrap().title, "Rust");
        let stored = entity_store
            .get(&ItemId::parse("reddit.com/r/rust").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(stored["data"]["display_name"], "rust");
    }

    #[tokio::test]
    async fn projected_replay_cursor_prevents_double_applying_votes() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let log = EventLog::new(log_path.to_string_lossy().into_owned());
        log.append(&event_record(
            1,
            Event::VoteRecorded {
                ts: 1,
                a: "alpha".into(),
                b: "beta".into(),
                ratio_left: 2,
                ratio_right: 1,
                scope: String::new(),
            },
        ))
        .await
        .unwrap();

        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let entity_store = EntityStore::from_db(&db).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();

        super::catch_up_projection(&log, &entity_store, &projection_store)
            .await
            .unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);
        let first = projection_store.scope_tree(&ItemId::root()).unwrap();
        let first_root = first.get(&ItemId::root()).unwrap();
        let first_edge_total: f64 = first_root.local_ranking.edges.values().sum();
        assert_eq!(first_edge_total, 3.0);

        super::catch_up_projection(&log, &entity_store, &projection_store)
            .await
            .unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);
        let second = projection_store.scope_tree(&ItemId::root()).unwrap();
        let second_root = second.get(&ItemId::root()).unwrap();
        let second_edge_total: f64 = second_root.local_ranking.edges.values().sum();
        assert_eq!(second_edge_total, first_edge_total);
    }

    #[tokio::test]
    async fn live_ensure_node_updates_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let state = AppState::new(AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        })
        .await;
        let id = ItemId::parse("reddit.com/r/rust").unwrap();

        state.ensure_node(&id).await.unwrap();

        assert_eq!(
            state.projection_store.last_applied_event_count().unwrap(),
            1
        );
        let projected = state.projection_store.load_tree().unwrap();
        assert!(projected.get(&id).is_some());
        let reddit = projected
            .get(&ItemId::parse("reddit.com").unwrap())
            .unwrap();
        assert!(reddit
            .children
            .contains(&ItemId::parse("reddit.com/r").unwrap()));
    }

    #[tokio::test]
    async fn live_record_vote_updates_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let state = AppState::new(AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        })
        .await;

        state
            .record_vote(&ItemId::root(), "alpha", "beta", 2, 1)
            .await
            .unwrap();

        assert_eq!(
            state.projection_store.last_applied_event_count().unwrap(),
            1
        );
        let projected = state.projection_store.load_tree().unwrap();
        let root = projected.get(&ItemId::root()).unwrap();
        assert!(root.children.contains(&ItemId::parse("alpha").unwrap()));
        assert!(root.children.contains(&ItemId::parse("beta").unwrap()));
        assert_eq!(root.local_ranking.idx_to_item.len(), 2);
        let edge_total: f64 = root.local_ranking.edges.values().sum();
        assert_eq!(edge_total, 3.0);
    }

    #[tokio::test]
    async fn startup_does_not_load_entire_existing_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let log = EventLog::new(format!("{data_dir}/events.jsonl"));
        log.append(&event_record(
            1,
            Event::NodeEnsured {
                id: "reddit.com/r/rust".into(),
            },
        ))
        .await
        .unwrap();
        log.append(&event_record(
            2,
            Event::NodeEnsured {
                id: "reddit.com/r/python".into(),
            },
        ))
        .await
        .unwrap();
        {
            let db = durable::Db::open(tmp.path().join("store")).unwrap();
            let entity_store = EntityStore::from_db(&db).unwrap();
            let projection_store = ProjectionStore::from_db(&db).unwrap();
            super::catch_up_projection(&log, &entity_store, &projection_store)
                .await
                .unwrap();
        }

        let cfg = AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        };
        let second = AppState::new(cfg).await;
        assert_eq!(
            second.projection_store.last_applied_event_count().unwrap(),
            2
        );
        let tree = second
            .scope_tree(&ItemId::parse("reddit.com/r/rust").unwrap())
            .unwrap();
        assert!(tree
            .get(&ItemId::parse("reddit.com/r/rust").unwrap())
            .is_some());
        assert!(tree
            .get(&ItemId::parse("reddit.com/r/python").unwrap())
            .is_none());
    }

    #[tokio::test]
    async fn record_vote_after_restart_hydrates_existing_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let log = EventLog::new(format!("{data_dir}/events.jsonl"));
        log.append(&event_record(
            1,
            Event::VoteRecorded {
                ts: 1,
                a: "alpha".into(),
                b: "beta".into(),
                ratio_left: 2,
                ratio_right: 1,
                scope: String::new(),
            },
        ))
        .await
        .unwrap();
        {
            let db = durable::Db::open(tmp.path().join("store")).unwrap();
            let entity_store = EntityStore::from_db(&db).unwrap();
            let projection_store = ProjectionStore::from_db(&db).unwrap();
            super::catch_up_projection(&log, &entity_store, &projection_store)
                .await
                .unwrap();
        }

        let cfg = AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        };
        let second = AppState::new(cfg).await;
        second
            .record_vote(&ItemId::root(), "alpha", "gamma", 3, 1)
            .await
            .unwrap();

        let tree = second.scope_tree(&ItemId::root()).unwrap();
        let root = tree.get(&ItemId::root()).unwrap();
        assert!(root.children.contains(&ItemId::parse("beta").unwrap()));
        assert!(root.children.contains(&ItemId::parse("gamma").unwrap()));
        assert_eq!(root.local_ranking.idx_to_item.len(), 3);
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
