pub mod api;
pub mod auth;
pub mod event_log;
pub mod events;
pub mod fetch;
pub mod form_template;
pub mod html;
pub mod identity;
pub mod journal;
pub mod nsfw;
pub mod pair;
pub mod parser;
pub mod path_types;
pub mod url_rules;
pub mod projection_apply;
pub mod projection_store;
pub mod ranking;
pub mod reddit;
pub mod reducer;
pub mod render;
pub mod state;
pub mod storage_dto;
pub mod storage_schema;
pub mod ui_action;
pub mod view_log;
pub mod views;

use axum::{
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

use crate::state::{AppConfig, AppState};

pub async fn create_app_state(cfg: AppConfig) -> AppState {
    AppState::new(cfg).await
}

pub fn create_app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/static/:filename", get(crate::html::serve_static))
        .route("/~/*item_path", get(crate::html::browse))
        .route("/", get(crate::html::home))
        .route("/vote", get(crate::html::vote::vote_page))
        .route("/login", get(crate::auth::login_page))
        .route("/login/alias", get(crate::auth::alias_page))
        .route("/auth/github", get(crate::auth::github_start))
        .route("/auth/github/callback", get(crate::auth::github_callback))
        .route("/auth/reddit", get(crate::auth::reddit_start))
        .route("/auth/reddit/callback", get(crate::auth::reddit_callback))
        .route("/auth/logout", post(crate::auth::logout))
        .route("/auth/switch", post(crate::auth::switch_pseudonym))
        .route("/nsfw/enter", post(crate::html::nsfw_enter))
        .route("/nsfw/leave", post(crate::html::nsfw_leave))
        .route("/ui", post(crate::api::ui_html::post_ui_html))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

pub async fn run(cfg: AppConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = AppState::try_new(cfg.clone()).await?;
    let app = create_app(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port));
    tracing::info!("listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
}
