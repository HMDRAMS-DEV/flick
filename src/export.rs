//! Sending kept photos on: into a new album in Photos, into Dropbox's Camera Uploads, or into
//! a folder on a drive. Each runs on a worker thread and answers with one line for the toast.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::app::tilde;
use crate::library::human_bytes;

/// Imports `files` into a new Photos album named `album` and shows it.
/// macOS has no API for making a shared album, so sharing it is one click in Photos.
pub fn to_photos(album: &str, files: &[PathBuf]) -> Result<String, String> {
    const SCRIPT: &str = r#"on run argv
    set fs to {}
    repeat with i from 2 to count of argv
        set end of fs to (POSIX file (item i of argv)) as alias
    end repeat
    with timeout of 3600 seconds
        tell application "Photos"
            activate
            set a to make new album named (item 1 of argv)
            import fs into a
            spotlight a
        end tell
    end timeout
end run"#;
    let o = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(SCRIPT)
        .arg(album)
        .args(files)
        .output()
        .map_err(|e| format!("Could not run osascript: {e}"))?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        if err.contains("-1743") {
            return Err("Flick may not control Photos. Allow it in System Settings > Privacy & Security > Automation.".into());
        }
        return Err(format!("Photos: {}", err.trim()));
    }
    Ok(format!("Added {} to the album “{album}” in Photos. To share it: Share > Shared Albums", plural(files.len(), "photo")))
}

/// Dropbox's Camera Uploads folder, from the path the Dropbox app records.
pub fn camera_uploads() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let info: Option<serde_json::Value> =
        std::fs::read(home.join(".dropbox/info.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let recorded = info.as_ref().and_then(|v| ["personal", "business"].iter().find_map(|k| v[k]["path"].as_str())).map(PathBuf::from);
    let root = recorded.into_iter().chain([home.join("Dropbox"), home.join("Library/CloudStorage/Dropbox")]).find(|p| p.is_dir())?;
    Some(root.join("Camera Uploads"))
}

/// Copies `files` into `dir`, keeping their dates. A file already there with the same name
/// and size counts as backed up; a different one with the same name gets a " (2)" suffix.
pub fn copy_into(dir: &Path, files: &[PathBuf]) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (mut copied, mut skipped, mut bytes) = (0usize, 0usize, 0u64);
    let mut errors: Vec<String> = vec![];
    for src in files {
        let Ok(meta) = std::fs::metadata(src) else {
            errors.push(format!("{} is gone", src.display()));
            continue;
        };
        let Some(dest) = free_name(dir, src, meta.len()) else {
            skipped += 1;
            continue;
        };
        // Copy under a temporary name, so an interrupted copy never looks backed up.
        let part = dest.with_file_name(format!(".{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
        let done = std::fs::copy(src, &part).and_then(|_| {
            if let Ok(t) = meta.modified() {
                std::fs::File::options().write(true).open(&part)?.set_modified(t)?;
            }
            std::fs::rename(&part, &dest)
        });
        match done {
            Ok(()) => {
                copied += 1;
                bytes += meta.len();
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                errors.push(format!("{}: {e}", src.display()));
            }
        }
    }
    let mut msg = format!("Copied {} ({}) to {}", plural(copied, "file"), human_bytes(bytes), tilde(dir));
    if skipped > 0 {
        msg = if copied == 0 { format!("All {} are already in {}", plural(skipped, "file"), tilde(dir)) } else { format!("{msg}, {skipped} already there") };
    }
    match errors.first() {
        None => Ok(msg),
        Some(e) if copied + skipped > 0 => Err(format!("{msg}. {} failed: {e}", errors.len())),
        Some(e) => Err(e.clone()),
    }
}

/// Where `src` goes in `dir`, or None if a file of the same name and size is already there.
fn free_name(dir: &Path, src: &Path, len: u64) -> Option<PathBuf> {
    let name = src.file_name()?.to_string_lossy().into_owned();
    let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{e}"))).unwrap_or((name.clone(), String::new()));
    let candidates = std::iter::once(name.clone()).chain((2..).map(|n| format!("{stem} ({n}){ext}")));
    for c in candidates {
        match std::fs::metadata(dir.join(&c)) {
            Err(_) => return Some(dir.join(c)),
            Ok(m) if m.len() == len => return None,
            Ok(_) => {}
        }
    }
    None
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

