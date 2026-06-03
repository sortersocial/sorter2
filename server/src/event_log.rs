use std::path::{Path, PathBuf};

use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};

use crate::events::{Event, EventRecord, CURRENT_EVENT_SCHEMA};

#[derive(Debug, Default)]
pub struct ReplayStats {
    pub applied: usize,
    pub skipped: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("apply error: {0}")]
    Apply(String),
    #[error("unsupported event schema: {0}")]
    UnsupportedSchema(u32),
    #[error("invalid event log line {line_no}: {detail}")]
    BadLine { line_no: usize, detail: String },
}

impl EventLogError {
    fn at_line(line_no: usize, err: Self) -> Self {
        match err {
            Self::Json(e) => Self::BadLine {
                line_no,
                detail: e.to_string(),
            },
            Self::UnsupportedSchema(v) => Self::BadLine {
                line_no,
                detail: format!("unsupported event schema: {v}"),
            },
            other => other,
        }
    }
}

fn parse_line(line: &str) -> Result<EventRecord, EventLogError> {
    let record = serde_json::from_str::<EventRecord>(line)?;
    if record.schema != CURRENT_EVENT_SCHEMA {
        return Err(EventLogError::UnsupportedSchema(record.schema));
    }
    Ok(record)
}

#[derive(Debug, Clone)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn ensure_parent_dir(&self) -> Result<(), EventLogError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    pub async fn append(&self, event: &Event) -> Result<(), EventLogError> {
        self.ensure_parent_dir().await?;
        let mut f: tokio::fs::File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        let record = EventRecord::new(event.clone());
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        f.sync_data().await?;
        Ok(())
    }

    /// Stream the log one line at a time — parse each [`Event`], apply, drop before the next line.
    pub async fn replay<F>(&self, mut apply: F) -> Result<ReplayStats, EventLogError>
    where
        F: FnMut(Event) -> Result<(), EventLogError>,
    {
        self.replay_from(0, |_, ev| apply(ev)).await
    }

    /// Stream valid events after `skip_valid_events`, passing each event's
    /// one-based valid-event count to the callback.
    pub async fn replay_from<F>(
        &self,
        skip_valid_events: u64,
        mut apply: F,
    ) -> Result<ReplayStats, EventLogError>
    where
        F: FnMut(u64, Event) -> Result<(), EventLogError>,
    {
        let mut stats = ReplayStats::default();
        if !fs::try_exists(&self.path).await? {
            return Ok(stats);
        }

        let f = fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(f).lines();

        let mut valid_events = 0_u64;
        let mut line_no = 0_usize;
        while let Some(line) = reader.next_line().await? {
            line_no += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record = parse_line(trimmed).map_err(|e| EventLogError::at_line(line_no, e))?;
            valid_events += 1;
            if valid_events <= skip_valid_events {
                stats.skipped += 1;
                continue;
            }
            match apply(valid_events, record.event) {
                Ok(()) => stats.applied += 1,
                Err(e) => return Err(e),
            }
        }

        Ok(stats)
    }

    /// Load every event into memory. Prefer [`Self::replay`] for startup.
    pub async fn load_all(&self) -> Result<(Vec<Event>, Vec<(usize, String)>), EventLogError> {
        let mut events = Vec::new();
        let stats = self
            .replay(|ev| {
                events.push(ev);
                Ok(())
            })
            .await?;
        let _ = stats;
        Ok((events, vec![]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;

    #[tokio::test]
    async fn replay_applies_one_line_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let log = EventLog::new(&path);
        log.append(&Event::NodeEnsured {
            id: "reddit.com/r/rust".into(),
        })
        .await
        .unwrap();
        log.append(&Event::VoteRecorded {
            ts: 1,
            a: "a".into(),
            b: "b".into(),
            ratio_left: 2,
            ratio_right: 1,
            scope: String::new(),
        })
        .await
        .unwrap();

        let mut seen = Vec::new();
        let stats = log
            .replay(|ev| {
                seen.push(ev);
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(stats.applied, 2);
        assert_eq!(seen.len(), 2);
    }

    #[tokio::test]
    async fn append_writes_schema_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let log = EventLog::new(&path);
        log.append(&Event::NodeEnsured {
            id: "reddit.com/r/rust".into(),
        })
        .await
        .unwrap();

        let line = std::fs::read_to_string(&path).unwrap();
        let record: EventRecord = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record.schema, CURRENT_EVENT_SCHEMA);
        assert!(matches!(record.event, Event::NodeEnsured { .. }));
    }

    #[tokio::test]
    async fn replay_fails_on_bare_event_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"node_ensured","id":"reddit.com/r/rust"}
{"schema":1,"event":{"type":"vote_recorded","ts":1,"a":"a","b":"b","ratio_left":2,"ratio_right":1,"scope":""}}
"#,
        )
        .unwrap();

        let log = EventLog::new(&path);
        let err = log.replay(|_| Ok(())).await.unwrap_err();
        assert!(matches!(err, EventLogError::BadLine { line_no: 1, .. }));
    }

    #[tokio::test]
    async fn replay_rejects_unsupported_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        std::fs::write(
            &path,
            r#"{"schema":99,"event":{"type":"node_ensured","id":"x"}}"#,
        )
        .unwrap();

        let log = EventLog::new(&path);
        let err = log
            .replay(|_| Ok(()))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            EventLogError::BadLine {
                line_no: 1,
                detail: ref d,
            } if d.contains("unsupported event schema: 99")
        ));
    }
}
