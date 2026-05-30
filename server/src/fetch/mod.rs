//! Entity import over `POST /ui` as SSE (Reddit worker in [`crate::reddit`]).

pub mod html;

use std::convert::Infallible;
use std::time::Duration;

use async_stream::stream;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde::Serialize;
use tokio::sync::oneshot;

use crate::{
    path_types::ItemId,
    reddit::FetchJobResult,
    reducer::NodeState,
    state::AppState,
};

pub fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

#[derive(Serialize)]
struct SseMorphPayload {
    selector: &'static str,
    html: String,
}

fn morph_complete_event(html: maud::Markup) -> Event {
    let payload = SseMorphPayload {
        selector: "#entity-section",
        html: html.into_string(),
    };
    let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into());
    Event::default().event("complete").data(data)
}

/// Stream `fetching` → `complete` / `error` for [`crate::ui_action::HtmlUiAction::FetchEntity`].
pub fn fetch_entity_stream(
    state: AppState,
    id: ItemId,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::debug!(item = %id, "fetch entity stream opened");

    let stream = stream! {
        if id.is_root() {
            yield Ok(Event::default().event("error").data("{\"message\":\"nothing to fetch for the root\"}"));
            return;
        }

        if !crate::reddit::is_fetchable(&id) {
            tracing::debug!(item = %id, "fetch stream: not fetchable");
            yield Ok(Event::default().event("error").data("{\"message\":\"this page cannot be fetched from Reddit\"}"));
            return;
        }

        let fetching_html = {
            let tree = state.tree.read().await;
            let empty = NodeState::default();
            let node = tree.get(&id).unwrap_or(&empty);
            html::entity_section(&id, node, true).into_string()
        };
        let fetching_payload = serde_json::json!({
            "selector": "#entity-section",
            "html": fetching_html,
        });
        yield Ok(Event::default().event("fetching").data(fetching_payload.to_string()));

        let (tx, rx) = oneshot::channel();
        state.reddit.request_fetch(id.clone(), true, Some(tx));
        tracing::debug!(item = %id, "fetch stream: queued reddit job");

        let result = match rx.await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(item = %id, "fetch stream: worker dropped oneshot");
                FetchJobResult::Failed("reddit worker stopped".into())
            }
        };

        tracing::debug!(item = %id, ?result, "fetch stream: job finished");

        match result {
            FetchJobResult::Imported | FetchJobResult::NotFound => {
                let tree = state.tree.read().await;
                let empty = NodeState::default();
                let node = tree.get(&id).unwrap_or(&empty);
                yield Ok(morph_complete_event(html::entity_section(&id, node, false)));
            }
            FetchJobResult::SkippedCached | FetchJobResult::SkippedDuplicate => {
                let tree = state.tree.read().await;
                let empty = NodeState::default();
                let node = tree.get(&id).unwrap_or(&empty);
                yield Ok(morph_complete_event(html::entity_section(&id, node, false)));
            }
            FetchJobResult::RateLimited { reset_secs } => {
                yield Ok(Event::default().event("error").data(
                    serde_json::json!({"message": format!("Reddit rate limit — retry in {reset_secs}s")}).to_string(),
                ));
            }
            FetchJobResult::Failed(msg) => {
                yield Ok(Event::default().event("error").data(
                    serde_json::json!({"message": msg}).to_string(),
                ));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
