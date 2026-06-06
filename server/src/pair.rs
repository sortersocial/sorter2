//! Pick two children of a parent scope for pairwise voting.
//!
//! Before the pool is one connected voted component, prefer an unvoted edge from
//! a never-voted child into a random member of the largest voted group.
//!
//! Once connected, run rank centrality once and **zip** adjacent ranks (1 vs 2,
//! 2 vs 3, …), skipping pairs that already have a vote.

use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};

use crate::{
    path_types::ItemId,
    ranking::{connected_components_from_voted_pairs, ranked_items},
    reducer::{GlobalTree, GroupState},
};

fn pairs_match(a: &ItemId, b: &ItemId, x: &ItemId, y: &ItemId) -> bool {
    (a == x && b == y) || (a == y && b == x)
}

fn pair_excluded(a: &ItemId, b: &ItemId, exclude: Option<(&ItemId, &ItemId)>) -> bool {
    exclude.is_some_and(|(x, y)| pairs_match(a, b, x, y))
}

fn pair_is_voted(group: &GroupState, a: &ItemId, b: &ItemId) -> bool {
    let Some(&ai) = group.item_to_idx.get(a) else {
        return false;
    };
    let Some(&bi) = group.item_to_idx.get(b) else {
        return false;
    };
    let (i, j) = if ai < bi { (ai, bi) } else { (bi, ai) };
    group.voted_pairs.contains(&(i, j))
}

struct ComponentLayout {
    ids: HashMap<ItemId, usize>,
    established: HashSet<usize>,
}

fn component_layout(group: &GroupState, pool: &[ItemId]) -> ComponentLayout {
    let n = group.idx_to_item.len();
    let (comps, isolates) =
        connected_components_from_voted_pairs(n, group.voted_pairs.iter().copied());

    let mut established = HashSet::new();
    let mut ids: HashMap<ItemId, usize> = HashMap::new();
    for (comp_idx, comp) in comps.iter().enumerate() {
        if comp.len() >= 2 {
            established.insert(comp_idx);
        }
        for &idx in comp {
            if idx < n {
                ids.insert(group.idx_to_item[idx].clone(), comp_idx);
            }
        }
    }
    let mut next = comps.len();
    for &idx in &isolates {
        if idx < n {
            ids.insert(group.idx_to_item[idx].clone(), next);
            next += 1;
        }
    }
    for item in pool {
        ids.entry(item.clone()).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
    }
    ComponentLayout { ids, established }
}

fn pool_fully_connected(layout: &ComponentLayout, pool: &[ItemId]) -> bool {
    if pool.len() < 2 {
        return false;
    }
    let mut comp_id = None;
    for item in pool {
        let Some(id) = layout.ids.get(item) else {
            return false;
        };
        if !layout.established.contains(id) {
            return false;
        }
        match comp_id {
            None => comp_id = Some(*id),
            Some(expected) if expected == *id => {}
            _ => return false,
        }
    }
    comp_id.is_some()
}

fn item_in_established(layout: &ComponentLayout, item: &ItemId) -> bool {
    layout
        .ids
        .get(item)
        .is_some_and(|id| layout.established.contains(id))
}

/// Established (multi-node voted) components among pool children, largest first.
fn established_groups_in_pool<'a>(
    layout: &ComponentLayout,
    pool: &'a [ItemId],
) -> Vec<Vec<&'a ItemId>> {
    let mut by_comp: HashMap<usize, Vec<&'a ItemId>> = HashMap::new();
    for item in pool {
        if !item_in_established(layout, item) {
            continue;
        }
        let Some(&cid) = layout.ids.get(item) else {
            continue;
        };
        by_comp.entry(cid).or_default().push(item);
    }
    let mut groups: Vec<Vec<&'a ItemId>> = by_comp.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    groups
}

fn ranked_pool_order(group: &GroupState, pool: &[ItemId]) -> Vec<ItemId> {
    let pool_set: HashSet<_> = pool.iter().collect();
    ranked_items(group)
        .into_iter()
        .map(|r| r.item)
        .filter(|id| pool_set.contains(id))
        .collect()
}

/// Walk 1↔2, 2↔3, …; optional `require_unvoted` skips voted edges.
fn zip_adjacent_pair(
    group: &GroupState,
    order: &[ItemId],
    exclude: Option<(&ItemId, &ItemId)>,
    require_unvoted: bool,
) -> Option<(ItemId, ItemId)> {
    for w in order.windows(2) {
        let a = &w[0];
        let b = &w[1];
        if pair_excluded(a, b, exclude) {
            continue;
        }
        if require_unvoted && pair_is_voted(group, a, b) {
            continue;
        }
        return Some((a.clone(), b.clone()));
    }
    None
}

/// Grow the voted graph toward one component (no rank centrality).
fn suggest_grow_pair(
    group: &GroupState,
    pool: &[ItemId],
    layout: &ComponentLayout,
    exclude: Option<(&ItemId, &ItemId)>,
) -> Option<(ItemId, ItemId)> {
    let mut rng = rand::thread_rng();
    let groups = established_groups_in_pool(layout, pool);
    let mut isolates: Vec<&ItemId> = pool
        .iter()
        .filter(|item| !item_in_established(layout, item))
        .collect();
    isolates.shuffle(&mut rng);

    // Attach a never-voted child to a random member of the largest voted group.
    for iso in &isolates {
        for comp in &groups {
            let candidates: Vec<&ItemId> = comp
                .iter()
                .copied()
                .filter(|est| {
                    !pair_is_voted(group, iso, est) && !pair_excluded(iso, est, exclude)
                })
                .collect();
            if let Some(&est) = candidates.choose(&mut rng) {
                return Some(((*iso).clone(), est.clone()));
            }
        }
    }

    // Bridge two established components (random endpoints, larger groups first).
    for i in 0..groups.len() {
        for j in (i + 1)..groups.len() {
            let mut pairs: Vec<(&ItemId, &ItemId)> = Vec::new();
            for a in &groups[i] {
                for b in &groups[j] {
                    if !pair_is_voted(group, a, b) && !pair_excluded(a, b, exclude) {
                        pairs.push((a, b));
                    }
                }
            }
            if let Some((a, b)) = pairs.choose(&mut rng) {
                return Some(((*a).clone(), (*b).clone()));
            }
        }
    }

    // Any other unvoted pair (e.g. two isolates).
    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let a = &pool[i];
            let b = &pool[j];
            if pair_is_voted(group, a, b) || pair_excluded(a, b, exclude) {
                continue;
            }
            return Some((a.clone(), b.clone()));
        }
    }

    None
}

/// Pick the next pair to vote on within `pool`.
pub fn suggest_next_pair_in_pool(
    group: &GroupState,
    pool: &[ItemId],
    exclude: Option<(&ItemId, &ItemId)>,
) -> Option<(ItemId, ItemId)> {
    if pool.len() < 2 {
        return None;
    }

    let layout = component_layout(group, pool);

    if pool_fully_connected(&layout, pool) {
        let order = ranked_pool_order(group, pool);
        if let Some(pair) = zip_adjacent_pair(group, &order, exclude, true) {
            return Some(pair);
        }
        if let Some(pair) = zip_adjacent_pair(group, &order, exclude, false) {
            return Some(pair);
        }
    }

    if let Some(pair) = suggest_grow_pair(group, pool, &layout, exclude) {
        return Some(pair);
    }

    // Re-vote: zip order when connected, else any non-excluded pair.
    if pool_fully_connected(&layout, pool) {
        let order = ranked_pool_order(group, pool);
        if let Some(pair) = zip_adjacent_pair(group, &order, exclude, false) {
            return Some(pair);
        }
    }

    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let a = &pool[i];
            let b = &pool[j];
            if !pair_excluded(a, b, exclude) {
                return Some((a.clone(), b.clone()));
            }
        }
    }

    None
}

/// Random distinct pair from `children` (legacy pair.rs behavior).
pub fn random_pair(children: &[ItemId]) -> Option<(ItemId, ItemId)> {
    if children.len() < 2 {
        return None;
    }
    let left = children.choose(&mut rand::thread_rng())?;
    let mut right = children.choose(&mut rand::thread_rng())?;
    let mut guard = 0;
    while left == right && guard < 32 {
        right = children.choose(&mut rand::thread_rng())?;
        guard += 1;
    }
    if left == right {
        return None;
    }
    Some((left.clone(), right.clone()))
}

/// Sorted children of `parent` from the global tree.
pub fn children_of(tree: &GlobalTree, parent: &ItemId) -> Vec<ItemId> {
    let Some(node) = tree.get(parent) else {
        return Vec::new();
    };
    let mut children: Vec<ItemId> = node.children.iter().cloned().collect();
    children.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    children
}

/// Resolve a pair to compare under `parent`.
pub fn resolve_pair(
    tree: &GlobalTree,
    parent: &ItemId,
    left: Option<&ItemId>,
    right: Option<&ItemId>,
) -> Result<(ItemId, ItemId), PairError> {
    let children = children_of(tree, parent);
    if children.len() < 2 {
        return Err(PairError::TooFewChildren);
    }
    let child_set: HashSet<_> = children.iter().collect();

    match (left, right) {
        (Some(l), Some(r)) => {
            if l == r {
                return Err(PairError::SameItem);
            }
            if !child_set.contains(l) || !child_set.contains(r) {
                return Err(PairError::NotChild);
            }
            Ok((l.clone(), r.clone()))
        }
        (None, None) => {
            let group = tree
                .get(parent)
                .map(|n| &n.local_ranking)
                .cloned()
                .unwrap_or_default();
            suggest_next_pair_in_pool(&group, &children, None).ok_or(PairError::NoPair)
        }
        _ => Err(PairError::IncompletePair),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    TooFewChildren,
    SameItem,
    NotChild,
    IncompletePair,
    NoPair,
}

impl PairError {
    pub fn status_message(&self) -> (&'static str, axum::http::StatusCode) {
        match self {
            Self::TooFewChildren => (
                "parent needs at least 2 children to vote",
                axum::http::StatusCode::BAD_REQUEST,
            ),
            Self::SameItem => (
                "left and right must differ",
                axum::http::StatusCode::BAD_REQUEST,
            ),
            Self::NotChild => (
                "left and right must be children of parent",
                axum::http::StatusCode::BAD_REQUEST,
            ),
            Self::IncompletePair => (
                "provide both left and right, or neither",
                axum::http::StatusCode::BAD_REQUEST,
            ),
            Self::NoPair => ("no pair available", axum::http::StatusCode::BAD_REQUEST),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::{GlobalTree, VoteData};

    fn seed_children(parent: &ItemId, ids: &[&str]) -> GlobalTree {
        let mut tree = GlobalTree::new();
        tree.ensure_path(parent);
        for id in ids {
            let child = ItemId::parse(id).unwrap();
            tree.ensure_path(&child);
            if let Some(p) = tree.nodes.get_mut(parent) {
                p.children.insert(child);
            }
        }
        tree
    }

    fn pair_set(pair: &(ItemId, ItemId)) -> HashSet<&str> {
        [pair.0.as_str(), pair.1.as_str()].into_iter().collect()
    }

    #[test]
    fn zero_weight_vote_leaves_pair_available_for_suggestion() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
            ],
        );
        let noop = VoteData::from_recorded(
            1,
            "https://reddit.com/r/rust/a",
            "https://reddit.com/r/rust/b",
            0,
            0,
        )
        .unwrap();
        tree.apply_vote(&parent, noop);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        assert!(!pair_is_voted(&group, &pool[0], &pool[1]));
        assert!(suggest_next_pair_in_pool(&group, &pool, None).is_some());
    }

    #[test]
    fn suggest_prefers_unvoted_pair() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
                "https://reddit.com/r/rust/c",
            ],
        );
        let vote =
            VoteData::from_recorded(1, "https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b", 2, 1).unwrap();
        tree.apply_vote(&parent, vote);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let (l, r) = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let voted_ab = (l.as_str() == "https://reddit.com/r/rust/a" && r.as_str() == "https://reddit.com/r/rust/b")
            || (l.as_str() == "https://reddit.com/r/rust/b" && r.as_str() == "https://reddit.com/r/rust/a");
        assert!(!voted_ab);
    }

    #[test]
    fn suggest_bridges_separate_components() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
                "https://reddit.com/r/rust/c",
                "https://reddit.com/r/rust/d",
            ],
        );
        let ab =
            VoteData::from_recorded(1, "https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b", 2, 1).unwrap();
        let cd =
            VoteData::from_recorded(2, "https://reddit.com/r/rust/c", "https://reddit.com/r/rust/d", 2, 1).unwrap();
        tree.apply_vote(&parent, ab);
        tree.apply_vote(&parent, cd);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        let from_ab =
            chosen.contains("https://reddit.com/r/rust/a") || chosen.contains("https://reddit.com/r/rust/b");
        let from_cd =
            chosen.contains("https://reddit.com/r/rust/c") || chosen.contains("https://reddit.com/r/rust/d");
        assert!(from_ab && from_cd, "expected bridge pair, got {:?}", chosen);
    }

    #[test]
    fn suggest_prefers_attach_over_isolate_pair_among_many_unranked() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
                "https://reddit.com/r/rust/c",
                "https://reddit.com/r/rust/d",
                "https://reddit.com/r/rust/e",
            ],
        );
        let ab =
            VoteData::from_recorded(1, "https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b", 2, 1).unwrap();
        tree.apply_vote(&parent, ab);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        let from_ab =
            chosen.contains("https://reddit.com/r/rust/a") || chosen.contains("https://reddit.com/r/rust/b");
        let from_cde = chosen.contains("https://reddit.com/r/rust/c")
            || chosen.contains("https://reddit.com/r/rust/d")
            || chosen.contains("https://reddit.com/r/rust/e");
        assert!(
            from_ab && from_cde,
            "expected ranked+unranked attach, got {:?}",
            chosen
        );
    }

    #[test]
    fn suggest_connects_isolate_to_existing_component() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
                "https://reddit.com/r/rust/c",
            ],
        );
        let ab =
            VoteData::from_recorded(1, "https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b", 2, 1).unwrap();
        tree.apply_vote(&parent, ab);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        assert!(chosen.contains("https://reddit.com/r/rust/c"));
        assert!(chosen.contains("https://reddit.com/r/rust/a") || chosen.contains("https://reddit.com/r/rust/b"));
    }

    #[test]
    fn suggest_zips_adjacent_ranks_when_tree_complete() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
                "https://reddit.com/r/rust/c",
            ],
        );
        for (a, b, l, r) in [
            ("https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b", 3, 1),
            ("https://reddit.com/r/rust/a", "https://reddit.com/r/rust/c", 2, 1),
        ] {
            let v = VoteData::from_recorded(1, a, b, l, r).unwrap();
            tree.apply_vote(&parent, v);
        }
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        assert!(chosen.contains("https://reddit.com/r/rust/b"));
        assert!(chosen.contains("https://reddit.com/r/rust/c"));
    }

    #[test]
    fn suggest_zip_prefers_1v2_before_2v3_when_both_unvoted() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "https://reddit.com/r/rust/a",
                "https://reddit.com/r/rust/b",
                "https://reddit.com/r/rust/c",
                "https://reddit.com/r/rust/d",
            ],
        );
        for (a, b, l, r) in [
            ("https://reddit.com/r/rust/c", "https://reddit.com/r/rust/d", 3, 1),
            ("https://reddit.com/r/rust/b", "https://reddit.com/r/rust/c", 2, 1),
            ("https://reddit.com/r/rust/a", "https://reddit.com/r/rust/c", 2, 1),
        ] {
            let v = VoteData::from_recorded(1, a, b, l, r).unwrap();
            tree.apply_vote(&parent, v);
        }
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        assert!(chosen.contains("https://reddit.com/r/rust/a"));
        assert!(chosen.contains("https://reddit.com/r/rust/b"));
    }

    #[test]
    fn resolve_pair_picks_from_pool() {
        let parent = ItemId::parse("https://reddit.com/r/rust").unwrap();
        let tree = seed_children(&parent, &["https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b"]);
        let pair = resolve_pair(&tree, &parent, None, None).unwrap();
        let pool: HashSet<_> = ["https://reddit.com/r/rust/a", "https://reddit.com/r/rust/b"]
            .into_iter()
            .collect();
        assert!(pool.contains(pair.0.as_str()));
        assert!(pool.contains(pair.1.as_str()));
    }
}
