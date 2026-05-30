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

    let cfg = AppConfig::from_env();
    std::fs::create_dir_all(&cfg.data_dir)?;
    sorter2_server::run(cfg).await
}
