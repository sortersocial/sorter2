//! URL canonicalization and hierarchy via a semantic graph (DFA + generic fallback).

mod graph;
mod parse;
mod registry;

#[cfg(test)]
mod registry_tests;

pub use registry::{
    canonicalize_raw, looks_like_url, navigable_breadcrumbs, parent_url, resolve_id, CanonicalResult,
};

/// Resolve raw input to canonical URL.
pub fn resolve_canonical(raw: &str) -> Option<String> {
    canonicalize_raw(raw.trim()).map(|r| r.canonical)
}
