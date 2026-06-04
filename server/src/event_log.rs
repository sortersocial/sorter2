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
    pub last_seq: u64,
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
        self.append_batch(std::slice::from_ref(record)).await
    }

    pub async fn append_batch(&self, records: &[EventRecord]) -> Result<(), EventLogError> {
        if records.is_empty() {
            return Ok(());
        }
        for pair in records.windows(2) {
            if pair[1].seq != pair[0].seq + 1 {
                return Err(EventLogError::Apply(format!(
                    "event batch sequence gap: {} followed by {}",
                    pair[0].seq, pair[1].seq
                )));
            }
        }
        self.ensure_parent_dir().await?;
        let mut f: tokio::fs::File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        for record in records {
            let mut line = serde_json::to_string(record)?;
            line.push('\n');
            f.write_all(line.as_bytes()).await?;
        }
        f.flush().await?;
        f.sync_data().await?;
        if let Some(last) = records.last() {
            self.write_tail(last.seq).await?;
        }
        Ok(())
    }

    fn tail_path(&self) -> PathBuf {
        self.path.with_extension("jsonl.tail")
    }

    async fn write_tail(&self, seq: u64) -> Result<(), EventLogError> {
        self.ensure_parent_dir().await?;
        let path = self.tail_path();
        let tmp = path.with_extension("tail.tmp");
        tokio::fs::write(&tmp, seq.to_le_bytes()).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    async fn read_tail(&self) -> Result<Option<u64>, EventLogError> {
        let path = self.tail_path();
        if !fs::try_exists(&path).await? {
            return Ok(None);
        }
        let bytes = fs::read(&path).await?;
        if bytes.len() != 8 {
            return Err(EventLogError::Apply(format!(
                "corrupt event log tail file {} (expected 8 bytes, got {})",
                path.display(),
                bytes.len()
            )));
        }
        let arr: [u8; 8] = bytes[..8]
            .try_into()
            .map_err(|_| EventLogError::Apply("corrupt event log tail file".into()))?;
        Ok(Some(u64::from_le_bytes(arr)))
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
        let mut expected_seq = 1_u64;

        while let Some(line) = reader.next_line().await? {
            line_no += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record = parse_line(trimmed).map_err(|e| EventLogError::at_line(line_no, e))?;
            if record.seq != expected_seq {
                return Err(EventLogError::BadLine {
                    line_no,
                    detail: format!(
                        "non-contiguous event sequence: expected {expected_seq}, got {}",
                        record.seq
                    ),
                });
            }
            expected_seq += 1;
            stats.last_seq = record.seq;
            if record.seq <= after_seq {
                stats.skipped += 1;
                continue;
            }
            apply(record)?;
            stats.applied += 1;
        }

        Ok(stats)
    }

    pub async fn last_sequence(&self) -> Result<u64, EventLogError> {
        if let Some(seq) = self.read_tail().await? {
            return Ok(seq);
        }
        let stats = self.replay_from(u64::MAX, |_| Ok(())).await?;
        if stats.last_seq > 0 {
            self.write_tail(stats.last_seq).await?;
        }
        Ok(stats.last_seq)
    }

    /// Load every event into memory. Prefer [`Self::replay`] for startup.
    pub async fn load_all(
        &self,
    ) -> Result<(Vec<EventRecord>, Vec<(usize, String)>), EventLogError> {
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
        log.append(&sample_record(1, event)).await.unwrap();

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
        let err = log.replay(|_| Ok(())).await.unwrap_err();
        assert!(matches!(
            err,
            EventLogError::BadLine {
                line_no: 1,
                detail: ref d,
            } if d.contains("unsupported event schema: 99")
        ));
    }

    #[tokio::test]
    async fn last_sequence_reads_tail_sidecar_without_scanning_log() {
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

        assert_eq!(log.last_sequence().await.unwrap(), 2);
        assert!(path.with_extension("jsonl.tail").exists());

        // Corrupt the log body; tail sidecar should still answer without reading JSONL.
        tokio::fs::write(&path, b"not jsonl\n").await.unwrap();
        assert_eq!(log.last_sequence().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn replay_rejects_sequence_gaps() {
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
            3,
            Event::NodeEnsured {
                id: "reddit.com/r/python".into(),
            },
        ))
        .await
        .unwrap();

        let err = log.replay(|_| Ok(())).await.unwrap_err();
        assert!(matches!(
            err,
            EventLogError::BadLine {
                line_no: 2,
                detail: ref d,
            } if d.contains("expected 2, got 3")
        ));
    }
}
