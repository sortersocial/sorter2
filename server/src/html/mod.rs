use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use maud::{html, Markup, DOCTYPE};

use crate::{
    form_template::template_json_compact,
    path_types::ItemId,
    ranking::{top_bottom, RankedItem},
    reducer::{GroupState, NodeState},
    state::AppState,
    ui_action::UI_RPC_FIELD,
};

const SORTER_CSS: &str = include_str!("../../static/sorter.css");
const SORTER_UI_JS: &str = include_str!("../../static/sorter_ui.js");

pub async fn serve_static(Path(filename): Path<String>) -> impl IntoResponse {
    let (content_type, body) = match filename.as_str() {
        "sorter.css" => ("text/css; charset=utf-8", SORTER_CSS),
        "sorter_ui.js" => ("text/javascript; charset=utf-8", SORTER_UI_JS),
        _ => return (StatusCode::NOT_FOUND, "static file not found").into_response(),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(body.to_string())
        .unwrap()
        .into_response()
}

pub(crate) fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s).expect("javascript string escaping")
}

pub(crate) struct JsBuilder {
    snippets: Vec<String>,
}

impl JsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            snippets: Vec::new(),
        }
    }

    pub(crate) fn morph_selector(mut self, selector: &str, markup: Markup) -> Self {
        let html = js_string_literal(&markup.into_string());
        self.snippets.push(format!(
            "var __el = document.querySelector({sel}); if (__el) {{ Idiomorph.morph(__el, {html}); }}",
            sel = js_string_literal(selector),
        ));
        self
    }

    pub(crate) fn raw(mut self, js: &str) -> Self {
        if !js.is_empty() {
            self.snippets.push(js.to_string());
        }
        self
    }

    pub(crate) fn build(self) -> String {
        self.snippets.join(" ")
    }

    pub(crate) fn into_response(self) -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
            .body(Body::from(self.build()))
            .unwrap()
    }
}

/// Short content hash of the bundled static assets, used as a `?v=` cache
/// buster so CSS/JS changes take effect immediately instead of being masked by
/// the `max-age` on `/static`.
fn asset_version() -> &'static str {
    use std::sync::OnceLock;
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        SORTER_CSS.hash(&mut h);
        SORTER_UI_JS.hash(&mut h);
        format!("{:x}", h.finish())
    })
}

pub fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

fn layout(title: &str, body: Markup, views: u64) -> Markup {
    let ver = asset_version();
    let css_href = format!("/static/sorter.css?v={ver}");
    let js_src = format!("/static/sorter_ui.js?v={ver}");
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href=(css_href);
                script src="https://unpkg.com/idiomorph@0.3.0/dist/idiomorph.min.js" {}
            }
            body class="home" {
                @if views > 0 {
                    span class="view-meta muted" { (views) " views" }
                }
                div id="errors" {}
                (body)
                script src=(js_src) {}
            }
        }
    }
}

fn item_href(id: &ItemId) -> String {
    id.browse_href()
}

fn segment_label(seg: &str) -> &str {
    seg
}

/// Generic breadcrumb trail from an [`ItemId`] path.
pub fn breadcrumb_path(item: &ItemId) -> Markup {
    html! {
        nav class="breadcrumbs" aria-label="Breadcrumb" {
            a href="/" { "~" }
            @for path in item.breadcrumb_paths() {
                @let seg = path.segments().last().map_or("", |v| *v);
                span class="separator" { " / " }
                a href=(item_href(&path)) { (segment_label(seg)) }
            }
        }
    }
}

fn entity_panel(node: &NodeState) -> Markup {
    html! {
        @if let Some(data) = &node.data {
            section id="entity-panel" class="demo-panel entity-card" {
                h2 { (data.title) }
                @if let Some(author) = &data.author {
                    p class="muted small" { "by " (author) }
                }
                @if let Some(body) = &data.body_html {
                    div class="entity-body" { (maud::PreEscaped(body)) }
                }
            }
        }
    }
}

fn rank_list(label: &str, items: &[RankedItem], start_rank: usize) -> Markup {
    html! {
        @if !items.is_empty() {
            h3 class="rank-heading muted small" { (label) }
            ol class="rank-list" {
                @for (i, r) in items.iter().enumerate() {
                    li {
                        span class="rank-num" { (start_rank + i) ". " }
                        a href=(item_href(&r.item)) {
                            strong { (display_label(&r.item)) }
                        }
                        span class="muted" {
                            " — "
                            ({ format!("{:.1}%", r.score * 100.0) })
                        }
                    }
                }
            }
        }
    }
}

fn display_label(id: &ItemId) -> String {
    id.segments()
        .last()
        .map_or("Internet", |v| *v)
        .to_string()
}

pub fn ranking_panel(item: &ItemId, group: &GroupState) -> Markup {
    let total = group.idx_to_item.len();
    let (top, bottom) = top_bottom(group, 8);
    html! {
        section id="ranking-panel" class="demo-panel" {
            @if total == 0 {
                p class="muted" {
                    @if item.is_root() {
                        "No votes yet — compare two items below."
                    } @else {
                        "No votes yet for " (item.as_str()) " — compare two items below to start the ranking."
                    }
                }
            } @else {
                (rank_list(if bottom.is_empty() { "" } else { "Top" }, &top, 1))
                @if !bottom.is_empty() {
                    p class="rank-gap muted small" { "⋯" }
                    (rank_list("Bottom", &bottom, total - bottom.len() + 1))
                }
            }
        }
    }
}

pub fn input_panel(query: &str, error: Option<&str>) -> Markup {
    let rpc = template_json_compact(&serde_json::json!({
        "action": "parse_query",
        "query": {"$form": "query"},
    }))
    .expect("parse_query rpc template");
    html! {
        section id="parser-panel" class="demo-panel" {
            form method="post" action="/ui" id="parser-form" {
                input
                    type="text"
                    name="query"
                    id="parser-input"
                    rows="3"
                    placeholder="https://reddit.com/r/rust or r/rust"
                    autocomplete="off"
                    spellcheck="false" {
                    (query)
                }
                input type="hidden" name=(UI_RPC_FIELD) value=(rpc);
                button type="submit" class="btn-primary" { "Go" }
            }
            @if let Some(msg) = error {
                p class="parser-error muted" { (msg) }
            }
        }
    }
}

async fn item_page(state: AppState, uri: Uri, item: ItemId) -> Markup {
    let path = uri.path().to_string();
    state.views.increment(path.clone());
    let views = state.views.get_views(&path);

    let tree = state.tree.read().await;
    let empty_node = NodeState::default();
    let node = tree.get(&item).unwrap_or(&empty_node);
    let group = &node.local_ranking;

    let body = html! {
        h1 { "sorter" }
        (input_panel("", None))
        (breadcrumb_path(&item))
        (entity_panel(node))
        (ranking_panel(&item, group))
    };
    layout("sorter2", body, views)
}

pub async fn home(State(state): State<AppState>, uri: Uri) -> impl IntoResponse {
    item_page(state, uri, ItemId::root()).await
}

pub async fn browse(State(state): State<AppState>, uri: Uri) -> impl IntoResponse {
    let item = ItemId::from_browse_uri(uri.path()).unwrap_or(ItemId::root());
    if item.as_str().starts_with("reddit.com") {
        let needs_fetch = {
            let tree = state.tree.read().await;
            tree.get(&item)
                .map(|n| n.data.is_none())
                .unwrap_or(true)
        };
        if needs_fetch {
            state.reddit.request_fetch(item.clone());
        }
    }
    item_page(state, uri, item).await
}
