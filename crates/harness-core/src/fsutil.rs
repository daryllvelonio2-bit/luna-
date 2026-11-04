//! Filesystem helpers whose behaviour is the same on Unix and Windows.

use std::io;
use std::path::{Path, PathBuf};

/// Drop the `\\?\` prefix `canonicalize` adds on Windows.
///
/// It denotes the same file but reads badly in the UI, cannot be handed to some tools, and does not
/// compare equal to the path the user typed. On every other platform this is the identity.
#[cfg(windows)]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy().into_owned();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

#[cfg(not(windows))]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// `std::fs::canonicalize`, minus the Windows verbatim prefix.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(strip_verbatim)
}

/// Move `source` over `destination`, replacing it when it already exists.
///
/// `std::fs::rename` replaces an existing destination on Unix but fails with "already exists" on
/// Windows. Every caller here writes a temporary file and then renames it into place, so the
/// difference turns every second save into an error on Windows; this closes it.
pub fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        match std::fs::rename(source, destination) {
            Ok(()) => Ok(()),
            Err(_) if destination.exists() => {
                std::fs::remove_file(destination)?;
                std::fs::rename(source, destination)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)
    }
}
