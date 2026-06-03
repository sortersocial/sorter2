use std::path::{Path, PathBuf};

use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};

use crate::events::{ViewRecord, CURRENT_LOG_SCHEMA};

#[derive(Debug, Default)]
pub struct ReplayStats {
    pub applied: usize,
    pub skipped: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ViewLogError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("apply error: {0}")]
    Apply(String),
    #[error("unsupported view log schema: {0}")]
    UnsupportedSchema(u32),
    #[error("invalid view log line {line_no}: {detail}")]
    BadLine { line_no: usize, detail: String },
}

impl ViewLogError {
    fn at_line(line_no: usize, err: Self) -> Self {
        match err {
            Self::Json(e) => Self::BadLine {
                line_no,
                detail: e.to_string(),
            },
            Self::UnsupportedSchema(v) => Self::BadLine {
                line_no,
                detail: format!("unsupported view log schema: {v}"),
            },
            other => other,
        }
    }
}

fn parse_line(line: &str) -> Result<ViewRecord, ViewLogError> {
    let record = serde_json::from_str::<ViewRecord>(line)?;
    if record.schema != CURRENT_LOG_SCHEMA {
        return Err(ViewLogError::UnsupportedSchema(record.schema));
    }
    Ok(record)
}

#[derive(Debug, Clone)]
pub struct ViewLog {
    path: PathBuf,
}

impl ViewLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn ensure_parent_dir(&self) -> Result<(), ViewLogError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    /// Append a batch of view records. Flushes but does not fsync (analytics path).
    pub async fn append_batch(&self, records: &[ViewRecord]) -> Result<(), ViewLogError> {
        if records.is_empty() {
            return Ok(());
        }
        self.ensure_parent_dir().await?;
        let mut f = OpenOptions::new()
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
        Ok(())
    }

    /// Fsync the log file (periodic / shutdown hook).
    pub async fn sync(&self) -> Result<(), ViewLogError> {
        if !fs::try_exists(&self.path).await? {
            return Ok(());
        }
        let f = fs::File::open(&self.path).await?;
        f.sync_data().await?;
        Ok(())
    }

    pub async fn replay<F>(&self, apply: F) -> Result<ReplayStats, ViewLogError>
    where
        F: FnMut(ViewRecord) -> Result<(), ViewLogError>,
    {
        self.replay_from(0, apply).await
    }

    /// Replay records with `seq` greater than `after_seq`.
    pub async fn replay_from<F>(
        &self,
        after_seq: u64,
        mut apply: F,
    ) -> Result<ReplayStats, ViewLogError>
    where
        F: FnMut(ViewRecord) -> Result<(), ViewLogError>,
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
            let record = parse_line(trimmed).map_err(|e| ViewLogError::at_line(line_no, e))?;
            if record.seq <= after_seq {
                stats.skipped += 1;
                continue;
            }
            apply(record)?;
            stats.applied += 1;
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::ViewEvent;

    #[tokio::test]
    async fn append_batch_writes_envelopes_without_sync_requirement() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("views.jsonl");
        let log = ViewLog::new(&path);
        log.append_batch(&[
            ViewRecord::new(1, 100, ViewEvent::PageView { path: "/".into() }),
            ViewRecord::new(2, 101, ViewEvent::PageView { path: "/vote".into() }),
        ])
        .await
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"seq\":1"));
        assert!(text.contains("\"page_view\""));
    }

    #[tokio::test]
    async fn replay_skips_by_sequence_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("views.jsonl");
        let log = ViewLog::new(&path);
        log.append_batch(&[
            ViewRecord::new(1, 1, ViewEvent::PageView { path: "/".into() }),
            ViewRecord::new(2, 2, ViewEvent::PageView { path: "/vote".into() }),
        ])
        .await
        .unwrap();

        let mut seen = Vec::new();
        let stats = log
            .replay_from(1, |record| {
                seen.push(record.seq);
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.applied, 1);
        assert_eq!(seen, vec![2]);
    }
}
