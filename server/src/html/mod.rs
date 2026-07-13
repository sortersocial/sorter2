use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup, DOCTYPE};

use std::collections::HashSet;

use crate::{
    auth::nav_pseudonym,
    fetch::html::entity_section,
    form_template::template_json_compact,
    path_types::ItemId,
    ranking::{
        ranked_items_subset, scope_components, RankedItem, MAX_ITERS, TOL,
    },
    reducer::{GlobalTree, NodeState},
    state::AppState,
    ui_action::UI_RPC_FIELD,
};

pub mod sanitize;
pub mod vote;

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

    pub(crate) fn morph_selector_flip(mut self, selector: &str, markup: Markup) -> Self {
        let html = js_string_literal(&markup.into_string());
        self.snippets.push(format!(
            "if (window.sorter2MorphWithFlip) {{ window.sorter2MorphWithFlip({sel}, {html}); }} else {{ var __el = document.querySelector({sel}); if (__el) {{ Idiomorph.morph(__el, {html}); }} }}",
            sel = js_string_literal(selector),
        ));
        self
    }

    pub(crate) fn morph_inner_selector(mut self, selector: &str, markup: Markup) -> Self {
        let html = js_string_literal(&markup.into_string());
        self.snippets.push(format!(
            "var __el = document.querySelector({sel}); if (__el) {{ Idiomorph.morph(__el, {html}, {{ morphStyle: 'innerHTML' }}); }}",
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

pub(crate) fn layout(title: &str, body: Markup, views: u64, nav_user: Option<&str>) -> Markup {
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
                nav class="top-nav" {
                    @if let Some(name) = nav_user {
                        span class="top-nav-user" data-testid="nav-user" { (name) }
                        a href="/login" { "account" }
                        form class="top-nav-logout" method="post" action="/auth/logout" data-navigate="full" {
                            button type="submit" data-testid="nav-logout" { "log out" }
                        }
                    } @else {
                        a href="/login" data-testid="nav-login" { "login" }
                    }
                }
                div id="errors" {}
                (body)
                script src=(js_src) {}
            }
        }
    }
}

pub(crate) fn item_href(id: &ItemId) -> String {
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

fn stable_hash(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325_u64;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn srgb_channel(v: f64) -> f64 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_channel(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn oklch_to_srgb(l: f64, c: f64, h_deg: f64) -> (f64, f64, f64) {
    let h = h_deg.to_radians();
    let a = c * h.cos();
    let b = c * h.sin();
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;
    let r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
    let g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
    let b = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;
    (
        srgb_channel(r).clamp(0.0, 1.0),
        srgb_channel(g).clamp(0.0, 1.0),
        srgb_channel(b).clamp(0.0, 1.0),
    )
}

fn relative_luminance((r, g, b): (f64, f64, f64)) -> f64 {
    0.2126 * linear_channel(r) + 0.7152 * linear_channel(g) + 0.0722 * linear_channel(b)
}

fn scope_base_hue(parent: &ItemId) -> f64 {
    let seed = format!("theme-seed-v1:{}", parent.as_str());
    (stable_hash(&seed) % 360) as f64
}

fn contrast_text_for_oklch(lightness: f64, chroma: f64, hue: f64) -> &'static str {
    let luminance = relative_luminance(oklch_to_srgb(lightness, chroma, hue));
    let contrast_black = (luminance + 0.05) / 0.05;
    let contrast_white = 1.05 / (luminance + 0.05);
    if contrast_black >= contrast_white {
        "#071014"
    } else {
        "#f8fbff"
    }
}

pub fn scope_theme_style(parent: &ItemId) -> String {
    let win_hue = scope_base_hue(parent);
    let lose_hue = (win_hue + 118.0) % 360.0;
    let accent_l = 0.76;
    let accent_c = 0.145;
    let bg_l = 0.13;
    let bg_c = 0.050;
    let accent_fg = contrast_text_for_oklch(accent_l, accent_c, win_hue);
    let fg = contrast_text_for_oklch(bg_l, bg_c, lose_hue);
    format!(
        "--accent: oklch({:.1}% {:.3} {:.1}); --accent-fg: {}; --bg: oklch({:.1}% {:.3} {:.1}); --panel: oklch(18.0% 0.055 {:.1}); --border: oklch(34.0% 0.065 {:.1}); --fg: {}; --muted: oklch(78.0% 0.040 {:.1});",
        accent_l * 100.0,
        accent_c,
        win_hue,
        accent_fg,
        bg_l * 100.0,
        bg_c,
        lose_hue,
        lose_hue,
        lose_hue,
        fg,
        lose_hue
    )
}

/// Map vote mass to gradient position using the group's score range, not raw mass or
/// list position. Vote mass sums to 1 across the component, so absolute values dilute
/// as N grows; min–max within the visible list preserves similar scores → similar colors.
fn score_gradient_t(score: f64, min_score: f64, max_score: f64) -> f64 {
    let spread = max_score - min_score;
    if spread < 1e-9 {
        return 0.5;
    }
    ((max_score - score) / spread).clamp(0.0, 1.0)
}

fn rank_row_style(parent: &ItemId, score: f64, min_score: f64, max_score: f64) -> String {
    let t = score_gradient_t(score, min_score, max_score);
    let base_hue = scope_base_hue(parent);
    let hue = (base_hue + 118.0 * t) % 360.0;
    let lightness = 0.74 - 0.34 * t;
    let chroma = 0.115 + 0.035 * (1.0 - (2.0 * t - 1.0).abs());
    let fg = contrast_text_for_oklch(lightness, chroma, hue);
    format!(
        "--rank-bg: oklch({:.1}% {:.3} {:.1}); --rank-fg: {}; --rank-border: oklch({:.1}% {:.3} {:.1});",
        lightness * 100.0,
        chroma,
        hue,
        fg,
        (lightness + 0.08).min(0.88) * 100.0,
        (chroma * 0.7).min(0.13),
        hue
    )
}

fn rank_list(
    parent: &ItemId,
    label: &str,
    items: &[RankedItem],
    start_rank: usize,
    highlighted: &HashSet<ItemId>,
    tree: &GlobalTree,
) -> Markup {
    let min_score = items.iter().map(|r| r.score).fold(f64::INFINITY, f64::min);
    let max_score = items.iter().map(|r| r.score).fold(f64::NEG_INFINITY, f64::max);
    html! {
        @if !items.is_empty() {
            h3 class="rank-heading muted small" { (label) }
            ol class="rank-list" {
                @for (i, r) in items.iter().enumerate() {
                    @let href = item_href(&r.item);
                    @let style = rank_row_style(parent, r.score, min_score, max_score);
                    @let class = rank_row_class(&r.item, highlighted);
                    li class=(class)
                        data-rank-item=(r.item.as_str())
                        style=(style) {
                        span class="rank-num" { (start_rank + i) ". " }
                        @if let Some(row) = crate::render::reddit::child_row_markup(tree, &r.item, &href) {
                            (row)
                        } @else {
                            a href=(href) {
                                strong { (display_label(&r.item)) }
                            }
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

fn rank_row_class(item: &ItemId, highlighted: &HashSet<ItemId>) -> String {
    let mut class = if crate::render::reddit::is_reddit_post(item) {
        "rank-row reddit-post-row".to_string()
    } else {
        "rank-row".to_string()
    };
    if highlighted.contains(item) {
        class.push_str(" is-compared");
    }
    class
}

fn display_label(id: &ItemId) -> String {
    id.segments().last().map_or("Internet", |v| *v).to_string()
}

fn child_label(tree: &GlobalTree, id: &ItemId) -> String {
    tree.get(id)
        .and_then(|n| n.data.as_ref())
        .map(|d| d.title.clone())
        .unwrap_or_else(|| display_label(id))
}

/// Plain (unscored) list of children that have no votes yet.
fn unranked_list(
    label: &str,
    items: &[ItemId],
    highlighted: &HashSet<ItemId>,
    tree: &GlobalTree,
) -> Markup {
    html! {
        @if !items.is_empty() {
            h3 class="rank-heading muted small" { (label) }
            ul class="rank-list unranked" {
                @for it in items {
                    @let href = item_href(it);
                    @let class = rank_row_class(it, highlighted);
                    li class=(class)
                        data-rank-item=(it.as_str()) {
                        @if let Some(row) = crate::render::reddit::child_row_markup(tree, it, &href) {
                            (row)
                        } @else {
                            a href=(href) {
                                strong { (child_label(tree, it)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub fn ranking_panel(item: &ItemId, node: &NodeState, tree: &GlobalTree) -> Markup {
    ranking_panel_with_highlights(item, node, tree, &HashSet::new())
}

pub fn ranking_panel_with_highlights(
    item: &ItemId,
    node: &NodeState,
    tree: &GlobalTree,
    highlighted: &HashSet<ItemId>,
) -> Markup {
    let scope = &node.votes;
    let (comps, _isolates, _) =
        scope_components(scope);

    // Each connected component of voted items is its own ranking; isolated and
    // never-voted children fall into the "unranked" bucket below.
    let mut ranked_ids: HashSet<ItemId> = HashSet::new();
    let mut ranked_groups: Vec<Vec<RankedItem>> = Vec::new();
    for comp in &comps {
        if comp.len() < 2 {
            continue;
        }
        let ranked = ranked_items_subset(scope, comp, MAX_ITERS, TOL);
        for r in &ranked {
            ranked_ids.insert(r.item.clone());
        }
        ranked_groups.push(ranked);
    }
    ranked_groups.sort_by(|a, b| b.len().cmp(&a.len()));

    let mut unranked: Vec<ItemId> = node
        .children
        .iter()
        .filter(|c| !ranked_ids.contains(*c))
        .cloned()
        .collect();
    unranked.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    let has_ranked = !ranked_groups.is_empty();
    let multi = ranked_groups.len() > 1;

    html! {
        section id="ranking-panel" class="demo-panel" {
            @if !has_ranked && unranked.is_empty() {
                p class="muted" {
                    @if item.is_root() {
                        "No votes yet — compare two items below."
                    } @else {
                        "No votes yet for " (item.as_str()) " — compare two items below to start the ranking."
                    }
                }
            } @else {
                @for (gi, ranked) in ranked_groups.iter().enumerate() {
                    @let label = if multi { format!("Ranking group {}", gi + 1) } else { "Ranking".to_string() };
                    (rank_list(item, &label, ranked, 1, highlighted, tree))
                }
                (unranked_list("Unranked", &unranked, highlighted, tree))
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

async fn item_page(state: AppState, uri: Uri, item: ItemId, jar: CookieJar) -> Markup {
    let path = uri.path().to_string();
    state.views.increment(path.clone());
    let views = state.views.get_views(&path);
    let nav_user = nav_pseudonym(state.projection_store.db(), &jar);

    let tree = state
        .scope_tree(&item)
        .unwrap_or_else(|_| GlobalTree::new());
    let empty_node = NodeState::default();
    let node = tree.get(&item).unwrap_or(&empty_node);

    let child_count = node.children.len();
    let vote_link = if child_count >= 2 {
        Some(vote::vote_href(&item))
    } else {
        None
    };

    let body = html! {
        div class="scope-theme" style=(scope_theme_style(&item)) {
            h1 { "sorter" }
            (input_panel("", None))
            (breadcrumb_path(&item))
            (entity_section(&item, node, false))
            @if let Some(href) = vote_link {
                p class="vote-cta" {
                    a class="btn-primary" href=(href) data-testid="vote-children" { "Vote on children" }
                }
            }
            (ranking_panel(&item, node, &tree))
        }
    };
    layout("sorter2", body, views, nav_user.as_deref())
}

pub async fn home(
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    item_page(state, uri, ItemId::root(), jar).await
}

pub async fn browse(
    State(state): State<AppState>,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let item = ItemId::from_browse_uri(uri.path()).unwrap_or(ItemId::root());
    item_page(state, uri, item, jar).await
}

#[cfg(test)]
mod tests {
    use super::{rank_row_style, score_gradient_t, SORTER_UI_JS};
    use crate::path_types::ItemId;

    #[test]
    fn score_gradient_t_uses_group_range_not_absolute_mass() {
        assert!((score_gradient_t(0.12, 0.08, 0.12) - 0.0).abs() < 1e-9);
        assert!((score_gradient_t(0.08, 0.08, 0.12) - 1.0).abs() < 1e-9);
        // Raw 12% mass would map near the dark end globally; within this group it's the top.
        assert!(score_gradient_t(0.12, 0.08, 0.12) < score_gradient_t(0.12, 0.0, 1.0));
    }

    #[test]
    fn score_gradient_t_similar_scores_similar_t() {
        let a = score_gradient_t(0.41, 0.20, 0.60);
        let b = score_gradient_t(0.40, 0.20, 0.60);
        assert!((a - b).abs() < 0.05);
        assert!((a - score_gradient_t(0.60, 0.20, 0.60)).abs() > 0.3);
    }

    #[test]
    fn score_gradient_t_tied_scores_neutral() {
        assert!((score_gradient_t(0.25, 0.25, 0.25) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn rank_row_style_same_inputs_same_color() {
        let parent = ItemId::opaque("test-scope");
        assert_eq!(
            rank_row_style(&parent, 0.33, 0.20, 0.60),
            rank_row_style(&parent, 0.33, 0.20, 0.60),
        );
    }

    #[test]
    fn vote_slider_left_position_favors_left_item() {
        assert!(SORTER_UI_JS.contains("Math.max(1, 100 - v)"));
        assert!(SORTER_UI_JS.contains("Math.max(1, v)"));
        assert!(SORTER_UI_JS.contains("var divisor = gcd(left, right)"));
        assert!(SORTER_UI_JS.contains("ratioDisplay.textContent = left + ':' + right"));
        assert!(SORTER_UI_JS.contains("--vote-slider-pct"));
        assert!(SORTER_UI_JS.contains("dataset.winner"));
    }
}
