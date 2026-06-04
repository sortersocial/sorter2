use sorter2_server::state::AppConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if std::env::var("SORTER2_SKIP_DOTENV").is_err() {
        let _ = dotenvy::dotenv();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sorter2_server=info,tower_http=info".into()),
        )
        .init();

    let mut cfg = AppConfig::from_env();
    if let Some(command) = std::env::args().nth(1) {
        if command == "replay-index" {
            apply_replay_index_args(&mut cfg)?;
            std::fs::create_dir_all(&cfg.data_dir)?;
            let stats = sorter2_server::state::rebuild_projection(&cfg).await?;
            println!(
                "rebuilt projection: applied={} last_seq={}",
                stats.applied, stats.last_seq
            );
            return Ok(());
        }

        return Err(format!("unknown command: {command}").into());
    }

    std::fs::create_dir_all(&cfg.data_dir)?;
    sorter2_server::run(cfg).await
}

fn apply_replay_index_args(
    cfg: &mut AppConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args().skip(2);
    let mut data_dir_overridden = false;
    let mut event_log_overridden = false;
    let mut views_log_overridden = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                cfg.data_dir = args
                    .next()
                    .ok_or_else(|| "--data-dir requires a value".to_string())?;
                data_dir_overridden = true;
            }
            "--event-log" => {
                cfg.event_log_path = args
                    .next()
                    .ok_or_else(|| "--event-log requires a value".to_string())?;
                event_log_overridden = true;
            }
            "--views-log" => {
                cfg.views_log_path = args
                    .next()
                    .ok_or_else(|| "--views-log requires a value".to_string())?;
                views_log_overridden = true;
            }
            "--port" => {
                let port = args
                    .next()
                    .ok_or_else(|| "--port requires a value".to_string())?;
                cfg.port = port.parse()?;
            }
            other => return Err(format!("unknown replay-index argument: {other}").into()),
        }
    }
    if data_dir_overridden && !event_log_overridden {
        cfg.event_log_path = format!("{}/events.jsonl", cfg.data_dir);
    }
    if data_dir_overridden && !views_log_overridden {
        cfg.views_log_path = format!("{}/views.jsonl", cfg.data_dir);
    }
    Ok(())
}
