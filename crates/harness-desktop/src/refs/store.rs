use std::collections::HashMap;
use std::time::Instant;

use super::entry::RefEntry;

/// What the store remembers about one snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotRecord {
    pub snapshot_id: String,
    pub app: String,
    pub created_at: Instant,
    pub entry_count: usize,
}

/// The refs a session has minted, and the snapshots they came from.
///
/// The store is what makes a ref mean something. Without it a ref is a string
/// that happens to look like an address; with it, a ref can be answered with
/// the app and the identity it stood for, and a ref from a dropped snapshot can
/// be refused instead of silently pointing at whatever is at that position now.
#[derive(Debug, Default)]
pub struct RefStore {
    snapshots: Vec<SnapshotRecord>,
    entries: HashMap<String, RefEntry>,
}

impl RefStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a new snapshot. The old snapshots stay until [`Self::prune`] runs.
    pub fn begin_snapshot(&mut self, snapshot_id: &str, app: &str) {
        self.snapshots.push(SnapshotRecord {
            snapshot_id: snapshot_id.to_string(),
            app: app.to_string(),
            created_at: Instant::now(),
            entry_count: 0,
        });
    }

    /// Remember one ref of the snapshot that is currently open.
    pub fn insert(&mut self, entry: RefEntry) {
        if let Some(current) = self.snapshots.last_mut() {
            if current.snapshot_id == entry.snapshot_id {
                current.entry_count += 1;
            }
        }
        self.entries.insert(entry.ref_id.clone(), entry);
    }

    /// What a ref stands for.
    pub fn get(&self, ref_id: &str) -> Option<&RefEntry> {
        self.entries.get(ref_id)
    }

    pub fn contains(&self, ref_id: &str) -> bool {
        self.entries.contains_key(ref_id)
    }

    pub fn snapshot(&self, snapshot_id: &str) -> Option<&SnapshotRecord> {
        self.snapshots
            .iter()
            .find(|record| record.snapshot_id == snapshot_id)
    }

    /// The most recent snapshot, if there is one.
    pub fn latest_snapshot(&self) -> Option<&SnapshotRecord> {
        self.snapshots.last()
    }

    /// Drop the oldest snapshots until at most `keep` remain.
    ///
    /// The refs of a dropped snapshot go with it: a ref from a snapshot the
    /// store no longer knows is a ref the agent cannot act on, and keeping the
    /// entry would let a later action use a tree that no longer exists.
    pub fn prune(&mut self, keep: usize) {
        if self.snapshots.len() <= keep {
            return;
        }
        let drop_until = self.snapshots.len() - keep;
        let dropped: Vec<String> = self.snapshots[..drop_until]
            .iter()
            .map(|record| record.snapshot_id.clone())
            .collect();
        self.snapshots.drain(..drop_until);
        self.entries
            .retain(|_, entry| !dropped.contains(&entry.snapshot_id));
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::identity::ElementIdentity;

    fn entry(snapshot: &str, local: &str) -> RefEntry {
        RefEntry::new(
            snapshot,
            local,
            "Calculator",
            ElementIdentity {
                role: "push button".to_string(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn a_ref_is_found_by_its_qualified_form() {
        let mut store = RefStore::new();
        store.begin_snapshot("s1", "Calculator");
        store.insert(entry("s1", "@e1"));
        assert!(store.contains("@s1:e1"));
        assert_eq!(store.get("@s1:e1").expect("entry").app, "Calculator");
    }

    #[test]
    fn pruning_drops_the_refs_of_the_snapshots_it_forgets() {
        let mut store = RefStore::new();
        for index in 1..=3 {
            let snapshot = format!("s{index}");
            store.begin_snapshot(&snapshot, "Calculator");
            store.insert(entry(&snapshot, "@e1"));
        }
        assert_eq!(store.snapshot_count(), 3);

        store.prune(1);
        assert_eq!(store.snapshot_count(), 1);
        assert!(!store.contains("@s1:e1"));
        assert!(!store.contains("@s2:e1"));
        assert!(store.contains("@s3:e1"));
    }

    #[test]
    fn pruning_below_the_current_count_keeps_everything() {
        let mut store = RefStore::new();
        store.begin_snapshot("s1", "Calculator");
        store.insert(entry("s1", "@e1"));
        store.prune(5);
        assert_eq!(store.snapshot_count(), 1);
        assert!(store.contains("@s1:e1"));
    }

    #[test]
    fn the_latest_snapshot_is_the_one_that_was_opened_last() {
        let mut store = RefStore::new();
        store.begin_snapshot("s1", "Calculator");
        store.begin_snapshot("s2", "Text Editor");
        assert_eq!(
            store.latest_snapshot().expect("a snapshot").snapshot_id,
            "s2"
        );
    }
}
