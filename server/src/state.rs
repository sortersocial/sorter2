use std::{error::Error, sync::Arc};

use crate::{
    event_log::EventLog,
    events::Event,
    fetch::now_ms,
    journal::JournalClient,
    path_types::ItemId,
    projection_apply,
    projection_store::ProjectionStore,
    reddit::{RedditApiConfig, RedditBroker, REDDIT_CONTENT_TTL},
    reducer::{GlobalTree, VoteData},
    view_log::ViewLog,
    views::ViewStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionRebuildStats {
    pub applied: usize,
    pub last_seq: u64,
}

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
    projection_store: &ProjectionStore,
) -> Result<(), crate::event_log::EventLogError> {
    let after_seq = projection_store
        .last_applied_event_count()
        .map_err(|e| crate::event_log::EventLogError::Apply(e.to_string()))?;

    let stats = event_log
        .replay_from(after_seq, |record| {
            projection_apply::apply_records(projection_store, &[record])
        })
        .await?;
    if after_seq > stats.last_seq {
        return Err(crate::event_log::EventLogError::Apply(format!(
            "projection cursor {after_seq} is ahead of event log tail {}",
            stats.last_seq
        )));
    }

    Ok(())
}

fn spawn_content_evictor(projection_store: ProjectionStore) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15 * 60));
        interval.tick().await;
        loop {
            interval.tick().await;
            let cutoff = now_ms() - REDDIT_CONTENT_TTL.as_millis() as i64;
            match projection_store.evict_content_older_than(cutoff) {
                Ok(0) => {}
                Ok(n) => tracing::info!(evicted = n, "reddit display content TTL eviction"),
                Err(e) => tracing::warn!(err = %e, "reddit content TTL eviction failed"),
            }
        }
    });
}

pub async fn rebuild_projection(
    cfg: &AppConfig,
) -> Result<ProjectionRebuildStats, Box<dyn Error + Send + Sync + 'static>> {
    let event_log = EventLog::new(cfg.event_log_path.clone());
    let store_path = format!("{}/store", cfg.data_dir);
    let db = durable::Db::open(std::path::Path::new(&store_path))?;
    let projection_store = ProjectionStore::from_db(&db)?;

    projection_store.reset()?;

    let stats = event_log
        .replay(|record| projection_apply::apply_records(&projection_store, &[record]))
        .await?;
    let cursor = projection_store.last_applied_event_count()?;
    if cursor != stats.last_seq {
        return Err(format!(
            "projection rebuild cursor mismatch: cursor {cursor}, log tail {}",
            stats.last_seq
        )
        .into());
    }

    Ok(ProjectionRebuildStats {
        applied: stats.applied,
        last_seq: stats.last_seq,
    })
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
    pub projection_store: ProjectionStore,
    pub views: ViewStore,
    journal: JournalClient,
    pub reddit: RedditBroker,
}

impl AppState {
    pub async fn try_new(cfg: AppConfig) -> Result<Self, Box<dyn Error + Send + Sync + 'static>> {
        let event_log = Arc::new(EventLog::new(cfg.event_log_path.clone()));
        let view_log = Arc::new(ViewLog::new(cfg.views_log_path.clone()));
        let store_path = format!("{}/store", cfg.data_dir);
        let db = durable::Db::open(std::path::Path::new(&store_path))?;
        let projection_store = ProjectionStore::from_db(&db)?;
        let views = ViewStore::from_db(&db)?;

        if let Err(e) = views.catch_up(&view_log).await {
            tracing::warn!(err = %e, "view log replay failed");
        }
        if let Err(e) = views.flush() {
            tracing::warn!(err = %e, "view store flush after catch-up failed");
        }
        views.spawn_worker(view_log.clone());

        catch_up_projection(&event_log, &projection_store).await?;
        let next_seq = event_log.last_sequence().await? + 1;

        let journal = JournalClient::spawn(
            event_log.clone(),
            projection_store.clone(),
            next_seq,
        );
        let reddit = RedditBroker::spawn(
            journal.clone(),
            projection_store.clone(),
            RedditApiConfig::from_env(),
        );
        spawn_content_evictor(projection_store.clone());

        Ok(Self {
            cfg: Arc::new(cfg),
            event_log,
            view_log,
            projection_store,
            views,
            journal,
            reddit,
        })
    }

    pub async fn new(cfg: AppConfig) -> Self {
        Self::try_new(cfg).await.expect("app state")
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
        let a_raw = a.trim();
        let b_raw = b.trim();
        if a_raw.is_empty() || b_raw.is_empty() || a_raw == b_raw {
            return Err("invalid vote: need two distinct non-empty items".to_string());
        }
        if ratio_left.max(0) == 0 && ratio_right.max(0) == 0 {
            return Err(
                "invalid vote: need a positive preference on at least one side".to_string(),
            );
        }
        // Validate items canonicalize (or are opaque keys) before append.
        let _ = VoteData::from_recorded(ts, a_raw, b_raw, ratio_left, ratio_right)
            .ok_or_else(|| "invalid vote: need two distinct parseable items".to_string())?;

        let event = Event::VoteRecorded {
            ts,
            a: a_raw.to_string(),
            b: b_raw.to_string(),
            ratio_left,
            ratio_right,
            scope: parent.as_str().to_string(),
        };

        self.journal.append(event).await
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_scope, parse_item_param, AppConfig, AppState};
    use crate::{
        event_log::EventLog, events::Event, path_types::ItemId, projection_apply,
        projection_store::ProjectionStore, reducer::EntityData,
    };

    fn event_record(seq: u64, event: Event) -> crate::events::EventRecord {
        crate::events::EventRecord::new(seq, crate::events::event_timestamp(&event), event)
    }

    #[tokio::test]
    async fn rebuild_projection_drops_ephemeral_content() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let log = EventLog::new(format!("{data_dir}/events.jsonl"));
        log.append(&event_record(
            1,
            Event::NodeEnsured {
                id: "https://reddit.com/r/rust".into(),
            },
        ))
        .await
        .unwrap();

        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();
        super::catch_up_projection(&log, &projection_store)
            .await
            .unwrap();
        let id = ItemId::parse("https://reddit.com/r/rust").unwrap();
        projection_store
            .put_ephemeral_content(
                &id,
                &EntityData {
                    title: "Rust".into(),
                    author: None,
                    body_html: None,
                    thumb_url: None,
                    image_url: None,
                    link_url: None,
                },
                1,
            )
            .unwrap();
        assert!(projection_store.load_node(&id).unwrap().unwrap().data.is_some());
        drop(projection_store);
        drop(db);

        super::rebuild_projection(&AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        })
        .await
        .unwrap();

        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();
        let node = projection_store.load_node(&id).unwrap().unwrap();
        assert!(node.data.is_none());
    }

    #[tokio::test]
    async fn rebuild_projection_restores_structure_and_cursor_from_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let log = EventLog::new(format!("{data_dir}/events.jsonl"));
        log.append_batch(&[
            event_record(
                1,
                Event::NodeEnsured {
                    id: "https://reddit.com/r/rust".into(),
                },
            ),
            event_record(
                2,
                Event::VoteRecorded {
                    ts: 2,
                    a: "alpha".into(),
                    b: "beta".into(),
                    ratio_left: 2,
                    ratio_right: 1,
                    scope: String::new(),
                },
            ),
        ])
        .await
        .unwrap();

        {
            let db = durable::Db::open(tmp.path().join("store")).unwrap();
            let projection_store = ProjectionStore::from_db(&db).unwrap();
            projection_apply::apply_records(
                &projection_store,
                &[event_record(
                    1,
                    Event::NodeEnsured {
                        id: "https://reddit.com/r/stale".into(),
                    },
                )],
            )
            .unwrap();
            assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);
        }

        let stats = super::rebuild_projection(&AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        })
        .await
        .unwrap();
        assert_eq!(stats.applied, 2);
        assert_eq!(stats.last_seq, 2);

        let db = durable::Db::open(tmp.path().join("store")).unwrap();
        let projection_store = ProjectionStore::from_db(&db).unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 2);
        let tree = projection_store.scope_tree(&ItemId::root()).unwrap();
        let root = tree.get(&ItemId::root()).unwrap();
        assert!(root.children.contains(&ItemId::parse("alpha").unwrap()));
        assert!(projection_store
            .load_node(&ItemId::parse("https://reddit.com/r/stale").unwrap())
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn startup_fails_when_projection_cursor_is_ahead_of_log() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let log = EventLog::new(format!("{data_dir}/events.jsonl"));
        log.append(&event_record(
            1,
            Event::NodeEnsured {
                id: "https://reddit.com/r/rust".into(),
            },
        ))
        .await
        .unwrap();

        {
            let db = durable::Db::open(tmp.path().join("store")).unwrap();
            let projection_store = ProjectionStore::from_db(&db).unwrap();
            projection_apply::apply_records(
                &projection_store,
                &[event_record(
                    2,
                    Event::NodeEnsured {
                        id: "https://reddit.com/r/rust".into(),
                    },
                )],
            )
            .unwrap();
            let err = super::catch_up_projection(&log, &projection_store)
                .await
                .unwrap_err();
            assert!(err
                .to_string()
                .contains("projection cursor 2 is ahead of event log tail 1"));
        }
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
        let projection_store = ProjectionStore::from_db(&db).unwrap();

        super::catch_up_projection(&log, &projection_store)
            .await
            .unwrap();
        assert_eq!(projection_store.last_applied_event_count().unwrap(), 1);
        let first = projection_store.scope_tree(&ItemId::root()).unwrap();
        let first_root = first.get(&ItemId::root()).unwrap();
        let first_edge_total: f64 = first_root.local_ranking.edges.values().sum();
        assert_eq!(first_edge_total, 3.0);

        super::catch_up_projection(&log, &projection_store)
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
        let id = ItemId::parse("https://reddit.com/r/rust").unwrap();

        state.ensure_node(&id).await.unwrap();

        assert_eq!(
            state.projection_store.last_applied_event_count().unwrap(),
            1
        );
        let projected = state.projection_store.load_tree().unwrap();
        assert!(projected.get(&id).is_some());
        let reddit = projected
            .get(&ItemId::from_url("https://reddit.com").unwrap())
            .unwrap();
        assert!(reddit
            .children
            .contains(&ItemId::from_url("https://reddit.com/r").unwrap()));
    }

    #[tokio::test]
    async fn record_vote_rejects_zero_zero_ratios() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().to_string_lossy().into_owned();
        let state = AppState::new(AppConfig {
            data_dir: data_dir.clone(),
            event_log_path: format!("{data_dir}/events.jsonl"),
            views_log_path: format!("{data_dir}/views.jsonl"),
            port: 0,
        })
        .await;

        let err = state
            .record_vote(&ItemId::root(), "alpha", "beta", 0, 0)
            .await
            .unwrap_err();
        assert!(err.contains("positive preference"));
        assert_eq!(state.projection_store.last_applied_event_count().unwrap(), 0);
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
                id: "https://reddit.com/r/rust".into(),
            },
        ))
        .await
        .unwrap();
        log.append(&event_record(
            2,
            Event::NodeEnsured {
                id: "https://reddit.com/r/python".into(),
            },
        ))
        .await
        .unwrap();
        {
            let db = durable::Db::open(tmp.path().join("store")).unwrap();
            let projection_store = ProjectionStore::from_db(&db).unwrap();
            super::catch_up_projection(&log, &projection_store)
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
            .scope_tree(&ItemId::parse("https://reddit.com/r/rust").unwrap())
            .unwrap();
        assert!(tree
            .get(&ItemId::parse("https://reddit.com/r/rust").unwrap())
            .is_some());
        assert!(tree
            .get(&ItemId::parse("https://reddit.com/r/python").unwrap())
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
            let projection_store = ProjectionStore::from_db(&db).unwrap();
            super::catch_up_projection(&log, &projection_store)
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
        assert_eq!(id.as_str(), "https://reddit.com/r/rust");
    }

    #[test]
    fn parse_item_param_empty_is_root() {
        assert!(parse_item_param("").is_root());
    }
}
