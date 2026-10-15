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
        SessionSelection::Resume(id) => {
            store.ensure_session(&id).await?;
            Ok(id)
        }
        SessionSelection::Continue => match store.most_recent().await? {
            Some(id) => Ok(id),
            // `--continue` on a board with no sessions is not an error: it starts one.
            None => create_session(store).await,
        },
        SessionSelection::New => create_session(store).await,
    }
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
                "{:<10} {:<19} {:<19} {:>5}  {:<16}  TITLE",
                "ID", "CREATED", "UPDATED", "MSGS", "NAME"
            );
            for session in sessions {
                let name = if session.name.is_empty() {
                    "-"
                } else {
                    session.name.as_str()
                };
                println!(
                    "{:<10} {:<19} {:<19} {:>5}  {:<16}  {}",
                    session.id,
                    session.created_at.format("%Y-%m-%d %H:%M:%S"),
                    session.updated_at.format("%Y-%m-%d %H:%M:%S"),
                    session.message_count,
                    name,
                    session.title
                );
            }
            Ok(())
        }
    }
}
