/// Shortens any file or directory path to at most 3 path components (e.g. `downloads/rust-agent-harness/agents.md`).
pub fn shorten_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return trimmed.to_string();
    }
    // `C:\...` — the drive counts as a component so the shortened form still says which disk.
    let drive = trimmed
        .as_bytes()
        .get(0..2)
        .filter(|head| head[0].is_ascii_alphabetic() && head[1] == b':')
        .map(|_| &trimmed[..2]);
    let parts: Vec<&str> = trimmed
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "~")
        .collect();
    if let Some(drive) = drive {
        let keep = 2.min(parts.len());
        return format!("{drive}/{}", parts[parts.len() - keep..].join("/"));
    }
    if parts.len() > 3 {
        parts[parts.len() - 3..].join("/")
    } else if parts.is_empty() {
        trimmed.to_string()
    } else if trimmed.starts_with('/') {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

/// Scans any string and shortens any embedded path with more than 3 segments to at most 3 segments.
pub fn shorten_paths_in_text(raw: &str) -> String {
    let mut out = Vec::new();
    for token in raw.split(' ') {
        let clean =
            token.trim_matches(|c| c == '\'' || c == '"' || c == '`' || c == '(' || c == ')');
        // A Windows path is only treated as one when it is absolute: a bare token with
        // backslashes is far more likely to be an escape sequence or regex than a path.
        let drive_absolute = clean
            .as_bytes()
            .get(0..3)
            .is_some_and(|head| head[0].is_ascii_alphabetic() && &head[1..] == b":\\");
        let looks_like_path = clean.starts_with('/')
            || clean.starts_with("~/")
            || clean.starts_with("~\\")
            || clean.starts_with(r"\\")
            || clean.contains('/')
            || drive_absolute;
        if looks_like_path
            && clean
                .split(['/', '\\'])
                .filter(|s| !s.is_empty() && *s != "~" && *s != ".")
                .count()
                > 3
        {
            let short = shorten_path(clean);
            out.push(token.replace(clean, &short));
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// The effective runtime paths, resolved once per process from the config file and the
/// environment.
///
/// The transcript renders tool calls by looking a record up in the stores it was written to, so
/// the paths have to be the *configured* ones (`paths.memory_db`, `paths.notes_dir`) — a hardcoded
/// `~/.luna` would show nothing for an install that moved its state elsewhere.
pub(crate) fn effective_paths() -> &'static harness_core::config::PathsSettings {
    static PATHS: std::sync::LazyLock<harness_core::config::PathsSettings> =
        std::sync::LazyLock::new(|| {
            harness_core::config::process_layers(harness_core::Config::default())
                .ok()
                .and_then(|layers| harness_core::config::resolve(&layers).ok())
                .map(|resolved| resolved.into_config().paths)
                .unwrap_or_default()
        });
    &PATHS
}

pub(crate) fn lookup_memory_from_db(id: &str) -> Option<(String, String)> {
    let db_path = effective_paths().memory_db.clone();
    if !db_path.exists() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut stmt = conn
        .prepare("SELECT subject, predicate FROM memories WHERE id = ?1 LIMIT 1")
        .ok()?;
    let mut rows = stmt.query([id]).ok()?;
    if let Ok(Some(row)) = rows.next() {
        let subject: String = row.get(0).ok()?;
        let predicate: String = row.get(1).ok()?;
        return Some((subject, predicate));
    }
    None
}

/// The title of a note, by its id (file stem) or by its exact title.
///
/// File-backed notes are plain markdown, so the title comes from the front matter of
/// `<notes_dir>/<id>.md`; a title that is not a file name is matched by scanning the folder.
pub(crate) fn lookup_note_title(id: &str) -> Option<String> {
    let paths = effective_paths();
    let dir = &paths.notes_dir;

    let direct = dir.join(format!("{}.md", id));
    if direct.exists() {
        if let Some(title) = read_note_title(&direct) {
            return Some(title);
        }
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if let Some(title) = read_note_title(&path) {
                if title.eq_ignore_ascii_case(id) {
                    return Some(title);
                }
            }
        }
    }

    // Legacy store, before the import ran (or for a note the import skipped).
    let db_path = if paths.notes_db.exists() {
        paths.notes_db.clone()
    } else {
        paths.notes_db.with_extension("db.imported")
    };
    if !db_path.exists() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut stmt = conn
        .prepare("SELECT title FROM notes WHERE id = ?1 OR title = ?1 COLLATE NOCASE LIMIT 1")
        .ok()?;
    let mut rows = stmt.query([id]).ok()?;
    if let Ok(Some(row)) = rows.next() {
        let title: String = row.get(0).ok()?;
        return Some(title);
    }
    None
}

/// The `title:` field of a note's front matter, if the file has one.
pub(crate) fn read_note_title(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        if line.trim() == "---" {
            // The front matter ended without a title.
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "title" {
                let title = value.trim().to_string();
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }
    None
}
