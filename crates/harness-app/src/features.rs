//! Runtime feature toggles shared across CLI/TUI and GUI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Memory,
    Notes,
    Skills,
    Mcp,
}

impl Feature {
    pub fn name(self) -> &'static str {
        match self {
            Feature::Memory => "memory",
            Feature::Notes => "notes",
            Feature::Skills => "skills",
            Feature::Mcp => "mcp",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Feature::Memory => "Saved memory",
            Feature::Notes => "Notes",
            Feature::Skills => "Skills",
            Feature::Mcp => "MCP",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeatureToggles {
    memory: Arc<AtomicBool>,
    notes: Arc<AtomicBool>,
    skills: Arc<AtomicBool>,
    mcp: Arc<AtomicBool>,
}

impl Default for FeatureToggles {
    fn default() -> Self {
        Self {
            memory: Arc::new(AtomicBool::new(true)),
            notes: Arc::new(AtomicBool::new(true)),
            skills: Arc::new(AtomicBool::new(true)),
            mcp: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl FeatureToggles {
    pub fn from_config(config: &harness_core::Config) -> Self {
        Self {
            memory: Arc::new(AtomicBool::new(config.memory_effective_enabled())),
            notes: Arc::new(AtomicBool::new(config.notes_effective_enabled())),
            skills: Arc::new(AtomicBool::new(config.skills_effective_enabled())),
            mcp: Arc::new(AtomicBool::new(config.mcp_effective_enabled())),
        }
    }

    pub fn sync_from_config(&self, config: &harness_core::Config) {
        self.set(Feature::Memory, config.memory_effective_enabled());
        self.set(Feature::Notes, config.notes_effective_enabled());
        self.set(Feature::Skills, config.skills_effective_enabled());
        self.set(Feature::Mcp, config.mcp_effective_enabled());
    }

    pub fn snapshot(&self) -> (bool, bool, bool, bool) {
        (
            self.enabled(Feature::Memory),
            self.enabled(Feature::Notes),
            self.enabled(Feature::Skills),
            self.enabled(Feature::Mcp),
        )
    }

    pub fn restore(&self, snap: (bool, bool, bool, bool)) {
        self.set(Feature::Memory, snap.0);
        self.set(Feature::Notes, snap.1);
        self.set(Feature::Skills, snap.2);
        self.set(Feature::Mcp, snap.3);
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::Memory => self.memory.load(Ordering::Relaxed),
            Feature::Notes => self.notes.load(Ordering::Relaxed),
            Feature::Skills => self.skills.load(Ordering::Relaxed),
            Feature::Mcp => self.mcp.load(Ordering::Relaxed),
        }
    }

    pub fn set(&self, feature: Feature, enabled: bool) {
        let slot = match feature {
            Feature::Memory => &self.memory,
            Feature::Notes => &self.notes,
            Feature::Skills => &self.skills,
            Feature::Mcp => &self.mcp,
        };
        slot.store(enabled, Ordering::SeqCst);
    }

    pub fn notes_atomic(&self) -> Arc<AtomicBool> {
        self.notes.clone()
    }
}
