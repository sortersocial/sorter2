use std::{
    error::Error,
    path::PathBuf,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use sorter2_server::{
    event_log::EventLog, events::Event, journal::JournalClient, projection_apply, storage_init,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let opts = Options::from_args()?;
    std::fs::create_dir_all(&opts.data_dir)?;

    let data_dir = opts.data_dir.to_string_lossy().into_owned();
    let event_log = Arc::new(EventLog::new(format!("{data_dir}/events.jsonl")));
    let db = durable::Db::open(opts.data_dir.join("store"))?;
    let (entity_store, projection_store) = storage_init::open_from_db(&db)?;
    let journal = JournalClient::spawn(
        event_log.clone(),
        entity_store.clone(),
        projection_store.clone(),
        event_log.last_sequence().await? + 1,
    );
    let write_start = Instant::now();
    for chunk_start in (0..opts.events).step_by(opts.batch_size) {
        let chunk_end = (chunk_start + opts.batch_size).min(opts.events);
        let events = (chunk_start..chunk_end)
            .map(|i| Event::VoteRecorded {
                ts: i as i64,
                a: format!("item-{i}"),
                b: format!("item-{}", i + 1),
                ratio_left: 2,
                ratio_right: 1,
                scope: String::new(),
            })
            .collect();
        journal.append_many(events).await?;
    }
    let write_elapsed = write_start.elapsed();
    drop(journal);

    let rebuild_start = Instant::now();
    entity_store.reset()?;
    projection_store.reset()?;
    let rebuild = event_log
        .replay(|record| {
            projection_apply::apply_records(&projection_store, &entity_store, &[record])
        })
        .await?;
    let rebuild_elapsed = rebuild_start.elapsed();

    println!("data_dir={data_dir}");
    println!(
        "writes: events={} elapsed_ms={} events_per_sec={:.1}",
        opts.events,
        write_elapsed.as_millis(),
        opts.events as f64 / write_elapsed.as_secs_f64()
    );
    println!(
        "rebuild: events={} elapsed_ms={} events_per_sec={:.1}",
        rebuild.applied,
        rebuild_elapsed.as_millis(),
        rebuild.applied as f64 / rebuild_elapsed.as_secs_f64()
    );
    Ok(())
}

struct Options {
    events: usize,
    batch_size: usize,
    data_dir: PathBuf,
}

impl Options {
    fn from_args() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut events = 1_000usize;
        let mut batch_size = 100usize;
        let mut data_dir: Option<PathBuf> = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--events" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--events requires a value".to_string())?;
                    events = value.parse()?;
                }
                "--data-dir" => {
                    data_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--data-dir requires a value".to_string())?,
                    ));
                }
                "--batch-size" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--batch-size requires a value".to_string())?;
                    batch_size = value.parse()?;
                }
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }

        let data_dir = data_dir.unwrap_or_else(|| {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            std::env::temp_dir().join(format!("sorter2-storage-bench-{ts}"))
        });

        Ok(Self {
            events,
            batch_size,
            data_dir,
        })
    }
}
