use std::sync::Arc;

use tokio::sync::RwLock;

use crate::{
    entity_store::EntityStore,
    event_log::EventLog,
    events::Event,
    journal::JournalClient,
    path_types::ItemId,
    projection_store::ProjectionStore,
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

fn parent_from_event_scope(scope: &str) -> ItemId {
    if scope.contains('/') {
        ItemId::parse(scope).unwrap_or_else(|| ItemId::from_legacy_scope(scope))
    } else {
        ItemId::from_legacy_scope(scope)
    }
}

fn apply_event(
    ev: Event,
    tree: &mut GlobalTree,
    entity_store: &EntityStore,
) -> Result<(), crate::event_log::EventLogError> {
    match ev {
        Event::VoteRecorded {
            ts,
            a,
            b,
            ratio_left,
            ratio_right,
            scope,
        } => {
            if let Some(vote) = VoteData::from_recorded(ts, &a, &b, ratio_left, ratio_right) {
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
                if let Err(e) = apply_entity_import(tree, entity_store, &parsed, payload) {
                    tracing::warn!(item = %id, err = %e, "entity replay failed");
                }
            }
        }
    }
    Ok(())
}

async fn load_projected_tree(
    event_log: &EventLog,
    entity_store: &EntityStore,
    projection_store: &ProjectionStore,
) -> Result<GlobalTree, crate::event_log::EventLogError> {
    let cursor = projection_store
        .last_applied_event_count()
        .map_err(|e| crate::event_log::EventLogError::Apply(e.to_string()))?;
    let mut tree = projection_store
        .load_tree()
        .map_err(|e| crate::event_log::EventLogError::Apply(e.to_string()))?;

    event_log
        .replay_from(cursor, |event_count, ev| {
            apply_event(ev.clone(), &mut tree, entity_store)?;
            projection_store
                .persist_event(&tree, event_count, &ev)
                .map_err(|e| crate::event_log::EventLogError::Apply(e.to_string()))
        })
        .await?;

    Ok(tree)
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
    pub entity_store: EntityStore,
    pub projection_store: ProjectionStore,
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
        let store_path = format!("{}/store", cfg.data_dir);
        let db = durable::Db::open(std::path::Path::new(&store_path)).expect("store db");
        let entity_store = EntityStore::from_db(&db).expect("entity store");
        let projection_store = ProjectionStore::from_db(&db).expect("projection store");

        let tree = match load_projected_tree(&event_log, &entity_store, &projection_store).await {
            Ok(tree) => tree,
            Err(e) => {
                tracing::warn!(err = %e, "event log replay failed");
                GlobalTree::new()
            }
        };

        let tree = Arc::new(RwLock::new(tree));
        let journal =
            JournalClient::spawn(tree.clone(), event_log.clone(), projection_store.clone());
        let reddit = RedditBroker::spawn(
            tree.clone(),
            event_log.clone(),
            entity_store.clone(),
            projection_store.clone(),
            RedditApiConfig::from_env(),
        );

        Self {
            cfg: Arc::new(cfg),
            event_log,
            entity_store,
            projection_store,
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
        self.event_log
            .append(&event)
            .await
            .map_err(|e| e.to_string())?;
        {
            let mut w = self.tree.write().await;
            w.ensure_path(id);
            self.projection_store
                .persist_next_event(&w, &event)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
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

        self.journal.record_vote(parent.clone(), vote, event).await
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_scope, parse_item_param};
    use crate::{
        entity_store::EntityStore, event_log::EventLog, events::Event, path_types::ItemId,
        projection_store::ProjectionStore, reducer::GlobalTree,
    };
    use serde_json::json;

    #[tokio::test]
    async fn replay_entity_imported_restores_view() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.jsonl");
        let log = EventLog::new(log_path.to_string_lossy().into_owned());
        let entity_store = EntityStore::open(&tmp.path().join("entity_db")).unwrap();
        let payload = json!({"kind":"t5","data":{"title":"Rust","display_name":"rust"}});
        log.append(&Event::EntityImported {
            id: "reddit.com/r/rust".into(),
            ts: 1,
            payload: payload.clone(),
        })
        .await
        .unwrap();

        let mut tree = GlobalTree::new();
        log.replay(|ev| super::apply_event(ev, &mut tree, &entity_store))
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
        log.append(&Event::VoteRecorded {
            ts: 1,
            a: "alpha".into(),
            b: "beta".into(),
            ratio_left: 2,
            ratio_right: 1,
            scope: String::new(),
        })
        .await
        .unwrap();

        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let entity_store = EntityStore::from_db(&db).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();

        let first = super::load_projected_tree(&log, &entity_store, &projection_store)
            .await
            .unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);
        let first_root = first.get(&ItemId::root()).unwrap();
        let first_edge_total: f64 = first_root.local_ranking.edges.values().sum();
        assert_eq!(first_edge_total, 3.0);

        let second = super::load_projected_tree(&log, &entity_store, &projection_store)
            .await
            .unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);
        let second_root = second.get(&ItemId::root()).unwrap();
        let second_edge_total: f64 = second_root.local_ranking.edges.values().sum();
        assert_eq!(second_edge_total, first_edge_total);
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
