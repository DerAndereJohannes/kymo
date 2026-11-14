//! An open recording and how it gets opened: on a background thread for the window, directly
//! for `--info`, through the same [`open`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use anyhow::{Context, Result, anyhow};
use kymo_core::{AnnotationStore, Recording};

use crate::ui::axis::{Clock, format_time};

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
            start: rec.start_time(),
            duration: rec.duration(),
            channels: rec.channels().len(),
            streams: (
                rec.streams().len(),
                rec.streams()
                    .iter()
                    .filter(|s| matches!(s.content, kymo_core::StreamContent::Channels(_)))
                    .count(),
            ),
            annotations: self.annotations.len(),
            grids: rec
                .grids()
                .iter()
                .map(|g| GridSummary {
                    rate: g.rate(),
                    irregular: g.times().is_some(),
                    samples: g.len(),
                    segments: g.segments().len(),
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
    pub start: Option<jiff::civil::DateTime>,
    /// Seconds.
    pub duration: f64,
    pub channels: usize,
    /// The file's streams (XDF) and how many of them are shown as channels; 0 for formats
    /// without streams.
    pub streams: (usize, usize),
    pub annotations: usize,
    pub grids: Vec<GridSummary>,
    pub warnings: Vec<String>,
}

/// One sampling grid: channels sharing a rate and gap structure.
#[derive(Clone, Debug, PartialEq)]
pub struct GridSummary {
    /// For an irregular time base, the average rate.
    pub rate: f64,
    /// Whether the samples have times of their own (an irregular time base).
    pub irregular: bool,
    pub samples: u64,
    /// Sample ranges holding data; more than one means the recording has gaps.
    pub segments: usize,
    pub channels: Vec<String>,
}

impl GridSummary {
    /// One line without the channel names, e.g. "512 Hz, 30720 samples, 5 channels", with
    /// the exact rate ("≈299.98 Hz, ..." for the average rate of an irregular time base).
    pub fn describe(&self) -> String {
        let n = self.channels.len();
        let rate = exact_rate_text(self.rate, self.irregular);
        let mut s = format!("{rate}, {} samples, {n} channel{}", self.samples, plural(n));
        if self.segments != 1 {
            s += &format!(", {} segments", self.segments);
        }
        s
    }
}

impl Summary {
    /// The sidebar's rows, key and value. One "Sampling" row lists the rates, rounded (see
    /// [`format_rate`]), a line each with its channel count if there are several; hovering it
    /// shows per rate the exact value, channels and samples. A "Gaps" row only for
    /// recordings with gaps. No sample type: for EDF and BDF the format implies it, and ours
    /// is the type in memory (BDF's int24 is widened to int32), not the file's. No
    /// annotation count: the legend has it, with the stim channels' annotations.
    pub fn sidebar_rows(&self) -> Vec<SidebarRow> {
        // The start to the millisecond, every digit the file has on hover.
        let start = match self.start {
            Some(t) => {
                let shown = format_start(t, true);
                let full = format_start(t, false);
                SidebarRow {
                    hover: (full != shown).then_some(full),
                    ..SidebarRow::new("Start", shown)
                }
            }
            None => SidebarRow::new("Start", "unknown".into()),
        };
        let mut rows = vec![
            SidebarRow::new("Format", self.format.to_owned()),
            start,
            SidebarRow::new("Duration", format_duration(self.duration)),
            SidebarRow::new("Channels", self.channels.to_string()),
        ];
        // Only where there are several, as only XDF files have them.
        let (streams, with_channels) = self.streams;
        if streams > 1 {
            let value = if with_channels == streams {
                streams.to_string()
            } else {
                format!("{streams} ({with_channels} shown as channels)")
            };
            rows.push(SidebarRow::new("Streams", value));
        }
        // Grids of the same rate (split by sample type) count as one; irregular ones show
        // their average rate.
        let mut rates: Vec<(f64, bool, usize, u64)> = Vec::new();
        for grid in &self.grids {
            let same = |r: &&mut (f64, bool, usize, u64)| r.0 == grid.rate && r.1 == grid.irregular;
            match rates.iter_mut().find(same) {
                Some((_, _, channels, samples)) => {
                    *channels += grid.channels.len();
                    *samples = (*samples).max(grid.samples);
                }
                None => rates.push((grid.rate, grid.irregular, grid.channels.len(), grid.samples)),
            }
        }
        let value = match &rates[..] {
            [(rate, irregular, ..)] => rate_text(*rate, *irregular),
            // A line each, so the row never truncates.
            _ => rates
                .iter()
                .map(|&(rate, irregular, n, _)| {
                    format!("{} ({n} channel{})", rate_text(rate, irregular), plural(n))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let hover = rates
            .iter()
            .map(|&(rate, irregular, n, samples)| {
                let rate = exact_rate_text(rate, irregular);
                format!("{rate}: {n} channel{}, {samples} samples", plural(n))
            })
            .collect::<Vec<_>>()
            .join("\n");
        rows.push(SidebarRow {
            hover: Some(hover),
            ..SidebarRow::new("Sampling", value)
        });
        let gaps = self
            .grids
            .iter()
            .map(|g| g.segments.saturating_sub(1))
            .max();
        if let Some(gaps) = gaps.filter(|&n| n > 0) {
            rows.push(SidebarRow::new("Gaps", gaps.to_string()));
        }
        rows
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "format       {}", self.format)?;
        let start = self.start.map(|t| format_start(t, false));
        writeln!(f, "start        {}", start.as_deref().unwrap_or("unknown"))?;
        writeln!(f, "duration     {}", format_duration(self.duration))?;
        writeln!(f, "channels     {}", self.channels)?;
        if self.streams.0 > 1 {
            writeln!(
                f,
                "streams      {} ({} shown as channels)",
                self.streams.0, self.streams.1
            )?;
        }
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

pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A start time in ISO's order, which reads the same everywhere, with a space for its "T":
/// "2024-05-01 13:45:07.25". Its fraction has every digit it has, trailing zeros dropped, or
/// with `rounded` at most three (to the millisecond).
pub fn format_start(time: jiff::civil::DateTime, rounded: bool) -> String {
    let time = if rounded {
        // Only a time within a millisecond of the latest one there is fails to round.
        time.round(jiff::Unit::Millisecond).unwrap_or(time)
    } else {
        time
    };
    time.to_string().replacen('T', " ", 1)
}

/// One row of the sidebar's summary.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarRow {
    pub key: String,
    pub value: String,
    /// What hovering the value shows: the exact rates, channels and samples for Sampling,
    /// the start with every digit where it has more than three decimals.
    pub hover: Option<String>,
}

impl SidebarRow {
    fn new(key: &str, value: String) -> SidebarRow {
        SidebarRow {
            key: key.to_owned(),
            value,
            hover: None,
        }
    }
}

/// A rate as shown: at most two decimals, but at least three significant digits, trailing
/// zeros dropped ("512 Hz", "500.01 Hz", "0.5 Hz", "0.0333 Hz").
pub fn format_rate(rate: f64) -> String {
    if !(rate.is_finite() && rate > 0.0) {
        return exact_rate(rate);
    }
    let decimals = (2 - rate.log10().floor() as i32).max(2) as usize;
    let text = format!("{rate:.decimals$}");
    format!("{} Hz", text.trim_end_matches('0').trim_end_matches('.'))
}

/// A grid's rate as [`format_rate`] rounds it, "≈300 Hz" for the average rate of an
/// irregular time base.
pub fn rate_text(rate: f64, irregular: bool) -> String {
    if irregular {
        format!("\u{2248}{}", format_rate(rate))
    } else {
        format_rate(rate)
    }
}

/// A grid's rate with every digit it has, "≈299.98374 Hz" for the average rate of an
/// irregular time base.
fn exact_rate_text(rate: f64, irregular: bool) -> String {
    if irregular {
        format!("\u{2248}{}", exact_rate(rate))
    } else {
        exact_rate(rate)
    }
}

/// The rate of `grid` as [`rate_text`] gives it.
pub fn grid_rate(grid: &kymo_core::Grid) -> String {
    rate_text(grid.rate(), grid.times().is_some())
}

/// A rate with every digit it has ("500.00649253529946 Hz").
fn exact_rate(rate: f64) -> String {
    format!("{rate} Hz")
}

/// Seconds on the clock the time axis would use for them (see [`Clock::for_end`]), to the
/// millisecond, trailing zeros dropped: "12.5 s", "30:00", "1:02:03.5".
pub fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return format!("{seconds} s");
    }
    // Rounded first, so 59.9996 s reads as "1:00", not "60 s".
    let seconds = (seconds * 1000.0).round() / 1000.0;
    let text = format_time(seconds, 3, Clock::for_end(seconds));
    let (number, unit) = match text.strip_suffix(" s") {
        Some(number) => (number, " s"),
        None => (text.as_str(), ""),
    };
    format!(
        "{}{unit}",
        number.trim_end_matches('0').trim_end_matches('.')
    )
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
    use std::time::Duration;

    use kymo_testgen::TempFile;

    use super::*;

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
        let doc = load(file.path()).unwrap();
        assert_eq!(doc.path, file.path());
        let summary = doc.summary();
        assert_eq!(summary.format, "BDF+C");
        assert_eq!(summary.duration, 60.0);
        assert_eq!(summary.channels, 5);
        assert!(summary.annotations > 0);
        assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
        assert_eq!(summary.grids.len(), 1);
        let grid = &summary.grids[0];
        assert_eq!(
            (grid.rate, grid.samples, grid.segments),
            (512.0, 60 * 512, 1)
        );
        assert_eq!(grid.channels[..4], ["A1", "A2", "A3", "EXG1"]);
        assert_eq!(grid.describe(), "512 Hz, 30720 samples, 5 channels");
    }

    #[test]
    fn summarises_grids_and_gaps() {
        let file = TempFile::preset("edf+d-mixed");
        let doc = load(file.path()).unwrap();
        let summary = doc.summary();
        assert_eq!(summary.format, "EDF+D");
        let rates: Vec<f64> = summary.grids.iter().map(|g| g.rate).collect();
        assert_eq!(rates, [256.0, 512.0, 64.0]);
        assert!(summary.grids.iter().all(|g| g.segments == 4));
        assert!(summary.grids[0].describe().ends_with(", 4 segments"));
        let text = summary.to_string();
        assert!(text.contains("EDF+D"), "{text}");
        assert!(text.contains("EEG DC"), "{text}");

        // Several rates in one row, with their channels; the samples on hover; the gaps
        // counted once, as the grids share them.
        let rows = summary.sidebar_rows();
        let row = |key: &str| rows.iter().find(|r| r.key == key).unwrap();
        assert_eq!(row("Format").value, "EDF+D");
        assert_eq!(row("Duration").value, format_duration(summary.duration));
        assert_eq!(
            row("Sampling").value,
            "256 Hz (4 channels)\n512 Hz (2 channels)\n64 Hz (1 channel)"
        );
        let lines: Vec<String> = summary
            .grids
            .iter()
            .map(|g| {
                let n = g.channels.len();
                format!(
                    "{} Hz: {n} channel{}, {} samples",
                    g.rate,
                    plural(n),
                    g.samples
                )
            })
            .collect();
        assert_eq!(row("Sampling").hover, Some(lines.join("\n")));
        assert_eq!(row("Gaps").value, "3");
        assert!(!rows.iter().any(|r| r.key.starts_with("Grid")));
    }

    #[test]
    fn sidebar_names_each_fact_once() {
        let file = TempFile::preset("bdf+c-small");
        let summary = load(file.path()).unwrap().summary();
        let rows = summary.sidebar_rows();
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            ["Format", "Start", "Duration", "Channels", "Sampling"]
        );
        let value = |key: &str| &rows.iter().find(|r| r.key == key).unwrap().value;
        assert_eq!(value("Format"), "BDF+C");
        assert_eq!(value("Start"), "2024-01-02 03:04:05");
        assert_eq!(value("Duration"), "1:00");
        assert_eq!(value("Channels"), "5");
        assert_eq!(value("Sampling"), "512 Hz");
    }

    #[test]
    fn rates_show_two_decimals_but_three_significant_digits() {
        let cases = [
            (512.0, "512 Hz"),
            (500.006_492_535_299_46, "500.01 Hz"),
            (600.614_990_234_375, "600.61 Hz"),
            (2048.0, "2048 Hz"),
            (999.999, "1000 Hz"),
            (128.5, "128.5 Hz"),
            (10.0, "10 Hz"),
            (0.5, "0.5 Hz"),
            // One sample per 30 s, as hypnogram channels in EDF have.
            (1.0 / 30.0, "0.0333 Hz"),
            (0.001_234_5, "0.00123 Hz"),
        ];
        for (rate, text) in cases {
            assert_eq!(format_rate(rate), text, "{rate}");
        }
    }

    /// The sidebar shows the start to the millisecond at most, trailing zeros dropped, and
    /// every digit on hover where there are more; `--info` has every digit.
    #[test]
    fn the_start_shows_at_most_three_decimals() {
        let at = |s: &str| s.parse::<jiff::civil::DateTime>().unwrap();
        let cases = [
            ("2024-01-02T03:04:05", "2024-01-02 03:04:05", None),
            ("2024-01-02T03:04:05.25", "2024-01-02 03:04:05.25", None),
            ("2024-01-02T03:04:05.123", "2024-01-02 03:04:05.123", None),
            (
                "2024-01-02T03:04:05.123456789",
                "2024-01-02 03:04:05.123",
                Some("2024-01-02 03:04:05.123456789"),
            ),
            // Rounded, to the next second (and day) too.
            (
                "2024-01-02T03:04:05.9876",
                "2024-01-02 03:04:05.988",
                Some("2024-01-02 03:04:05.9876"),
            ),
            (
                "2024-12-31T23:59:59.9996",
                "2025-01-01 00:00:00",
                Some("2024-12-31 23:59:59.9996"),
            ),
        ];
        for (time, shown, hover) in cases {
            let summary = Summary {
                format: "EDF+C",
                start: Some(at(time)),
                duration: 2.0,
                channels: 1,
                streams: (0, 0),
                annotations: 0,
                grids: vec![GridSummary {
                    rate: 100.0,
                    irregular: false,
                    samples: 200,
                    segments: 1,
                    channels: vec!["Fz".into()],
                }],
                warnings: Vec::new(),
            };
            let rows = summary.sidebar_rows();
            let row = rows.iter().find(|r| r.key == "Start").unwrap();
            assert_eq!(row.value, shown, "{time}");
            assert_eq!(row.hover.as_deref(), hover, "{time}");
            let full = hover.unwrap_or(shown);
            assert!(
                summary
                    .to_string()
                    .contains(&format!("start        {full}\n")),
                "{time}"
            );
        }
    }

    /// A "Streams" row (and `--info` line) only where there are several streams.
    #[test]
    fn several_streams_are_counted() {
        let summary = |streams: (usize, usize)| Summary {
            format: "XDF",
            start: None,
            duration: 2.0,
            channels: 2,
            streams,
            annotations: 0,
            grids: vec![GridSummary {
                rate: 100.0,
                irregular: false,
                samples: 200,
                segments: 1,
                channels: vec!["Fz".into(), "Cz".into()],
            }],
            warnings: Vec::new(),
        };
        let row = |s: &Summary| {
            s.sidebar_rows()
                .into_iter()
                .find(|r| r.key == "Streams")
                .map(|r| r.value)
        };
        assert_eq!(row(&summary((0, 0))), None);
        assert_eq!(row(&summary((1, 1))), None);
        assert!(!summary((1, 1)).to_string().contains("streams"));
        assert_eq!(row(&summary((2, 2))).as_deref(), Some("2"));
        let nine = summary((9, 5));
        assert_eq!(row(&nine).as_deref(), Some("9 (5 shown as channels)"));
        let keys: Vec<String> = nine.sidebar_rows().into_iter().map(|r| r.key).collect();
        assert_eq!(keys[3..5], ["Channels", "Streams"]);
        assert!(
            nine.to_string()
                .contains("streams      9 (5 shown as channels)\n")
        );
    }

    /// The sidebar rounds a rate and shows the exact one on hover, with channels and samples;
    /// `--info` prints it exactly.
    #[test]
    fn rounded_rates_keep_the_exact_one() {
        let grid = |rate: f64| GridSummary {
            rate,
            irregular: false,
            samples: 1000,
            segments: 1,
            channels: vec!["Fz".into(), "Cz".into()],
        };
        let summary = |grids: Vec<GridSummary>| Summary {
            format: "BrainVision",
            start: None,
            duration: 2.0,
            channels: 2,
            streams: (0, 0),
            annotations: 0,
            grids,
            warnings: Vec::new(),
        };
        let exact = 500.006_492_535_299_46;
        let one = summary(vec![grid(exact)]);
        let rows = one.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(sampling.value, "500.01 Hz");
        assert_eq!(
            sampling.hover.as_deref(),
            Some("500.00649253529946 Hz: 2 channels, 1000 samples")
        );
        assert!(
            rows.iter()
                .filter(|r| r.key != "Sampling")
                .all(|r| r.hover.is_none())
        );
        assert!(
            one.to_string()
                .contains("500.00649253529946 Hz, 1000 samples, 2 channels"),
            "{one}"
        );

        let two = summary(vec![grid(exact), grid(250.0)]);
        let rows = two.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(
            sampling.value,
            "500.01 Hz (2 channels)\n250 Hz (2 channels)"
        );
        assert_eq!(
            sampling.hover.as_deref(),
            Some(
                "500.00649253529946 Hz: 2 channels, 1000 samples\n\
                 250 Hz: 2 channels, 1000 samples"
            )
        );

        // Grids of one rate (split by sample type) are one rate.
        let split = summary(vec![grid(exact), grid(exact)]);
        let rows = split.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(sampling.value, "500.01 Hz");
        assert_eq!(
            sampling.hover.as_deref(),
            Some("500.00649253529946 Hz: 4 channels, 1000 samples")
        );

        // An irregular grid shows its average rate as such, apart from a regular grid of
        // that rate.
        let irregular = GridSummary {
            irregular: true,
            ..grid(250.0)
        };
        let mixed = summary(vec![grid(250.0), irregular.clone()]);
        let rows = mixed.sidebar_rows();
        let sampling = rows.iter().find(|r| r.key == "Sampling").unwrap();
        assert_eq!(
            sampling.value,
            "250 Hz (2 channels)\n\u{2248}250 Hz (2 channels)"
        );
        assert!(
            sampling
                .hover
                .as_deref()
                .unwrap()
                .ends_with("\u{2248}250 Hz: 2 channels, 1000 samples")
        );
        assert!(
            irregular
                .describe()
                .starts_with("\u{2248}250 Hz, 1000 samples")
        );
    }

    #[test]
    fn info_and_window_open_the_same_way() {
        let file = TempFile::preset("bdf+c-small");
        let direct = open(file.path()).unwrap().summary();
        let loaded = load(file.path()).unwrap().summary();
        assert_eq!(direct, loaded);
    }

    #[test]
    fn a_missing_file_is_an_error_naming_it() {
        let file = TempFile::new("bdf");
        let err = load(file.path()).err().unwrap();
        let msg = error_text(&err);
        assert!(msg.starts_with("cannot read "), "{msg}");
        assert!(msg.contains(&file.path().display().to_string()), "{msg}");
        assert_eq!(msg.matches("os error").count(), 1, "cause repeated: {msg}");
        assert_eq!(
            open(file.path()).err().map(|e| error_text(&e)),
            Some(msg),
            "--info reports the same error"
        );
    }

    #[test]
    fn an_unsupported_file_is_an_error() {
        let file = TempFile::new("txt");
        std::fs::write(file.path(), "not a recording").unwrap();
        let msg = error_text(&load(file.path()).err().unwrap());
        assert!(msg.contains("not a supported file format"), "{msg}");
    }

    #[test]
    fn a_truncated_header_is_an_error() {
        let full = TempFile::preset("bdf+c-small");
        let cut = TempFile::new("bdf");
        let bytes = std::fs::read(full.path()).unwrap();
        std::fs::write(cut.path(), &bytes[..300]).unwrap();
        assert!(load(cut.path()).is_err());
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
        assert_eq!(format_duration(0.0), "0 s");
        assert_eq!(format_duration(12.5), "12.5 s");
        assert_eq!(format_duration(10.0), "10 s");
        assert_eq!(format_duration(60.0), "1:00");
        assert_eq!(format_duration(1800.0), "30:00");
        assert_eq!(format_duration(90.25), "1:30.25");
        assert_eq!(format_duration(3723.5), "1:02:03.5");
        assert_eq!(format_duration(8.0 * 3600.0), "8:00:00");
        assert_eq!(format_duration(59.9996), "1:00");
        assert_eq!(format_duration(0.0004), "0 s");
    }
}
