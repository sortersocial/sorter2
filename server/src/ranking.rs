use std::collections::{HashMap, HashSet};

use crate::path_types::ItemId;
use crate::reducer::GroupState;

#[derive(Debug, Clone)]
pub struct RankedItem {
    pub item: ItemId,
    pub score: f64,
}

/// Power-iteration cap and convergence tolerance for rank centrality.
pub const MAX_ITERS: usize = 10_000;
pub const TOL: f64 = 1e-8;

/// Compute connected components over the voted-pairs graph (treated as undirected).
///
/// Returns:
/// - `components`: each component is a sorted list of node indices, excluding isolates.
/// - `isolates`: sorted list of node indices with degree 0 (no voted pairs).
pub fn connected_components_from_voted_pairs(
    n: usize,
    voted_pairs: impl Iterator<Item = (usize, usize)>,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, b) in voted_pairs {
        if a >= n || b >= n || a == b {
            continue;
        }
        adj[a].push(b);
        adj[b].push(a);
    }

    let mut isolates: Vec<usize> = (0..n).filter(|&i| adj[i].is_empty()).collect();
    isolates.sort();

    let mut seen = vec![false; n];
    for &i in &isolates {
        seen[i] = true;
    }

    let mut comps: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        if seen[i] {
            continue;
        }
        let mut stack = vec![i];
        seen[i] = true;
        let mut comp: Vec<usize> = Vec::new();
        while let Some(x) = stack.pop() {
            comp.push(x);
            for &y in &adj[x] {
                if !seen[y] {
                    seen[y] = true;
                    stack.push(y);
                }
            }
        }
        comp.sort();
        comps.push(comp);
    }

    (comps, isolates)
}

/// Compute rank-centrality scores for the whole group and return items sorted
/// by score (descending). Recomputed fresh from the edge set on every call —
/// there is no score cache.
pub fn ranked_items(group: &GroupState) -> Vec<RankedItem> {
    let n = group.idx_to_item.len();
    let scores =
        compute_scores_from_edges(n, group.edges.iter().map(|(&k, &w)| (k, w)), MAX_ITERS, TOL);

    let mut items: Vec<RankedItem> = group
        .idx_to_item
        .iter()
        .enumerate()
        .map(|(i, item)| RankedItem {
            item: item.clone(),
            score: *scores.get(i).unwrap_or(&0.0),
        })
        .collect();

    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    items
}

/// Highest- and lowest-ranked items for a group. Returns up to `k` items from
/// each end with no overlap. If the group has `2*k` items or fewer, `top` holds
/// the full ranking and `bottom` is empty (so nothing is shown twice).
pub fn top_bottom(group: &GroupState, k: usize) -> (Vec<RankedItem>, Vec<RankedItem>) {
    let items = ranked_items(group);
    if k == 0 || items.len() <= 2 * k {
        return (items, Vec::new());
    }
    let top = items[..k].to_vec();
    let bottom = items[items.len() - k..].to_vec();
    (top, bottom)
}

pub fn compute_scores_from_edges(
    n: usize,
    edges: impl Iterator<Item = ((usize, usize), f64)>,
    max_iters: usize,
    tol: f64,
) -> Vec<f64> {
    if n == 0 {
        return vec![];
    }
    if n == 1 {
        return vec![1.0];
    }

    // Collect raw edges into a map for pairwise normalization.
    let mut raw: HashMap<(usize, usize), f64> = HashMap::new();
    for ((src, dst), w) in edges {
        if src >= n || dst >= n || w <= 0.0 {
            continue;
        }
        *raw.entry((src, dst)).or_insert(0.0) += w;
    }

    // Pairwise normalization: a_ij = A_ij / (A_ij + A_ji).
    // This ensures repeated votes on the same pair don't inflate influence
    // beyond what the ratio implies.
    let keys: Vec<(usize, usize)> = raw.keys().copied().collect();
    let mut normalized: HashMap<(usize, usize), f64> = HashMap::new();
    for (i, j) in keys {
        if normalized.contains_key(&(i, j)) {
            continue;
        }
        let w_ij = *raw.get(&(i, j)).unwrap_or(&0.0);
        let w_ji = *raw.get(&(j, i)).unwrap_or(&0.0);
        let total = w_ij + w_ji;
        if total <= 0.0 {
            continue;
        }
        normalized.insert((i, j), w_ij / total);
        if w_ji > 0.0 {
            normalized.insert((j, i), w_ji / total);
        }
    }

    // Rank Centrality (Negahban, Oh, Shah 2012, §3.1):
    //   P_ij = (1/d_max) * A_ij           for i ≠ j compared
    //   P_ii = 1 - (1/d_max) * Σ_k A_ik
    // where d_i is the *degree* (number of distinct neighbors compared) and
    // d_max = max_i d_i. Using the unweighted degree — not the sum of
    // pairwise-normalized weights — is what guarantees aperiodicity: it
    // forces P_ii > 0 for every non-maximum-degree node, and for max-degree
    // nodes whenever any neighbor weight is below 1 (i.e. not a unanimous
    // loss). Without this, regular comparison graphs (e.g. a pure star at
    // ratio 2:1) produce a bipartite chain that oscillates instead of
    // converging — see issue #146.
    let mut out_edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut neighbors: Vec<HashSet<usize>> = vec![HashSet::new(); n];

    for ((src, dst), w) in &normalized {
        out_edges[*src].push((*dst, *w));
        neighbors[*src].insert(*dst);
        neighbors[*dst].insert(*src);
    }

    let weight_sum: Vec<f64> = out_edges
        .iter()
        .map(|es| es.iter().map(|(_, w)| *w).sum())
        .collect();
    let d_max = neighbors.iter().map(|s| s.len()).max().unwrap_or(0);
    if d_max == 0 {
        return vec![1.0 / n as f64; n];
    }
    let d_max_f = d_max as f64;

    let mut scores = vec![1.0 / n as f64; n];
    let mut next = vec![0.0f64; n];

    for _ in 0..max_iters {
        next.fill(0.0);
        for i in 0..n {
            let stay_prob = (d_max_f - weight_sum[i]) / d_max_f;
            next[i] += scores[i] * stay_prob;

            if out_edges[i].is_empty() {
                continue;
            }
            for &(dst, w) in &out_edges[i] {
                next[dst] += scores[i] * (w / d_max_f);
            }
        }

        let diff: f64 = scores
            .iter()
            .zip(next.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();

        scores.clone_from_slice(&next);
        if diff < tol {
            break;
        }
    }

    let sum: f64 = scores.iter().sum();
    if sum.is_finite() && sum > 0.0 {
        for s in &mut scores {
            *s /= sum;
        }
    }
    scores
}

/// Rank-centrality within a subset of items (an induced subgraph), using the group's aggregated edges.
///
/// `idxs` are indices into `group.idx_to_item`. The returned items use the original item names.
pub fn ranked_items_subset(
    group: &GroupState,
    idxs: &[usize],
    max_iters: usize,
    tol: f64,
) -> Vec<RankedItem> {
    if idxs.is_empty() {
        return vec![];
    }

    // Map original idx -> compact idx [0..m)
    let mut map: HashMap<usize, usize> = HashMap::with_capacity(idxs.len());
    for (j, &i) in idxs.iter().enumerate() {
        map.insert(i, j);
    }

    let edges_iter = group.edges.iter().filter_map(|(&(src, dst), &w)| {
        let s = *map.get(&src)?;
        let d = *map.get(&dst)?;
        Some(((s, d), w))
    });

    let scores = compute_scores_from_edges(idxs.len(), edges_iter, max_iters, tol);

    // Filter out entries where idx_to_item doesn't have the slot (shouldn't happen, but be safe).
    let mut items: Vec<RankedItem> = idxs
        .iter()
        .enumerate()
        .filter_map(|(j, &orig)| {
            let item = group.idx_to_item.get(orig)?.clone();
            Some(RankedItem {
                item,
                score: *scores.get(j).unwrap_or(&0.0),
            })
        })
        .collect();

    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    items
}

pub fn group_summary_scores(group: &GroupState) -> HashMap<ItemId, f64> {
    ranked_items(group)
        .into_iter()
        .map(|r| (r.item, r.score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::VoteData;

    fn mk_group() -> GroupState {
        GroupState::new()
    }

    fn vote(ts: i64, a: &str, b: &str, l: i32, r: i32) -> VoteData {
        use crate::path_types::ItemId;
        VoteData {
            ts,
            a: ItemId::parse(a).unwrap(),
            b: ItemId::parse(b).unwrap(),
            ratio_left: l,
            ratio_right: r,
            body: "because".to_string(),
            principal: "test".to_string(),
            delegate: Some("00000000-0000-0000-0000-000000000000:test:local/test".to_string()),
            thread_tag: "untagged".to_string(),
        }
    }

    /// Regression for issue #146: pure forward star at default `>` ratio (2:1).
    /// Under the old (sum-of-weights) divisor every node had P_ii = 0 and the
    /// chain was bipartite; power iteration oscillated and returned the
    /// uniform initial distribution after an even number of steps. Using the
    /// paper's degree-based d_max gives every node a positive self-loop and
    /// the chain converges to the correct stationary distribution.
    #[test]
    fn star_topology_winner_at_top_via_subset() {
        let mut g = mk_group();
        g.apply_vote(vote(1, "zebra", "alpha", 2, 1));
        g.apply_vote(vote(2, "zebra", "beta", 2, 1));

        let mut items: Vec<(usize, String)> = g
            .idx_to_item
            .iter()
            .enumerate()
            .map(|(i, it)| (i, it.as_str().to_string()))
            .collect();
        items.sort_by(|a, b| a.1.cmp(&b.1));
        let idxs: Vec<usize> = items.iter().map(|(i, _)| *i).collect();

        let ranked = ranked_items_subset(&g, &idxs, 10000, 1e-8);
        for r in &ranked {
            eprintln!("{}: {}", r.item.as_str(), r.score);
        }
        assert_eq!(
            ranked[0].item.as_str(),
            "zebra",
            "zebra won both votes and should rank #1"
        );
    }

    #[test]
    fn top_bottom_splits_ends_without_overlap() {
        let mut g = mk_group();
        // Chain a > b > c > d > e > f so ranks are well separated.
        for (hi, lo) in [("a", "b"), ("b", "c"), ("c", "d"), ("d", "e"), ("e", "f")] {
            g.apply_vote(vote(1, hi, lo, 2, 1));
        }
        let (top, bottom) = top_bottom(&g, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(bottom.len(), 2);
        // No overlap between the two ends.
        for t in &top {
            assert!(bottom.iter().all(|b| b.item != t.item));
        }
        // Best item ranks above the worst item.
        assert!(top[0].score >= bottom[bottom.len() - 1].score);
    }

    #[test]
    fn top_bottom_small_group_has_empty_bottom() {
        let mut g = mk_group();
        g.apply_vote(vote(1, "a", "b", 2, 1));
        let (top, bottom) = top_bottom(&g, 5);
        assert_eq!(top.len(), 2);
        assert!(bottom.is_empty());
    }

    #[test]
    fn connected_components_split_disconnected_pairs() {
        let mut g = mk_group();
        // Two disconnected edges: (a,b) and (c,d)
        g.apply_vote(vote(1, "a", "b", 3, 1));
        g.apply_vote(vote(2, "c", "d", 3, 1));

        let n = g.idx_to_item.len();
        let (mut comps, isolates) =
            connected_components_from_voted_pairs(n, g.voted_pairs.iter().copied());
        assert!(isolates.is_empty());
        // Order-independent: sort components by their item names for stable assert.
        comps.sort_by_key(|c| {
            c.iter()
                .map(|&i| g.idx_to_item[i].clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(comps.len(), 2);
        let comp0 = comps[0]
            .iter()
            .map(|&i| g.idx_to_item[i].as_str())
            .collect::<Vec<_>>();
        let comp1 = comps[1]
            .iter()
            .map(|&i| g.idx_to_item[i].as_str())
            .collect::<Vec<_>>();
        assert_eq!(comp0, vec!["a", "b"]);
        assert_eq!(comp1, vec!["c", "d"]);
    }

    /// A random spanning tree over 26 items needs only n−1 = 25 pairwise votes.
    /// When each vote uses the "perfect" ratio (strength left : strength right =
    /// (idx_left+1) : (idx_right+1)), rank centrality recovers the true order.
    /// See `rank-eric.py` (Eric's demo of Negahban–Oh–Shah rank centrality).
    #[test]
    fn twenty_five_random_votes_perfect_ratios_sort_alphabet() {
        use rand::seq::SliceRandom;

        const N: usize = 26;
        let letters: Vec<char> = (0..N).map(|i| char::from(b'a' + i as u8)).collect();

        let mut rng = rand::thread_rng();
        let mut perm: Vec<usize> = (0..N).collect();
        perm.shuffle(&mut rng);

        let mut g = mk_group();
        for k in 1..N {
            let i = *perm[..k].choose(&mut rng).unwrap();
            let j = perm[k];
            let (a, b) = (letters[i], letters[j]);
            g.apply_vote(vote(
                k as i64,
                &a.to_string(),
                &b.to_string(),
                (i + 1) as i32,
                (j + 1) as i32,
            ));
        }

        let ranked = ranked_items(&g);
        assert_eq!(ranked.len(), N);
        for (rank, item) in ranked.iter().enumerate() {
            let expected = char::from(b'a' + (N - 1 - rank) as u8);
            assert_eq!(
                item.item.as_str(),
                expected.to_string(),
                "rank {rank}: expected '{expected}', got '{}'",
                item.item.as_str()
            );
        }
    }

    #[test]
    fn subset_ranking_ranks_within_component_only() {
        let mut g = mk_group();
        g.apply_vote(vote(1, "a", "b", 3, 1)); // a > b
        g.apply_vote(vote(2, "c", "d", 1, 4)); // d > c

        let (comps, _) = connected_components_from_voted_pairs(
            g.idx_to_item.len(),
            g.voted_pairs.iter().copied(),
        );
        assert_eq!(comps.len(), 2);

        // Rank each component and ensure winner is first within that component.
        for comp in comps {
            let ranked = ranked_items_subset(&g, &comp, 10000, 1e-8);
            assert_eq!(ranked.len(), 2);
            let names = ranked.iter().map(|r| r.item.as_str()).collect::<Vec<_>>();
            if names.contains(&"a") {
                assert_eq!(names[0], "a");
            } else {
                assert_eq!(names[0], "d");
            }
        }
    }
}
