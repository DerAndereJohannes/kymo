//! An open recording and how it gets opened: on a background thread for the window, directly
//! for `--info`, through the same [`open`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::{Context, Result, anyhow};
use kymo_core::{AnnotationStore, Recording, SampleType};

/// A recording opened for viewing, with the annotation store beside it.
pub struct Document {
    pub path: PathBuf,
    pub recording: Arc<Recording>,
    pub annotations: AnnotationStore,
}

/// Opens and validates a recording. Reads the header and index, never the samples.
pub fn open(path: &Path) -> Result<Document> {
    // The core's errors already name the file.
    let opened = kymo_core::open(path)?;
    let (recording, annotations) =
        Recording::new(opened).with_context(|| format!("invalid recording {}", path.display()))?;
    Ok(Document {
        path: path.to_owned(),
        recording: Arc::new(recording),
        annotations: AnnotationStore::from_data(annotations),
    })
}

impl Document {
    pub fn summary(&self) -> Summary {
        let rec = &self.recording;
        Summary {
            format: rec.format(),
            start: rec.start_time().map(|t| t.to_string()),
            duration: rec.duration(),
            channels: rec.channels().len(),
            annotations: self.annotations.len(),
            grids: rec
                .grids()
                .iter()
                .map(|g| GridSummary {
                    rate: g.rate(),
                    samples: g.len(),
                    segments: g.segments().len(),
                    sample_type: g.sample_type(),
                    channels: g
                        .channels()
                        .iter()
                        .map(|&c| rec.channel(c).name.clone())
                        .collect(),
                })
                .collect(),
            warnings: rec.warnings().to_vec(),
        }
    }

    /// The file name, for window titles.
    pub fn name(&self) -> String {
        file_name(&self.path)
    }
}

/// The file name of `path`, or the whole path if it has none.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// What the header panel and `--info` show.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub format: &'static str,
    /// Wall-clock start, if the file has one.
    pub start: Option<String>,
    /// Seconds.
    pub duration: f64,
    pub channels: usize,
    pub annotations: usize,
    pub grids: Vec<GridSummary>,
    pub warnings: Vec<String>,
}

/// One sampling grid: channels sharing a rate and gap structure.
#[derive(Clone, Debug, PartialEq)]
pub struct GridSummary {
    pub rate: f64,
    pub samples: u64,
    /// Sample ranges holding data; more than one means the recording has gaps.
    pub segments: usize,
    pub sample_type: SampleType,
    pub channels: Vec<String>,
}

impl GridSummary {
    /// One line without the channel names, e.g. "512 Hz, 30720 samples, int32, 5 channels".
    pub fn describe(&self) -> String {
        let mut s = format!(
            "{} Hz, {} samples, {}, {} channel{}",
            self.rate,
            self.samples,
            sample_type_name(self.sample_type),
            self.channels.len(),
            plural(self.channels.len()),
        );
        if self.segments != 1 {
            s += &format!(", {} segments", self.segments);
        }
        s
    }
}

impl Summary {
    /// The sidebar's rows, key and value. The sample type goes with the format, and with one
    /// grid its sample count goes with the duration, leaving the rate (and gaps) for
    /// "Sampling"; several grids each keep their samples and channels.
    pub fn sidebar_rows(&self) -> Vec<(String, String)> {
        let mut types: Vec<&str> = Vec::new();
        for grid in &self.grids {
            let name = sample_type_name(grid.sample_type);
            if !types.contains(&name) {
                types.push(name);
            }
        }
        let format = if types.is_empty() {
            self.format.to_owned()
        } else {
            format!("{} ({})", self.format, types.join(", "))
        };
        let mut duration = format_duration(self.duration);
        if let [grid] = &self.grids[..] {
            duration += &format!(" ({} samples)", grid.samples);
        }
        let mut rows = vec![
            ("Format".to_owned(), format),
            (
                "Start".to_owned(),
                self.start.clone().unwrap_or_else(|| "unknown".into()),
            ),
            ("Duration".to_owned(), duration),
            ("Channels".to_owned(), self.channels.to_string()),
            ("Annotations".to_owned(), self.annotations.to_string()),
        ];
        let single = self.grids.len() == 1;
        for (i, grid) in self.grids.iter().enumerate() {
            let mut value = format!("{} Hz", grid.rate);
            if !single {
                value += &format!(
                    ", {} samples, {} channel{}",
                    grid.samples,
                    grid.channels.len(),
                    plural(grid.channels.len())
                );
            }
            if grid.segments != 1 {
                value += &format!(", {} segments", grid.segments);
            }
            let key = if single {
                "Sampling".to_owned()
            } else {
                format!("Grid {}", i + 1)
            };
            rows.push((key, value));
        }
        rows
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "format       {}", self.format)?;
        writeln!(
            f,
            "start        {}",
            self.start.as_deref().unwrap_or("unknown")
        )?;
        writeln!(f, "duration     {}", format_duration(self.duration))?;
        writeln!(f, "channels     {}", self.channels)?;
        writeln!(f, "annotations  {}", self.annotations)?;
        for (i, grid) in self.grids.iter().enumerate() {
            writeln!(f, "grid {:<7} {}", i + 1, grid.describe())?;
            writeln!(f, "             {}", grid.channels.join(", "))?;
        }
        for warning in &self.warnings {
            writeln!(f, "warning      {warning}")?;
        }
        Ok(())
    }
}

pub fn sample_type_name(t: SampleType) -> &'static str {
    match t {
        SampleType::I16 => "int16",
        SampleType::I32 => "int32",
        SampleType::F32 => "float32",
        _ => "other",
    }
}

pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Seconds as "12.500 s" below a minute, "1:02:03.500" (h:mm:ss) above.
pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return format!("{seconds} s");
    }
    let millis = (seconds * 1000.0).round() as u64;
    if millis < 60_000 {
        return format!("{}.{:03} s", millis / 1000, millis % 1000);
    }
    let (h, rest) = (millis / 3_600_000, millis % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    format!("{h}:{m:02}:{s:02}.{ms:03}")
}

/// An error and its causes on one line, joined by ": ". A cause is left out when the message
/// before it already ends with it, as kymo-core's errors include their source.
pub fn error_text(error: &anyhow::Error) -> String {
    let mut text = String::new();
    for cause in error.chain().map(ToString::to_string) {
        if text.is_empty() {
            text = cause;
        } else if !text.ends_with(&cause) {
            text = format!("{text}: {cause}");
        }
    }
    text
}

/// A recording being opened on a background thread, so the window never waits for the disk.
pub struct Loading<T = Document> {
    path: PathBuf,
    result: Receiver<Result<T>>,
}

impl<T: Send + 'static> Loading<T> {
    /// Starts `open(path)` on a thread. `notify` runs once the result is ready (or the
    /// thread died), so the window can repaint and pick it up with [`poll`](Self::poll).
    pub fn start(
        path: PathBuf,
        open: impl FnOnce(&Path) -> Result<T> + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread_path = path.clone();
        // If spawning fails, `tx` is dropped with the closure and `poll` reports it.
        let _ = std::thread::Builder::new()
            .name("kymo-open".into())
            .spawn(move || {
                // Dropped after `tx` (locals drop in reverse order), so a panic still ends
                // with a disconnected channel and a notification.
                let _notify = OnDrop(Some(notify));
                let _ = tx.send(open(&thread_path));
            });
        Loading { path, result: rx }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The result, once it is there. Call again after it returned `Some` and it reports an
    /// error.
    pub fn poll(&self) -> Option<Result<T>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(anyhow!(
                "opening {} failed unexpectedly",
                self.path.display()
            ))),
        }
    }
}

struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// A file in the temp directory, deleted on drop. Declare it before anything that maps
    /// it, so the mapping is gone first (Windows refuses to delete mapped files).
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(extension: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("kymo-viewer-test-{}-{n}.{extension}", std::process::id());
            TempFile(std::env::temp_dir().join(name))
        }

        fn preset(name: &str) -> Self {
            let spec = kymo_testgen::Spec::preset(name).unwrap();
            let file = TempFile::new(spec.format.extension());
            kymo_testgen::write(&spec, &file.0).unwrap();
            file
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Opens through the window's path: a background thread, then a notification.
    fn load(path: &Path) -> Result<Document> {
        let (tx, rx) = mpsc::channel();
        let loading = Loading::start(path.to_owned(), open, move || tx.send(()).unwrap());
        assert_eq!(loading.path(), path);
        rx.recv_timeout(Duration::from_secs(30))
            .expect("no notification");
        loading.poll().expect("notified without a result")
    }

    #[test]
    fn opens_a_generated_file() {
        let file = TempFile::preset("bdf+c-small");
        let doc = load(&file.0).unwrap();
        assert_eq!(doc.path, file.0);
        let summary = doc.summary();
        assert_eq!(summary.format, "BDF+C");
        assert_eq!(summary.duration, 60.0);
        assert_eq!(summary.channels, 5);
        assert!(summary.annotations > 0);
        assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
        assert_eq!(summary.grids.len(), 1);
        let grid = &summary.grids[0];
        assert_eq!(
            (grid.rate, grid.samples, grid.segments, grid.sample_type),
            (512.0, 60 * 512, 1, SampleType::I32)
        );
        assert_eq!(grid.channels[..4], ["A1", "A2", "A3", "EXG1"]);
        assert_eq!(grid.describe(), "512 Hz, 30720 samples, int32, 5 channels");
    }

    #[test]
    fn summarises_grids_and_gaps() {
        let file = TempFile::preset("edf+d-mixed");
        let doc = load(&file.0).unwrap();
        let summary = doc.summary();
        assert_eq!(summary.format, "EDF+D");
        let rates: Vec<f64> = summary.grids.iter().map(|g| g.rate).collect();
        assert_eq!(rates, [256.0, 512.0, 64.0]);
        assert!(summary.grids.iter().all(|g| g.segments == 4));
        assert!(summary.grids[0].describe().ends_with(", 4 segments"));
        let text = summary.to_string();
        assert!(text.contains("EDF+D"), "{text}");
        assert!(text.contains("EEG DC"), "{text}");

        // Several grids: each row keeps its samples and channels, the duration none.
        let rows = summary.sidebar_rows();
        let value = |key: &str| &rows.iter().find(|(k, _)| k == key).unwrap().1;
        assert_eq!(value("Format"), "EDF+D (int16)");
        assert_eq!(*value("Duration"), format_duration(summary.duration));
        assert!(!rows.iter().any(|(k, _)| k == "Sampling"));
        for (i, grid) in summary.grids.iter().enumerate() {
            let n = grid.channels.len();
            assert_eq!(
                *value(&format!("Grid {}", i + 1)),
                format!(
                    "{} Hz, {} samples, {n} channel{}, 4 segments",
                    grid.rate,
                    grid.samples,
                    plural(n)
                )
            );
        }
    }

    #[test]
    fn sidebar_names_each_fact_once() {
        let file = TempFile::preset("bdf+c-small");
        let summary = load(&file.0).unwrap().summary();
        let rows = summary.sidebar_rows();
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "Format",
                "Start",
                "Duration",
                "Channels",
                "Annotations",
                "Sampling"
            ]
        );
        let value = |key: &str| &rows.iter().find(|(k, _)| k == key).unwrap().1;
        assert_eq!(value("Format"), "BDF+C (int32)");
        assert_eq!(value("Duration"), "0:01:00.000 (30720 samples)");
        assert_eq!(value("Channels"), "5");
        assert_eq!(value("Sampling"), "512 Hz");
    }

    #[test]
    fn info_and_window_open_the_same_way() {
        let file = TempFile::preset("bdf+c-small");
        let direct = open(&file.0).unwrap().summary();
        let loaded = load(&file.0).unwrap().summary();
        assert_eq!(direct, loaded);
    }

    #[test]
    fn a_missing_file_is_an_error_naming_it() {
        let file = TempFile::new("bdf");
        let err = load(&file.0).err().unwrap();
        let msg = error_text(&err);
        assert!(msg.starts_with("cannot read "), "{msg}");
        assert!(msg.contains(&file.0.display().to_string()), "{msg}");
        assert_eq!(msg.matches("os error").count(), 1, "cause repeated: {msg}");
        assert_eq!(
            open(&file.0).err().map(|e| error_text(&e)),
            Some(msg),
            "--info reports the same error"
        );
    }

    #[test]
    fn an_unsupported_file_is_an_error() {
        let file = TempFile::new("txt");
        std::fs::write(&file.0, "not a recording").unwrap();
        let msg = error_text(&load(&file.0).err().unwrap());
        assert!(msg.contains("not a supported file format"), "{msg}");
    }

    #[test]
    fn a_truncated_header_is_an_error() {
        let full = TempFile::preset("bdf+c-small");
        let cut = TempFile::new("bdf");
        let bytes = std::fs::read(&full.0).unwrap();
        std::fs::write(&cut.0, &bytes[..300]).unwrap();
        assert!(load(&cut.0).is_err());
    }

    #[test]
    fn a_dead_loader_thread_reports_an_error() {
        let (tx, rx) = mpsc::channel::<Result<Document>>();
        drop(tx);
        let loading = Loading {
            path: "x.edf".into(),
            result: rx,
        };
        let msg = loading.poll().unwrap().err().unwrap().to_string();
        assert!(msg.contains("x.edf"), "{msg}");
    }

    #[test]
    fn error_text_joins_causes_once() {
        let inner = anyhow!("disk on fire");
        assert_eq!(error_text(&inner), "disk on fire");
        let wrapped = inner.context("cannot read a.edf");
        assert_eq!(error_text(&wrapped), "cannot read a.edf: disk on fire");
        let repeated = anyhow!("disk on fire").context("cannot read a.edf: disk on fire");
        assert_eq!(error_text(&repeated), "cannot read a.edf: disk on fire");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0.0), "0.000 s");
        assert_eq!(format_duration(12.5), "12.500 s");
        assert_eq!(format_duration(60.0), "0:01:00.000");
        assert_eq!(format_duration(3723.5), "1:02:03.500");
        assert_eq!(format_duration(8.0 * 3600.0), "8:00:00.000");
        assert_eq!(format_duration(59.9996), "0:01:00.000");
        assert_eq!(format_duration(0.0004), "0.000 s");
    }
}
