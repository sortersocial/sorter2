//! Actor identity: pseudonym display names mapped to stable UUIDs for vote dedup.

use durable::Db;

use crate::storage_schema::{Store, StoreFields};

/// Default pseudonym until session/auth UI exists.
pub const DEFAULT_PSEUDONYM: &str = "anon";

/// UUID for the default single-user dev principal.
pub const DEFAULT_ACTOR_UUID: &str = "00000000-0000-0000-0000-000000000001";

/// UUID for in-memory unit tests.
pub const TEST_ACTOR_UUID: &str = "00000000-0000-0000-0000-000000000099";

/// Resolve the trust anchor for a pseudonym (must exist in the pseudonyms map).
pub fn resolve_actor_uuid(db: &Db, pseudonym: &str) -> Result<String, String> {
    Store::root()
        .pseudonyms()
        .key(&pseudonym.to_string())
        .get(db)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("unknown pseudonym: {pseudonym}"))
}

/// Ensure the default pseudonym → UUID mapping exists (operational seed, not event-logged).
pub fn seed_default_pseudonym(db: &Db) -> Result<(), durable::Error> {
    let path = Store::root()
        .pseudonyms()
        .key(&DEFAULT_PSEUDONYM.to_string());
    if path.get(db)?.is_none() {
        db.run(path.set(&DEFAULT_ACTOR_UUID.to_string()), durable::Durability::SyncWal)?;
    }
    Ok(())
}
