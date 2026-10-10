//! Decisions and recent folders persist in ~/Library/Application Support/Flick,
//! never on the card, so quitting mid-cull loses nothing.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Decision {
    Keep,
    Delete,
}

fn dir() -> PathBuf {
    let d = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("Flick");
    let _ = std::fs::create_dir_all(d.join("sessions"));
    d
}

fn session_file(root: &Path) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    dir().join("sessions").join(format!("{:016x}.json", h.finish()))
}

pub fn load_decisions(root: &Path) -> HashMap<String, Decision> {
    std::fs::read(session_file(root))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_decisions(root: &Path, d: &HashMap<String, Decision>) {
    if let Ok(b) = serde_json::to_vec(d) {
        let path = session_file(root);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

pub fn recents() -> Vec<PathBuf> {
    std::fs::read(dir().join("recents.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<PathBuf>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.is_dir())
        .collect()
}

pub fn push_recent(root: &Path) {
    let mut r = recents();
    r.retain(|p| p != root);
    r.insert(0, root.to_path_buf());
    r.truncate(9);
    if let Ok(b) = serde_json::to_vec(&r) {
        let _ = std::fs::write(dir().join("recents.json"), b);
    }
}

/// Mounted volumes that look like camera cards.
pub fn cards() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/Volumes") else { return vec![] };
    let mut v: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("DCIM").is_dir())
        .collect();
    v.sort();
    v
}
