//! Session cookie resolution and durable session CRUD.

use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use durable::{Db, Durability};
use rand::Rng;

use crate::{
    auth::config::{self, AUTH_RETURN_COOKIE},
    fetch::now_ms,
    identity::{DEFAULT_ACTOR_UUID, DEFAULT_PSEUDONYM},
    storage_dto::{SessionDataV1, SESSION_DATA_VERSION},
    storage_schema::{delete_session, load_session, user_trust_weight, write_session},
};

pub const SESSION_COOKIE: &str = "sorter2_session";
pub const OAUTH_STATE_COOKIE: &str = "sorter2_oauth_state";

/// Session lifetime (30 days).
pub const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub fn session_has_pseudonym(session: &SessionDataV1) -> bool {
    !session.current_pseudonym.trim().is_empty()
}

pub fn load_valid_session(db: &Db, session_id: &str) -> Option<SessionDataV1> {
    let session = load_session(db, session_id).ok()??;
    if session.expires_at <= now_ms() {
        return None;
    }
    Some(session)
}

#[derive(Debug, Clone)]
pub struct VoteActor {
    pub uuid: String,
    pub pseudonym: String,
    pub trust_weight: f64,
}

impl VoteActor {
    /// Test / bench helper: seed votes as the default pseudonym without a session.
    pub fn anon() -> Self {
        Self {
            uuid: DEFAULT_ACTOR_UUID.to_string(),
            pseudonym: DEFAULT_PSEUDONYM.to_string(),
            trust_weight: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionActor {
    pub session_id: String,
    pub uuid: String,
    pub pseudonym: String,
    pub trust_weight: f64,
    pub expires_at: i64,
}

pub fn new_session_id() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    hex_encode(&bytes)
}

pub fn new_oauth_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes);
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn build_cookie(name: &'static str, value: String) -> Cookie<'static> {
    let mut builder = Cookie::build((name, value))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/");
    if config::cookies_secure() {
        builder = builder.secure(true);
    }
    builder.build()
}

fn clear_cookie(name: &'static str) -> Cookie<'static> {
    let mut builder = Cookie::build((name, ""))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .removal();
    if config::cookies_secure() {
        builder = builder.secure(true);
    }
    builder.build()
}

/// Resolve the vote actor from a live session. Fail-closed: never falls back to anon.
pub fn resolve_vote_actor(db: &Db, session_id: Option<&str>) -> Result<VoteActor, &'static str> {
    let session_id = session_id.ok_or("sign in to vote")?;
    let session = load_valid_session(db, session_id).ok_or("session expired")?;
    if !session_has_pseudonym(&session) {
        return Err("choose an alias first");
    }
    let trust_weight = user_trust_weight(db, &session.uuid).unwrap_or(1.0);
    Ok(VoteActor {
        uuid: session.uuid,
        pseudonym: session.current_pseudonym,
        trust_weight,
    })
}

/// Display name for the top nav, if any session is active.
pub fn nav_pseudonym(db: &Db, jar: &CookieJar) -> Option<String> {
    let session_id = session_id_from_jar(jar)?;
    let session = load_valid_session(db, &session_id)?;
    if session_has_pseudonym(&session) {
        Some(session.current_pseudonym)
    } else {
        None
    }
}

pub fn load_session_actor(db: &Db, session_id: &str) -> Option<SessionActor> {
    let session = load_session(db, session_id).ok()??;
    if session.expires_at <= now_ms() {
        return None;
    }
    let trust_weight = user_trust_weight(db, &session.uuid).ok()?;
    Some(SessionActor {
        session_id: session_id.to_string(),
        uuid: session.uuid,
        pseudonym: session.current_pseudonym,
        trust_weight,
        expires_at: session.expires_at,
    })
}

pub fn create_session(
    db: &Db,
    uuid: &str,
    pseudonym: &str,
) -> Result<(String, SessionDataV1), String> {
    let session_id = new_session_id();
    let expires_at = now_ms() + SESSION_TTL_MS;
    let data = SessionDataV1 {
        version: SESSION_DATA_VERSION,
        uuid: uuid.to_string(),
        current_pseudonym: pseudonym.to_string(),
        expires_at,
    };
    let mut batch = db.batch();
    write_session(&mut batch, &session_id, &data);
    batch
        .commit_with(Durability::SyncWal)
        .map_err(|e| e.to_string())?;
    Ok((session_id, data))
}

pub fn update_session_pseudonym(db: &Db, session_id: &str, pseudonym: &str) -> Result<(), String> {
    let mut session = load_session(db, session_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "session not found".to_string())?;
    if session.expires_at <= now_ms() {
        return Err("session expired".to_string());
    }
    session.current_pseudonym = pseudonym.to_string();
    let mut batch = db.batch();
    write_session(&mut batch, session_id, &session);
    batch
        .commit_with(Durability::SyncWal)
        .map_err(|e| e.to_string())
}

pub fn destroy_session(db: &Db, session_id: &str) -> Result<(), String> {
    let mut batch = db.batch();
    delete_session(&mut batch, session_id);
    batch
        .commit_with(Durability::SyncWal)
        .map_err(|e| e.to_string())
}

pub fn session_cookie_value(session_id: &str) -> Cookie<'static> {
    build_cookie(SESSION_COOKIE, session_id.to_string())
}

pub fn clear_session_cookie() -> Cookie<'static> {
    clear_cookie(SESSION_COOKIE)
}

pub fn oauth_state_cookie_value(state: &str) -> Cookie<'static> {
    build_cookie(OAUTH_STATE_COOKIE, state.to_string())
}

pub fn clear_oauth_state_cookie() -> Cookie<'static> {
    clear_cookie(OAUTH_STATE_COOKIE)
}

pub fn auth_return_cookie_value(return_to: &str) -> Cookie<'static> {
    build_cookie(AUTH_RETURN_COOKIE, return_to.to_string())
}

pub fn clear_auth_return_cookie() -> Cookie<'static> {
    clear_cookie(AUTH_RETURN_COOKIE)
}

pub fn auth_return_from_jar(jar: &CookieJar) -> Option<String> {
    jar.get(AUTH_RETURN_COOKIE).map(|c| c.value().to_string())
}

pub fn session_id_from_jar(jar: &CookieJar) -> Option<String> {
    jar.get(SESSION_COOKIE).map(|c| c.value().to_string())
}

pub fn oauth_state_from_jar(jar: &CookieJar) -> Option<String> {
    jar.get(OAUTH_STATE_COOKIE).map(|c| c.value().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        assert_eq!(
            resolve_vote_actor(&db, None).unwrap_err(),
            "sign in to vote"
        );
    }

    #[test]
    fn session_without_pseudonym_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let (id, _) = create_session(&db, DEFAULT_ACTOR_UUID, "").unwrap();
        assert_eq!(
            resolve_vote_actor(&db, Some(&id)).unwrap_err(),
            "choose an alias first"
        );
    }

    #[test]
    fn session_with_pseudonym_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path()).unwrap();
        let (id, _) = create_session(&db, DEFAULT_ACTOR_UUID, "alice").unwrap();
        let actor = resolve_vote_actor(&db, Some(&id)).unwrap();
        assert_eq!(actor.pseudonym, "alice");
        assert_eq!(actor.trust_weight, 1.0);
    }
}
