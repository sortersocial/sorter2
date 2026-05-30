//! Pick two children of a parent scope for pairwise voting.
//!
//! Pair selection prefers **bridge** votes — comparisons between items in
//! different connected components of the voted-pairs graph — so the pool
//! merges into one ranking group before refining within it.

use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};

use crate::{
    path_types::ItemId,
    ranking::connected_components_from_voted_pairs,
    reducer::{GlobalTree, GroupState},
};

fn pairs_match(a: &ItemId, b: &ItemId, x: &ItemId, y: &ItemId) -> bool {
    (a == x && b == y) || (a == y && b == x)
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

/// Component id per pool item: voted-pairs graph components plus one id per
/// never-voted child.
fn component_ids(group: &GroupState, pool: &[ItemId]) -> HashMap<ItemId, usize> {
    let n = group.idx_to_item.len();
    let (comps, isolates) =
        connected_components_from_voted_pairs(n, group.voted_pairs.iter().copied());

    let mut out: HashMap<ItemId, usize> = HashMap::new();
    for (comp_idx, comp) in comps.iter().enumerate() {
        for &idx in comp {
            if idx < n {
                out.insert(group.idx_to_item[idx].clone(), comp_idx);
            }
        }
    }
    let mut next = comps.len();
    for &idx in &isolates {
        if idx < n {
            out.insert(group.idx_to_item[idx].clone(), next);
            next += 1;
        }
    }
    for item in pool {
        out.entry(item.clone()).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PairPriority {
    /// Unvoted edge between two components — grows the ranking group.
    BridgeUnvoted = 0,
    /// Unvoted edge inside one component — refines order.
    WithinUnvoted = 1,
    /// Re-vote across components (rare once merged).
    BridgeVoted = 2,
    /// Re-vote within a component.
    WithinVoted = 3,
}

fn pair_priority(
    group: &GroupState,
    components: &HashMap<ItemId, usize>,
    a: &ItemId,
    b: &ItemId,
) -> PairPriority {
    let voted = pair_is_voted(group, a, b);
    let bridge = components.get(a) != components.get(b);
    match (bridge, voted) {
        (true, false) => PairPriority::BridgeUnvoted,
        (false, false) => PairPriority::WithinUnvoted,
        (true, true) => PairPriority::BridgeVoted,
        (false, true) => PairPriority::WithinVoted,
    }
}

/// All unordered pairs from `pool`, optionally skipping `exclude`.
fn candidate_pairs(
    pool: &[ItemId],
    exclude: Option<(&ItemId, &ItemId)>,
) -> Vec<(ItemId, ItemId)> {
    let mut out = Vec::new();
    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let a = &pool[i];
            let b = &pool[j];
            if a == b {
                continue;
            }
            if exclude.is_some_and(|(x, y)| pairs_match(a, b, x, y)) {
                continue;
            }
            out.push((a.clone(), b.clone()));
        }
    }
    out
}

/// Pick the next pair to vote on within `pool`.
///
/// 1. Prefer unvoted **bridge** pairs (connect separate ranking components).
/// 2. Then unvoted within-component pairs (refinement).
/// 3. Then already-voted pairs (re-compare).
pub fn suggest_next_pair_in_pool(
    group: &GroupState,
    pool: &[ItemId],
    exclude: Option<(&ItemId, &ItemId)>,
) -> Option<(ItemId, ItemId)> {
    let candidates = candidate_pairs(pool, exclude);
    if candidates.is_empty() {
        return None;
    }
    let components = component_ids(group, pool);
    let best = candidates
        .iter()
        .map(|(a, b)| (pair_priority(group, &components, a, b), (a, b)))
        .min_by_key(|(p, _)| *p)?
        .0;
    let best_pairs: Vec<(ItemId, ItemId)> = candidates
        .into_iter()
        .filter(|(a, b)| pair_priority(group, &components, a, b) == best)
        .collect();
    best_pairs.choose(&mut rand::thread_rng()).cloned()
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
            Self::NoPair => (
                "no pair available",
                axum::http::StatusCode::BAD_REQUEST,
            ),
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
    fn suggest_prefers_unvoted_pair() {
        let parent = ItemId::parse("reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "reddit.com/r/rust/a",
                "reddit.com/r/rust/b",
                "reddit.com/r/rust/c",
            ],
        );
        let vote =
            VoteData::from_recorded(1, "reddit.com/r/rust/a", "reddit.com/r/rust/b", 2, 1).unwrap();
        tree.apply_vote(&parent, vote);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let (l, r) = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let voted_ab = (l.as_str() == "reddit.com/r/rust/a" && r.as_str() == "reddit.com/r/rust/b")
            || (l.as_str() == "reddit.com/r/rust/b" && r.as_str() == "reddit.com/r/rust/a");
        assert!(!voted_ab);
    }

    #[test]
    fn suggest_bridges_separate_components() {
        let parent = ItemId::parse("reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "reddit.com/r/rust/a",
                "reddit.com/r/rust/b",
                "reddit.com/r/rust/c",
                "reddit.com/r/rust/d",
            ],
        );
        let ab = VoteData::from_recorded(1, "reddit.com/r/rust/a", "reddit.com/r/rust/b", 2, 1).unwrap();
        let cd = VoteData::from_recorded(2, "reddit.com/r/rust/c", "reddit.com/r/rust/d", 2, 1).unwrap();
        tree.apply_vote(&parent, ab);
        tree.apply_vote(&parent, cd);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        let from_ab = chosen.contains("reddit.com/r/rust/a") || chosen.contains("reddit.com/r/rust/b");
        let from_cd = chosen.contains("reddit.com/r/rust/c") || chosen.contains("reddit.com/r/rust/d");
        assert!(from_ab && from_cd, "expected bridge pair, got {:?}", chosen);
    }

    #[test]
    fn suggest_connects_isolate_to_existing_component() {
        let parent = ItemId::parse("reddit.com/r/rust").unwrap();
        let mut tree = seed_children(
            &parent,
            &[
                "reddit.com/r/rust/a",
                "reddit.com/r/rust/b",
                "reddit.com/r/rust/c",
            ],
        );
        let ab = VoteData::from_recorded(1, "reddit.com/r/rust/a", "reddit.com/r/rust/b", 2, 1).unwrap();
        tree.apply_vote(&parent, ab);
        let group = tree.get(&parent).unwrap().local_ranking.clone();
        let pool = children_of(&tree, &parent);
        let pair = suggest_next_pair_in_pool(&group, &pool, None).unwrap();
        let chosen = pair_set(&pair);
        assert!(chosen.contains("reddit.com/r/rust/c"));
        assert!(chosen.contains("reddit.com/r/rust/a") || chosen.contains("reddit.com/r/rust/b"));
    }

    #[test]
    fn resolve_pair_picks_from_pool() {
        let parent = ItemId::parse("reddit.com/r/rust").unwrap();
        let tree = seed_children(&parent, &["reddit.com/r/rust/a", "reddit.com/r/rust/b"]);
        let pair = resolve_pair(&tree, &parent, None, None).unwrap();
        let pool: HashSet<_> = ["reddit.com/r/rust/a", "reddit.com/r/rust/b"]
            .into_iter()
            .collect();
        assert!(pool.contains(pair.0.as_str()));
        assert!(pool.contains(pair.1.as_str()));
    }
}
