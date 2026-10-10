//! Folder scanning. Files that share a directory and stem (BF_0001.JPG + BF_0001.DNG)
//! become one photo, so a decision always applies to every file of that shot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Jpeg,
    Image,
    Raw,
    Video,
}

pub struct Photo {
    /// Stable for the lifetime of a scan; caches key on it.
    pub id: u32,
    /// Path used for previews.
    pub primary: PathBuf,
    pub kind: Kind,
    /// Every file deleted with this photo, primary included.
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    /// Path relative to the scan root, used as the persistence key.
    pub key: String,
    pub name: String,
}

fn kind_of(ext: &str) -> Option<(Kind, u8)> {
    // Lower rank = better preview source.
    Some(match ext {
        "jpg" | "jpeg" => (Kind::Jpeg, 0),
        "png" | "webp" | "tif" | "tiff" => (Kind::Image, 1),
        "dng" | "arw" | "cr2" | "cr3" | "nef" | "raf" | "rw2" | "orf" | "x3f" => (Kind::Raw, 2),
        "heic" | "heif" => (Kind::Raw, 3),
        "mov" | "mp4" | "m4v" => (Kind::Video, 4),
        _ => return None,
    })
}

const SIDECARS: &[&str] = &["xmp", "pp3", "dop"];

pub fn scan(root: &Path) -> Vec<Photo> {
    struct Group {
        files: Vec<(PathBuf, u64)>,
        best: Option<(u8, Kind, PathBuf, SystemTime)>,
        sidecars: Vec<(PathBuf, u64)>,
    }
    let mut groups: HashMap<(PathBuf, String), Group> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skips dotfiles and exFAT AppleDouble "._" files.
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let Some((stem, ext)) = name.rsplit_once('.') else { continue };
            let ext = ext.to_ascii_lowercase();
            let key = (dir.clone(), stem.to_ascii_lowercase());
            let size = meta.len();
            if SIDECARS.contains(&ext.as_str()) {
                groups
                    .entry(key)
                    .or_insert_with(|| Group { files: vec![], best: None, sidecars: vec![] })
                    .sidecars
                    .push((path, size));
                continue;
            }
            let Some((kind, rank)) = kind_of(&ext) else { continue };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let g = groups
                .entry(key)
                .or_insert_with(|| Group { files: vec![], best: None, sidecars: vec![] });
            g.files.push((path.clone(), size));
            if g.best.as_ref().is_none_or(|b| rank < b.0) {
                g.best = Some((rank, kind, path, mtime));
            }
        }
    }

    let mut photos: Vec<(SystemTime, Photo)> = groups
        .into_values()
        .filter_map(|g| {
            let (_, kind, primary, mtime) = g.best?;
            let mut files = g.files;
            files.extend(g.sidecars);
            let bytes = files.iter().map(|f| f.1).sum();
            let key = primary
                .strip_prefix(root)
                .unwrap_or(&primary)
                .to_string_lossy()
                .into_owned();
            let name = primary.file_name()?.to_string_lossy().into_owned();
            Some((
                mtime,
                Photo {
                    id: 0,
                    primary,
                    kind,
                    files: files.into_iter().map(|f| f.0).collect(),
                    bytes,
                    key,
                    name,
                },
            ))
        })
        .collect();

    // Capture order: mtime is what cameras stamp, path breaks ties within a second.
    photos.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.primary.cmp(&b.1.primary)));
    photos
        .into_iter()
        .enumerate()
        .map(|(i, (_, mut p))| {
            p.id = i as u32;
            p
        })
        .collect()
}

pub fn human_bytes(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}
