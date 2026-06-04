//! URL canonicalization and hierarchy rules for [`crate::path_types::ItemId`].

mod engine;
mod registry;

pub use registry::{
    canonicalize_raw, looks_like_url, navigable_breadcrumbs, parent_url, resolve_id, CanonicalResult,
};

/// Resolve raw input to canonical URL.
pub fn resolve_canonical(raw: &str) -> Option<String> {
    canonicalize_raw(raw.trim()).map(|r| r.canonical)
}
