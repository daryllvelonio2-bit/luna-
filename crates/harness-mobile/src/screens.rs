use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;

use harness_ui_spec::Screen;

use crate::MobileError;

/// In-memory screen registry backed by one JSON file per screen. The renderer reads from here, so
/// a freshly generated screen is visible immediately and survives an app restart.
pub struct UiSpecStore {
    dir: Option<PathBuf>,
    screens: RwLock<HashMap<String, Screen>>,
}

impl UiSpecStore {
    pub fn new(dir: Option<PathBuf>) -> Result<Self, MobileError> {
        let store = Self {
            dir,
            screens: RwLock::new(HashMap::new()),
        };
        store.load()?;
        Ok(store)
    }

    pub fn upsert(&self, screen: Screen) -> Result<Screen, MobileError> {
        screen.validate()?;
        if let Some(dir) = &self.dir {
            fs::write(dir.join(file_name(&screen.id)), screen.to_json()?)?;
        }
        self.screens
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(screen.id.clone(), screen.clone());
        Ok(screen)
    }

    pub fn get(&self, id: &str) -> Option<Screen> {
        self.screens
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }

    pub fn list(&self) -> Vec<Screen> {
        let screens = self
            .screens
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut screens: Vec<Screen> = screens.values().cloned().collect();
        screens.sort_by(|left, right| left.id.cmp(&right.id));
        screens
    }

    fn load(&self) -> Result<(), MobileError> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        fs::create_dir_all(dir)?;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let raw = fs::read_to_string(&path)?;
            match Screen::from_json(&raw) {
                Ok(screen) => {
                    self.screens
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(screen.id.clone(), screen);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "skipping an unreadable stored screen"
                    );
                }
            }
        }
        Ok(())
    }
}

fn file_name(id: &str) -> String {
    let mut name = String::with_capacity(id.len() + 5);
    for character in id.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            name.push(character);
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        name.push_str("screen");
    }
    name.push_str(".json");
    name
}
