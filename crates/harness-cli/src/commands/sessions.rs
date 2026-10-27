//! `luna sessions list` and the session a chat run attaches to.

use clap::Subcommand;
use colored::Colorize;

use harness_core::{Config, HarnessError, SessionStore, SqliteSessionStore};

#[derive(Subcommand, Debug)]
pub enum SessionsCommand {
    /// List stored chat sessions, most recently updated first
    List,
    /// Interactively select a chat session (Up/Down arrows to select)
    #[command(alias = "pick")]
    Select,
    /// Fork an existing chat session into a new branch session
    Fork {
        /// Session id or name to fork
        session: String,
        /// Optional title for the forked session
        #[arg(short, long)]
        title: Option<String>,
    },
    /// Delete a stored chat session by id or name
    #[command(alias = "rm")]
    Delete {
        /// Session id or name to delete
        session: String,
    },
}

/// Which session `luna chat` runs against.
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
            // `--continue` on a board with no sessions is not an error: it starts one.
            None => create_session(store).await,
        },
        SessionSelection::New => create_session(store).await,
    }
}

/// The id of the most recently updated session carrying this name. `list()` is ordered
/// newest-first, so the first match is the one the user last used.
pub(crate) async fn find_by_name(
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

async fn create_session(store: &SqliteSessionStore) -> Result<String, HarnessError> {
    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let id = format!("s{}", &uuid[..8]);
    store.ensure_session(&id).await?;
    Ok(id)
}

async fn list_sessions(config: &Config) -> Result<(), HarnessError> {
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

/// `luna sessions list`: id, created, updated, live message count, first user line.
/// Returns `Some(session_id)` when an interactive selection was made.
pub async fn handle_sessions_command(
    cmd: SessionsCommand,
    config: &Config,
) -> Result<Option<String>, HarnessError> {
    match cmd {
        SessionsCommand::List => {
            list_sessions(config).await?;
            Ok(None)
        }
        SessionsCommand::Select => {
            let store = open_store(config)?;
            let sessions = store.list().await?;

            if sessions.is_empty() {
                println!("no sessions yet — `luna chat` creates one");
                return Ok(None);
            }

            if !dialoguer::console::Term::stdout().features().is_attended() {
                list_sessions(config).await?;
                return Ok(None);
            }

            let items: Vec<String> = sessions
                .iter()
                .map(|s| {
                    let name = if s.name.is_empty() {
                        "-".to_string()
                    } else {
                        format!("\"{}\"", s.name)
                    };
                    let title = if s.title.is_empty() {
                        "untitled"
                    } else {
                        s.title.as_str()
                    };
                    let model = match &s.model {
                        Some((_, m)) => m.as_str(),
                        None => "-",
                    };
                    format!(
                        "{:<9} {:<14} {:>3} msgs  {:<16} {:<16} {}",
                        s.id,
                        name,
                        s.message_count,
                        s.updated_at.format("%Y-%m-%d %H:%M"),
                        model,
                        title
                    )
                })
                .collect();

            let sel = dialoguer::Select::new()
                .with_prompt("Select a session to resume (↑/↓ to move, Enter to select, Esc to cancel)")
                .items(&items)
                .default(0)
                .interact_opt()
                .map_err(|e| HarnessError::Io(e.to_string()))?;

            if let Some(idx) = sel {
                Ok(Some(sessions[idx].id.clone()))
            } else {
                Ok(None)
            }
        }
        SessionsCommand::Fork { session, title } => {
            let store = open_store(config)?;
            let id = if store.get(&session).await.is_ok() {
                session
            } else if let Some(resolved) = find_by_name(&store, &session).await? {
                resolved
            } else {
                return Err(HarnessError::SessionError(format!(
                    "session '{}' not found",
                    session
                )));
            };

            let forked_id = store.fork_session(&id, title.as_deref()).await?;
            println!(
                "{} Forked session {} -> {}",
                "✔".green().bold(),
                id.dimmed(),
                forked_id.cyan().bold()
            );
            Ok(None)
        }
        SessionsCommand::Delete { session } => {
            let store = open_store(config)?;
            let id = if store.get(&session).await.is_ok() {
                session
            } else if let Some(resolved) = find_by_name(&store, &session).await? {
                resolved
            } else {
                return Err(HarnessError::SessionError(format!(
                    "session '{}' not found",
                    session
                )));
            };

            let deleted = store.delete_session(&id).await?;
            if deleted {
                println!("{} Deleted session {}", "✔".green().bold(), id.cyan());
            } else {
                println!(
                    "{} Session {} not found or already deleted",
                    "ℹ".dimmed(),
                    id
                );
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sessions_command_delete() {
        let db_path =
            std::env::temp_dir().join(format!("luna_test_sessions_{}.db", uuid::Uuid::new_v4()));
        let store = SqliteSessionStore::new(Some(db_path.clone())).unwrap();

        // Create two sessions
        let s1 = create_session(&store).await.unwrap();
        store.set_name(&s1, "work-session").await.unwrap();

        let mut config = Config::default();
        config.paths.sessions_db = db_path.clone();

        // Delete by name
        handle_sessions_command(
            SessionsCommand::Delete {
                session: "work-session".to_string(),
            },
            &config,
        )
        .await
        .unwrap();

        assert!(
            store.get(&s1).await.is_err(),
            "deleted session must be gone"
        );

        // Delete non-existent session returns error
        let err = handle_sessions_command(
            SessionsCommand::Delete {
                session: "non-existent".to_string(),
            },
            &config,
        )
        .await;
        assert!(err.is_err());

        // Select and List commands run cleanly
        let list_res = handle_sessions_command(SessionsCommand::List, &config).await;
        assert!(list_res.is_ok());

        let select_res = handle_sessions_command(SessionsCommand::Select, &config).await;
        assert!(select_res.is_ok());

        let _ = std::fs::remove_file(db_path);
    }
}
