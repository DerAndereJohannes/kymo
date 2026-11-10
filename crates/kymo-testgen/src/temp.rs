//! Generated files and directories in the temp directory, deleted on drop, for tests.
//!
//! These panic on any I/O error: a test can't go on without its file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{Spec, write};

/// A path in the temp directory, unique within the process.
fn fresh_path(extension: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("kymo-test-{}-{n}.{extension}", std::process::id());
    std::env::temp_dir().join(name)
}

/// A path in the temp directory, unique within the process, whose file is deleted on drop
/// (with the files written beside it, for formats of several files).
///
/// Declare it before anything that maps the file, so the mapping is gone first: Windows
/// refuses to delete mapped files.
#[derive(Debug)]
pub struct TempFile {
    path: PathBuf,
    /// Files written beside `path`, deleted with it.
    companions: Vec<PathBuf>,
}

impl TempFile {
    /// A fresh path ending in `.extension`; nothing is created.
    pub fn new(extension: &str) -> TempFile {
        TempFile {
            path: fresh_path(extension),
            companions: Vec::new(),
        }
    }

    /// `spec` written to a fresh file; for BrainVision, [`path`](Self::path) is the header,
    /// with the data and marker files beside it.
    ///
    /// # Panics
    /// If the spec is invalid or the file can't be written.
    pub fn written(spec: &Spec) -> TempFile {
        let mut file = TempFile::new(spec.format.extension());
        file.companions = spec.format.files(&file.path).split_off(1);
        if let Err(error) = write(spec, &file.path) {
            panic!("cannot write {}: {error}", file.path.display());
        }
        file
    }

    /// The preset `name` (see [`Spec::preset`]) written to a fresh file.
    ///
    /// # Panics
    /// If there is no such preset or the file can't be written.
    pub fn preset(name: &str) -> TempFile {
        let Some(spec) = Spec::preset(name) else {
            panic!("no preset {name:?}");
        };
        TempFile::written(&spec)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path of the companion file with `extension` (BrainVision's "eeg" or "vmrk").
    ///
    /// # Panics
    /// If there is none.
    pub fn companion(&self, extension: &str) -> &Path {
        match self
            .companions
            .iter()
            .find(|p| p.extension().is_some_and(|e| e == extension))
        {
            Some(path) => path,
            None => panic!("{} has no .{extension} companion", self.path.display()),
        }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        for companion in &self.companions {
            let _ = std::fs::remove_file(companion);
        }
    }
}

/// A fresh directory in the temp directory, deleted with its contents on drop.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// # Panics
    /// If the directory can't be created.
    pub fn new() -> TempDir {
        let path = fresh_path("dir");
        if let Err(error) = std::fs::create_dir_all(&path) {
            panic!("cannot create {}: {error}", path.display());
        }
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Names of the files in the directory, sorted.
    ///
    /// # Panics
    /// If the directory can't be read.
    pub fn files(&self) -> Vec<String> {
        let entries = match std::fs::read_dir(&self.0) {
            Ok(entries) => entries,
            Err(error) => panic!("cannot read {}: {error}", self.0.display()),
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        names
    }
}

impl Default for TempDir {
    fn default() -> Self {
        TempDir::new()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
