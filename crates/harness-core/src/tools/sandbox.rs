//! The filesystem boundary: which paths a tool may touch, independent of the approval gate.
//!
//! The gate and the sandbox answer different questions. The gate asks *may this call run?* and its
//! answer depends on a human, a config mode and a timeout. The sandbox asks *what can this call
//! reach?* and its answer is a property of the path, not of the moment. A one-keypress prompt is
//! not a boundary, and a boundary is not a human decision — so this module never consults the gate,
//! and the gate never widens what it returns.
//!
//! Resolution is deliberately paranoid in one specific way: **the deepest existing ancestor is
//! canonicalized, then the rest is appended**. Lexical normalization alone (`..` folding) is not
//! enough, because `ws/link -> /etc` makes `ws/link/passwd` lexically "inside" the workspace while
//! the kernel writes `/etc/passwd`. Canonicalizing the ancestor that exists is what closes that.

use std::path::{Component, Path, PathBuf};

use crate::config::expand_tilde;

/// A resolved set of paths tools may read and write.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
    extra_roots: Vec<PathBuf>,
}

impl Sandbox {
    /// A sandbox confined to `root` and nothing else.
    pub fn workspace(root: impl Into<PathBuf>) -> Self {
        Self {
            root: canonical_or_self(root.into()),
            extra_roots: Vec::new(),
        }
    }

    /// [`Self::workspace`] plus roots the operator opted into (`paths.extra_roots`).
    ///
    /// A root that does not exist is kept verbatim rather than dropped: the operator may be
    /// pointing at a directory a hook or a job creates later, and silently narrowing the sandbox
    /// after the fact is the kind of surprise this module exists to avoid.
    pub fn with_extra_roots(
        root: impl Into<PathBuf>,
        extra_roots: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        Self {
            root: canonical_or_self(root.into()),
            extra_roots: extra_roots.into_iter().map(canonical_or_self).collect(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn extra_roots(&self) -> &[PathBuf] {
        &self.extra_roots
    }

    /// Resolve `user_path` and refuse anything outside the sandbox.
    ///
    /// The returned path has its deepest existing ancestor canonicalized, so a caller that writes
    /// to it writes where this check looked.
    pub fn resolve(&self, user_path: &str) -> Result<PathBuf, String> {
        let trimmed = user_path.trim();
        if trimmed.is_empty() {
            return Err("Path is empty".to_string());
        }

        let expanded = expand_tilde(trimmed);
        let raw = Path::new(&expanded);
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.root.join(raw)
        };

        let resolved = canonicalize_deepest(&lexical_normalize(&joined));
        if self.allows(&resolved) {
            Ok(resolved)
        } else {
            Err(self.escape_message(&resolved, user_path))
        }
    }

    /// Whether an already-resolved path is inside the sandbox.
    pub fn allows(&self, resolved: &Path) -> bool {
        resolved.starts_with(&self.root)
            || self
                .extra_roots
                .iter()
                .any(|extra| resolved.starts_with(extra))
    }

    fn escape_message(&self, resolved: &Path, user_path: &str) -> String {
        let mut allowed = format!("'{}'", self.root.display());
        if !self.extra_roots.is_empty() {
            let extras: Vec<String> = self
                .extra_roots
                .iter()
                .map(|extra| format!("'{}'", extra.display()))
                .collect();
            allowed.push_str(&format!(" and {}", extras.join(", ")));
        }
        format!(
            "Path escapes workspace root {}: {} (resolved to {})",
            allowed,
            user_path,
            resolved.display()
        )
    }
}

fn canonical_or_self(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

/// Fold `.` and `..` without touching the filesystem.
///
/// `..` pops a component, and popping past the root is a no-op — which is what `Path::pop` already
/// does for `/`. Symlinks are *not* resolved here; [`canonicalize_deepest`] is what makes the
/// result trustworthy.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// Canonicalize the longest existing prefix of `path`, then re-append the rest.
///
/// A path that does not exist yet cannot be canonicalized, but its parent usually can — and the
/// parent is where a symlink would be hiding. When nothing along the path exists (a fresh
/// workspace), the lexical form is returned unchanged.
fn canonicalize_deepest(path: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut candidate = path.to_path_buf();

    // Bounded so a pathological path cannot spin; 256 components is far past any real layout.
    for _ in 0..256 {
        if let Ok(canonical) = candidate.canonicalize() {
            let mut resolved = canonical;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        match candidate.components().next_back() {
            Some(Component::Normal(part)) => {
                rest.push(part.to_os_string());
                candidate.pop();
            }
            // `..` cannot appear after normalization, but a root with nothing to pop means there
            // is no existing ancestor to canonicalize.
            _ => return path.to_path_buf(),
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_dot_folds_and_cannot_climb_above_the_root() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(lexical_normalize(Path::new("/../..")), PathBuf::from("/"));
    }
}
