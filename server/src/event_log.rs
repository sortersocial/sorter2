use std::path::{Path, PathBuf};

use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};

use crate::events::Event;

#[derive(Debug, Default)]
pub struct ReplayStats {
    pub applied: usize,
    pub bad_lines: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
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

        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        Ok(())
    }

    /// Stream the log one line at a time — parse each [`Event`], apply, drop before the next line.
    pub async fn replay<F>(&self, mut apply: F) -> Result<ReplayStats, EventLogError>
    where
        F: FnMut(Event) -> Result<(), EventLogError>,
    {
        let mut stats = ReplayStats::default();
        if !fs::try_exists(&self.path).await? {
            return Ok(stats);
        }

        let f = fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(f).lines();

        while let Some(line) = reader.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(trimmed) {
                Ok(ev) => match apply(ev) {
                    Ok(()) => stats.applied += 1,
                    Err(e) => return Err(e),
                },
                Err(_) => stats.bad_lines += 1,
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
        assert_eq!(stats.bad_lines, 0);
        assert_eq!(seen.len(), 2);
    }
}
