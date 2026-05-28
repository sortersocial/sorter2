use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;

use crate::{
    form_template::template_json_compact,
    parser_action::ParserAction,
    parser_render::parser_panel,
    ranking::ranked_items_cached,
    reducer::GroupState,
    state::AppState,
    ui_action::UI_RPC_FIELD,
};

const THEME_DEFAULT_CSS: &str = include_str!("../../static/theme_default.css");
const THEME_RETRO_CSS: &str = include_str!("../../static/theme_retro.css");
const SORTER_UI_JS: &str = include_str!("../../static/sorter_ui.js");

pub const SORTER_THEME_COOKIE: &str = "sorter-theme";

pub fn normalize_theme(raw: &str) -> &'static str {
    match raw {
        "retro" => "retro",
        _ => "default",
    }
}

pub fn theme_from_jar(jar: &CookieJar) -> &'static str {
    jar.get(SORTER_THEME_COOKIE)
        .map(|c| normalize_theme(c.value()))
        .unwrap_or("default")
}

pub fn theme_next_from_uri(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|pq| pq.as_str().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

pub fn theme_cookie_header_value(theme: &str) -> HeaderValue {
    let t = normalize_theme(theme);
    let s = format!("{SORTER_THEME_COOKIE}={t}; Path=/; SameSite=Lax; Max-Age=31536000");
    HeaderValue::from_str(&s).expect("theme cookie must be ASCII")
}

fn sanitize_theme_next(next: Option<&str>) -> String {
    let s = next.unwrap_or("/").trim();
    if s.starts_with('/') && !s.starts_with("//") && s.len() < 8192 {
        s.to_string()
    } else {
        "/".to_string()
    }
}

#[derive(Debug, Deserialize)]
pub struct ThemeForm {
    theme: String,
    next: Option<String>,
}

pub async fn post_theme(Form(form): Form<ThemeForm>) -> impl IntoResponse {
    let theme = normalize_theme(&form.theme);
    let next = sanitize_theme_next(form.next.as_deref());
    let loc =
        HeaderValue::try_from(next.as_str()).unwrap_or_else(|_| HeaderValue::from_static("/"));
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, loc)
        .header(header::SET_COOKIE, theme_cookie_header_value(theme))
        .body(Body::empty())
        .expect("theme redirect response")
}

pub async fn serve_static(Path(filename): Path<String>) -> impl IntoResponse {
    if filename == "sorter_ui.js" {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(SORTER_UI_JS.to_string())
            .unwrap()
            .into_response();
    }

    let theme = filename
        .strip_prefix("theme_")
        .and_then(|s| s.strip_suffix(".css"));

    let css = match theme {
        Some("default") => THEME_DEFAULT_CSS,
        Some("retro") => THEME_RETRO_CSS,
        _ => return (StatusCode::NOT_FOUND, "static file not found").into_response(),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(css.to_string())
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

pub fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

fn layout(title: &str, body: Markup, views: u64, theme: &str, theme_next: &str) -> Markup {
    let css_href = format!("/static/theme_{theme}.css");
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href=(css_href) id="theme-stylesheet";
                script src="https://unpkg.com/idiomorph@0.3.0/dist/idiomorph.min.js" {}
            }
            body class="home" {
                @if views > 0 {
                    span class="view-meta muted" { (views) " views" }
                }
                div id="errors" {}
                (body)
                div id="controls" {
                    a href="https://github.com/sortersocial/sorter2" id="src-link" { "src" }
                    form id="sorter-theme-form" method="post" action="/theme" data-navigate="full" {
                        input type="hidden" name="next" value=(theme_next);
                        select id="theme-select" name="theme" onchange="this.form.submit()" aria-label="Theme" {
                            @for (val, label) in [("default", "default"), ("retro", "retro")] {
                                @if theme == val {
                                    option value=(val) selected { (label) }
                                } @else {
                                    option value=(val) { (label) }
                                }
                            }
                        }
                    }
                }
                script src="/static/sorter_ui.js" {}
            }
        }
    }
}

pub fn ranking_panel(group: &GroupState) -> Markup {
    let items = ranked_items_cached(group);
    html! {
        section id="ranking-panel" class="demo-panel" {
            h2 { "Ranking" }
            @if items.is_empty() {
                p class="muted" { "No votes yet — compare two items below." }
            } @else {
                ol class="rank-list" {
                    @for (i, r) in items.iter().enumerate() {
                        li {
                            span class="rank-num" { (i + 1) ". " }
                            strong { (r.item.as_str()) }
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
}

pub fn vote_panel() -> Markup {
    let rpc = template_json_compact(&serde_json::json!({
        "action": "record_vote",
        "a": {"$form": "item_a"},
        "b": {"$form": "item_b"},
        "ratio_left": 2,
        "ratio_right": 1
    }))
    .expect("vote rpc json");
    html! {
        section id="vote-panel" class="demo-panel" {
            h2 { "Compare" }
            p class="muted small" {
                "Left item wins at 2:1. Votes append to the JSONL log and update rank centrality."
            }
            form method="post" action="/ui" id="vote-form" {
                input type="hidden" name=(UI_RPC_FIELD) value=(rpc);
                div class="vote-fields" {
                    label {
                        "Left (wins) "
                        input type="text" name="item_a" required placeholder="alpha" autocomplete="off";
                    }
                    label {
                        "Right "
                        input type="text" name="item_b" required placeholder="beta" autocomplete="off";
                    }
                }
                button type="submit" class="btn-primary" { "Vote" }
            }
        }
    }
}


pub async fn home(
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let path = uri.path().to_string();
    state.views.increment(path.clone());
    let views = state.views.get_views(&path);
    let theme = theme_from_jar(&jar);
    let theme_next = theme_next_from_uri(&uri);
    let group = state.group.read().await;
    let empty_action = ParserAction::suggest(String::new(), None);
    let body = html! {
        h1 { "sorter2" }
        (parser_panel("", &empty_action))
        (vote_panel())
        (ranking_panel(&group))
    };
    layout("sorter2", body, views, theme, &theme_next)
}
