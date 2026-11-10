//! The recordings opened most recently, for File → Open Recent, kept between runs.

use std::path::{Path, PathBuf};

use eframe::Storage;

use crate::settings::{escape, unescape};

const RECENT_KEY: &str = "kymo.recent";

/// The recordings opened most recently, newest first, each once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recent {
    paths: Vec<PathBuf>,
}

impl Recent {
    /// How many are kept.
    pub const MAX: usize = 10;

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn get(&self, index: usize) -> Option<&Path> {
        self.paths.get(index).map(PathBuf::as_path)
    }

    /// Puts `path` first (made absolute, so it means the same whatever the working
    /// directory), dropping it from further down and the oldest beyond [`Self::MAX`].
    pub fn add(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        self.paths.retain(|p| *p != path);
        self.paths.insert(0, path);
        self.paths.truncate(Self::MAX);
    }

    /// Drops `path`, as it could not be opened.
    pub fn remove(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        self.paths.retain(|p| *p != path);
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    /// The menu's labels: each file's name, with its folder's ("a.edf — day1") where
    /// another recording has the same name.
    pub fn labels(&self) -> Vec<String> {
        let name = |p: &Path| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        self.paths
            .iter()
            .map(|path| {
                let own = name(path);
                let shared = self.paths.iter().filter(|other| name(other) == own).count() > 1;
                match path.parent().and_then(Path::file_name) {
                    Some(folder) if shared => format!("{own} — {}", folder.to_string_lossy()),
                    _ => own,
                }
            })
            .collect()
    }

    /// The list kept in `storage`: a path per line, escaped as the annotation colors are.
    pub fn load(storage: Option<&dyn Storage>) -> Recent {
        let text = storage
            .and_then(|s| s.get_string(RECENT_KEY))
            .unwrap_or_default();
        let mut paths: Vec<PathBuf> = Vec::new();
        for path in text.lines().filter_map(unescape).map(PathBuf::from) {
            if !path.as_os_str().is_empty() && !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths.truncate(Self::MAX);
        Recent { paths }
    }

    /// Keeps the list in `storage`; paths that aren't valid UTF-8 are left out.
    pub fn save(&self, storage: &mut dyn Storage) {
        let text: String = self
            .paths
            .iter()
            .filter_map(|p| p.to_str())
            .map(|p| format!("{}\n", escape(p)))
            .collect();
        storage.set_string(RECENT_KEY, text);
    }
}
