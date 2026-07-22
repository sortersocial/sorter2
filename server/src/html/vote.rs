//! Pairwise vote UI — `/vote?parent=` with optional `left` / `right`.

use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};
use serde::Deserialize;
use std::collections::HashSet;

use crate::{
    auth::nav_pseudonym,
    fetch::html::{entity_section, nsfw_enter_panel},
    form_template::template_json_compact,
    html::{ranking_panel_with_highlights, scope_theme_style, JsBuilder},
    nsfw::{item_is_nsfw, nsfw_allowed},
    pair::{resolve_pair_in_pool, suggest_next_pair_in_pool, PairError},
    path_types::ItemId,
    reducer::{GlobalTree, NodeState, ScopeVotes, VoteData},
    skip::{for_jar as user_skips_for_jar, visible_unskipped_children},
    state::{parse_item_param, AppState},
    ui_action::UI_RPC_FIELD,
};

use super::{breadcrumb_path, layout};

#[derive(Debug, Deserialize)]
pub struct VoteQuery {
    pub parent: String,
    #[serde(default)]
    pub left: Option<String>,
    #[serde(default)]
    pub right: Option<String>,
}

pub fn vote_href(parent: &ItemId) -> String {
    format!("/vote?parent={}", urlencoding::encode(parent.as_str()))
}

fn vote_compare_href(parent: &ItemId, left: &ItemId, right: &ItemId) -> String {
    format!(
        "/vote?parent={}&left={}&right={}",
        urlencoding::encode(parent.as_str()),
        urlencoding::encode(left.as_str()),
        urlencoding::encode(right.as_str()),
    )
}

fn display_label(id: &ItemId) -> String {
    id.segments()
        .last()
        .map_or("item".into(), |v| v.to_string())
}

fn child_title(tree: &GlobalTree, id: &ItemId) -> String {
    tree.get(id)
        .and_then(|n| n.data.as_ref())
        .map(|d| d.title.clone())
        .unwrap_or_else(|| display_label(id))
}

fn ratios_for_page(v: &VoteData, page_left: &ItemId, page_right: &ItemId) -> (i32, i32) {
    match (v.a.as_str(), v.b.as_str()) {
        (a, b) if a == page_left.as_str() && b == page_right.as_str() => {
            (v.ratio_left, v.ratio_right)
        }
        (a, b) if a == page_right.as_str() && b == page_left.as_str() => {
            (v.ratio_right, v.ratio_left)
        }
        _ => (v.ratio_left, v.ratio_right),
    }
}

fn edge_votes(scope: &ScopeVotes, left: &ItemId, right: &ItemId) -> Vec<VoteData> {
    scope
        .recent_votes
        .iter()
        .filter(|v| {
            (v.a.as_str() == left.as_str() && v.b.as_str() == right.as_str())
                || (v.a.as_str() == right.as_str() && v.b.as_str() == left.as_str())
        })
        .cloned()
        .collect()
}

/// HUD `data-winner` value: which side the ratio favours on this page.
fn winner_side(r_left: i32, r_right: i32) -> &'static str {
    if r_left > r_right {
        "left"
    } else if r_right > r_left {
        "right"
    } else {
        "even"
    }
}

fn winner_text(r_left: i32, r_right: i32) -> &'static str {
    match winner_side(r_left, r_right) {
        "left" => "left wins",
        "right" => "right wins",
        _ => "tie",
    }
}

/// Map stored ratios to the live slider position (0 = full left, 100 = full right).
/// Matches `sorter_ui.js`: `left = 100 - v`, `right = v`.
fn slider_value_from_ratios(r_left: i32, r_right: i32) -> i32 {
    let l = r_left.max(0) as f64;
    let r = r_right.max(0) as f64;
    let sum = l + r;
    if sum <= 0.0 {
        return 50;
    }
    ((r / sum) * 100.0).round().clamp(0.0, 100.0) as i32
}

fn vote_edge_history(
    tree: &GlobalTree,
    scope: &ScopeVotes,
    left: &ItemId,
    right: &ItemId,
) -> Markup {
    let mut votes = edge_votes(scope, left, right);
    votes.sort_by(|a, b| b.ts.cmp(&a.ts));
    let legend_left = child_title(tree, left);
    let legend_right = child_title(tree, right);
    html! {
        @if votes.is_empty() {
            p class="muted vote-edge-empty" { "no votes on this pair yet" }
        } @else {
            h3 class="vote-edge-history-title" {
                "votes on this pair"
            }
            p class="muted small vote-edge-legend" {
                (format!("left: {legend_left} — right: {legend_right}"))
            }
            ul class="vote-edge-history" {
                @for v in &votes {
                    @let (r_left, r_right) = ratios_for_page(v, left, right);
                    @let slider_val = slider_value_from_ratios(r_left, r_right);
                    @let side = winner_side(r_left, r_right);
                    @let label = winner_text(r_left, r_right);
                    li class="vote-edge-history-row" {
                        div class="vote-edge-meta" {
                            span class="vote-edge-ratio" { (format!("{}:{}", r_left, r_right)) }
                            span class="vote-edge-winner muted small" { " · " (label) }
                        }
                        label class="vote-hud-slider vote-edge-slider" aria-hidden="true" {
                            input type="range" class="vote-edge-range" min="0" max="100" value=(slider_val)
                                data-winner=(side)
                                style={(format!("--vote-slider-pct: {}%;", slider_val))}
                                disabled
                                tabindex="-1";
                        }
                    }
                }
            }
        }
    }
}

fn vote_hud_form(
    parent: &ItemId,
    left: &ItemId,
    right: &ItemId,
    rpc_json: &str,
    next_pair: Option<&(ItemId, ItemId)>,
) -> Markup {
    html! {
        div id="vote-hud" class="vote-hud" role="region" aria-label="Vote controls" {
            form id="vote-compare-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(rpc_json);
                input type="hidden" name="ratio_left" id="vote-ratio-left" value="1";
                input type="hidden" name="ratio_right" id="vote-ratio-right" value="1";
                div class="vote-hud-inner" {
                    div class="vote-ratio-readout" {
                        span class="muted small" { "ratio " }
                        strong id="vote-ratio-display" { "1:1" }
                    }
                    label class="vote-hud-slider" {
                        input type="range" id="vote-preference-slider" min="0" max="100" value="50"
                            data-winner="even"
                            aria-valuemin="0" aria-valuemax="100" aria-valuenow="50"
                            aria-label=(format!("Preference: {} vs {}", left.as_str(), right.as_str()));
                    }
                    (vote_compare_actions(parent, next_pair))
                }
            }
        }
    }
}

fn vote_compare_actions(parent: &ItemId, next: Option<&(ItemId, ItemId)>) -> Markup {
    let next_href = next.map(|(l, r)| vote_compare_href(parent, l, r));
    html! {
        div id="vote-compare-actions" class="vote-compare-actions" {
            button type="submit" class="btn-primary" data-testid="vote-post" { "post vote" }
            @if let Some(href) = &next_href {
                a class="btn-secondary vote-compare-next" data-testid="vote-next-pair" href=(href) { "next pair" }
            } @else {
                span class="btn-secondary vote-compare-next is-disabled" { "no next pair" }
            }
        }
    }
}

fn vote_ranking_sidebar(
    tree: &GlobalTree,
    parent: &ItemId,
    left: &ItemId,
    right: &ItemId,
    nsfw_ok: bool,
    skipped: &HashSet<ItemId>,
) -> Markup {
    let empty = NodeState::default();
    let node = tree.get(parent).unwrap_or(&empty);
    let highlighted: HashSet<ItemId> = [left.clone(), right.clone()].into_iter().collect();
    html! {
        aside id="vote-ranking-panel" class="vote-ranking-panel demo-panel" aria-live="polite" {
            h2 { "live ranking" }
            p class="muted small" { "updates as comparisons land" }
            (ranking_panel_with_highlights(parent, node, tree, &highlighted, nsfw_ok, skipped))
        }
    }
}

/// After recording a vote on the compare page: refresh edge history and next-pair link.
pub(crate) fn vote_recorded_morph(
    tree: &GlobalTree,
    parent: &ItemId,
    left: &ItemId,
    right: &ItemId,
    nsfw_ok: bool,
    skipped: &HashSet<ItemId>,
) -> JsBuilder {
    let pool = visible_unskipped_children(tree, parent, nsfw_ok, skipped);
    let empty = NodeState::default();
    let scope = tree.get(parent).unwrap_or(&empty).votes.clone();
    let edge_history = vote_edge_history(tree, &scope, left, right);
    let next_pair = suggest_next(&scope, left, right, &pool);
    let actions = vote_compare_actions(parent, next_pair.as_ref());
    let sidebar = vote_ranking_sidebar(tree, parent, left, right, nsfw_ok, skipped);
    JsBuilder::new()
        .morph_inner_selector("#vote-edge-history-region", edge_history)
        .morph_selector("#vote-compare-actions", actions)
        .morph_selector_flip("#vote-ranking-panel", sidebar)
}

fn vote_compare_item_card(
    tree: &GlobalTree,
    parent: &ItemId,
    item: &ItemId,
    side_class: &str,
    side_name: &str,
    nsfw_ok: bool,
) -> Markup {
    let node = tree.get(item).cloned().unwrap_or_else(|| NodeState {
        id: item.clone(),
        ..Default::default()
    });
    let skip_rpc = template_json_compact(&serde_json::json!({
        "action": "skip_item",
        "item": item.as_str(),
        "parent": parent.as_str(),
    }))
    .expect("skip item rpc json");
    html! {
        div class=(format!("vote-compare-side {side_class}")) {
            (entity_section(item, &node, false, nsfw_ok))
            form class="vote-skip-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(skip_rpc);
                button type="submit" class="btn-secondary vote-skip"
                    data-testid=(format!("vote-skip-{side_name}")) {
                    "skip this item"
                }
            }
        }
    }
}

fn suggest_next(
    scope: &ScopeVotes,
    left: &ItemId,
    right: &ItemId,
    pool: &[ItemId],
) -> Option<(ItemId, ItemId)> {
    suggest_next_pair_in_pool(scope, pool, Some((left, right)))
}

pub async fn vote_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(q): Query<VoteQuery>,
) -> impl IntoResponse {
    let parent = parse_item_param(&q.parent);
    let left_param = q.left.as_deref().map(parse_item_param);
    let right_param = q.right.as_deref().map(parse_item_param);
    let nsfw_ok = nsfw_allowed(&jar);
    let skipped = user_skips_for_jar(&state.projection_store, &jar);

    let tree = state
        .scope_tree(&parent)
        .unwrap_or_else(|_| GlobalTree::new());
    let empty = NodeState::default();
    let parent_node = tree.get(&parent).unwrap_or(&empty);

    let path = format!("/vote?parent={}", urlencoding::encode(parent.as_str()));
    let return_to = match (&q.left, &q.right) {
        (Some(l), Some(r)) => format!(
            "{}&left={}&right={}",
            path,
            urlencoding::encode(l),
            urlencoding::encode(r)
        ),
        _ => path.clone(),
    };

    if item_is_nsfw(&tree, &parent) && !nsfw_ok {
        state.views.increment(path.clone());
        let views = state.views.get_views(&path);
        let nav_user = nav_pseudonym(state.projection_store.db(), &jar);
        let body = html! {
            div class="scope-theme vote-page" style=(scope_theme_style(&parent)) {
                h1 { "compare" }
                (breadcrumb_path(&parent))
                (nsfw_enter_panel(&return_to))
            }
        };
        return Html(
            layout(
                "vote · NSFW",
                body,
                views,
                nav_user.as_deref(),
                false,
                &return_to,
            )
            .into_string(),
        )
        .into_response();
    }

    let pool = visible_unskipped_children(&tree, &parent, nsfw_ok, &skipped);
    let (left, right) = match resolve_pair_in_pool(
        &tree,
        &parent,
        &pool,
        left_param.as_ref(),
        right_param.as_ref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            let (msg, status) = e.status_message();
            return (status, msg).into_response();
        }
    };

    if (!nsfw_ok) && (item_is_nsfw(&tree, &left) || item_is_nsfw(&tree, &right)) {
        let (msg, status) = PairError::NotChild.status_message();
        return (status, msg).into_response();
    }

    let scope = &parent_node.votes;
    let next_pair = suggest_next(&scope, &left, &right, &pool);
    let edge_history = vote_edge_history(&tree, &scope, &left, &right);

    let rpc_json = template_json_compact(&serde_json::json!({
        "action": "record_vote",
        "a": left.as_str(),
        "b": right.as_str(),
        "ratio_left": {"$form:i32": "ratio_left"},
        "ratio_right": {"$form:i32": "ratio_right"},
        "scope": parent.as_str(),
        "vote_compare": true,
    }))
    .expect("vote rpc json");

    let title = format!(
        "vote — {} vs {}",
        child_title(&tree, &left),
        child_title(&tree, &right)
    );

    let body = html! {
        div class="scope-theme vote-page" style=(scope_theme_style(&parent)) {
            div class="vote-page-grid" {
                section class="vote-compare-shell" {
                    h1 { "compare" }
                    (breadcrumb_path(&parent))
                    div class="vote-compare-pair" data-winner="even" {
                        (vote_compare_item_card(&tree, &parent, &left, "vote-compare-left", "left", nsfw_ok))
                        span class="vote-compare-vs" { "vs" }
                        (vote_compare_item_card(&tree, &parent, &right, "vote-compare-right", "right", nsfw_ok))
                    }
                    div id="vote-edge-history-region" {
                        (edge_history)
                    }
                }
                (vote_ranking_sidebar(&tree, &parent, &left, &right, nsfw_ok, &skipped))
            }
            (vote_hud_form(&parent, &left, &right, &rpc_json, next_pair.as_ref()))
        }
    };

    state.views.increment(path.clone());
    let views = state.views.get_views(&path);
    let nav_user = nav_pseudonym(state.projection_store.db(), &jar);

    Html(
        layout(
            &title,
            body,
            views,
            nav_user.as_deref(),
            nsfw_ok,
            &return_to,
        )
        .into_string(),
    )
    .into_response()
}

#[cfg(test)]
mod polarity_tests {
    use super::*;
    use crate::identity::{DEFAULT_PSEUDONYM, TEST_ACTOR_UUID};
    use crate::ranking::ranked_items;
    use crate::reducer::GlobalTree;

    fn id(s: &str) -> ItemId {
        ItemId::parse(s).unwrap()
    }

    /// The page's left number must always equal the vote's weight for the
    /// item shown on the left, regardless of which order the vote stored a/b.
    #[test]
    fn ratios_for_page_orients_to_page_left() {
        let left = id("left_item");
        let right = id("right_item");

        // Stored a == page left: keep order.
        let v1 = VoteData::from_event(
            1,
            left.as_str(),
            right.as_str(),
            9,
            1,
            DEFAULT_PSEUDONYM.to_string(),
            1.0,
        )
        .unwrap();
        assert_eq!(ratios_for_page(&v1, &left, &right), (9, 1));

        // Stored a == page right: swap so left stays left.
        let v2 = VoteData::from_event(
            2,
            right.as_str(),
            left.as_str(),
            9,
            1,
            DEFAULT_PSEUDONYM.to_string(),
            1.0,
        )
        .unwrap();
        assert_eq!(ratios_for_page(&v2, &left, &right), (1, 9));
    }

    #[test]
    fn winner_side_follows_larger_ratio() {
        assert_eq!(winner_side(9, 1), "left");
        assert_eq!(winner_side(1, 9), "right");
        assert_eq!(winner_side(1, 1), "even");
    }

    #[test]
    fn slider_value_matches_hud_mapping() {
        assert_eq!(slider_value_from_ratios(9, 1), 10);
        assert_eq!(slider_value_from_ratios(1, 4), 80);
        assert_eq!(slider_value_from_ratios(1, 1), 50);
    }

    /// End-to-end polarity invariant: a vote that favours the LEFT item (higher
    /// `ratio_left`, recorded as the RPC's `a`) must make that item rank #1.
    /// This is the property the UI must preserve: sliding left => left wins.
    #[test]
    fn sliding_left_makes_left_item_win_ranking() {
        let parent = id("scope");
        let left = id("left_item");
        let right = id("right_item");

        // Slider dragged left yields e.g. 9:1 with a = left item.
        let vote = VoteData::from_event(
            1,
            left.as_str(),
            right.as_str(),
            9,
            1,
            DEFAULT_PSEUDONYM.to_string(),
            1.0,
        )
        .unwrap();
        let mut tree = GlobalTree::new();
        tree.apply_vote(&parent, vote, TEST_ACTOR_UUID);

        let scope = &tree.get(&parent).unwrap().votes;
        let ranked = ranked_items(scope);
        assert_eq!(
            ranked[0].item, left,
            "left item should rank first when ratio favours the left"
        );
    }
}
