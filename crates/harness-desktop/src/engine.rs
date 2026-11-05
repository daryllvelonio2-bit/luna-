use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::adapter::{ActionOutcome, ActionRequest, DesktopAdapter, DesktopStatus};
use crate::deadline::Deadline;
use crate::error::{DesktopError, ErrorCode};
use crate::refs::entry::RefEntry;
use crate::refs::store::RefStore;
use crate::refs::token::{qualify_ref_id, resolve_ref_target, validate_ref_token};
use crate::tree::identity::ElementIdentity;
use crate::tree::node::AccessibilityNode;
use crate::tree::options::TreeOptions;

/// How many snapshots the engine keeps. A ref from an older snapshot can still
/// be resolved — the identity is re-read from the live tree — but the store
/// stops growing without bound.
pub const MAX_SNAPSHOTS: usize = 8;

/// The engine that drives one desktop.
///
/// It owns the two things a platform cannot: the ref store and the rule that a
/// ref resolves to exactly one element. Everything else is the adapter's.
pub struct DesktopEngine {
    adapter: Arc<dyn DesktopAdapter>,
    refs: Mutex<RefStore>,
    timeout: std::time::Duration,
}

impl DesktopEngine {
    pub fn new(adapter: Arc<dyn DesktopAdapter>) -> Self {
        Self {
            adapter,
            refs: Mutex::new(RefStore::new()),
            timeout: crate::deadline::DEFAULT_TIMEOUT,
        }
    }

    /// The adapter this engine drives.
    pub fn adapter(&self) -> &Arc<dyn DesktopAdapter> {
        &self.adapter
    }

    /// The platform name, for the tool descriptions and the skill.
    pub fn platform(&self) -> &'static str {
        self.adapter.platform()
    }

    /// The budget one command runs under.
    pub fn deadline(&self) -> Deadline {
        Deadline::new(self.timeout)
    }

    /// What this desktop can do right now.
    pub fn status(&self) -> Result<DesktopStatus, DesktopError> {
        self.adapter.status(self.deadline())
    }

    /// Observe a tree, mint its refs and answer with the snapshot.
    pub fn observe(&self, app: &str, options: &TreeOptions) -> Result<Value, DesktopError> {
        let mut options = options.clone();
        if let Some(root) = options.root.clone() {
            // A drill-down names its start by ref; the platform needs the
            // element the ref stood for.
            let entry = self.resolve(&root)?;
            options.root_identity = Some(entry.identity);
        }
        let observed = self.adapter.observe(app, &options, self.deadline())?;
        let snapshot_id = new_snapshot_id();
        let mut root = observed.root;
        let mut counter = 0usize;

        {
            let mut store = self.refs.lock().unwrap_or_else(|error| error.into_inner());
            store.begin_snapshot(&snapshot_id, app);
            mint_refs(&mut root, &snapshot_id, app, &mut counter, &mut store);
            store.prune(MAX_SNAPSHOTS);
        }

        Ok(json!({
            "app": app,
            "snapshot_id": snapshot_id,
            "ref_count": counter,
            "complete": !root.subtree_truncated,
            "tree": root,
        }))
    }

    /// What a ref stands for.
    ///
    /// A ref from a snapshot the store still knows but no longer holds an entry
    /// for is stale; a ref from a snapshot the store has forgotten is not
    /// found. The two are different failures and an agent recovers from them
    /// differently.
    pub fn resolve(&self, ref_id: &str) -> Result<RefEntry, DesktopError> {
        validate_ref_token(ref_id)?;
        let store = self.refs.lock().unwrap_or_else(|error| error.into_inner());
        match store.get(ref_id) {
            Some(entry) => Ok(entry.clone()),
            None => {
                let (snapshot_id, _) = resolve_ref_target(ref_id)?;
                let known = store.snapshot(&snapshot_id).is_some();
                drop(store);
                Err(if known {
                    DesktopError::new(
                        ErrorCode::StaleRef,
                        format!("The element '{ref_id}' is no longer in the window"),
                    )
                    .with_suggestion("Take a fresh snapshot and use the ref it prints")
                    .with_details(json!({ "kind": "saved_ref_absent" }))
                    .not_delivered()
                } else {
                    DesktopError::new(
                        ErrorCode::SnapshotNotFound,
                        format!("The snapshot '{snapshot_id}' is no longer available"),
                    )
                    .with_suggestion("Take a fresh snapshot and use the ref it prints")
                    .not_delivered()
                })
            }
        }
    }

    /// Run an action against the element a ref names.
    pub fn act(&self, ref_id: &str, request: ActionRequest) -> Result<ActionOutcome, DesktopError> {
        let entry = self.resolve(ref_id)?;
        self.adapter
            .execute(&entry.app, &entry.identity, &request, self.deadline())
    }
}

/// Give every actionable node in the tree a ref, and remember what it stands for.
fn mint_refs(
    node: &mut AccessibilityNode,
    snapshot_id: &str,
    app: &str,
    counter: &mut usize,
    store: &mut RefStore,
) {
    if node.is_addressable() {
        *counter += 1;
        let local_ref = format!("@e{counter}");
        let identity: ElementIdentity = node.element_identity();
        node.ref_id = Some(qualify_ref_id(snapshot_id, &local_ref));
        store.insert(RefEntry::new(snapshot_id, &local_ref, app, identity));
    }
    for child in &mut node.children {
        mint_refs(child, snapshot_id, app, counter, store);
    }
}

/// A short, readable snapshot id.
fn new_snapshot_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    let mut remaining: u32 = rng.gen();
    let mut encoded = String::new();
    while remaining > 0 && encoded.len() < 8 {
        encoded.push(ALPHABET[(remaining % ALPHABET.len() as u32) as usize] as char);
        remaining /= ALPHABET.len() as u32;
    }
    if encoded.is_empty() {
        encoded.push('s');
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::action::ActionOps;
    use crate::adapter::observation::ObservationOps;
    use crate::adapter::system::SystemOps;
    use crate::tree::identity::NodeIdentity;
    use crate::tree::presentation::NodePresentation;

    /// An adapter that serves one fixed tree.
    struct Fake {
        tree: AccessibilityNode,
    }

    impl Fake {
        fn new() -> Self {
            let button = AccessibilityNode {
                ref_id: None,
                role: "push button".to_string(),
                identity: NodeIdentity {
                    name: Some("7".to_string()),
                    ..Default::default()
                },
                presentation: NodePresentation {
                    available_actions: vec!["click".to_string()],
                    ..Default::default()
                },
                handle: None,
                children_count: None,
                subtree_truncated: false,
                children: Vec::new(),
            };
            let root = AccessibilityNode {
                ref_id: None,
                role: "window".to_string(),
                identity: NodeIdentity {
                    name: Some("Calculator".to_string()),
                    ..Default::default()
                },
                presentation: NodePresentation::default(),
                handle: None,
                children_count: Some(1),
                subtree_truncated: false,
                children: vec![button],
            };
            Self { tree: root }
        }
    }

    impl ObservationOps for Fake {
        fn observe(
            &self,
            _app: &str,
            _options: &TreeOptions,
            _deadline: Deadline,
        ) -> Result<crate::adapter::types::ObservedTree, DesktopError> {
            Ok(crate::adapter::types::ObservedTree::new(
                "Calculator",
                self.tree.clone(),
            ))
        }
    }

    impl ActionOps for Fake {
        fn execute(
            &self,
            _app: &str,
            _identity: &ElementIdentity,
            request: &ActionRequest,
            _deadline: Deadline,
        ) -> Result<ActionOutcome, DesktopError> {
            Ok(ActionOutcome::verified(vec![request.name().to_string()]))
        }
    }

    impl SystemOps for Fake {
        fn platform(&self) -> &'static str {
            "test"
        }
    }

    fn engine() -> DesktopEngine {
        DesktopEngine::new(Arc::new(Fake::new()))
    }

    #[test]
    fn a_snapshot_mints_a_ref_for_the_actionable_node_only() {
        let snapshot = engine().observe("Calculator", &TreeOptions::default()).expect("data");
        assert_eq!(snapshot["ref_count"], 1);
        let ref_id = snapshot["tree"]["children"][0]["ref_id"].as_str().expect("a ref");
        assert!(ref_id.starts_with('@') && ref_id.contains(":e1"), "{ref_id}");
    }

    #[test]
    fn the_snapshot_id_is_short_and_readable() {
        let id = new_snapshot_id();
        assert!(id.len() <= 8, "{id}");
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[test]
    fn a_minted_ref_resolves_to_its_element() {
        let engine = engine();
        let snapshot = engine.observe("Calculator", &TreeOptions::default()).expect("data");
        let ref_id = snapshot["tree"]["children"][0]["ref_id"]
            .as_str()
            .expect("a ref")
            .to_string();
        let entry = engine.resolve(&ref_id).expect("the ref resolves");
        assert_eq!(entry.app, "Calculator");
        assert_eq!(entry.identity.name.as_deref(), Some("7"));
    }

    #[test]
    fn an_unknown_snapshot_is_reported_as_not_found() {
        let error = engine().resolve("@nosuch:e1").expect_err("no such snapshot");
        assert_eq!(error.code, ErrorCode::SnapshotNotFound);
    }

    #[test]
    fn a_malformed_ref_is_reported_as_bad_arguments() {
        let error = engine().resolve("@e1").expect_err("a bare ref");
        assert_eq!(error.code, ErrorCode::InvalidArgs);
    }
}
