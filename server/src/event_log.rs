use std::path::{Path, PathBuf};

use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};

use crate::events::{EventRecord, CURRENT_LOG_SCHEMA};

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
    if record.schema != CURRENT_LOG_SCHEMA {
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

    pub async fn append(&self, record: &EventRecord) -> Result<(), EventLogError> {
        self.ensure_parent_dir().await?;
        let mut f: tokio::fs::File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        f.sync_data().await?;
        Ok(())
    }

    /// Stream the log one line at a time — parse each record, apply, drop before the next line.
    pub async fn replay<F>(&self, apply: F) -> Result<ReplayStats, EventLogError>
    where
        F: FnMut(EventRecord) -> Result<(), EventLogError>,
    {
        self.replay_from(0, apply).await
    }

    /// Replay records with `seq` greater than `after_seq`.
    pub async fn replay_from<F>(
        &self,
        after_seq: u64,
        mut apply: F,
    ) -> Result<ReplayStats, EventLogError>
    where
        F: FnMut(EventRecord) -> Result<(), EventLogError>,
    {
        let mut stats = ReplayStats::default();
        if !fs::try_exists(&self.path).await? {
            return Ok(stats);
        }

        let f = fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(f).lines();
        let mut line_no = 0_usize;

        while let Some(line) = reader.next_line().await? {
            line_no += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record = parse_line(trimmed).map_err(|e| EventLogError::at_line(line_no, e))?;
            if record.seq <= after_seq {
                stats.skipped += 1;
                continue;
            }
            apply(record)?;
            stats.applied += 1;
        }

        Ok(stats)
    }

    /// Load every event into memory. Prefer [`Self::replay`] for startup.
    pub async fn load_all(&self) -> Result<(Vec<EventRecord>, Vec<(usize, String)>), EventLogError> {
        let mut events = Vec::new();
        let stats = self
            .replay(|record| {
                events.push(record);
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
    use crate::events::{event_timestamp, Event};

    fn sample_record(seq: u64, event: Event) -> EventRecord {
        EventRecord::new(seq, event_timestamp(&event), event)
    }

    #[tokio::test]
    async fn replay_applies_one_line_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let log = EventLog::new(&path);
        log.append(&sample_record(
            1,
            Event::NodeEnsured {
                id: "reddit.com/r/rust".into(),
            },
        ))
        .await
        .unwrap();
        log.append(&sample_record(
            2,
            Event::VoteRecorded {
                ts: 1,
                a: "a".into(),
                b: "b".into(),
                ratio_left: 2,
                ratio_right: 1,
                scope: String::new(),
            },
        ))
        .await
        .unwrap();

        let mut seen = Vec::new();
        let stats = log
            .replay(|record| {
                seen.push(record.seq);
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(stats.applied, 2);
        assert_eq!(seen, vec![1, 2]);
    }

    #[tokio::test]
    async fn append_writes_schema_envelope_with_seq_and_ts() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let log = EventLog::new(&path);
        let event = Event::NodeEnsured {
            id: "reddit.com/r/rust".into(),
        };
        log.append(&sample_record(1, event))
            .await
            .unwrap();

        let line = std::fs::read_to_string(&path).unwrap();
        let record: EventRecord = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(record.schema, CURRENT_LOG_SCHEMA);
        assert_eq!(record.seq, 1);
        assert!(record.ts > 0);
        assert!(matches!(record.event, Event::NodeEnsured { .. }));
    }

    #[tokio::test]
    async fn replay_fails_on_bare_event_line() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"node_ensured","id":"reddit.com/r/rust"}
{"schema":1,"seq":1,"ts":1,"event":{"type":"vote_recorded","ts":1,"a":"a","b":"b","ratio_left":2,"ratio_right":1,"scope":""}}
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
            r#"{"schema":99,"seq":1,"ts":1,"event":{"type":"node_ensured","id":"x"}}"#,
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
