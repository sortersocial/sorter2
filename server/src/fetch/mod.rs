//! Entity import over `POST /ui` as SSE (Reddit worker in [`crate::reddit`]).
//!
//! Each SSE event's `data` is a JS snippet that the browser `eval`s — the same
//! Idiomorph-morph snippets the non-streaming `/ui` responses use. There is no
//! bespoke JSON envelope; the client just evals whatever each event carries.

pub mod html;

use std::convert::Infallible;
use std::time::Duration;

use async_stream::stream;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use tokio::sync::oneshot;

use crate::{
    html::{ranking_panel, JsBuilder},
    path_types::ItemId,
    reddit::{FetchJobResult, FetchKind},
    reducer::NodeState,
    state::AppState,
    ui_action::FetchTarget,
};

pub fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

fn js_event(js: String) -> Event {
    Event::default().data(js)
}

/// JS that surfaces a transient message in the page's `#errors` region.
fn error_js(message: &str) -> String {
    JsBuilder::new()
        .morph_selector(
            "#errors",
            maud::html! { div id="errors" { p class="muted" { (message) } } },
        )
        .build()
}

/// Stream Idiomorph-morph JS snippets for [`crate::ui_action::HtmlUiAction::FetchEntity`].
pub fn fetch_entity_stream(
    state: AppState,
    id: ItemId,
    target: FetchTarget,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let kind = match target {
        FetchTarget::SelfEntity => FetchKind::SelfEntity,
        FetchTarget::Children => FetchKind::Children,
    };
    tracing::debug!(item = %id, ?kind, "fetch entity stream opened");

    let stream = stream! {
        if id.is_root() {
            yield Ok(js_event(error_js("Nothing to fetch for the root.")));
            return;
        }

        let fetchable = match kind {
            FetchKind::SelfEntity => crate::reddit::is_fetchable(&id),
            FetchKind::Children => crate::reddit::is_children_fetchable(&id),
        };
        if !fetchable {
            tracing::debug!(item = %id, ?kind, "fetch stream: not fetchable");
            yield Ok(js_event(error_js("This page cannot be fetched from Reddit.")));
            return;
        }

        // Optimistic "Fetching…" morph of the entity section.
        let fetching_js = {
            let tree = state.scope_tree(&id).unwrap_or_else(|_| crate::reducer::GlobalTree::new());
            let empty = NodeState::default();
            let node = tree.get(&id).unwrap_or(&empty);
            let sel = html::entity_section_selector(&id);
            JsBuilder::new()
                .morph_selector(&sel, html::entity_section(&id, node, true))
                .build()
        };
        yield Ok(js_event(fetching_js));

        let (tx, rx) = oneshot::channel();
        state.queue_entity_fetch(id.clone(), kind, Some(tx));
        tracing::debug!(item = %id, ?kind, "fetch stream: queued reddit job");

        let result = match rx.await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(item = %id, "fetch stream: worker dropped oneshot");
                FetchJobResult::Failed("reddit worker stopped".into())
            }
        };
        tracing::debug!(item = %id, ?result, "fetch stream: job finished");

        match result {
            FetchJobResult::Imported(_)
            | FetchJobResult::NotFound
            | FetchJobResult::SkippedCached
            |             FetchJobResult::SkippedDuplicate => {
                let tree = state.scope_tree(&id).unwrap_or_else(|_| crate::reducer::GlobalTree::new());
                let empty = NodeState::default();
                let node = tree.get(&id).unwrap_or(&empty);
                let sel = html::entity_section_selector(&id);
                let mut b = JsBuilder::new()
                    .morph_selector(&sel, html::entity_section(&id, node, false));
                if kind == FetchKind::Children {
                    b = b.morph_selector("#ranking-panel", ranking_panel(&id, node, &tree));
                }
                yield Ok(js_event(b.build()));
            }
            FetchJobResult::RateLimited { reset_secs } => {
                let tree = state.scope_tree(&id).unwrap_or_else(|_| crate::reducer::GlobalTree::new());
                let empty = NodeState::default();
                let node = tree.get(&id).unwrap_or(&empty);
                let sel = html::entity_section_selector(&id);
                let js = JsBuilder::new()
                    .morph_selector(&sel, html::entity_section(&id, node, false))
                    .raw(&error_js(&format!("Reddit rate limit — retry in {reset_secs}s.")))
                    .build();
                yield Ok(js_event(js));
            }
            FetchJobResult::Failed(msg) => {
                let tree = state.scope_tree(&id).unwrap_or_else(|_| crate::reducer::GlobalTree::new());
                let empty = NodeState::default();
                let node = tree.get(&id).unwrap_or(&empty);
                let sel = html::entity_section_selector(&id);
                let js = JsBuilder::new()
                    .morph_selector(&sel, html::entity_section(&id, node, false))
                    .raw(&error_js(&format!("Fetch failed: {msg}")))
                    .build();
                yield Ok(js_event(js));
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

/// Batch-refresh ranked items in a scope (`SelfEntity` per id), then morph `#ranking-panel`.
pub fn fetch_entities_batch_stream(
    state: AppState,
    parent: ItemId,
    items: Vec<ItemId>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    tracing::debug!(parent = %parent, count = items.len(), "fetch entities batch stream opened");

    let stream = stream! {
        if parent.is_root() && items.is_empty() {
            yield Ok(js_event(error_js("Nothing to refresh.")));
            return;
        }

        let fetchable: Vec<ItemId> = items
            .into_iter()
            .filter(|id| crate::reddit::is_fetchable(id))
            .collect();
        if fetchable.is_empty() {
            yield Ok(js_event(error_js("No fetchable ranked items to refresh.")));
            return;
        }

        tracing::debug!(parent = %parent, count = fetchable.len(), "batch fetch queuing jobs");

        let mut pending = Vec::with_capacity(fetchable.len());
        for id in fetchable {
            let (tx, rx) = oneshot::channel();
            state.queue_entity_fetch(id, FetchKind::SelfEntity, Some(tx));
            pending.push(rx);
        }

        let mut failures = 0usize;
        for rx in pending {
            match rx.await {
                Ok(FetchJobResult::Failed(_)) | Err(_) => failures += 1,
                _ => {}
            }
        }

        let tree = state
            .scope_tree(&parent)
            .unwrap_or_else(|_| crate::reducer::GlobalTree::new());
        let empty = NodeState::default();
        let node = tree.get(&parent).unwrap_or(&empty);
        let mut b = JsBuilder::new().morph_selector(
            "#ranking-panel",
            ranking_panel(&parent, node, &tree),
        );
        if failures > 0 {
            b = b.raw(&error_js(&format!(
                "{failures} refresh request(s) failed — try again in a moment."
            )));
        }
        yield Ok(js_event(b.build()));
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
