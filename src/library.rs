//! Folder scanning. Files that share a directory and stem (BF_0001.JPG + BF_0001.DNG)
//! become one photo, so a decision always applies to every file of that shot.
//! A Claude fix saved as BF_0001-fixed.jpg joins the same shot.

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
    /// Every file deleted with this photo, primary included, with its size.
    pub files: Vec<(PathBuf, u64)>,
    /// A saved Claude fix (`<stem>-fixed.jpg`), shown instead of the primary.
    pub edit: Option<PathBuf>,
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
        e if RAWS.contains(&e) => (Kind::Raw, 2),
        "heic" | "heif" => (Kind::Raw, 3),
        "mov" | "mp4" | "m4v" => (Kind::Video, 4),
        _ => return None,
    })
}

const RAWS: &[&str] = &["dng", "arw", "cr2", "cr3", "nef", "raf", "rw2", "orf", "x3f"];
const SIDECARS: &[&str] = &["xmp", "pp3", "dop"];
pub const FIXED_SUFFIX: &str = "-fixed";

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

pub fn is_raw(p: &Path) -> bool {
    RAWS.contains(&ext_of(p).as_str())
}

/// Raw edits live in sidecars, so they go when the raw goes.
fn is_sidecar(p: &Path) -> bool {
    SIDECARS.contains(&ext_of(p).as_str())
}

fn is_jpeg(p: &Path) -> bool {
    matches!(ext_of(p).as_str(), "jpg" | "jpeg")
}

impl Photo {
    /// Path and kind to preview: the saved fix if there is one.
    pub fn shown(&self) -> (&Path, Kind) {
        match &self.edit {
            Some(e) => (e, Kind::Jpeg),
            None => (&self.primary, self.kind),
        }
    }

    pub fn raw(&self) -> Option<&Path> {
        self.files.iter().map(|f| f.0.as_path()).find(|f| is_raw(f))
    }

    /// The camera's JPEG, not a saved fix.
    pub fn camera_jpeg(&self) -> Option<&Path> {
        self.files.iter().map(|f| f.0.as_path()).find(|f| is_jpeg(f) && Some(*f) != self.edit.as_deref())
    }

    /// Shot as RAW + JPEG, so either half can be dropped.
    pub fn is_pair(&self) -> bool {
        self.raw().is_some() && self.camera_jpeg().is_some()
    }

    /// Files to remove to keep only the JPEG (`jpeg_only`) or only the raw.
    /// A saved fix is always kept.
    pub fn drop_half(&self, jpeg_only: bool) -> Vec<(PathBuf, u64)> {
        let edit = self.edit.as_deref();
        self.files
            .iter()
            .filter(|f| {
                if jpeg_only { is_raw(&f.0) || is_sidecar(&f.0) } else { is_jpeg(&f.0) && Some(f.0.as_path()) != edit }
            })
            .cloned()
            .collect()
    }
}

pub fn scan(root: &Path) -> Vec<Photo> {
    struct Group {
        files: Vec<(PathBuf, u64)>,
        best: Option<(u8, Kind, PathBuf, SystemTime)>,
        sidecars: Vec<(PathBuf, u64)>,
        edit: Option<PathBuf>,
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
            let fixed = matches!(ext.as_str(), "jpg" | "jpeg") && stem.ends_with(FIXED_SUFFIX);
            let stem = if fixed { &stem[..stem.len() - FIXED_SUFFIX.len()] } else { stem };
            let key = (dir.clone(), stem.to_ascii_lowercase());
            let size = meta.len();
            if SIDECARS.contains(&ext.as_str()) {
                groups
                    .entry(key)
                    .or_insert_with(|| Group { files: vec![], best: None, sidecars: vec![], edit: None })
                    .sidecars
                    .push((path, size));
                continue;
            }
            let Some((kind, rank)) = kind_of(&ext) else { continue };
            // A fix only becomes the primary when its originals are gone.
            let rank = if fixed { u8::MAX } else { rank };
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let g = groups
                .entry(key)
                .or_insert_with(|| Group { files: vec![], best: None, sidecars: vec![], edit: None });
            g.files.push((path.clone(), size));
            if fixed {
                g.edit = Some(path.clone());
            }
            if g.best.as_ref().is_none_or(|b| rank < b.0) {
                g.best = Some((rank, kind, path, mtime));
            }
        }
    }

    let mut photos: Vec<(SystemTime, Photo)> = groups
        .into_values()
        .filter_map(|g| {
            let (_, kind, primary, mtime) = g.best?;
            let edit = g.edit.filter(|e| *e != primary);
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
                    files,
                    edit,
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
