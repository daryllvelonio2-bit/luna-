//! Session store helpers shared across CLI/TUI and native GUI.

use harness_core::{Config, HarnessError, SessionRecord, SessionStore, SqliteSessionStore};

/// Which session a run attaches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelection {
    /// A fresh session id.
    New,
    /// `--session <id>`; the session is created when it does not exist yet.
    Resume(String),
    /// Fork an existing session and start on the new copy.
    Fork(String),
    /// `--continue`: the most recently updated session.
    Continue,
}

/// The store at `paths.sessions_db`.
pub fn open_store(config: &Config) -> Result<SqliteSessionStore, HarnessError> {
    SqliteSessionStore::new(Some(config.paths.sessions_db.clone()))
}

/// Resolve the selection to an existing session id.
pub async fn resolve_session(
    store: &SqliteSessionStore,
    selection: SessionSelection,
) -> Result<String, HarnessError> {
    match selection {
        SessionSelection::Resume(value) => {
            if store.get(&value).await.is_err() {
                if let Some(id) = find_by_name(store, &value).await? {
                    return Ok(id);
                }
            }
            store.ensure_session(&value).await?;
            Ok(value)
        }
        SessionSelection::Fork(source) => {
            let id = if store.get(&source).await.is_ok() {
                source
            } else if let Some(resolved) = find_by_name(store, &source).await? {
                resolved
            } else {
                return Err(HarnessError::SessionError(format!(
                    "source session '{}' not found",
                    source
                )));
            };
            store.fork_session(&id, None).await
        }
        SessionSelection::Continue => match store.most_recent().await? {
            Some(id) => Ok(id),
            None => create_session(store).await,
        },
        SessionSelection::New => create_session(store).await,
    }
}

/// Most recently updated session carrying `name`.
pub async fn find_by_name(
    store: &SqliteSessionStore,
    name: &str,
) -> Result<Option<String>, HarnessError> {
    if name.trim().is_empty() {
        return Ok(None);
    }
    Ok(store
        .list()
        .await?
        .into_iter()
        .find(|session| session.name == name)
        .map(|session| session.id))
}

/// Create a fresh random session id and persist it.
pub async fn create_session(store: &SqliteSessionStore) -> Result<String, HarnessError> {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let id = format!("s{}", &uuid[..8]);
    store.ensure_session(&id).await?;
    Ok(id)
}

/// List stored sessions ordered newest-first.
pub async fn list_sessions(store: &SqliteSessionStore) -> Result<Vec<SessionRecord>, HarnessError> {
    store.list().await
}
