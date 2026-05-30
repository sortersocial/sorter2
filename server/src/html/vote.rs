//! Pairwise vote UI — `/vote?parent=` with optional `left` / `right`.

use axum::{
    extract::{Query, State},
    response::{Html, IntoResponse},
};
use maud::{html, Markup};
use serde::Deserialize;

use crate::{
    fetch::html::entity_section,
    form_template::template_json_compact,
    html::JsBuilder,
    pair::{children_of, resolve_pair, suggest_next_pair_in_pool},
    path_types::ItemId,
    reducer::{GlobalTree, GroupState, NodeState, VoteData},
    state::{parse_item_param, AppState},
    ui_action::UI_RPC_FIELD,
};

use super::{breadcrumb_path, item_href, layout};

#[derive(Debug, Deserialize)]
pub struct VoteQuery {
    pub parent: String,
    #[serde(default)]
    pub left: Option<String>,
    #[serde(default)]
    pub right: Option<String>,
}

pub fn vote_href(parent: &ItemId) -> String {
    format!(
        "/vote?parent={}",
        urlencoding::encode(parent.as_str())
    )
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

fn ratio_pct(ratio_left: i32, ratio_right: i32) -> f64 {
    let l = ratio_left.max(0) as f64;
    let r = ratio_right.max(0) as f64;
    let sum = l + r;
    if sum <= 0.0 {
        50.0
    } else {
        (l / sum) * 100.0
    }
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

fn edge_votes(group: &GroupState, left: &ItemId, right: &ItemId) -> Vec<VoteData> {
    group
        .recent_votes
        .iter()
        .filter(|v| {
            (v.a.as_str() == left.as_str() && v.b.as_str() == right.as_str())
                || (v.a.as_str() == right.as_str() && v.b.as_str() == left.as_str())
        })
        .cloned()
        .collect()
}

fn vote_edge_history(tree: &GlobalTree, group: &GroupState, left: &ItemId, right: &ItemId) -> Markup {
    let mut votes = edge_votes(group, left, right);
    votes.sort_by(|a, b| b.ts.cmp(&a.ts));
    let legend_left = child_title(tree, left);
    let legend_right = child_title(tree, right);
    html! {
        @if votes.is_empty() {
            p class="muted vote-edge-empty" { "no votes on this pair yet" }
        } @else {
            h3 class="vote-edge-history-title" {
                "votes on this pair"
                span class="vote-edge-history-axis muted" { " · " (legend_left) " : " (legend_right) }
            }
            ul class="vote-edge-history" {
                @for v in &votes {
                    @let (r_left, r_right) = ratios_for_page(v, left, right);
                    @let pct = ratio_pct(r_left, r_right);
                    li class="vote-edge-history-row" {
                        div class="vote-edge-meta" {
                            span class="vote-edge-ratio" { (format!("{}:{}", r_left, r_right)) }
                        }
                        div class="ratio-bar vote-edge-bar" aria-hidden="true" {
                            div class="ratio-left" style={(format!("width: {:.3}%;", pct))} {}
                            div class="ratio-right" style={(format!("width: {:.3}%;", 100.0 - pct))} {}
                        }
                    }
                }
            }
        }
    }
}

fn vote_back_nav(parent: &ItemId) -> Markup {
    html! {
        div class="vote-compare-nav" {
            a class="vote-compare-back muted" href=(item_href(parent)) { "← back to " (display_label(parent)) }
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

/// After recording a vote on the compare page: refresh edge history and next-pair link.
pub(crate) fn vote_recorded_morph(
    tree: &GlobalTree,
    parent: &ItemId,
    left: &ItemId,
    right: &ItemId,
) -> JsBuilder {
    let pool = children_of(tree, parent);
    let empty = NodeState::default();
    let group = tree
        .get(parent)
        .unwrap_or(&empty)
        .local_ranking
        .clone();
    let edge_history = vote_edge_history(tree, &group, left, right);
    let next_pair = suggest_next(&group, left, right, &pool);
    let actions = vote_compare_actions(parent, next_pair.as_ref());
    JsBuilder::new()
        .morph_inner_selector("#vote-edge-history-region", edge_history)
        .morph_selector("#vote-compare-actions", actions)
}

fn vote_compare_item_card(tree: &GlobalTree, item: &ItemId, side_class: &str) -> Markup {
    let node = tree.get(item).cloned().unwrap_or_else(|| NodeState {
        id: item.clone(),
        ..Default::default()
    });
    html! {
        div class=(format!("vote-compare-side {side_class}")) {
            (entity_section(item, &node, false))
        }
    }
}


fn suggest_next(group: &GroupState, left: &ItemId, right: &ItemId, pool: &[ItemId]) -> Option<(ItemId, ItemId)> {
    suggest_next_pair_in_pool(group, pool, Some((left, right)))
}

pub async fn vote_page(
    State(state): State<AppState>,
    Query(q): Query<VoteQuery>,
) -> impl IntoResponse {
    let parent = parse_item_param(&q.parent);
    let left_param = q.left.as_deref().map(parse_item_param);
    let right_param = q.right.as_deref().map(parse_item_param);

    let tree = state.tree.read().await;
    let empty = NodeState::default();
    let parent_node = tree.get(&parent).unwrap_or(&empty);

    let (left, right) = match resolve_pair(
        &tree,
        &parent,
        left_param.as_ref(),
        right_param.as_ref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            let (msg, status) = e.status_message();
            return (status, msg).into_response();
        }
    };

    let pool = children_of(&tree, &parent);
    let group = &parent_node.local_ranking;
    let next_pair = suggest_next(group, &left, &right, &pool);
    let edge_history = vote_edge_history(&tree, group, &left, &right);

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
        section class="vote-compare-shell" {
            h1 { "compare" }
            (breadcrumb_path(&parent))
            p class="muted vote-compare-scope" {
                "ranking children of "
                a href=(item_href(&parent)) { (child_title(&tree, &parent)) }
            }
            div class="vote-compare-pair" {
                (vote_compare_item_card(&tree, &left, "vote-compare-left"))
                span class="vote-compare-vs" { "vs" }
                (vote_compare_item_card(&tree, &right, "vote-compare-right"))
            }
            (vote_back_nav(&parent))
            form id="vote-compare-form" method="POST" action="/ui" {
                input type="hidden" name=(UI_RPC_FIELD) value=(rpc_json);
                input type="hidden" name="ratio_left" id="vote-ratio-left" value="50";
                input type="hidden" name="ratio_right" id="vote-ratio-right" value="50";
                label class="vote-compare-slider-label" {
                    span id="vote-slider-left-label" { (child_title(&tree, &left)) }
                    input type="range" id="vote-preference-slider" min="0" max="100" value="50"
                        aria-valuemin="0" aria-valuemax="100";
                    span id="vote-slider-right-label" { (child_title(&tree, &right)) }
                }
                (vote_compare_actions(&parent, next_pair.as_ref()))
            }
            div id="vote-edge-history-region" {
                (edge_history)
            }
        }
    };

    drop(tree);

    let path = format!("/vote?parent={}", urlencoding::encode(parent.as_str()));
    state.views.increment(path.clone());
    let views = state.views.get_views(&path);

    Html(
        layout(
            &title,
            body,
            views,
        )
        .into_string(),
    )
    .into_response()
}
