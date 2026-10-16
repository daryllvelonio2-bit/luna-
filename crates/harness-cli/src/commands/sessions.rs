//! `luna sessions list` and the session a chat run attaches to.

use clap::Subcommand;

use harness_core::{Config, HarnessError, SessionStore, SqliteSessionStore};

#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List stored chat sessions, most recently updated first
    List,
}

/// Which session `luna chat` runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSelection {
    /// A fresh session id.
    New,
    /// `--session <id>`; the session is created when it does not exist yet.
    Resume(String),
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
            // A session *name* is accepted as well as an id. Someone who named a session "luna"
            // types `luna --session luna`; creating a fresh session called "luna" instead would
            // silently strand the transcript they were trying to continue.
            if store.get(&value).await.is_err() {
                if let Some(id) = find_by_name(store, &value).await? {
                    return Ok(id);
                }
            }
            store.ensure_session(&value).await?;
            Ok(value)
        }
        SessionSelection::Continue => match store.most_recent().await? {
            Some(id) => Ok(id),
            // `--continue` on a board with no sessions is not an error: it starts one.
            None => create_session(store).await,
        },
        SessionSelection::New => create_session(store).await,
    }
}

/// The id of the most recently updated session carrying this name. `list()` is ordered
/// newest-first, so the first match is the one the user last used.
async fn find_by_name(store: &SqliteSessionStore, name: &str) -> Result<Option<String>, HarnessError> {
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

async fn create_session(store: &SqliteSessionStore) -> Result<String, HarnessError> {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let id = format!("s{}", &uuid[..8]);
    store.ensure_session(&id).await?;
    Ok(id)
}

/// `luna sessions list`: id, created, updated, live message count, first user line.
pub async fn handle_sessions_command(
    cmd: SessionsCommand,
    config: &Config,
) -> Result<(), HarnessError> {
    match cmd {
        SessionsCommand::List => {
            let store = open_store(config)?;
            let sessions = store.list().await?;

            if sessions.is_empty() {
                println!("no sessions yet — `luna chat` creates one");
                return Ok(());
            }

            println!(
                "{:<10} {:<19} {:<19} {:>5}  {:<16}  {:<24}  TITLE",
                "ID", "CREATED", "UPDATED", "MSGS", "NAME", "MODEL"
            );
            for session in sessions {
                let name = if session.name.is_empty() {
                    "-"
                } else {
                    session.name.as_str()
                };
                // The pair the session last ran with; a session that has not picked one yet
                // (and will resume on the config default) shows a placeholder.
                let model = match &session.model {
                    Some((provider, model)) => format!("{} ({})", model, provider),
                    None => "-".to_string(),
                };
                println!(
                    "{:<10} {:<19} {:<19} {:>5}  {:<16}  {:<24}  {}",
                    session.id,
                    session.created_at.format("%Y-%m-%d %H:%M:%S"),
                    session.updated_at.format("%Y-%m-%d %H:%M:%S"),
                    session.message_count,
                    name,
                    model,
                    session.title
                );
            }
            Ok(())
        }
    }
}
